//! storage/agent_hub_repo — Agent Hub canonical SQLite 持久化
//!
//! Business Logic（为什么需要这个模块）:
//!     Multi-CLI Agent Hub 需要可崩溃恢复的 canonical 状态：scope、资产、DAG revision、
//!     target binding、materialization 与 conflict。写入必须经 maintenance gate，旧库升级幂等。
//!
//! Code Logic（这个模块做什么）:
//!     `ensure_schema` 建 Agent Hub 表与索引（含 Gate D plugin 边表），并对
//!     project_mappings/checkout_bindings/projection_jobs 做 PRAGMA 列升级；
//!     `insert_scope/insert_asset/append_revision/upsert_target_binding`、
//!     mapping/binding/materialization/projection_job 写路径走 `with_shared_write_lease`；
//!     revision 多行更新同事务；package revision 与 component/residual refs 同事务。

use crate::agent_hub::assets::{
    canonical_bytes, ensure_kind_matches_payload, from_canonical_bytes, PortableAssetPayload,
};
use crate::agent_hub::models::{
    AdoptionRecord, AdoptionState, AgentHubConflict, AgentTarget, AssetKind, AssetPolicy,
    DesiredPresence, LogicalAsset, Materialization, MaterializationStatus, NewLogicalAsset,
    NewMaterialization, NewProjectionJob, NewRevision, NewScopeNode, NewTargetBinding,
    ProjectionJob, ProjectionJobState, ProjectionPayloadKind, Revision, RevisionId,
    RevisionOperation, RevisionOriginKind, ScopeKind, ScopeNode, TargetBinding,
    UserInstructionOwnershipRecord, UserInstructionPlanClaim, UserInstructionPlanRecord,
};
use crate::agent_hub::object_store::ObjectStore;
use crate::agent_hub::plugins::ownership::{decide_component_delete, ComponentDeleteDecision};
use crate::agent_hub::plugins::{
    canonical_plugin_package_bytes, canonical_portable_hook_bytes, from_plugin_package_bytes,
    from_portable_hook_bytes, sort_plugin_package_payload, validate_plugin_package_payload,
    validate_portable_hook, ComponentOwnership, PluginComponentRef, PluginPackagePayload,
    PluginResidualRef, PortableHook, ResidualKind,
};
use crate::agent_hub::snapshot::envelope::default_snapshot_limits;
use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::{Row, Sqlite, Transaction};
#[cfg(any(test, debug_assertions))]
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// Agent Hub SQLite 仓库。
///
/// Business Logic（为什么需要这个结构体）:
///     sidecar owner 与单测 fixture 共用同一 schema/语义；生产路径共享 restore 写屏障。
///
/// Code Logic（这个结构体做什么）:
///     持有 SqlitePool + DatabaseMaintenanceGate；test/debug 另持 per-instance import fault 槽位。
#[derive(Clone)]
pub struct AgentHubRepo {
    pool: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
    /// 测试/debug：import TX 一次消费故障注入（per-repo，避免并行测试抢进程全局槽）。
    #[cfg(any(test, debug_assertions))]
    import_fault: Arc<AtomicU8>,
}

/// package 删除时单个 component 的决策结果。
///
/// Business Logic: preview/测试需要 asset id + kind + decision。
/// Code Logic: 聚合字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentDeleteOutcome {
    /// component 逻辑资产 id
    pub component_asset_id: String,
    /// component kind
    pub kind: AssetKind,
    /// 引用派生决策
    pub decision: ComponentDeleteDecision,
}

/// `delete_plugin_package_with_ownership` 的返回。
///
/// Business Logic: 调用方需要 package tombstone revision 与每个 child 决策。
/// Code Logic: package_revision + decisions 列表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPackageDeleteResult {
    /// package 资产 id
    pub package_asset_id: String,
    /// package Delete revision
    pub package_revision: Revision,
    /// 每个 component 的决策
    pub component_decisions: Vec<ComponentDeleteOutcome>,
}

impl AgentHubRepo {
    /// 测试/局部 fixture 构造。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     隔离内存库不需要跨进程 maintenance 锁。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `with_gate(pool, Arc::new(DatabaseMaintenanceGate::new()))`。
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_gate(pool, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     ordinary writer 与 restore exclusive 必须共用同一 gate。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 pool + Arc gate；test/debug 初始化独立 import fault 槽位。
    pub fn with_gate(pool: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self {
            pool,
            gate,
            #[cfg(any(test, debug_assertions))]
            import_fault: Arc::new(AtomicU8::new(0)),
        }
    }

    /// 返回底层 pool（fixture 跨实例验证）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单测需要第二 repo 实例读同一库。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone pool。
    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    /// 幂等创建 Agent Hub 领域表与索引。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧库无 sqlx migrate；CREATE IF NOT EXISTS 保证升级与二次调用安全，不丢行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     建 13 张表 + 关键唯一/查询索引；bootstrap 不经 write lease。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        for stmt in AGENT_HUB_SCHEMA_STATEMENTS {
            sqlx::query(stmt).execute(pool).await?;
        }
        migrate_agent_hub_columns(pool).await?;
        Ok(())
    }

    /// 插入 scope 节点。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     资产必须挂在稳定 scope 下，才能跨设备映射而不依赖本机绝对路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成/使用 id，INSERT 后返回完整 ScopeNode。
    pub async fn insert_scope(&self, input: NewScopeNode) -> Result<ScopeNode, AppError> {
        with_shared_write_lease(&self.gate, async {
            let id = input
                .id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let created_at = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO agent_hub_scopes (id, kind, hub_project_id, relative_path, created_at)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(input.kind.as_str())
            .bind(&input.hub_project_id)
            .bind(&input.relative_path)
            .bind(&created_at)
            .execute(&self.pool)
            .await?;
            Ok(ScopeNode {
                id,
                kind: input.kind,
                hub_project_id: input.hub_project_id,
                relative_path: input.relative_path,
                created_at,
            })
        })
        .await
    }

    /// 按 id 读取 scope。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     写入资产前需确认 scope 存在。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT 一行并解析枚举。
    pub async fn get_scope(&self, id: &str) -> Result<Option<ScopeNode>, AppError> {
        let row = sqlx::query(
            "SELECT id, kind, hub_project_id, relative_path, created_at
             FROM agent_hub_scopes WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_scope(&r)).transpose()
    }

    /// 插入逻辑资产并登记初始 lineage。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     同 scope 下唯一键 `(scope, kind, origin_namespace, logical_key)` 标识独立资产。
    ///
    /// Code Logic（这个函数做什么）:
    ///     事务内 INSERT assets + asset_lineages(self)；冲突返回 Conflict。
    pub async fn insert_asset(&self, input: NewLogicalAsset) -> Result<LogicalAsset, AppError> {
        with_shared_write_lease(&self.gate, async {
            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            let mut tx = self.pool.begin().await?;
            let result = sqlx::query(
                "INSERT INTO agent_hub_assets
                 (id, scope_id, kind, origin_namespace, logical_key, display_name, policy,
                  current_revision_id, deleted_at, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?, ?)",
            )
            .bind(&id)
            .bind(&input.scope_id)
            .bind(input.kind.as_str())
            .bind(&input.origin_namespace)
            .bind(&input.logical_key)
            .bind(&input.display_name)
            .bind(input.policy.as_str())
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await;
            if let Err(e) = result {
                if is_unique_violation(&e) {
                    return Err(AppError::conflict(
                        "agent_hub_asset_unique_key_conflict".to_string(),
                    ));
                }
                return Err(e.into());
            }
            // 初始 lineage = 自身 id
            sqlx::query(
                "INSERT INTO agent_hub_asset_lineages (asset_id, lineage_id, created_at)
                 VALUES (?, ?, ?)",
            )
            .bind(&id)
            .bind(&id)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(LogicalAsset {
                id,
                scope_id: input.scope_id,
                kind: input.kind,
                origin_namespace: input.origin_namespace,
                logical_key: input.logical_key,
                display_name: input.display_name,
                policy: input.policy,
                current_revision_id: None,
                deleted_at: None,
                created_at: now.clone(),
                updated_at: now,
            })
        })
        .await
    }

    /// 按 id 读取逻辑资产。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     追加 revision / 绑定 target 前需要完整资产视图。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT 并解析枚举与可选 revision id。
    pub async fn get_asset(&self, id: &str) -> Result<Option<LogicalAsset>, AppError> {
        let row = sqlx::query(
            "SELECT id, scope_id, kind, origin_namespace, logical_key, display_name, policy,
                    current_revision_id, deleted_at, created_at, updated_at
             FROM agent_hub_assets WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_asset(&r)).transpose()
    }

    /// 按唯一键查询资产。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     纳管/扫描时按 (scope, kind, namespace, key) 查找既有资产。
    ///
    /// Code Logic（这个函数做什么）:
    ///     唯一索引查询。
    pub async fn get_asset_by_unique_key(
        &self,
        scope_id: &str,
        kind: AssetKind,
        origin_namespace: &str,
        logical_key: &str,
    ) -> Result<Option<LogicalAsset>, AppError> {
        let row = sqlx::query(
            "SELECT id, scope_id, kind, origin_namespace, logical_key, display_name, policy,
                    current_revision_id, deleted_at, created_at, updated_at
             FROM agent_hub_assets
             WHERE scope_id = ? AND kind = ? AND origin_namespace = ? AND logical_key = ?",
        )
        .bind(scope_id)
        .bind(kind.as_str())
        .bind(origin_namespace)
        .bind(logical_key)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_asset(&r)).transpose()
    }

    /// 追加不可变 revision 并推进资产 head。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     每次接受的编辑/删除必须形成 DAG 节点；generation 用于祖先搜索加速。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验 delete 无 payload、parent 存在；generation=max(parent)+1 或 0；
    ///     同事务写 revisions/parents/assets.current_revision_id。
    pub async fn append_revision(&self, input: NewRevision) -> Result<Revision, AppError> {
        with_shared_write_lease(&self.gate, async {
            if input.operation == RevisionOperation::Delete
                && (input.payload_hash.is_some() || input.tree_manifest_hash.is_some())
            {
                return Err(AppError::validation(
                    "agent_hub_delete_revision_rejects_payload_hash".to_string(),
                ));
            }

            let mut tx = self.pool.begin().await?;

            // 校验 lineage/asset 存在
            let asset_exists: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_assets WHERE id = ?")
                    .bind(&input.asset_lineage_id)
                    .fetch_one(&mut *tx)
                    .await?;
            if asset_exists == 0 {
                // lineage 也可能是远端 alias，但 Task1 至少要求 assets 行或 lineage 表登记
                let lineage_exists: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM agent_hub_asset_lineages WHERE lineage_id = ?",
                )
                .bind(&input.asset_lineage_id)
                .fetch_one(&mut *tx)
                .await?;
                if lineage_exists == 0 {
                    return Err(AppError::not_found(
                        "agent_hub_asset_lineage_not_found".to_string(),
                    ));
                }
            }

            let mut max_parent_generation: Option<u64> = None;
            for parent in &input.parents {
                let gen: Option<i64> =
                    sqlx::query_scalar("SELECT generation FROM agent_hub_revisions WHERE id = ?")
                        .bind(parent.as_str())
                        .fetch_optional(&mut *tx)
                        .await?;
                let Some(g) = gen else {
                    return Err(AppError::validation(format!(
                        "agent_hub_revision_parent_missing:{}",
                        parent.as_str()
                    )));
                };
                let g = g as u64;
                max_parent_generation = Some(max_parent_generation.map_or(g, |m| m.max(g)));
            }
            let generation = max_parent_generation.map_or(0, |m| m.saturating_add(1));

            let insert_result = sqlx::query(
                "INSERT INTO agent_hub_revisions
                 (id, asset_lineage_id, generation, operation, origin_kind, origin_target,
                  origin_replica_id, payload_hash, tree_manifest_hash, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(input.id.as_str())
            .bind(&input.asset_lineage_id)
            .bind(generation as i64)
            .bind(input.operation.as_str())
            .bind(input.origin_kind.as_str())
            .bind(input.origin_target.map(|t| t.as_str()))
            .bind(&input.origin_replica_id)
            .bind(&input.payload_hash)
            .bind(&input.tree_manifest_hash)
            .bind(&input.created_at)
            .execute(&mut *tx)
            .await;
            if let Err(e) = insert_result {
                if is_unique_violation(&e) {
                    return Err(AppError::conflict(
                        "agent_hub_revision_id_conflict".to_string(),
                    ));
                }
                return Err(e.into());
            }

            for (pos, parent) in input.parents.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO agent_hub_revision_parents
                     (revision_id, parent_revision_id, parent_order)
                     VALUES (?, ?, ?)",
                )
                .bind(input.id.as_str())
                .bind(parent.as_str())
                .bind(pos as i64)
                .execute(&mut *tx)
                .await?;
            }

            // 推进资产 head：lineage_id 通常等于 asset.id；否则按 lineage 反查 asset
            let asset_id: Option<String> =
                sqlx::query_scalar("SELECT id FROM agent_hub_assets WHERE id = ?")
                    .bind(&input.asset_lineage_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let asset_id = if let Some(id) = asset_id {
                Some(id)
            } else {
                sqlx::query_scalar(
                    "SELECT asset_id FROM agent_hub_asset_lineages WHERE lineage_id = ? LIMIT 1",
                )
                .bind(&input.asset_lineage_id)
                .fetch_optional(&mut *tx)
                .await?
            };
            if let Some(asset_id) = asset_id {
                let now = chrono::Utc::now().to_rfc3339();
                let deleted_at = if input.operation == RevisionOperation::Delete {
                    Some(input.created_at.clone())
                } else {
                    None
                };
                // Head CAS：仅当调用方提供 expected_parent_id 时 fail-closed 原子推进 head
                // （UI/产品写路径）；None 时无条件推进 head（migration/DAG 构图/import）。
                // 单 parent 且 expected 缺省时仍按 parents[0] 做 CAS，避免并发静默丢写。
                // 仅 expected_parent_id=Some 时 hard CAS（Ui/Filesystem 产品写路径会设置）；
                // None 时无条件推进 head，兼容 revision_graph DAG 构图与 migration。
                if let Some(expected) = input
                    .expected_parent_id
                    .as_ref()
                    .map(|r| r.as_str().to_string())
                {
                    let result = sqlx::query(
                        "UPDATE agent_hub_assets
                         SET current_revision_id = ?, deleted_at = ?, updated_at = ?
                         WHERE id = ?
                           AND (current_revision_id IS NULL OR current_revision_id = ?)",
                    )
                    .bind(input.id.as_str())
                    .bind(&deleted_at)
                    .bind(&now)
                    .bind(&asset_id)
                    .bind(&expected)
                    .execute(&mut *tx)
                    .await?;
                    if result.rows_affected() == 0 {
                        return Err(AppError::conflict(
                            "agent_hub_revision_conflict".to_string(),
                        ));
                    }
                } else if input.parents.is_empty() {
                    // 首 revision 且无 expected：允许 NULL head 或幂等覆盖（migration）
                    sqlx::query(
                        "UPDATE agent_hub_assets
                         SET current_revision_id = ?, deleted_at = ?, updated_at = ?
                         WHERE id = ?",
                    )
                    .bind(input.id.as_str())
                    .bind(&deleted_at)
                    .bind(&now)
                    .bind(&asset_id)
                    .execute(&mut *tx)
                    .await?;
                } else {
                    // 无 expected：无条件推进 head（DAG 构图/migration 后续）
                    sqlx::query(
                        "UPDATE agent_hub_assets
                         SET current_revision_id = ?, deleted_at = ?, updated_at = ?
                         WHERE id = ?",
                    )
                    .bind(input.id.as_str())
                    .bind(&deleted_at)
                    .bind(&now)
                    .bind(&asset_id)
                    .execute(&mut *tx)
                    .await?;
                }
            }

            tx.commit().await?;

            Ok(Revision {
                id: input.id,
                asset_lineage_id: input.asset_lineage_id,
                parents: input.parents,
                generation,
                operation: input.operation,
                origin_kind: input.origin_kind,
                origin_target: input.origin_target,
                origin_replica_id: input.origin_replica_id,
                payload_hash: input.payload_hash,
                tree_manifest_hash: input.tree_manifest_hash,
                created_at: input.created_at,
            })
        })
        .await
    }

    /// 追加可移植资产 revision（typed payload → CAS blob → DAG）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Skill/Command/Agent/MCP 的 revision payload 只存 typed canonical JSON；
    ///     Skill supporting files 通过 `tree_manifest_hash` 引用，不重写脚本。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在开启 SQL 事务前校验 `AssetKind` 与 payload tag 一致并 `validate`；
    ///     Skill 可选校验 CAS 树含 SKILL.md；`canonical_bytes` → `put_blob` →
    ///     `append_revision`（payload_hash + 可选 tree_manifest_hash）。
    #[allow(clippy::too_many_arguments)] // portable revision CAS 需 origin + parent 全上下文
    pub async fn append_portable_asset_revision(
        &self,
        asset_id: &str,
        payload: &PortableAssetPayload,
        store: &ObjectStore,
        origin_kind: RevisionOriginKind,
        origin_target: Option<AgentTarget>,
        origin_replica_id: impl Into<String>,
        expected_parent_id: Option<RevisionId>,
    ) -> Result<Revision, AppError> {
        let asset = self
            .get_asset(asset_id)
            .await?
            .ok_or_else(|| AppError::not_found("agent_hub_asset_not_found".to_string()))?;
        // Fail-closed before SQL transaction / CAS write
        ensure_kind_matches_payload(asset.kind, payload)?;
        payload.validate()?;
        if let PortableAssetPayload::Skill(skill) = payload {
            // Validate tree without rewriting scripts when tree is already in CAS
            if let Ok(manifest) = store.get_tree(&skill.tree_manifest_hash).await {
                skill.validate_tree_manifest(&manifest)?;
            }
        }
        let bytes = canonical_bytes(payload)?;
        let stored = store.put_blob(&bytes).await?;
        let tree_manifest_hash = payload.tree_manifest_hash().map(|s| s.to_string());
        let parents = expected_parent_id
            .clone()
            .or_else(|| asset.current_revision_id.clone())
            .into_iter()
            .collect::<Vec<_>>();
        let expected = expected_parent_id.or_else(|| asset.current_revision_id.clone());
        self.append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset.id,
            parents,
            operation: RevisionOperation::Upsert,
            origin_kind,
            origin_target,
            origin_replica_id: origin_replica_id.into(),
            payload_hash: Some(stored.hash),
            tree_manifest_hash,
            created_at: chrono::Utc::now().to_rfc3339(),
            expected_parent_id: expected,
        })
        .await
    }

    /// 加载资产当前 head 的可移植 typed payload。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     投影/UI/扫描需要从 CAS 还原 Skill/Command/Agent/MCP canonical 形态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     get_asset → current revision → payload_hash → get_blob → from_canonical_bytes；
    ///     无 head / 非 portable kind / 缺 payload 返回错误或 None。
    pub async fn load_portable_asset(
        &self,
        asset_id: &str,
        store: &ObjectStore,
    ) -> Result<Option<PortableAssetPayload>, AppError> {
        let Some(asset) = self.get_asset(asset_id).await? else {
            return Ok(None);
        };
        match asset.kind {
            AssetKind::Skill | AssetKind::Command | AssetKind::Agent | AssetKind::Mcp => {}
            _ => {
                return Err(AppError::validation(format!(
                    "agent_hub_asset_kind_not_portable:{}",
                    asset.kind.as_str()
                )));
            }
        }
        let Some(rev_id) = asset.current_revision_id else {
            return Ok(None);
        };
        let Some(revision) = self.get_revision(&rev_id).await? else {
            return Ok(None);
        };
        if revision.operation == RevisionOperation::Delete {
            return Ok(None);
        }
        let Some(hash) = revision.payload_hash.as_deref() else {
            return Err(AppError::validation(
                "agent_hub_portable_revision_missing_payload_hash".to_string(),
            ));
        };
        let bytes = store.get_blob(hash).await?;
        let payload = from_canonical_bytes(&bytes)?;
        ensure_kind_matches_payload(asset.kind, &payload)?;
        Ok(Some(payload))
    }

    /// 追加 PluginPackage revision，并同事务写入 component/residual 边表。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     package 固定引用 component revision；后续 component 更新不得改写旧 package 行；
    ///     边表支撑 Snapshot 闭包与删除时引用计数（不维护易漂移计数器）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     事务前：排序/校验 payload、显式校验 component asset/revision kind 与 residual tree 存在；
    ///     CAS put_blob package JSON；同事务 append revision + 边表 INSERT。
    #[allow(clippy::too_many_arguments)]
    pub async fn append_plugin_package_revision(
        &self,
        asset_id: &str,
        payload: &PluginPackagePayload,
        store: &ObjectStore,
        origin_kind: RevisionOriginKind,
        origin_target: Option<AgentTarget>,
        origin_replica_id: impl Into<String>,
        expected_parent_id: Option<RevisionId>,
    ) -> Result<Revision, AppError> {
        let asset = self
            .get_asset(asset_id)
            .await?
            .ok_or_else(|| AppError::not_found("agent_hub_asset_not_found".to_string()))?;
        if asset.kind != AssetKind::Plugin {
            return Err(AppError::validation(format!(
                "agent_hub_plugin_asset_kind_mismatch:{}",
                asset.kind.as_str()
            )));
        }
        let mut payload = payload.clone();
        sort_plugin_package_payload(&mut payload);
        validate_plugin_package_payload(&payload)?;

        // 显式 FK 校验（不依赖 SQLite foreign_keys pragma）
        for cref in &payload.component_refs {
            let Some(comp_asset) = self.get_asset(&cref.asset_id).await? else {
                return Err(AppError::validation(format!(
                    "agent_hub_plugin_component_asset_missing:{}",
                    cref.asset_id
                )));
            };
            if comp_asset.kind != cref.kind {
                return Err(AppError::validation(format!(
                    "agent_hub_plugin_component_kind_mismatch:asset={},expected={},actual={}",
                    cref.asset_id,
                    cref.kind.as_str(),
                    comp_asset.kind.as_str()
                )));
            }
            let Some(comp_rev) = self.get_revision(&cref.revision_id).await? else {
                return Err(AppError::validation(format!(
                    "agent_hub_plugin_component_revision_missing:{}",
                    cref.revision_id.as_str()
                )));
            };
            if comp_rev.asset_lineage_id != cref.asset_id
                && comp_rev.asset_lineage_id != comp_asset.id
            {
                // lineage_id 通常等于 asset.id；否则必须可映射到该 component
                let lineages = self
                    .list_lineages_for_assets(std::slice::from_ref(&cref.asset_id))
                    .await?;
                let ok = lineages
                    .iter()
                    .any(|(_, lid)| lid == &comp_rev.asset_lineage_id);
                if !ok {
                    return Err(AppError::validation(format!(
                        "agent_hub_plugin_component_revision_asset_mismatch:{}:{}",
                        cref.asset_id,
                        cref.revision_id.as_str()
                    )));
                }
            }
        }
        for rref in &payload.residual_refs {
            store
                .get_tree(&rref.tree_manifest_hash)
                .await
                .map_err(|_| {
                    AppError::validation(format!(
                        "agent_hub_plugin_residual_tree_missing:{}:{}",
                        rref.target.as_str(),
                        rref.residual_kind.as_str()
                    ))
                })?;
        }

        let bytes = canonical_plugin_package_bytes(&payload)?;
        let stored = store.put_blob(&bytes).await?;
        let origin_replica_id = origin_replica_id.into();
        let parents = expected_parent_id
            .clone()
            .or_else(|| asset.current_revision_id.clone())
            .into_iter()
            .collect::<Vec<_>>();
        let expected = expected_parent_id.or_else(|| asset.current_revision_id.clone());
        let revision_id = RevisionId::new_v7();
        let created_at = chrono::Utc::now().to_rfc3339();

        with_shared_write_lease(&self.gate, async {
            let mut tx = self.pool.begin().await?;

            let mut max_parent_generation: Option<u64> = None;
            for parent in &parents {
                let gen: Option<i64> =
                    sqlx::query_scalar("SELECT generation FROM agent_hub_revisions WHERE id = ?")
                        .bind(parent.as_str())
                        .fetch_optional(&mut *tx)
                        .await?;
                let Some(g) = gen else {
                    return Err(AppError::validation(format!(
                        "agent_hub_revision_parent_missing:{}",
                        parent.as_str()
                    )));
                };
                let g = g as u64;
                max_parent_generation = Some(max_parent_generation.map_or(g, |m| m.max(g)));
            }
            let generation = max_parent_generation.map_or(0, |m| m.saturating_add(1));

            sqlx::query(
                "INSERT INTO agent_hub_revisions
                 (id, asset_lineage_id, generation, operation, origin_kind, origin_target,
                  origin_replica_id, payload_hash, tree_manifest_hash, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(revision_id.as_str())
            .bind(&asset.id)
            .bind(generation as i64)
            .bind(RevisionOperation::Upsert.as_str())
            .bind(origin_kind.as_str())
            .bind(origin_target.map(|t| t.as_str()))
            .bind(&origin_replica_id)
            .bind(&stored.hash)
            .bind::<Option<String>>(None)
            .bind(&created_at)
            .execute(&mut *tx)
            .await?;

            for (pos, parent) in parents.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO agent_hub_revision_parents
                     (revision_id, parent_revision_id, parent_order)
                     VALUES (?, ?, ?)",
                )
                .bind(revision_id.as_str())
                .bind(parent.as_str())
                .bind(pos as i64)
                .execute(&mut *tx)
                .await?;
            }

            // head CAS / 推进
            let now = created_at.clone();
            if let Some(expected) = expected.as_ref().map(|r| r.as_str().to_string()) {
                let result = sqlx::query(
                    "UPDATE agent_hub_assets
                     SET current_revision_id = ?, deleted_at = NULL, updated_at = ?
                     WHERE id = ?
                       AND (current_revision_id IS NULL OR current_revision_id = ?)",
                )
                .bind(revision_id.as_str())
                .bind(&now)
                .bind(&asset.id)
                .bind(&expected)
                .execute(&mut *tx)
                .await?;
                if result.rows_affected() == 0 {
                    return Err(AppError::conflict(
                        "agent_hub_revision_conflict".to_string(),
                    ));
                }
            } else {
                sqlx::query(
                    "UPDATE agent_hub_assets
                     SET current_revision_id = ?, deleted_at = NULL, updated_at = ?
                     WHERE id = ?",
                )
                .bind(revision_id.as_str())
                .bind(&now)
                .bind(&asset.id)
                .execute(&mut *tx)
                .await?;
            }

            for cref in &payload.component_refs {
                // 事务内再验 revision 行存在
                let exists: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_revisions WHERE id = ?")
                        .bind(cref.revision_id.as_str())
                        .fetch_one(&mut *tx)
                        .await?;
                if exists == 0 {
                    return Err(AppError::validation(format!(
                        "agent_hub_plugin_component_revision_missing:{}",
                        cref.revision_id.as_str()
                    )));
                }
                let asset_exists: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_assets WHERE id = ?")
                        .bind(&cref.asset_id)
                        .fetch_one(&mut *tx)
                        .await?;
                if asset_exists == 0 {
                    return Err(AppError::validation(format!(
                        "agent_hub_plugin_component_asset_missing:{}",
                        cref.asset_id
                    )));
                }
                sqlx::query(
                    "INSERT INTO agent_hub_plugin_components
                     (package_revision_id, component_kind, component_asset_id,
                      component_revision_id, ownership)
                     VALUES (?, ?, ?, ?, ?)",
                )
                .bind(revision_id.as_str())
                .bind(cref.kind.as_str())
                .bind(&cref.asset_id)
                .bind(cref.revision_id.as_str())
                .bind(cref.ownership.as_str())
                .execute(&mut *tx)
                .await?;
            }

            for rref in &payload.residual_refs {
                sqlx::query(
                    "INSERT INTO agent_hub_plugin_residuals
                     (package_revision_id, target, residual_kind, tree_manifest_hash)
                     VALUES (?, ?, ?, ?)",
                )
                .bind(revision_id.as_str())
                .bind(rref.target.as_str())
                .bind(rref.residual_kind.as_str())
                .bind(&rref.tree_manifest_hash)
                .execute(&mut *tx)
                .await?;
            }

            tx.commit().await?;
            Ok(Revision {
                id: revision_id,
                asset_lineage_id: asset.id,
                parents,
                generation,
                operation: RevisionOperation::Upsert,
                origin_kind,
                origin_target,
                origin_replica_id,
                payload_hash: Some(stored.hash),
                tree_manifest_hash: None,
                created_at,
            })
        })
        .await
    }

    /// 加载 package 资产 head 的 PluginPackagePayload。
    ///
    /// Business Logic: UI/Snapshot/删除路径需要 typed package 视图。
    /// Code Logic: get_asset(Plugin) → head revision → payload blob → from_plugin_package_bytes。
    pub async fn load_plugin_package(
        &self,
        asset_id: &str,
        store: &ObjectStore,
    ) -> Result<Option<PluginPackagePayload>, AppError> {
        let Some(asset) = self.get_asset(asset_id).await? else {
            return Ok(None);
        };
        if asset.kind != AssetKind::Plugin {
            return Err(AppError::validation(format!(
                "agent_hub_asset_kind_not_plugin:{}",
                asset.kind.as_str()
            )));
        }
        let Some(rev_id) = asset.current_revision_id else {
            return Ok(None);
        };
        self.load_plugin_package_revision(&rev_id, store).await
    }

    /// 按 package revision id 加载 PluginPackagePayload。
    ///
    /// Business Logic: 历史 package revision 必须可精确还原固定 refs。
    /// Code Logic: get_revision → payload_hash → from_plugin_package_bytes。
    pub async fn load_plugin_package_revision(
        &self,
        revision_id: &RevisionId,
        store: &ObjectStore,
    ) -> Result<Option<PluginPackagePayload>, AppError> {
        let Some(revision) = self.get_revision(revision_id).await? else {
            return Ok(None);
        };
        if revision.operation == RevisionOperation::Delete {
            return Ok(None);
        }
        let Some(hash) = revision.payload_hash.as_deref() else {
            return Err(AppError::validation(
                "agent_hub_plugin_revision_missing_payload_hash".to_string(),
            ));
        };
        let bytes = store.get_blob(hash).await?;
        Ok(Some(from_plugin_package_bytes(&bytes)?))
    }

    /// 列出 package revision 的 component 边。
    ///
    /// Business Logic: Snapshot 与删除引用计数从边表查询。
    /// Code Logic: SELECT ORDER BY kind, asset, revision。
    pub async fn list_plugin_components_for_revision(
        &self,
        package_revision_id: &str,
    ) -> Result<Vec<PluginComponentRef>, AppError> {
        let rows = sqlx::query(
            "SELECT component_kind, component_asset_id, component_revision_id, ownership
             FROM agent_hub_plugin_components
             WHERE package_revision_id = ?
             ORDER BY component_kind ASC, component_asset_id ASC, component_revision_id ASC",
        )
        .bind(package_revision_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let kind_s: String = row.try_get("component_kind")?;
            let kind = AssetKind::parse(&kind_s).ok_or_else(|| {
                AppError::validation(format!("agent_hub_plugin_component_kind_unknown:{kind_s}"))
            })?;
            let asset_id: String = row.try_get("component_asset_id")?;
            let rev: String = row.try_get("component_revision_id")?;
            let own_s: String = row.try_get("ownership")?;
            let ownership = ComponentOwnership::parse(&own_s).ok_or_else(|| {
                AppError::validation(format!("agent_hub_plugin_ownership_unknown:{own_s}"))
            })?;
            out.push(PluginComponentRef {
                kind,
                asset_id,
                revision_id: RevisionId::from(rev),
                ownership,
            });
        }
        Ok(out)
    }

    /// 列出 package revision 的 residual 边。
    ///
    /// Business Logic: Snapshot 必须带走 residual tree 即使 package 非 active head。
    /// Code Logic: SELECT ORDER BY target, kind, hash。
    pub async fn list_plugin_residuals_for_revision(
        &self,
        package_revision_id: &str,
    ) -> Result<Vec<PluginResidualRef>, AppError> {
        let rows = sqlx::query(
            "SELECT target, residual_kind, tree_manifest_hash
             FROM agent_hub_plugin_residuals
             WHERE package_revision_id = ?
             ORDER BY target ASC, residual_kind ASC, tree_manifest_hash ASC",
        )
        .bind(package_revision_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let target_s: String = row.try_get("target")?;
            let target = AgentTarget::parse(&target_s).ok_or_else(|| {
                AppError::validation(format!(
                    "agent_hub_plugin_residual_target_unknown:{target_s}"
                ))
            })?;
            let kind_s: String = row.try_get("residual_kind")?;
            let residual_kind = ResidualKind::parse(&kind_s).ok_or_else(|| {
                AppError::validation(format!("agent_hub_plugin_residual_kind_unknown:{kind_s}"))
            })?;
            let tree_manifest_hash: String = row.try_get("tree_manifest_hash")?;
            out.push(PluginResidualRef {
                target,
                residual_kind,
                tree_manifest_hash,
            });
        }
        Ok(out)
    }

    /// 登记 component → standalone 逻辑资产引用（删除时 PreserveStandalone）。
    ///
    /// Business Logic: 引用数从边表查询，不维护计数器。
    /// Code Logic: INSERT OR IGNORE。
    pub async fn upsert_component_standalone_ref(
        &self,
        component_asset_id: &str,
        standalone_asset_id: &str,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT OR IGNORE INTO agent_hub_component_standalone_refs
                 (component_asset_id, standalone_asset_id, created_at)
                 VALUES (?, ?, ?)",
            )
            .bind(component_asset_id)
            .bind(standalone_asset_id)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    /// 清除 component 的全部 standalone 边。
    ///
    /// Business Logic: 显式 standalone 删除路径在 tombstone 前解除边。
    /// Code Logic: DELETE FROM agent_hub_component_standalone_refs WHERE component_asset_id。
    pub async fn clear_component_standalone_refs(
        &self,
        component_asset_id: &str,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "DELETE FROM agent_hub_component_standalone_refs
                 WHERE component_asset_id = ?",
            )
            .bind(component_asset_id)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    /// 是否存在 component → standalone 边。
    ///
    /// Business Logic: 删除决策 standalone 优先。
    /// Code Logic: COUNT(*) > 0。
    pub async fn component_has_standalone_ref(
        &self,
        component_asset_id: &str,
    ) -> Result<bool, AppError> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_hub_component_standalone_refs
             WHERE component_asset_id = ?",
        )
        .bind(component_asset_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(n > 0)
    }

    /// 统计其它 live package head 对该 component 的引用数。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     引用数从边表 + 当前 head 联查，禁止维护计数器；并发须在事务内重读。
    ///
    /// Code Logic（这个函数做什么）:
    ///     package asset kind=plugin 且 deleted_at IS NULL 且 current_revision_id 边表命中 component；
    ///     `exclude_package_asset_id` 排除正在删除的 package。
    pub async fn count_other_live_package_head_refs(
        &self,
        component_asset_id: &str,
        exclude_package_asset_id: Option<&str>,
    ) -> Result<u64, AppError> {
        let n: i64 = if let Some(exclude) = exclude_package_asset_id {
            sqlx::query_scalar(
                "SELECT COUNT(*)
                 FROM agent_hub_plugin_components c
                 INNER JOIN agent_hub_assets a
                   ON a.current_revision_id = c.package_revision_id
                 WHERE c.component_asset_id = ?
                   AND a.kind = 'plugin'
                   AND a.deleted_at IS NULL
                   AND a.id != ?",
            )
            .bind(component_asset_id)
            .bind(exclude)
            .fetch_one(&self.pool)
            .await?
        } else {
            sqlx::query_scalar(
                "SELECT COUNT(*)
                 FROM agent_hub_plugin_components c
                 INNER JOIN agent_hub_assets a
                   ON a.current_revision_id = c.package_revision_id
                 WHERE c.component_asset_id = ?
                   AND a.kind = 'plugin'
                   AND a.deleted_at IS NULL",
            )
            .bind(component_asset_id)
            .fetch_one(&self.pool)
            .await?
        };
        Ok(n as u64)
    }

    /// 在删除事务外先读一遍决策（测试/preview）；生产删除路径会事务内重读。
    ///
    /// Business Logic: concurrent ref 创建后必须以 fresh read 重算，禁止陈旧计数。
    /// Code Logic: has_standalone + other_live_package_head_refs → decide_component_delete。
    pub async fn decide_component_delete(
        &self,
        component_asset_id: &str,
        exclude_package_asset_id: Option<&str>,
    ) -> Result<ComponentDeleteDecision, AppError> {
        let has_standalone = self
            .component_has_standalone_ref(component_asset_id)
            .await?;
        let other = self
            .count_other_live_package_head_refs(component_asset_id, exclude_package_asset_id)
            .await?;
        Ok(decide_component_delete(has_standalone, other))
    }

    /// 删除 package：单 shared-write lease TX 内 package tombstone + component 决策与条件删除。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     只 tombstone 独占 component；共享/standalone 保留；旧 package revision 不改写。
    ///     multi-step 删除会在 concurrent package-head 创建时 TOCTOU 误删/漏删。
    ///
    /// Code Logic（这个函数做什么）:
    ///     单 with_shared_write_lease + 单 TX：
    ///     1) 读 package head 的 component 边；
    ///     2) package Delete tombstone first（head CAS）；
    ///     3) 对每个 component 在同一 TX 内 re-decide（edges/heads 一致读）；
    ///     4) TombstoneOwned 才 append child Delete。
    pub async fn delete_plugin_package_with_ownership(
        &self,
        package_asset_id: &str,
        _store: &ObjectStore,
        origin_kind: RevisionOriginKind,
        origin_replica_id: impl Into<String>,
    ) -> Result<PluginPackageDeleteResult, AppError> {
        let origin_replica_id = origin_replica_id.into();
        with_shared_write_lease(&self.gate, async {
            let mut tx = self.pool.begin().await?;

            let package_row = sqlx::query(
                "SELECT id, kind, current_revision_id, deleted_at
                 FROM agent_hub_assets WHERE id = ?",
            )
            .bind(package_asset_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::not_found("agent_hub_asset_not_found".to_string()))?;
            let kind_s: String = package_row.try_get("kind")?;
            let kind = AssetKind::parse(&kind_s).ok_or_else(|| {
                AppError::validation(format!("agent_hub_asset_kind_unknown:{kind_s}"))
            })?;
            if kind != AssetKind::Plugin {
                return Err(AppError::validation(format!(
                    "agent_hub_asset_kind_not_plugin:{}",
                    kind.as_str()
                )));
            }
            let head: Option<String> = package_row.try_get("current_revision_id")?;
            let Some(head) = head else {
                return Err(AppError::validation(
                    "agent_hub_plugin_no_head_revision".to_string(),
                ));
            };
            // 历史 head 的 component 列表（tombstone 后 head 变为 delete rev，边表仍挂在旧 rev）
            let components = list_plugin_components_on_tx(&mut tx, &head).await?;

            let package_created_at = chrono::Utc::now().to_rfc3339();
            let package_tombstone_id = RevisionId::new_v7();
            let package_revision = insert_delete_revision_on_tx(
                &mut tx,
                package_asset_id,
                package_asset_id,
                &package_tombstone_id,
                &[RevisionId::from(head.clone())],
                Some(RevisionId::from(head.clone())),
                origin_kind,
                &origin_replica_id,
                &package_created_at,
            )
            .await?;

            let mut component_decisions = Vec::new();
            for cref in components {
                // 同一 TX 内 fresh decide（package head 已 tombstone，exclude 生效）
                let has_standalone =
                    component_has_standalone_ref_on_tx(&mut tx, &cref.asset_id).await?;
                // ownership 标签与边表不一致时 fail-closed：禁止误 tombstone 独立资产
                if cref.ownership == ComponentOwnership::Standalone && !has_standalone {
                    return Err(AppError::conflict(format!(
                        "agent_hub_plugin_standalone_ref_missing:{}",
                        cref.asset_id
                    )));
                }
                let other = count_other_live_package_head_refs_on_tx(
                    &mut tx,
                    &cref.asset_id,
                    Some(package_asset_id),
                )
                .await?;
                let decision = decide_component_delete(has_standalone, other);
                component_decisions.push(ComponentDeleteOutcome {
                    component_asset_id: cref.asset_id.clone(),
                    kind: cref.kind,
                    decision,
                });
                if decision != ComponentDeleteDecision::TombstoneOwned {
                    continue;
                }

                let comp_row = sqlx::query(
                    "SELECT id, current_revision_id, deleted_at
                     FROM agent_hub_assets WHERE id = ?",
                )
                .bind(&cref.asset_id)
                .fetch_optional(&mut *tx)
                .await?;
                let Some(comp_row) = comp_row else {
                    continue;
                };
                let deleted_at: Option<String> = comp_row.try_get("deleted_at")?;
                if deleted_at.is_some() {
                    continue;
                }
                let current: Option<String> = comp_row.try_get("current_revision_id")?;
                let parents: Vec<RevisionId> =
                    current.clone().into_iter().map(RevisionId::from).collect();
                let expected = current.map(RevisionId::from);
                let child_created_at = chrono::Utc::now().to_rfc3339();
                let child_tombstone_id = RevisionId::new_v7();
                let _ = insert_delete_revision_on_tx(
                    &mut tx,
                    &cref.asset_id,
                    &cref.asset_id,
                    &child_tombstone_id,
                    &parents,
                    expected,
                    origin_kind,
                    &origin_replica_id,
                    &child_created_at,
                )
                .await?;
            }

            // 同 TX fan-out：package + TombstoneOwned components → 全部 binding Absent
            let mut fan_asset_ids = vec![package_asset_id.to_string()];
            for d in &component_decisions {
                if d.decision == ComponentDeleteDecision::TombstoneOwned {
                    fan_asset_ids.push(d.component_asset_id.clone());
                }
            }
            for aid in fan_asset_ids {
                let rows = sqlx::query(
                    "SELECT id, asset_id, target, local_scope_mapping_id, checkout_binding_id,
                            desired_presence, desired_enabled, created_at, updated_at
                     FROM agent_hub_target_bindings WHERE asset_id = ?",
                )
                .bind(&aid)
                .fetch_all(&mut *tx)
                .await?;
                let bind_now = chrono::Utc::now().to_rfc3339();
                if rows.is_empty() {
                    // 无既有 binding 时插入三目标 Absent，保证 scheduler 可见删除意图
                    for t in [
                        crate::agent_hub::models::AgentTarget::Claude,
                        crate::agent_hub::models::AgentTarget::Codex,
                        crate::agent_hub::models::AgentTarget::OpenCode,
                    ] {
                        let id = uuid::Uuid::new_v4().to_string();
                        sqlx::query(
                            "INSERT INTO agent_hub_target_bindings
                             (id, asset_id, target, local_scope_mapping_id, checkout_binding_id,
                              desired_presence, desired_enabled, created_at, updated_at)
                             VALUES (?, ?, ?, NULL, NULL, 'absent', 0, ?, ?)",
                        )
                        .bind(&id)
                        .bind(&aid)
                        .bind(t.as_str())
                        .bind(&bind_now)
                        .bind(&bind_now)
                        .execute(&mut *tx)
                        .await?;
                    }
                } else {
                    for row in rows {
                        let id: String = row.try_get("id")?;
                        sqlx::query(
                            "UPDATE agent_hub_target_bindings
                             SET desired_presence = 'absent', desired_enabled = 0, updated_at = ?
                             WHERE id = ?",
                        )
                        .bind(&bind_now)
                        .bind(&id)
                        .execute(&mut *tx)
                        .await?;
                    }
                }
            }

            tx.commit().await?;
            Ok(PluginPackageDeleteResult {
                package_asset_id: package_asset_id.to_string(),
                package_revision,
                component_decisions,
            })
        })
        .await
    }

    /// 追加 Hook 资产 revision（PortableHook → CAS）。
    ///
    /// Business Logic: Hook 作为独立 component 进入 DAG；合同体积受 Snapshot limits 约束。
    /// Code Logic: kind=Hook 校验 → canonical_portable_hook_bytes → append_revision。
    #[allow(clippy::too_many_arguments)]
    pub async fn append_portable_hook_revision(
        &self,
        asset_id: &str,
        hook: &PortableHook,
        store: &ObjectStore,
        origin_kind: RevisionOriginKind,
        origin_target: Option<AgentTarget>,
        origin_replica_id: impl Into<String>,
        expected_parent_id: Option<RevisionId>,
    ) -> Result<Revision, AppError> {
        let asset = self
            .get_asset(asset_id)
            .await?
            .ok_or_else(|| AppError::not_found("agent_hub_asset_not_found".to_string()))?;
        if asset.kind != AssetKind::Hook {
            return Err(AppError::validation(format!(
                "agent_hub_hook_asset_kind_mismatch:{}",
                asset.kind.as_str()
            )));
        }
        validate_portable_hook(hook, &default_snapshot_limits())?;
        if let Some(hash) = &hook.command_tree_hash {
            // residual/command tree 存在才可钉住
            let _ = store.get_tree(hash).await.map_err(|_| {
                AppError::validation("agent_hub_hook_command_tree_missing".to_string())
            })?;
        }
        let bytes = canonical_portable_hook_bytes(hook)?;
        let stored = store.put_blob(&bytes).await?;
        let parents = expected_parent_id
            .clone()
            .or_else(|| asset.current_revision_id.clone())
            .into_iter()
            .collect::<Vec<_>>();
        let expected = expected_parent_id.or_else(|| asset.current_revision_id.clone());
        self.append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset.id,
            parents,
            operation: RevisionOperation::Upsert,
            origin_kind,
            origin_target,
            origin_replica_id: origin_replica_id.into(),
            payload_hash: Some(stored.hash),
            tree_manifest_hash: hook.command_tree_hash.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            expected_parent_id: expected,
        })
        .await
    }

    /// 加载 Hook 资产 head 的 PortableHook。
    ///
    /// Business Logic: 投影/诊断需要 typed Hook 合同。
    /// Code Logic: get_asset(Hook) → head → from_portable_hook_bytes。
    pub async fn load_portable_hook(
        &self,
        asset_id: &str,
        store: &ObjectStore,
    ) -> Result<Option<PortableHook>, AppError> {
        let Some(asset) = self.get_asset(asset_id).await? else {
            return Ok(None);
        };
        if asset.kind != AssetKind::Hook {
            return Err(AppError::validation(format!(
                "agent_hub_asset_kind_not_hook:{}",
                asset.kind.as_str()
            )));
        }
        let Some(rev_id) = asset.current_revision_id else {
            return Ok(None);
        };
        let Some(revision) = self.get_revision(&rev_id).await? else {
            return Ok(None);
        };
        if revision.operation == RevisionOperation::Delete {
            return Ok(None);
        }
        let Some(hash) = revision.payload_hash.as_deref() else {
            return Err(AppError::validation(
                "agent_hub_hook_revision_missing_payload_hash".to_string(),
            ));
        };
        let bytes = store.get_blob(hash).await?;
        Ok(Some(from_portable_hook_bytes(&bytes)?))
    }

    /// 按 id 读取 revision（含有序 parents）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     合并/投影需要完整 DAG 节点。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 revisions + 按 parent_order 读 parents。
    pub async fn get_revision(&self, id: &RevisionId) -> Result<Option<Revision>, AppError> {
        let row = sqlx::query(
            "SELECT id, asset_lineage_id, generation, operation, origin_kind, origin_target,
                    origin_replica_id, payload_hash, tree_manifest_hash, created_at
             FROM agent_hub_revisions WHERE id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let parent_rows = sqlx::query(
            "SELECT parent_revision_id FROM agent_hub_revision_parents
             WHERE revision_id = ? ORDER BY parent_order ASC",
        )
        .bind(id.as_str())
        .fetch_all(&self.pool)
        .await?;
        let parents = parent_rows
            .iter()
            .map(|r| {
                let s: String = r.try_get("parent_revision_id")?;
                Ok::<_, AppError>(RevisionId(s))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(row_to_revision(&row, parents)?))
    }

    /// 创建或更新 target binding。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     desired_enabled 是 target-local，不得隐式跨 target 同步。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 (asset_id, target, mapping, checkout) 唯一键 upsert desired 字段。
    pub async fn upsert_target_binding(
        &self,
        input: NewTargetBinding,
    ) -> Result<TargetBinding, AppError> {
        with_shared_write_lease(&self.gate, async {
            let mapping = input.local_scope_mapping_id.clone().unwrap_or_default();
            let checkout = input.checkout_binding_id.clone().unwrap_or_default();
            let existing: Option<(String, String)> = sqlx::query_as(
                "SELECT id, created_at FROM agent_hub_target_bindings
                 WHERE asset_id = ? AND target = ?
                   AND IFNULL(local_scope_mapping_id, '') = ?
                   AND IFNULL(checkout_binding_id, '') = ?",
            )
            .bind(&input.asset_id)
            .bind(input.target.as_str())
            .bind(&mapping)
            .bind(&checkout)
            .fetch_optional(&self.pool)
            .await?;

            let now = chrono::Utc::now().to_rfc3339();
            if let Some((id, created_at)) = existing {
                sqlx::query(
                    "UPDATE agent_hub_target_bindings
                     SET desired_presence = ?, desired_enabled = ?, updated_at = ?
                     WHERE id = ?",
                )
                .bind(input.desired_presence.as_str())
                .bind(if input.desired_enabled { 1_i64 } else { 0_i64 })
                .bind(&now)
                .bind(&id)
                .execute(&self.pool)
                .await?;
                Ok(TargetBinding {
                    id,
                    asset_id: input.asset_id,
                    target: input.target,
                    local_scope_mapping_id: input.local_scope_mapping_id,
                    checkout_binding_id: input.checkout_binding_id,
                    desired_presence: input.desired_presence,
                    desired_enabled: input.desired_enabled,
                    created_at,
                    updated_at: now,
                })
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO agent_hub_target_bindings
                     (id, asset_id, target, local_scope_mapping_id, checkout_binding_id,
                      desired_presence, desired_enabled, created_at, updated_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(&input.asset_id)
                .bind(input.target.as_str())
                .bind(&input.local_scope_mapping_id)
                .bind(&input.checkout_binding_id)
                .bind(input.desired_presence.as_str())
                .bind(if input.desired_enabled { 1_i64 } else { 0_i64 })
                .bind(&now)
                .bind(&now)
                .execute(&self.pool)
                .await?;
                Ok(TargetBinding {
                    id,
                    asset_id: input.asset_id,
                    target: input.target,
                    local_scope_mapping_id: input.local_scope_mapping_id,
                    checkout_binding_id: input.checkout_binding_id,
                    desired_presence: input.desired_presence,
                    desired_enabled: input.desired_enabled,
                    created_at: now.clone(),
                    updated_at: now,
                })
            }
        })
        .await
    }

    /// 删除 V1 自动生成且从未投影的用户级 absent bindings。
    ///
    /// Business Logic: 这些行不是用户选择，V2 必须还原为真正 unmanaged。
    /// Code Logic: 仅删 absent+disabled+无 mapping/checkout+无 materialization 的行，返回删除数。
    pub async fn delete_unmaterialized_absent_user_bindings(
        &self,
        asset_id: &str,
    ) -> Result<u64, AppError> {
        with_shared_write_lease(&self.gate, async {
            let result = sqlx::query(
                "DELETE FROM agent_hub_target_bindings
                 WHERE asset_id = ?
                   AND desired_presence = 'absent'
                   AND desired_enabled = 0
                   AND local_scope_mapping_id IS NULL
                   AND checkout_binding_id IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM agent_hub_materializations m
                       WHERE m.target_binding_id = agent_hub_target_bindings.id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM agent_hub_user_instruction_ownership o
                       WHERE o.asset_id = agent_hub_target_bindings.asset_id
                         AND o.target = agent_hub_target_bindings.target
                   )",
            )
            .bind(asset_id)
            .execute(&self.pool)
            .await?;
            Ok(result.rows_affected())
        })
        .await
    }

    /// 单 write-lease 事务：一条 Delete tombstone + fan-out Absent bindings。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     delete_everywhere 不得在 tombstone 成功、fan-out 中途失败时留下半状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     with_shared_write_lease → 同事务 append_revision(Delete, CAS head) + 逐 binding upsert Absent/disabled。
    pub async fn delete_asset_everywhere_atomic(
        &self,
        asset_id: &str,
        tombstone: NewRevision,
        fan_out: Vec<NewTargetBinding>,
    ) -> Result<Revision, AppError> {
        with_shared_write_lease(&self.gate, async {
            if tombstone.operation != RevisionOperation::Delete {
                return Err(AppError::validation(
                    "agent_hub_delete_everywhere_requires_delete_revision",
                ));
            }
            if tombstone.payload_hash.is_some() || tombstone.tree_manifest_hash.is_some() {
                return Err(AppError::validation(
                    "agent_hub_delete_revision_rejects_payload_hash",
                ));
            }
            let mut tx = self.pool.begin().await?;

            // lineage/asset 存在性
            let asset_exists: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_assets WHERE id = ?")
                    .bind(asset_id)
                    .fetch_one(&mut *tx)
                    .await?;
            if asset_exists == 0 {
                return Err(AppError::not_found("agent_hub_asset_not_found"));
            }

            let mut max_parent_generation: Option<u64> = None;
            for parent in &tombstone.parents {
                let gen: Option<i64> =
                    sqlx::query_scalar("SELECT generation FROM agent_hub_revisions WHERE id = ?")
                        .bind(parent.as_str())
                        .fetch_optional(&mut *tx)
                        .await?;
                let Some(g) = gen else {
                    return Err(AppError::validation(format!(
                        "agent_hub_revision_parent_missing:{}",
                        parent.as_str()
                    )));
                };
                let g = g as u64;
                max_parent_generation = Some(max_parent_generation.map_or(g, |m| m.max(g)));
            }
            let generation = max_parent_generation.map_or(0, |m| m.saturating_add(1));

            let insert_result = sqlx::query(
                "INSERT INTO agent_hub_revisions
                 (id, asset_lineage_id, generation, operation, origin_kind, origin_target,
                  origin_replica_id, payload_hash, tree_manifest_hash, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(tombstone.id.as_str())
            .bind(&tombstone.asset_lineage_id)
            .bind(generation as i64)
            .bind(tombstone.operation.as_str())
            .bind(tombstone.origin_kind.as_str())
            .bind(tombstone.origin_target.map(|t| t.as_str()))
            .bind(&tombstone.origin_replica_id)
            .bind(&tombstone.payload_hash)
            .bind(&tombstone.tree_manifest_hash)
            .bind(&tombstone.created_at)
            .execute(&mut *tx)
            .await;
            if let Err(e) = insert_result {
                if is_unique_violation(&e) {
                    return Err(AppError::conflict(
                        "agent_hub_revision_id_conflict".to_string(),
                    ));
                }
                return Err(e.into());
            }

            for (pos, parent) in tombstone.parents.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO agent_hub_revision_parents
                     (revision_id, parent_revision_id, parent_order)
                     VALUES (?, ?, ?)",
                )
                .bind(tombstone.id.as_str())
                .bind(parent.as_str())
                .bind(pos as i64)
                .execute(&mut *tx)
                .await?;
            }

            let deleted_at = Some(tombstone.created_at.clone());
            let now = chrono::Utc::now().to_rfc3339();
            if let Some(expected) = tombstone
                .expected_parent_id
                .as_ref()
                .map(|r| r.as_str().to_string())
            {
                let result = sqlx::query(
                    "UPDATE agent_hub_assets
                     SET current_revision_id = ?, deleted_at = ?, updated_at = ?
                     WHERE id = ?
                       AND (current_revision_id IS NULL OR current_revision_id = ?)",
                )
                .bind(tombstone.id.as_str())
                .bind(&deleted_at)
                .bind(&now)
                .bind(asset_id)
                .bind(&expected)
                .execute(&mut *tx)
                .await?;
                if result.rows_affected() == 0 {
                    return Err(AppError::conflict(
                        "agent_hub_revision_conflict".to_string(),
                    ));
                }
            } else {
                sqlx::query(
                    "UPDATE agent_hub_assets
                     SET current_revision_id = ?, deleted_at = ?, updated_at = ?
                     WHERE id = ?",
                )
                .bind(tombstone.id.as_str())
                .bind(&deleted_at)
                .bind(&now)
                .bind(asset_id)
                .execute(&mut *tx)
                .await?;
            }

            // fan-out Absent/disabled
            for input in fan_out {
                let mapping = input.local_scope_mapping_id.clone().unwrap_or_default();
                let checkout = input.checkout_binding_id.clone().unwrap_or_default();
                let existing: Option<(String, String)> = sqlx::query_as(
                    "SELECT id, created_at FROM agent_hub_target_bindings
                     WHERE asset_id = ? AND target = ?
                       AND IFNULL(local_scope_mapping_id, '') = ?
                       AND IFNULL(checkout_binding_id, '') = ?",
                )
                .bind(&input.asset_id)
                .bind(input.target.as_str())
                .bind(&mapping)
                .bind(&checkout)
                .fetch_optional(&mut *tx)
                .await?;
                let bind_now = chrono::Utc::now().to_rfc3339();
                if let Some((id, _)) = existing {
                    sqlx::query(
                        "UPDATE agent_hub_target_bindings
                         SET desired_presence = ?, desired_enabled = ?, updated_at = ?
                         WHERE id = ?",
                    )
                    .bind(input.desired_presence.as_str())
                    .bind(if input.desired_enabled { 1_i64 } else { 0_i64 })
                    .bind(&bind_now)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await?;
                } else {
                    let id = uuid::Uuid::new_v4().to_string();
                    sqlx::query(
                        "INSERT INTO agent_hub_target_bindings
                         (id, asset_id, target, local_scope_mapping_id, checkout_binding_id,
                          desired_presence, desired_enabled, created_at, updated_at)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(&id)
                    .bind(&input.asset_id)
                    .bind(input.target.as_str())
                    .bind(&input.local_scope_mapping_id)
                    .bind(&input.checkout_binding_id)
                    .bind(input.desired_presence.as_str())
                    .bind(if input.desired_enabled { 1_i64 } else { 0_i64 })
                    .bind(&bind_now)
                    .bind(&bind_now)
                    .execute(&mut *tx)
                    .await?;
                }
            }

            tx.commit().await?;
            Ok(Revision {
                id: tombstone.id,
                asset_lineage_id: tombstone.asset_lineage_id,
                parents: tombstone.parents,
                generation,
                operation: tombstone.operation,
                origin_kind: tombstone.origin_kind,
                origin_target: tombstone.origin_target,
                origin_replica_id: tombstone.origin_replica_id,
                payload_hash: tombstone.payload_hash,
                tree_manifest_hash: tombstone.tree_manifest_hash,
                created_at: tombstone.created_at,
            })
        })
        .await
    }

    /// 按 id 读取 target binding。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     投影与 UI 需要查询 desired 状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT 并解析枚举/布尔。
    pub async fn get_target_binding(&self, id: &str) -> Result<Option<TargetBinding>, AppError> {
        let row = sqlx::query(
            "SELECT id, asset_id, target, local_scope_mapping_id, checkout_binding_id,
                    desired_presence, desired_enabled, created_at, updated_at
             FROM agent_hub_target_bindings WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_target_binding(&r)).transpose()
    }

    /// 列出某资产在所有 target 上的 binding。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     验证 desired_enabled 是否 target-local 时需要并排比较。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 target 排序列出。
    pub async fn list_target_bindings_for_asset(
        &self,
        asset_id: &str,
    ) -> Result<Vec<TargetBinding>, AppError> {
        let rows = sqlx::query(
            "SELECT id, asset_id, target, local_scope_mapping_id, checkout_binding_id,
                    desired_presence, desired_enabled, created_at, updated_at
             FROM agent_hub_target_bindings WHERE asset_id = ? ORDER BY target ASC, id ASC",
        )
        .bind(asset_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_target_binding).collect()
    }

    /// 按本地 Workbench project id 读取 Hub 项目映射。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     project opt-in 与 checkout binding 刷新都需要从本机 Workbench 项目定位 portable hubProjectId。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `SELECT ... WHERE local_workbench_project_id = ?`，解析 opted_in 整型为 bool。
    pub async fn get_project_mapping_by_local_workbench_id(
        &self,
        local_workbench_project_id: &str,
    ) -> Result<Option<AgentHubProjectMappingRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, hub_project_id, local_workbench_project_id, git_remote_fingerprint,
                    local_absolute_path, opted_in, created_at, updated_at
             FROM agent_hub_project_mappings
             WHERE local_workbench_project_id = ?",
        )
        .bind(local_workbench_project_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_project_mapping(&r)).transpose()
    }

    /// 按 hub_project_id 读取项目映射。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     跨设备 portable 身份以 hub_project_id 为主键，需要按它回查本机映射。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `SELECT ... WHERE hub_project_id = ? LIMIT 1`。
    pub async fn get_project_mapping_by_hub_project_id(
        &self,
        hub_project_id: &str,
    ) -> Result<Option<AgentHubProjectMappingRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, hub_project_id, local_workbench_project_id, git_remote_fingerprint,
                    local_absolute_path, opted_in, created_at, updated_at
             FROM agent_hub_project_mappings
             WHERE hub_project_id = ?
             LIMIT 1",
        )
        .bind(hub_project_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_project_mapping(&r)).transpose()
    }

    /// 幂等写入/更新项目映射（含 opt-in）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户确认 project opt-in 后需要持久化 hubProjectId、Git remote fingerprint 与本机路径映射。
    ///
    /// Code Logic（这个函数做什么）:
    ///     若 local_workbench_project_id 或 hub_project_id 已有行则 UPDATE，否则 INSERT；写路径经 shared lease。
    pub async fn upsert_project_mapping(
        &self,
        input: UpsertAgentHubProjectMapping,
    ) -> Result<AgentHubProjectMappingRow, AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            let existing = if let Some(local_id) = input.local_workbench_project_id.as_deref() {
                self.get_project_mapping_by_local_workbench_id(local_id)
                    .await?
            } else {
                None
            };
            let existing = match existing {
                Some(row) => Some(row),
                None => {
                    self.get_project_mapping_by_hub_project_id(&input.hub_project_id)
                        .await?
                }
            };
            if let Some(prev) = existing {
                sqlx::query(
                    "UPDATE agent_hub_project_mappings
                     SET hub_project_id = ?, local_workbench_project_id = ?,
                         git_remote_fingerprint = ?, local_absolute_path = ?,
                         opted_in = ?, updated_at = ?
                     WHERE id = ?",
                )
                .bind(&input.hub_project_id)
                .bind(&input.local_workbench_project_id)
                .bind(&input.git_remote_fingerprint)
                .bind(&input.local_absolute_path)
                .bind(if input.opted_in { 1 } else { 0 })
                .bind(&now)
                .bind(&prev.id)
                .execute(&self.pool)
                .await?;
                return Ok(AgentHubProjectMappingRow {
                    id: prev.id,
                    hub_project_id: input.hub_project_id,
                    local_workbench_project_id: input.local_workbench_project_id,
                    git_remote_fingerprint: input.git_remote_fingerprint,
                    local_absolute_path: input.local_absolute_path,
                    opted_in: input.opted_in,
                    created_at: prev.created_at,
                    updated_at: now,
                });
            }
            let id = uuid::Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO agent_hub_project_mappings
                 (id, hub_project_id, local_workbench_project_id, git_remote_fingerprint,
                  local_absolute_path, opted_in, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&input.hub_project_id)
            .bind(&input.local_workbench_project_id)
            .bind(&input.git_remote_fingerprint)
            .bind(&input.local_absolute_path)
            .bind(if input.opted_in { 1 } else { 0 })
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(AgentHubProjectMappingRow {
                id,
                hub_project_id: input.hub_project_id,
                local_workbench_project_id: input.local_workbench_project_id,
                git_remote_fingerprint: input.git_remote_fingerprint,
                local_absolute_path: input.local_absolute_path,
                opted_in: input.opted_in,
                created_at: now.clone(),
                updated_at: now,
            })
        })
        .await
    }

    /// 列出某 hub 项目下全部 checkout bindings。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     preview/status/refresh 需要看到主 checkout 与全部 worktree 绑定状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `SELECT ... WHERE hub_project_id = ? ORDER BY created_at ASC`。
    pub async fn list_checkout_bindings_by_hub_project(
        &self,
        hub_project_id: &str,
    ) -> Result<Vec<AgentHubCheckoutBindingRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, hub_project_id, workbench_worktree_id, checkout_kind, relative_root,
                    local_absolute_path, enabled, status, warning, created_at, updated_at
             FROM agent_hub_checkout_bindings
             WHERE hub_project_id = ?
             ORDER BY created_at ASC, id ASC",
        )
        .bind(hub_project_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_checkout_binding).collect()
    }

    /// 按主键读取 checkout binding。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     投影前需按 target_binding.checkout_binding_id 判定 blocked，避免覆盖预存 AGENTS.md。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT by id → row_to_checkout_binding。
    pub async fn get_checkout_binding(
        &self,
        id: &str,
    ) -> Result<Option<AgentHubCheckoutBindingRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, hub_project_id, workbench_worktree_id, checkout_kind, relative_root,
                    local_absolute_path, enabled, status, warning, created_at, updated_at
             FROM agent_hub_checkout_bindings WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_checkout_binding(&r)).transpose()
    }

    /// 按 hub + worktree id 读取单条 binding。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     upsert 前需要定位既有 binding，main 与 feature worktree 均以 workbench_worktree_id 区分。
    ///
    /// Code Logic（这个函数做什么）:
    ///     worktree_id 为 None 时匹配 `workbench_worktree_id IS NULL`，否则精确匹配。
    pub async fn get_checkout_binding_by_worktree(
        &self,
        hub_project_id: &str,
        workbench_worktree_id: Option<&str>,
    ) -> Result<Option<AgentHubCheckoutBindingRow>, AppError> {
        let row = if let Some(wt) = workbench_worktree_id {
            sqlx::query(
                "SELECT id, hub_project_id, workbench_worktree_id, checkout_kind, relative_root,
                        local_absolute_path, enabled, status, warning, created_at, updated_at
                 FROM agent_hub_checkout_bindings
                 WHERE hub_project_id = ? AND workbench_worktree_id = ?",
            )
            .bind(hub_project_id)
            .bind(wt)
            .fetch_optional(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, hub_project_id, workbench_worktree_id, checkout_kind, relative_root,
                        local_absolute_path, enabled, status, warning, created_at, updated_at
                 FROM agent_hub_checkout_bindings
                 WHERE hub_project_id = ? AND workbench_worktree_id IS NULL",
            )
            .bind(hub_project_id)
            .fetch_optional(&self.pool)
            .await?
        };
        row.map(|r| row_to_checkout_binding(&r)).transpose()
    }

    /// 幂等 upsert checkout binding（绝对路径仅存本表）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     opt-in 后主 checkout 与 Workbench worktree 需要本地 binding，供 projection 寻址。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 hub_project_id + workbench_worktree_id 定位，UPDATE 或 INSERT；经 shared lease。
    pub async fn upsert_checkout_binding(
        &self,
        input: UpsertAgentHubCheckoutBinding,
    ) -> Result<AgentHubCheckoutBindingRow, AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            let existing = self
                .get_checkout_binding_by_worktree(
                    &input.hub_project_id,
                    input.workbench_worktree_id.as_deref(),
                )
                .await?;
            if let Some(prev) = existing {
                sqlx::query(
                    "UPDATE agent_hub_checkout_bindings
                     SET checkout_kind = ?, relative_root = ?, local_absolute_path = ?,
                         enabled = ?, status = ?, warning = ?, updated_at = ?
                     WHERE id = ?",
                )
                .bind(&input.checkout_kind)
                .bind(&input.relative_root)
                .bind(&input.local_absolute_path)
                .bind(if input.enabled { 1 } else { 0 })
                .bind(&input.status)
                .bind(&input.warning)
                .bind(&now)
                .bind(&prev.id)
                .execute(&self.pool)
                .await?;
                return Ok(AgentHubCheckoutBindingRow {
                    id: prev.id,
                    hub_project_id: input.hub_project_id,
                    workbench_worktree_id: input.workbench_worktree_id,
                    checkout_kind: input.checkout_kind,
                    relative_root: input.relative_root,
                    local_absolute_path: input.local_absolute_path,
                    enabled: input.enabled,
                    status: input.status,
                    warning: input.warning,
                    created_at: prev.created_at,
                    updated_at: now,
                });
            }
            let id = uuid::Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO agent_hub_checkout_bindings
                 (id, hub_project_id, workbench_worktree_id, checkout_kind, relative_root,
                  local_absolute_path, enabled, status, warning, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&input.hub_project_id)
            .bind(&input.workbench_worktree_id)
            .bind(&input.checkout_kind)
            .bind(&input.relative_root)
            .bind(&input.local_absolute_path)
            .bind(if input.enabled { 1 } else { 0 })
            .bind(&input.status)
            .bind(&input.warning)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(AgentHubCheckoutBindingRow {
                id,
                hub_project_id: input.hub_project_id,
                workbench_worktree_id: input.workbench_worktree_id,
                checkout_kind: input.checkout_kind,
                relative_root: input.relative_root,
                local_absolute_path: input.local_absolute_path,
                enabled: input.enabled,
                status: input.status,
                warning: input.warning,
                created_at: now.clone(),
                updated_at: now,
            })
        })
        .await
    }

    /// 将 binding 标记为 detached（保留行，不删 canonical 资产）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Workbench 删除 worktree 后 binding 应变 detached，且不得 tombstone Hub 资产。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `UPDATE status='detached', enabled=0, updated_at=now WHERE id=?`。
    pub async fn mark_checkout_binding_detached(&self, id: &str) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "UPDATE agent_hub_checkout_bindings
                 SET status = 'detached', enabled = 0, updated_at = ?
                 WHERE id = ?",
            )
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    /// 按 hub_project_id 查找 project 级 ScopeNode。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     enable 时需要幂等确保 project scope 存在，且 scope 不得写入本机绝对路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `SELECT ... WHERE kind='project' AND hub_project_id=? LIMIT 1`。
    pub async fn get_project_scope_by_hub_project_id(
        &self,
        hub_project_id: &str,
    ) -> Result<Option<ScopeNode>, AppError> {
        let row = sqlx::query(
            "SELECT id, kind, hub_project_id, relative_path, created_at
             FROM agent_hub_scopes
             WHERE kind = 'project' AND hub_project_id = ?
             LIMIT 1",
        )
        .bind(hub_project_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_scope(&r)).transpose()
    }

    /// 插入 projection job（初始 prepared）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Hub revision commit 后必须持久化 projection job，崩溃后可对账恢复。
    ///
    /// Code Logic（这个函数做什么）:
    ///     INSERT prepared job；返回完整 ProjectionJob。
    pub async fn insert_projection_job(
        &self,
        input: NewProjectionJob,
    ) -> Result<ProjectionJob, AppError> {
        with_shared_write_lease(&self.gate, async {
            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            let state = ProjectionJobState::Prepared;
            let desired_revision = input
                .desired_revision_id
                .as_ref()
                .map(|r| r.as_str().to_string());
            sqlx::query(
                "INSERT INTO agent_hub_projection_jobs (
                    id, asset_id, target, target_binding_id, desired_revision_id, state, attempt,
                    last_error, target_path, expected_external_hash, rendered_hash,
                    rendered_object_hash, write_token, desired_presence, desired_enabled,
                    payload_kind, managed_paths_json, hub_project_id, staging_path, backup_path,
                    base_hash, created_at, updated_at
                 ) VALUES (
                    ?,?,?,?,?,?,0,NULL,?,?,?,?,?,?,?,?,?,?,NULL,NULL,?,?,?
                 )",
            )
            .bind(&id)
            .bind(&input.asset_id)
            .bind(input.target.as_str())
            .bind(&input.target_binding_id)
            .bind(desired_revision.as_deref())
            .bind(state.as_str())
            .bind(&input.target_path)
            .bind(input.expected_external_hash.as_deref())
            .bind(&input.rendered_hash)
            .bind(&input.rendered_object_hash)
            .bind(&input.write_token)
            .bind(input.desired_presence.as_str())
            .bind(if input.desired_enabled { 1_i64 } else { 0_i64 })
            .bind(input.payload_kind.as_str())
            .bind(input.managed_paths_json.as_deref())
            .bind(input.hub_project_id.as_deref())
            .bind(input.base_hash.as_deref())
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(ProjectionJob {
                id,
                asset_id: input.asset_id,
                target: input.target,
                target_binding_id: input.target_binding_id,
                desired_revision_id: input.desired_revision_id,
                state,
                attempt: 0,
                last_error: None,
                target_path: input.target_path,
                expected_external_hash: input.expected_external_hash,
                rendered_hash: input.rendered_hash,
                rendered_object_hash: input.rendered_object_hash,
                write_token: input.write_token,
                desired_presence: input.desired_presence,
                desired_enabled: input.desired_enabled,
                payload_kind: input.payload_kind,
                managed_paths_json: input.managed_paths_json,
                hub_project_id: input.hub_project_id,
                staging_path: None,
                backup_path: None,
                base_hash: input.base_hash,
                created_at: now.clone(),
                updated_at: now,
            })
        })
        .await
    }

    /// 按 id 读取 projection job。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     crash recovery 与测试需按 id 取 job 最新状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT by id → row_to_projection_job。
    pub async fn get_projection_job(&self, id: &str) -> Result<Option<ProjectionJob>, AppError> {
        let row = sqlx::query(
            "SELECT id, asset_id, target, target_binding_id, desired_revision_id, state, attempt,
                    last_error, target_path, expected_external_hash, rendered_hash,
                    rendered_object_hash, write_token, desired_presence, desired_enabled,
                    payload_kind, managed_paths_json, hub_project_id, staging_path, backup_path,
                    base_hash, created_at, updated_at
             FROM agent_hub_projection_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_projection_job(&r)).transpose()
    }

    /// 列出 prepared/writing 的可恢复 job。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     owner 启动时必须先对账未完成 job，再处理 watcher 事件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT WHERE state IN (prepared, writing) ORDER BY created_at。
    pub async fn list_recoverable_projection_jobs(&self) -> Result<Vec<ProjectionJob>, AppError> {
        let rows = sqlx::query(
            "SELECT id, asset_id, target, target_binding_id, desired_revision_id, state, attempt,
                    last_error, target_path, expected_external_hash, rendered_hash,
                    rendered_object_hash, write_token, desired_presence, desired_enabled,
                    payload_kind, managed_paths_json, hub_project_id, staging_path, backup_path,
                    base_hash, created_at, updated_at
             FROM agent_hub_projection_jobs
             WHERE state IN ('prepared', 'writing')
             ORDER BY created_at ASC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row_to_projection_job(&row)?);
        }
        Ok(out)
    }

    /// 列出 ready 可跑的 prepared job。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     scheduler 每轮领取 prepared job。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT prepared ORDER BY created_at LIMIT。
    pub async fn list_prepared_projection_jobs(
        &self,
        limit: i64,
    ) -> Result<Vec<ProjectionJob>, AppError> {
        let rows = sqlx::query(
            "SELECT id, asset_id, target, target_binding_id, desired_revision_id, state, attempt,
                    last_error, target_path, expected_external_hash, rendered_hash,
                    rendered_object_hash, write_token, desired_presence, desired_enabled,
                    payload_kind, managed_paths_json, hub_project_id, staging_path, backup_path,
                    base_hash, created_at, updated_at
             FROM agent_hub_projection_jobs
             WHERE state = 'prepared'
             ORDER BY created_at ASC, id ASC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row_to_projection_job(&row)?);
        }
        Ok(out)
    }

    /// 更新 projection job 状态与路径元数据。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     writing/committed/failed/drifted 必须原子写回 ledger。
    ///
    /// Code Logic（这个函数做什么）:
    ///     UPDATE state/attempt/error/staging/backup/updated_at。
    pub async fn update_projection_job_state(
        &self,
        id: &str,
        state: ProjectionJobState,
        attempt: u32,
        last_error: Option<&str>,
        staging_path: Option<&str>,
        backup_path: Option<&str>,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            let result = sqlx::query(
                "UPDATE agent_hub_projection_jobs
                 SET state = ?, attempt = ?, last_error = ?, staging_path = ?, backup_path = ?,
                     updated_at = ?
                 WHERE id = ?",
            )
            .bind(state.as_str())
            .bind(attempt as i64)
            .bind(last_error)
            .bind(staging_path)
            .bind(backup_path)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
            if result.rows_affected() == 0 {
                return Err(AppError::not_found(format!(
                    "agent_hub_projection_job_not_found:{id}"
                )));
            }
            Ok(())
        })
        .await
    }

    /// 标记 job committed 并 upsert materialization。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     只有目标 hash 校验通过后才能同时提交 job + materialization。
    ///
    /// Code Logic（这个函数做什么）:
    ///     同事务 UPDATE job=committed 并 upsert materialization synced。
    pub async fn commit_projection_job(
        &self,
        job: &ProjectionJob,
        observed_hash: &str,
    ) -> Result<Materialization, AppError> {
        with_shared_write_lease(&self.gate, async {
            let mut tx = self.pool.begin().await?;
            let now = chrono::Utc::now().to_rfc3339();
            // state CAS：仅 writing/prepared 与 Gate B 激活中间态可提交，防止已 committed/failed 旧 job 覆盖
            // （attempt 在 writing 路径已 +1，内存 job.attempt 可能落后，故不强制 attempt 等值）
            let result = sqlx::query(
                "UPDATE agent_hub_projection_jobs
                 SET state = ?, last_error = NULL, updated_at = ?
                 WHERE id = ?
                   AND state IN (
                     'writing',
                     'prepared',
                     'packageWritten',
                     'activationRequested',
                     'activationVerified'
                   )",
            )
            .bind(ProjectionJobState::Committed.as_str())
            .bind(&now)
            .bind(&job.id)
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() == 0 {
                return Err(AppError::conflict(format!(
                    "agent_hub_projection_job_commit_cas_miss:{}",
                    job.id
                )));
            }

            let existing = sqlx::query(
                "SELECT id, asset_id, target, target_binding_id, native_path,
                        last_projected_revision_id, rendered_hash, observed_external_hash,
                        status, last_error, created_at, updated_at
                 FROM agent_hub_materializations
                 WHERE target_binding_id = ?",
            )
            .bind(&job.target_binding_id)
            .fetch_optional(&mut *tx)
            .await?;

            let mat = if let Some(row) = existing {
                let id: String = row.try_get("id")?;
                let created_at: String = row.try_get("created_at")?;
                sqlx::query(
                    "UPDATE agent_hub_materializations
                     SET native_path = ?, last_projected_revision_id = ?, rendered_hash = ?,
                         observed_external_hash = ?, status = ?, last_error = NULL, updated_at = ?
                     WHERE id = ?",
                )
                .bind(&job.target_path)
                .bind(
                    job.desired_revision_id
                        .as_ref()
                        .map(|r| r.as_str().to_string()),
                )
                .bind(&job.rendered_hash)
                .bind(observed_hash)
                .bind(MaterializationStatus::Synced.as_str())
                .bind(&now)
                .bind(&id)
                .execute(&mut *tx)
                .await?;
                Materialization {
                    id,
                    asset_id: job.asset_id.clone(),
                    target: job.target,
                    target_binding_id: job.target_binding_id.clone(),
                    native_path: Some(job.target_path.clone()),
                    last_projected_revision_id: job.desired_revision_id.clone(),
                    rendered_hash: Some(job.rendered_hash.clone()),
                    observed_external_hash: Some(observed_hash.to_string()),
                    status: MaterializationStatus::Synced,
                    last_error: None,
                    created_at,
                    updated_at: now,
                }
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO agent_hub_materializations (
                        id, asset_id, target, target_binding_id, native_path,
                        last_projected_revision_id, rendered_hash, observed_external_hash,
                        status, last_error, created_at, updated_at
                     ) VALUES (?,?,?,?,?,?,?,?,?,NULL,?,?)",
                )
                .bind(&id)
                .bind(&job.asset_id)
                .bind(job.target.as_str())
                .bind(&job.target_binding_id)
                .bind(&job.target_path)
                .bind(
                    job.desired_revision_id
                        .as_ref()
                        .map(|r| r.as_str().to_string()),
                )
                .bind(&job.rendered_hash)
                .bind(observed_hash)
                .bind(MaterializationStatus::Synced.as_str())
                .bind(&now)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
                Materialization {
                    id,
                    asset_id: job.asset_id.clone(),
                    target: job.target,
                    target_binding_id: job.target_binding_id.clone(),
                    native_path: Some(job.target_path.clone()),
                    last_projected_revision_id: job.desired_revision_id.clone(),
                    rendered_hash: Some(job.rendered_hash.clone()),
                    observed_external_hash: Some(observed_hash.to_string()),
                    status: MaterializationStatus::Synced,
                    last_error: None,
                    created_at: now.clone(),
                    updated_at: now,
                }
            };

            tx.commit().await?;
            Ok(mat)
        })
        .await
    }

    /// 标记 materialization 为 drift/blocked 等观测状态。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     precondition 失败或未知外部文件时不能写盘，只更新观测。
    ///
    /// Code Logic（这个函数做什么）:
    ///     upsert materialization by target_binding_id。
    pub async fn upsert_materialization(
        &self,
        input: NewMaterialization,
    ) -> Result<Materialization, AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            let existing = sqlx::query(
                "SELECT id, created_at FROM agent_hub_materializations
                 WHERE target_binding_id = ?",
            )
            .bind(&input.target_binding_id)
            .fetch_optional(&self.pool)
            .await?;
            if let Some(row) = existing {
                let id: String = row.try_get("id")?;
                let created_at: String = row.try_get("created_at")?;
                sqlx::query(
                    "UPDATE agent_hub_materializations
                     SET native_path = ?, last_projected_revision_id = ?, rendered_hash = ?,
                         observed_external_hash = ?, status = ?, last_error = ?, updated_at = ?
                     WHERE id = ?",
                )
                .bind(input.native_path.as_deref())
                .bind(
                    input
                        .last_projected_revision_id
                        .as_ref()
                        .map(|r| r.as_str().to_string()),
                )
                .bind(input.rendered_hash.as_deref())
                .bind(input.observed_external_hash.as_deref())
                .bind(input.status.as_str())
                .bind(input.last_error.as_deref())
                .bind(&now)
                .bind(&id)
                .execute(&self.pool)
                .await?;
                Ok(Materialization {
                    id,
                    asset_id: input.asset_id,
                    target: input.target,
                    target_binding_id: input.target_binding_id,
                    native_path: input.native_path,
                    last_projected_revision_id: input.last_projected_revision_id,
                    rendered_hash: input.rendered_hash,
                    observed_external_hash: input.observed_external_hash,
                    status: input.status,
                    last_error: input.last_error,
                    created_at,
                    updated_at: now,
                })
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO agent_hub_materializations (
                        id, asset_id, target, target_binding_id, native_path,
                        last_projected_revision_id, rendered_hash, observed_external_hash,
                        status, last_error, created_at, updated_at
                     ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                )
                .bind(&id)
                .bind(&input.asset_id)
                .bind(input.target.as_str())
                .bind(&input.target_binding_id)
                .bind(input.native_path.as_deref())
                .bind(
                    input
                        .last_projected_revision_id
                        .as_ref()
                        .map(|r| r.as_str().to_string()),
                )
                .bind(input.rendered_hash.as_deref())
                .bind(input.observed_external_hash.as_deref())
                .bind(input.status.as_str())
                .bind(input.last_error.as_deref())
                .bind(&now)
                .bind(&now)
                .execute(&self.pool)
                .await?;
                Ok(Materialization {
                    id,
                    asset_id: input.asset_id,
                    target: input.target,
                    target_binding_id: input.target_binding_id,
                    native_path: input.native_path,
                    last_projected_revision_id: input.last_projected_revision_id,
                    rendered_hash: input.rendered_hash,
                    observed_external_hash: input.observed_external_hash,
                    status: input.status,
                    last_error: input.last_error,
                    created_at: now.clone(),
                    updated_at: now,
                })
            }
        })
        .await
    }

    /// 按 target_binding 读取 materialization。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     调度与 UI 需查询当前投影观测。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT by target_binding_id。
    pub async fn get_materialization_by_binding(
        &self,
        target_binding_id: &str,
    ) -> Result<Option<Materialization>, AppError> {
        let row = sqlx::query(
            "SELECT id, asset_id, target, target_binding_id, native_path,
                    last_projected_revision_id, rendered_hash, observed_external_hash,
                    status, last_error, created_at, updated_at
             FROM agent_hub_materializations
             WHERE target_binding_id = ?",
        )
        .bind(target_binding_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_materialization(&r)).transpose()
    }

    /// 列出全部 materialization（owner scan 用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     runtime full/dirty scan 需要对照全部目标路径的 rendered/observed hash。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT * FROM agent_hub_materializations ORDER BY updated_at ASC, id ASC。
    pub async fn list_materializations(&self) -> Result<Vec<Materialization>, AppError> {
        let rows = sqlx::query(
            "SELECT id, asset_id, target, target_binding_id, native_path,
                    last_projected_revision_id, rendered_hash, observed_external_hash,
                    status, last_error, created_at, updated_at
             FROM agent_hub_materializations
             ORDER BY updated_at ASC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_materialization).collect()
    }

    /// 列出阻塞中的 materialization（Attention/status 投影用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Inbox 与 status 需要展示 blocked/drift/conflict 投影，让用户导航到受影响资产。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT WHERE status IN ('blocked','drift','conflict') ORDER BY updated_at DESC, id ASC。
    pub async fn list_blocked_materializations(&self) -> Result<Vec<Materialization>, AppError> {
        let rows = sqlx::query(
            "SELECT id, asset_id, target, target_binding_id, native_path,
                    last_projected_revision_id, rendered_hash, observed_external_hash,
                    status, last_error, created_at, updated_at
             FROM agent_hub_materializations
             WHERE status IN ('blocked', 'drift', 'conflict')
             ORDER BY updated_at DESC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_materialization).collect()
    }

    /// 按 (scope, kind, origin_namespace, logical_key) 查找资产。
    ///
    /// Business Logic: 纳管同一 skill 名时复用 shared LogicalAsset，避免双资产。
    /// Code Logic: SELECT 唯一键。
    pub async fn find_asset_by_key(
        &self,
        scope_id: &str,
        kind: AssetKind,
        origin_namespace: &str,
        logical_key: &str,
    ) -> Result<Option<LogicalAsset>, AppError> {
        let row = sqlx::query(
            "SELECT id, scope_id, kind, origin_namespace, logical_key, display_name, policy,
                    current_revision_id, deleted_at, created_at, updated_at
             FROM agent_hub_assets
             WHERE scope_id = ? AND kind = ? AND origin_namespace = ? AND logical_key = ?
               AND deleted_at IS NULL
             LIMIT 1",
        )
        .bind(scope_id)
        .bind(kind.as_str())
        .bind(origin_namespace)
        .bind(logical_key)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_asset(&r)).transpose()
    }

    /// 写入/更新 adoption 行（Gate B Task 6）。
    ///
    /// Business Logic: prepared→activated→archived→committed 需可崩溃恢复。
    /// Code Logic: INSERT OR REPLACE by id。
    pub async fn upsert_adoption(&self, rec: AdoptionRecord) -> Result<AdoptionRecord, AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO agent_hub_adoptions (
                    id, asset_id, target, origin_path, origin_tree_hash, archive_tree_hash,
                    materialization_id, package_id, staging_path, state, last_error,
                    confirmed, created_at, updated_at
                 ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)
                 ON CONFLICT(id) DO UPDATE SET
                    asset_id=excluded.asset_id,
                    target=excluded.target,
                    origin_path=excluded.origin_path,
                    origin_tree_hash=excluded.origin_tree_hash,
                    archive_tree_hash=excluded.archive_tree_hash,
                    materialization_id=excluded.materialization_id,
                    package_id=excluded.package_id,
                    staging_path=excluded.staging_path,
                    state=excluded.state,
                    last_error=excluded.last_error,
                    confirmed=excluded.confirmed,
                    updated_at=excluded.updated_at",
            )
            .bind(&rec.id)
            .bind(rec.asset_id.as_deref())
            .bind(rec.target.as_str())
            .bind(&rec.origin_path)
            .bind(&rec.origin_tree_hash)
            .bind(rec.archive_tree_hash.as_deref())
            .bind(rec.materialization_id.as_deref())
            .bind(rec.package_id.as_deref())
            .bind(rec.staging_path.as_deref())
            .bind(rec.state.as_str())
            .bind(rec.last_error.as_deref())
            .bind(if rec.confirmed { 1 } else { 0 })
            .bind(&rec.created_at)
            .bind(&rec.updated_at)
            .execute(&self.pool)
            .await?;
            Ok(rec)
        })
        .await
    }

    /// 更新 adoption 状态与可选字段。
    ///
    /// Business Logic: 事务步骤推进时局部更新，避免整行重写竞态。
    /// Code Logic: UPDATE SET state/... WHERE id。
    #[allow(clippy::too_many_arguments)]
    pub async fn update_adoption_state(
        &self,
        id: &str,
        state: AdoptionState,
        last_error: Option<&str>,
        asset_id: Option<&str>,
        package_id: Option<&str>,
        archive_tree_hash: Option<&str>,
        materialization_id: Option<&str>,
        staging_path: Option<&str>,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            let n = sqlx::query(
                "UPDATE agent_hub_adoptions
                 SET state = ?,
                     last_error = COALESCE(?, last_error),
                     asset_id = COALESCE(?, asset_id),
                     package_id = COALESCE(?, package_id),
                     archive_tree_hash = COALESCE(?, archive_tree_hash),
                     materialization_id = COALESCE(?, materialization_id),
                     staging_path = COALESCE(?, staging_path),
                     updated_at = ?
                 WHERE id = ?",
            )
            .bind(state.as_str())
            .bind(last_error)
            .bind(asset_id)
            .bind(package_id)
            .bind(archive_tree_hash)
            .bind(materialization_id)
            .bind(staging_path)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
            if n == 0 {
                return Err(AppError::not_found(format!(
                    "agent_hub_adoption_not_found:{id}"
                )));
            }
            Ok(())
        })
        .await
    }

    /// 读取单条 adoption。
    pub async fn get_adoption(&self, id: &str) -> Result<Option<AdoptionRecord>, AppError> {
        let row = sqlx::query(
            "SELECT id, asset_id, target, origin_path, origin_tree_hash, archive_tree_hash,
                    materialization_id, package_id, staging_path, state, last_error,
                    confirmed, created_at, updated_at
             FROM agent_hub_adoptions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_adoption(&r)).transpose()
    }

    /// 列出全部 adoption（recovery / 测试）。
    pub async fn list_adoptions(&self) -> Result<Vec<AdoptionRecord>, AppError> {
        let rows = sqlx::query(
            "SELECT id, asset_id, target, origin_path, origin_tree_hash, archive_tree_hash,
                    materialization_id, package_id, staging_path, state, last_error,
                    confirmed, created_at, updated_at
             FROM agent_hub_adoptions
             ORDER BY updated_at ASC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_adoption).collect()
    }

    /// 列出未完成 adoption（prepared/activated/archived）供 startup recovery。
    pub async fn list_incomplete_adoptions(&self) -> Result<Vec<AdoptionRecord>, AppError> {
        let rows = sqlx::query(
            "SELECT id, asset_id, target, origin_path, origin_tree_hash, archive_tree_hash,
                    materialization_id, package_id, staging_path, state, last_error,
                    confirmed, created_at, updated_at
             FROM agent_hub_adoptions
             WHERE state IN ('prepared', 'activated', 'archived')
             ORDER BY updated_at ASC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_adoption).collect()
    }

    /// 写入或更新用户级指令 ownership。
    ///
    /// Business Logic: 安全 overwrite/delete 必须有与 package adoption 分离的指令所有权记录。
    /// Code Logic: 按 asset+target upsert，保留首次 created_at。
    pub async fn upsert_user_instruction_ownership(
        &self,
        record: UserInstructionOwnershipRecord,
    ) -> Result<UserInstructionOwnershipRecord, AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO agent_hub_user_instruction_ownership (
                    asset_id, target, resolved_path, adopted_hash, adopted_revision_id,
                    adoption_operation, confirmed_plan_token, created_at, updated_at
                 ) VALUES (?,?,?,?,?,?,?,?,?)
                 ON CONFLICT(asset_id, target) DO UPDATE SET
                    resolved_path=excluded.resolved_path,
                    adopted_hash=excluded.adopted_hash,
                    adopted_revision_id=excluded.adopted_revision_id,
                    adoption_operation=excluded.adoption_operation,
                    confirmed_plan_token=excluded.confirmed_plan_token,
                    updated_at=excluded.updated_at",
            )
            .bind(&record.asset_id)
            .bind(record.target.as_str())
            .bind(&record.resolved_path)
            .bind(record.adopted_hash.as_deref())
            .bind(
                record
                    .adopted_revision_id
                    .as_ref()
                    .map(|revision| revision.as_str()),
            )
            .bind(&record.adoption_operation)
            .bind(&record.confirmed_plan_token)
            .bind(&record.created_at)
            .bind(&record.updated_at)
            .execute(&self.pool)
            .await?;
            Ok(record)
        })
        .await
    }

    /// 读取单 target 用户级指令 ownership。
    pub async fn get_user_instruction_ownership(
        &self,
        asset_id: &str,
        target: AgentTarget,
    ) -> Result<Option<UserInstructionOwnershipRecord>, AppError> {
        let row = sqlx::query(
            "SELECT asset_id, target, resolved_path, adopted_hash, adopted_revision_id,
                    adoption_operation, confirmed_plan_token, created_at, updated_at
             FROM agent_hub_user_instruction_ownership
             WHERE asset_id = ? AND target = ?",
        )
        .bind(asset_id)
        .bind(target.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| row_to_user_instruction_ownership(&row))
            .transpose()
    }

    /// 列出资产的用户级指令 ownership。
    pub async fn list_user_instruction_ownerships(
        &self,
        asset_id: &str,
    ) -> Result<Vec<UserInstructionOwnershipRecord>, AppError> {
        let rows = sqlx::query(
            "SELECT asset_id, target, resolved_path, adopted_hash, adopted_revision_id,
                    adoption_operation, confirmed_plan_token, created_at, updated_at
             FROM agent_hub_user_instruction_ownership
             WHERE asset_id = ? ORDER BY target ASC",
        )
        .bind(asset_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_user_instruction_ownership).collect()
    }

    /// 持久化短期 V2 preview plan。
    ///
    /// Business Logic: plan 由 owner 管理，GuiClient 只回传不可猜 token。
    /// Code Logic: token 冲突 fail-closed，不在日志中输出 plan_json。
    pub async fn insert_user_instruction_plan(
        &self,
        record: UserInstructionPlanRecord,
    ) -> Result<UserInstructionPlanRecord, AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO agent_hub_user_instruction_plans (
                    plan_token, owner_fingerprint, expires_at, base_revision_id,
                    inventory_snapshot_hash, plan_json, client_request_id, claimed_at,
                    consumed_at, result_json, created_at
                 ) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            )
            .bind(&record.plan_token)
            .bind(&record.owner_fingerprint)
            .bind(&record.expires_at)
            .bind(
                record
                    .base_revision_id
                    .as_ref()
                    .map(|revision| revision.as_str()),
            )
            .bind(&record.inventory_snapshot_hash)
            .bind(&record.plan_json)
            .bind(record.client_request_id.as_deref())
            .bind(record.claimed_at.as_deref())
            .bind(record.consumed_at.as_deref())
            .bind(record.result_json.as_deref())
            .bind(&record.created_at)
            .execute(&self.pool)
            .await?;
            Ok(record)
        })
        .await
    }

    /// 读取 V2 preview plan。
    pub async fn get_user_instruction_plan(
        &self,
        plan_token: &str,
    ) -> Result<Option<UserInstructionPlanRecord>, AppError> {
        let row = sqlx::query(
            "SELECT plan_token, owner_fingerprint, expires_at, base_revision_id,
                    inventory_snapshot_hash, plan_json, client_request_id, claimed_at,
                    consumed_at, result_json, created_at
             FROM agent_hub_user_instruction_plans WHERE plan_token = ?",
        )
        .bind(plan_token)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| row_to_user_instruction_plan(&row))
            .transpose()
    }

    /// 原子 claim preview plan。
    ///
    /// Business Logic: get→write 不能留并发窗口；同 token 同时只有一个文件写执行者。
    /// Code Logic: 事务内 CAS client_request_id NULL→id，再判定同 id pending/replay 或异 id conflict。
    pub async fn claim_user_instruction_plan(
        &self,
        plan_token: &str,
        client_request_id: &str,
    ) -> Result<UserInstructionPlanClaim, AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            let mut tx = self.pool.begin().await?;
            let claimed = sqlx::query(
                "UPDATE agent_hub_user_instruction_plans
                 SET client_request_id = ?, claimed_at = ?
                 WHERE plan_token = ? AND client_request_id IS NULL AND consumed_at IS NULL",
            )
            .bind(client_request_id)
            .bind(&now)
            .bind(plan_token)
            .execute(&mut *tx)
            .await?;
            let row = sqlx::query(
                "SELECT plan_token, owner_fingerprint, expires_at, base_revision_id,
                        inventory_snapshot_hash, plan_json, client_request_id, claimed_at,
                        consumed_at, result_json, created_at
                 FROM agent_hub_user_instruction_plans WHERE plan_token = ?",
            )
            .bind(plan_token)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(row) = row else {
                return Err(AppError::not_found("USER_INSTRUCTION_PLAN_NOT_FOUND"));
            };
            let record = row_to_user_instruction_plan(&row)?;
            let outcome = if claimed.rows_affected() == 1 {
                UserInstructionPlanClaim::Claimed(record)
            } else if record.client_request_id.as_deref() != Some(client_request_id) {
                return Err(AppError::conflict(
                    "USER_INSTRUCTION_PLAN_CLAIMED_BY_ANOTHER_REQUEST",
                ));
            } else if let Some(result_json) = record.result_json {
                UserInstructionPlanClaim::Replay(result_json)
            } else {
                UserInstructionPlanClaim::Pending
            };
            tx.commit().await?;
            Ok(outcome)
        })
        .await
    }

    /// 完成已 claim 的 preview plan 并持久化幂等结果。
    pub async fn complete_user_instruction_plan(
        &self,
        plan_token: &str,
        client_request_id: &str,
        result_json: &str,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            let result = sqlx::query(
                "UPDATE agent_hub_user_instruction_plans
                 SET consumed_at = ?, result_json = ?
                 WHERE plan_token = ? AND client_request_id = ? AND consumed_at IS NULL",
            )
            .bind(&now)
            .bind(result_json)
            .bind(plan_token)
            .bind(client_request_id)
            .execute(&self.pool)
            .await?;
            if result.rows_affected() != 1 {
                return Err(AppError::conflict(
                    "USER_INSTRUCTION_PLAN_COMPLETE_CONFLICT",
                ));
            }
            Ok(())
        })
        .await
    }

    /// 列出全部未解决 conflict（Attention 投影用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     未解决 conflict 必须进入 Inbox 决策列表，并冻结相关投影。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT WHERE resolved=0 ORDER BY created_at DESC, id ASC。
    pub async fn list_unresolved_conflicts(&self) -> Result<Vec<AgentHubConflict>, AppError> {
        let rows = sqlx::query(
            "SELECT id, asset_id, target, base_revision_id, hub_revision_id,
                    external_revision_id, detail_json, resolved, created_at, resolved_at
             FROM agent_hub_conflicts
             WHERE resolved = 0
             ORDER BY created_at DESC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_conflict).collect()
    }

    /// 是否存在未解决的 asset 级（canonical）conflict。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     canonical conflict 冻结该资产全部 target 投影。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT EXISTS unresolved conflict WHERE target IS NULL。
    pub async fn has_unresolved_canonical_conflict(
        &self,
        asset_id: &str,
    ) -> Result<bool, AppError> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(1) FROM agent_hub_conflicts
             WHERE asset_id = ? AND resolved = 0 AND target IS NULL",
        )
        .bind(asset_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0 > 0)
    }

    /// 是否存在未解决的 target 级 conflict。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     target conflict 仅冻结该 target/checkout 投影。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT COUNT unresolved conflict for asset+target。
    pub async fn has_unresolved_target_conflict(
        &self,
        asset_id: &str,
        target: AgentTarget,
    ) -> Result<bool, AppError> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(1) FROM agent_hub_conflicts
             WHERE asset_id = ? AND resolved = 0 AND target = ?",
        )
        .bind(asset_id)
        .bind(target.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0 > 0)
    }

    /// 插入未解决 conflict（调度冻结测试与 reconcile 共用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     冲突出现时必须落库，投影入口据此冻结。
    ///
    /// Code Logic（这个函数做什么）:
    ///     INSERT conflict resolved=0。
    pub async fn insert_conflict(
        &self,
        asset_id: &str,
        target: Option<AgentTarget>,
        detail_json: &str,
    ) -> Result<String, AppError> {
        with_shared_write_lease(&self.gate, async {
            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO agent_hub_conflicts (
                    id, asset_id, target, base_revision_id, hub_revision_id,
                    external_revision_id, detail_json, resolved, created_at, resolved_at
                 ) VALUES (?,?,?,NULL,NULL,NULL,?,0,?,NULL)",
            )
            .bind(&id)
            .bind(asset_id)
            .bind(target.map(|t| t.as_str().to_string()))
            .bind(detail_json)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(id)
        })
        .await
    }

    /// 列出逻辑资产（默认排除软删除）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Agent Hub UI 与 service 层需要按 scope/kind 过滤当前可管理资产。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `deleted_at IS NULL`；可选 `scope_id` / `kind` 过滤；按 display_name/id 排序。
    pub async fn list_assets(
        &self,
        scope_id: Option<&str>,
        kind: Option<AssetKind>,
    ) -> Result<Vec<LogicalAsset>, AppError> {
        let rows = sqlx::query(
            "SELECT id, scope_id, kind, origin_namespace, logical_key, display_name, policy,
                    current_revision_id, deleted_at, created_at, updated_at
             FROM agent_hub_assets
             WHERE deleted_at IS NULL
               AND (?1 IS NULL OR scope_id = ?1)
               AND (?2 IS NULL OR kind = ?2)
             ORDER BY display_name ASC, id ASC",
        )
        .bind(scope_id)
        .bind(kind.map(|k| k.as_str().to_string()))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_asset).collect()
    }

    /// 列出全部 scope 节点。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     前端 scope 过滤器与服务层映射需要完整 scope 列表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT scopes 按 kind/id 排序。
    pub async fn list_scopes(&self) -> Result<Vec<ScopeNode>, AppError> {
        let rows = sqlx::query(
            "SELECT id, kind, hub_project_id, relative_path, created_at
             FROM agent_hub_scopes
             ORDER BY kind ASC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_scope).collect()
    }

    /// 按 id 读取 conflict。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     resolve 与详情页需要单条 conflict 完整字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT by id → row_to_conflict。
    pub async fn get_conflict(&self, id: &str) -> Result<Option<AgentHubConflict>, AppError> {
        let row = sqlx::query(
            "SELECT id, asset_id, target, base_revision_id, hub_revision_id,
                    external_revision_id, detail_json, resolved, created_at, resolved_at
             FROM agent_hub_conflicts WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_conflict(&r)).transpose()
    }

    /// 标记 conflict 已解决。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户选择 KeepHub/KeepExternal/Manual 后必须解冻投影。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写 `resolved=1` + `resolved_at=now`；已解决幂等返回；缺失 → not_found。
    pub async fn resolve_conflict(
        &self,
        id: &str,
        now: &str,
    ) -> Result<AgentHubConflict, AppError> {
        with_shared_write_lease(&self.gate, async {
            let updated = sqlx::query(
                "UPDATE agent_hub_conflicts
                 SET resolved = 1, resolved_at = ?
                 WHERE id = ? AND resolved = 0",
            )
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
            if updated == 0 {
                let existing = self.get_conflict(id).await?;
                return match existing {
                    Some(c) if c.resolved => Ok(c),
                    Some(_) => Err(AppError::conflict(
                        "agent_hub_conflict_resolve_race".to_string(),
                    )),
                    None => Err(AppError::not_found(format!(
                        "agent_hub_conflict_not_found:{id}"
                    ))),
                };
            }
            self.get_conflict(id)
                .await?
                .ok_or_else(|| AppError::not_found(format!("agent_hub_conflict_not_found:{id}")))
        })
        .await
    }

    /// 检查 hub_project 是否已 opt-in。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     未 opt-in 项目禁止插入 projection job。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT opted_in FROM project_mappings。
    pub async fn is_hub_project_opted_in(&self, hub_project_id: &str) -> Result<bool, AppError> {
        let row =
            sqlx::query("SELECT opted_in FROM agent_hub_project_mappings WHERE hub_project_id = ?")
                .bind(hub_project_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(match row {
            Some(r) => {
                let v: i64 = r.try_get("opted_in")?;
                v != 0
            }
            None => false,
        })
    }

    // ── Snapshot builder 只读辅助（Gate C Task 2）────────────────────────

    /// 在**单次** SQLite 读事务内加载 snapshot 身份集合。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Snapshot builder 必须在同一一致快照下读 heads/ancestry/variants/conflicts/aliases；
    ///     多条 auto-commit 读会与并发 Hub writer 撕裂，产出从未提交过的 envelope 或假 missing。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `pool.begin()` 延迟读事务 → 解析 selection assets → lineages/heads/ancestry/variants/
    ///     conflicts/scopes/aliases → `commit`；CAS 流式读取由调用方在 TX 外完成。
    pub async fn load_snapshot_identity_bundle(
        &self,
        request: &SnapshotIdentityRequest,
    ) -> Result<SnapshotIdentityBundle, AppError> {
        let mut tx = self.pool.begin().await?;
        let assets = resolve_selected_assets_on_tx(&mut tx, request).await?;
        let asset_ids: Vec<String> = assets.iter().map(|a| a.id.clone()).collect();

        let mut head_ids: Vec<String> = Vec::new();
        for a in &assets {
            if let Some(rev) = &a.current_revision_id {
                head_ids.push(rev.as_str().to_string());
            }
        }

        let variant_rows = list_variants_for_assets_on_tx(&mut tx, &asset_ids).await?;
        let mut seed_rev_ids = head_ids;
        for v in &variant_rows {
            seed_rev_ids.push(v.revision_id.clone());
        }
        seed_rev_ids.sort();
        seed_rev_ids.dedup();

        let revisions = if request.include_history {
            collect_revision_ancestry_on_tx(&mut tx, &seed_rev_ids).await?
        } else {
            let mut only = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            for id in &seed_rev_ids {
                if !seen.insert(id.clone()) {
                    continue;
                }
                if let Some(r) = get_revision_on_tx(&mut tx, id).await? {
                    only.push(r);
                }
            }
            only.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
            only
        };

        // Gate D：package revision 固定 component revision refs / residual trees 进入闭包，
        // 即使 component 不是 selection active head。闭包内每个 revision 都查边表
        // （非 package 修订返回空集）。
        let mut package_rev_ids: Vec<String> = revisions
            .iter()
            .map(|r| r.id.as_str().to_string())
            .collect();
        for a in &assets {
            if a.kind == AssetKind::Plugin {
                if let Some(rev) = &a.current_revision_id {
                    package_rev_ids.push(rev.as_str().to_string());
                }
            }
        }
        package_rev_ids.sort();
        package_rev_ids.dedup();

        let mut extra_asset_ids: std::collections::BTreeSet<String> =
            asset_ids.iter().cloned().collect();
        let mut extra_seed_revs: Vec<String> = seed_rev_ids.clone();
        let mut residual_tree_hashes: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();

        for pkg_rev in &package_rev_ids {
            let comps = list_plugin_components_on_tx(&mut tx, pkg_rev).await?;
            for c in comps {
                extra_asset_ids.insert(c.asset_id.clone());
                extra_seed_revs.push(c.revision_id.as_str().to_string());
            }
            let residuals = list_plugin_residuals_on_tx(&mut tx, pkg_rev).await?;
            for r in residuals {
                residual_tree_hashes.insert(r.tree_manifest_hash);
            }
        }

        // 扩展 assets 集合（component 可能不在原 selection）
        let mut expanded_assets = assets;
        for id in &extra_asset_ids {
            if expanded_assets.iter().any(|a| a.id == *id) {
                continue;
            }
            if let Some(a) = get_asset_on_tx(&mut tx, id).await? {
                expanded_assets.push(a);
            }
        }
        expanded_assets.sort_by(|a, b| a.id.cmp(&b.id));
        expanded_assets.dedup_by(|a, b| a.id == b.id);
        let assets = expanded_assets;
        let asset_ids: Vec<String> = assets.iter().map(|a| a.id.clone()).collect();
        let lineages = list_lineages_for_assets_on_tx(&mut tx, &asset_ids).await?;
        // variants 可能因新增 component assets 而缺失；重新加载
        let variant_rows = list_variants_for_assets_on_tx(&mut tx, &asset_ids).await?;

        extra_seed_revs.sort();
        extra_seed_revs.dedup();
        let revisions = if request.include_history {
            collect_revision_ancestry_on_tx(&mut tx, &extra_seed_revs).await?
        } else {
            let mut only = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            for id in &extra_seed_revs {
                if !seen.insert(id.clone()) {
                    continue;
                }
                if let Some(r) = get_revision_on_tx(&mut tx, id).await? {
                    only.push(r);
                }
            }
            only.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
            only
        };

        let residual_tree_hashes: Vec<String> = residual_tree_hashes.into_iter().collect();

        let conflicts = list_unresolved_conflicts_for_assets_on_tx(&mut tx, &asset_ids).await?;

        let mut hub_ids: std::collections::BTreeSet<String> =
            request.hub_project_ids.iter().cloned().collect();
        for a in &assets {
            if let Some(scope) = get_scope_on_tx(&mut tx, &a.scope_id).await? {
                if let Some(hub) = scope.hub_project_id {
                    hub_ids.insert(hub);
                }
            }
        }
        let hub_project_ids: Vec<String> = hub_ids.into_iter().collect();
        let aliases = list_portable_project_aliases_on_tx(&mut tx, &hub_project_ids).await?;

        tx.commit().await?;
        Ok(SnapshotIdentityBundle {
            assets,
            lineages,
            revisions,
            variants: variant_rows,
            conflicts,
            aliases,
            hub_project_ids,
            residual_tree_hashes,
        })
    }

    /// 列出全部资产（含 tombstone / deleted_at 非空）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Snapshot 必须导出 tombstone 资产身份，不能只读 live 行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT 全表 assets，按 id 升序。
    pub async fn list_all_assets_including_deleted(&self) -> Result<Vec<LogicalAsset>, AppError> {
        let rows = sqlx::query(
            "SELECT id, scope_id, kind, origin_namespace, logical_key, display_name, policy,
                    current_revision_id, deleted_at, created_at, updated_at
             FROM agent_hub_assets
             ORDER BY id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_asset).collect()
    }

    /// 按 id 列表读取资产（含 deleted）。
    ///
    /// Business Logic: 显式 asset selection 需要精确集合且保留 tombstone。
    /// Code Logic: 逐 id get_asset 并过滤 None；保持调用方顺序去重后按 id 排序。
    pub async fn list_assets_by_ids_including_deleted(
        &self,
        ids: &[String],
    ) -> Result<Vec<LogicalAsset>, AppError> {
        let mut out = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for id in ids {
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(a) = self.get_asset(id).await? {
                out.push(a);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// 按 scope id 列表读取资产（含 deleted）。
    ///
    /// Business Logic: user/project scope selection 闭包。
    /// Code Logic: IN 列表逐条 OR 查询后按 id 排序。
    pub async fn list_assets_in_scopes_including_deleted(
        &self,
        scope_ids: &[String],
    ) -> Result<Vec<LogicalAsset>, AppError> {
        if scope_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for scope_id in scope_ids {
            let rows = sqlx::query(
                "SELECT id, scope_id, kind, origin_namespace, logical_key, display_name, policy,
                        current_revision_id, deleted_at, created_at, updated_at
                 FROM agent_hub_assets
                 WHERE scope_id = ?
                 ORDER BY id ASC",
            )
            .bind(scope_id)
            .fetch_all(&self.pool)
            .await?;
            for row in rows {
                out.push(row_to_asset(&row)?);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out.dedup_by(|a, b| a.id == b.id);
        Ok(out)
    }

    /// 读取资产关联的 lineage id 列表。
    ///
    /// Business Logic: snapshot lineages 必须覆盖 selected assets 的全部 lineage。
    /// Code Logic: SELECT asset_id, lineage_id WHERE asset_id = ?。
    pub async fn list_lineages_for_assets(
        &self,
        asset_ids: &[String],
    ) -> Result<Vec<(String, String)>, AppError> {
        let mut out = Vec::new();
        for asset_id in asset_ids {
            let rows = sqlx::query(
                "SELECT asset_id, lineage_id FROM agent_hub_asset_lineages
                 WHERE asset_id = ? ORDER BY lineage_id ASC",
            )
            .bind(asset_id)
            .fetch_all(&self.pool)
            .await?;
            for row in rows {
                let a: String = row.try_get("asset_id")?;
                let l: String = row.try_get("lineage_id")?;
                out.push((a, l));
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// 从 heads 出发收集 revision ancestry（含 heads 自身）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Snapshot 必须闭合到 retained merge bases；缺 parent 则整包失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     队列 BFS parents；get_revision；按 id 排序返回。
    pub async fn collect_revision_ancestry(
        &self,
        head_ids: &[String],
    ) -> Result<Vec<Revision>, AppError> {
        use std::collections::{BTreeMap, BTreeSet, VecDeque};
        let mut visited: BTreeSet<String> = BTreeSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        for h in head_ids {
            if !h.is_empty() {
                queue.push_back(h.clone());
            }
        }
        let mut by_id: BTreeMap<String, Revision> = BTreeMap::new();
        while let Some(id) = queue.pop_front() {
            if !visited.insert(id.clone()) {
                continue;
            }
            let rev = self
                .get_revision(&RevisionId(id.clone()))
                .await?
                .ok_or_else(|| {
                    AppError::not_found(format!("agent_hub_snapshot_revision_missing:{id}"))
                })?;
            for p in &rev.parents {
                queue.push_back(p.as_str().to_string());
            }
            by_id.insert(id, rev);
        }
        Ok(by_id.into_values().collect())
    }

    /// 列出资产的 target variants。
    ///
    /// Business Logic: targetOnly/adapted 变体必须进入 snapshot。
    /// Code Logic: SELECT asset_id,target,revision_id FROM agent_hub_variants。
    pub async fn list_variants_for_assets(
        &self,
        asset_ids: &[String],
    ) -> Result<Vec<AgentHubVariantRow>, AppError> {
        let mut out = Vec::new();
        for asset_id in asset_ids {
            let rows = sqlx::query(
                "SELECT id, asset_id, target, revision_id, extension_payload_hash, created_at
                 FROM agent_hub_variants
                 WHERE asset_id = ?
                 ORDER BY target ASC, revision_id ASC",
            )
            .bind(asset_id)
            .fetch_all(&self.pool)
            .await?;
            for row in rows {
                out.push(row_to_variant(&row)?);
            }
        }
        out.sort_by(|a, b| {
            a.asset_id
                .cmp(&b.asset_id)
                .then(a.target.as_str().cmp(b.target.as_str()))
                .then(a.revision_id.cmp(&b.revision_id))
        });
        Ok(out)
    }

    /// 写入/覆盖一条 target variant（测试与 future importer）。
    ///
    /// Business Logic: variant head 需可持久化。
    /// Code Logic: INSERT OR REPLACE by UNIQUE(asset,target,revision)。
    pub async fn upsert_variant(
        &self,
        asset_id: &str,
        target: AgentTarget,
        revision_id: &str,
    ) -> Result<AgentHubVariantRow, AppError> {
        with_shared_write_lease(&self.gate, async {
            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            // 先删同 asset+target 旧行，再插当前 head（snapshot 只关心当前变体 head）
            sqlx::query("DELETE FROM agent_hub_variants WHERE asset_id = ? AND target = ?")
                .bind(asset_id)
                .bind(target.as_str())
                .execute(&self.pool)
                .await?;
            sqlx::query(
                "INSERT INTO agent_hub_variants
                 (id, asset_id, target, revision_id, extension_payload_hash, created_at)
                 VALUES (?, ?, ?, ?, NULL, ?)",
            )
            .bind(&id)
            .bind(asset_id)
            .bind(target.as_str())
            .bind(revision_id)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(AgentHubVariantRow {
                id,
                asset_id: asset_id.to_string(),
                target,
                revision_id: revision_id.to_string(),
                extension_payload_hash: None,
                created_at: now,
            })
        })
        .await
    }

    /// 列出与 hub project ids 相关的便携别名（不含本机绝对路径）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Snapshot aliases 只导出 portable identity / external id，禁止 absolute checkout path。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 project_mappings；输出 kind=`hubProjectId`，external=local fingerprint/workbench，
    ///     local=hub_project_id；永不返回 local_absolute_path。
    pub async fn list_portable_project_aliases(
        &self,
        hub_project_ids: &[String],
    ) -> Result<Vec<AgentHubPortableAliasRow>, AppError> {
        let mut out = Vec::new();
        for hub_id in hub_project_ids {
            let Some(row) = self.get_project_mapping_by_hub_project_id(hub_id).await? else {
                // 仍导出 hub 自身 identity alias
                out.push(AgentHubPortableAliasRow {
                    kind: "hubProjectId".into(),
                    external_id: hub_id.clone(),
                    local_id: hub_id.clone(),
                });
                continue;
            };
            out.push(AgentHubPortableAliasRow {
                kind: "hubProjectId".into(),
                external_id: hub_id.clone(),
                local_id: hub_id.clone(),
            });
            if let Some(fp) = row.git_remote_fingerprint {
                if !fp.is_empty() {
                    out.push(AgentHubPortableAliasRow {
                        kind: "gitRemoteFingerprint".into(),
                        external_id: fp,
                        local_id: hub_id.clone(),
                    });
                }
            }
            if let Some(local) = row.local_workbench_project_id {
                if !local.is_empty() {
                    out.push(AgentHubPortableAliasRow {
                        kind: "workbenchProjectId".into(),
                        external_id: local,
                        local_id: hub_id.clone(),
                    });
                }
            }
            // 故意不导出 local_absolute_path
            let _ = row.local_absolute_path;
        }
        out.sort_by(|a, b| {
            a.kind
                .cmp(&b.kind)
                .then(a.external_id.cmp(&b.external_id))
                .then(a.local_id.cmp(&b.local_id))
        });
        out.dedup();
        Ok(out)
    }

    /// 按资产过滤未解决 conflicts。
    ///
    /// Business Logic: 仅导出 selection 内资产的 freeze 状态。
    /// Code Logic: list_unresolved_conflicts 后按 asset_id 过滤。
    pub async fn list_unresolved_conflicts_for_assets(
        &self,
        asset_ids: &[String],
    ) -> Result<Vec<AgentHubConflict>, AppError> {
        let set: std::collections::BTreeSet<&str> = asset_ids.iter().map(String::as_str).collect();
        let all = self.list_unresolved_conflicts().await?;
        let mut out: Vec<_> = all
            .into_iter()
            .filter(|c| set.contains(c.asset_id.as_str()))
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// 按 hub_project_id 查 project scope（若存在）。
    ///
    /// Business Logic: project selection 需要 scope id。
    /// Code Logic: 复用 get_project_scope_by_hub_project_id。
    pub async fn resolve_project_scope_id(
        &self,
        hub_project_id: &str,
    ) -> Result<Option<String>, AppError> {
        Ok(self
            .get_project_scope_by_hub_project_id(hub_project_id)
            .await?
            .map(|s| s.id))
    }

    /// 查找 user scope id（kind=user，取字典序最小以稳定）。
    ///
    /// Business Logic: user-scope selection。
    /// Code Logic: list_scopes 过滤 User。
    pub async fn resolve_user_scope_id(&self) -> Result<Option<String>, AppError> {
        let scopes = self.list_scopes().await?;
        Ok(scopes
            .into_iter()
            .filter(|s| s.kind == ScopeKind::User)
            .map(|s| s.id)
            .min())
    }

    /// 注入 import 故障点（仅 test/debug）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     quality_faults 需模拟 CAS 后 / head 更新前崩溃，证明无脏 head。
    ///     故障槽必须 per-repo，否则并行 `cargo test agent_hub` 会互相偷走注入。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入本实例 `import_fault` 一次消费的 fail point。
    #[cfg(any(test, debug_assertions))]
    pub fn inject_import_fault(&self, fault: AgentHubImportFault) {
        self.import_fault.store(fault.as_u8(), Ordering::SeqCst);
    }

    /// 读取并清除 import 故障点。
    ///
    /// Business Logic: 测试断言后复位；不得影响其它 repo 实例。
    /// Code Logic: 对本实例 `import_fault` swap 0。
    #[cfg(any(test, debug_assertions))]
    pub fn take_import_fault(&self) -> AgentHubImportFault {
        AgentHubImportFault::from_u8(self.import_fault.swap(0, Ordering::SeqCst))
    }

    /// 在单写事务中提交 snapshot import 身份与 DAG 收敛（不含 CAS 写）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Phase B 必须原子 upsert aliases/lineages/assets/revisions/parents/variants/conflicts，
    ///     并按 MCA 更新 head 或开 conflict；中途失败不得激活非法 head。
    ///
    /// Code Logic（这个函数做什么）:
    ///     with_shared_write_lease → begin TX → apply bundle → optional fail inject → commit。
    pub async fn commit_import_bundle(
        &self,
        bundle: ImportBundle,
    ) -> Result<ImportBundleResult, AppError> {
        with_shared_write_lease(&self.gate, async {
            let mut tx = self.pool.begin().await?;
            let result = apply_import_bundle_on_tx(&mut tx, &bundle).await?;

            #[cfg(any(test, debug_assertions))]
            {
                // per-repo 槽：并行 import 测试互不抢 fault。
                let fault = self.import_fault.swap(0, Ordering::SeqCst);
                if fault == AgentHubImportFault::BeforeHeadUpdate.as_u8() {
                    return Err(AppError::generic(
                        "agent_hub_import_injected_before_head_update".to_string(),
                    ));
                }
                if fault == AgentHubImportFault::BeforeTxCommit.as_u8() {
                    return Err(AppError::generic(
                        "agent_hub_import_injected_before_tx_commit".to_string(),
                    ));
                }
            }

            // head 收敛与 conflict 已在 apply 中完成；此处仅 commit。
            tx.commit().await?;
            Ok(result)
        })
        .await
    }

    /// 在调用方已开启的写事务上应用 import bundle（供 LAN ledger 同 TX 提交）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     LAN commit 要求 claim prepared → import → outcome committed 原子完成；
    ///     若 import 与 ledger 分事务，崩溃重放会重复副作用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `apply_import_bundle_on_tx`；不 begin/commit；可选 fault inject 与独立 commit 对齐。
    pub async fn apply_import_bundle_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        bundle: &ImportBundle,
    ) -> Result<ImportBundleResult, AppError> {
        let result = apply_import_bundle_on_tx(tx, bundle).await?;
        #[cfg(any(test, debug_assertions))]
        {
            let fault = self.import_fault.swap(0, Ordering::SeqCst);
            if fault == AgentHubImportFault::BeforeHeadUpdate.as_u8() {
                return Err(AppError::generic(
                    "agent_hub_import_injected_before_head_update".to_string(),
                ));
            }
            if fault == AgentHubImportFault::BeforeTxCommit.as_u8() {
                return Err(AppError::generic(
                    "agent_hub_import_injected_before_tx_commit".to_string(),
                ));
            }
        }
        Ok(result)
    }

    /// 返回共享 maintenance gate（供 replication 与 import 同 lease 原子提交）。
    ///
    /// Business Logic: LAN commit 需与 AgentHubRepo 写路径互斥。
    /// Code Logic: clone Arc gate。
    pub fn gate(&self) -> Arc<DatabaseMaintenanceGate> {
        Arc::clone(&self.gate)
    }

    /// 插入 lineage 映射（幂等）。
    ///
    /// Business Logic: 远端 asset id 作为 lineage alias 挂到本地 logical asset。
    /// Code Logic: INSERT OR IGNORE asset_lineages。
    pub async fn upsert_asset_lineage(
        &self,
        asset_id: &str,
        lineage_id: &str,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT OR IGNORE INTO agent_hub_asset_lineages (asset_id, lineage_id, created_at)
                 VALUES (?, ?, ?)",
            )
            .bind(asset_id)
            .bind(lineage_id)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    /// 按 unique key 查找资产（含 deleted）。
    ///
    /// Business Logic: import 去重 logical identity。
    /// Code Logic: 委托 get_asset_by_unique_key。
    pub async fn find_asset_by_unique_key_including_deleted(
        &self,
        scope_id: &str,
        kind: AssetKind,
        origin_namespace: &str,
        logical_key: &str,
    ) -> Result<Option<LogicalAsset>, AppError> {
        self.get_asset_by_unique_key(scope_id, kind, origin_namespace, logical_key)
            .await
    }

    /// 在 import TX 内写入 durable LAN projection intent。
    ///
    /// Business Logic: commit 后崩溃窗口不得丢投影；但仅导入 canonical、尚未选择本机 target
    ///     的资产不得伪报 queued，必须停留在待本机配置状态。
    /// Code Logic: 仅当 asset 已有本机 target binding 时 INSERT OR IGNORE queued intent。
    pub async fn insert_lan_projection_intents_on_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        transfer_id: &str,
        asset_ids: &[String],
    ) -> Result<u64, AppError> {
        let now = chrono::Utc::now().to_rfc3339();
        let mut n = 0u64;
        for asset_id in asset_ids {
            let res = sqlx::query(
                "INSERT OR IGNORE INTO agent_hub_lan_projection_intents
                 (transfer_id, asset_id, status, created_at, updated_at)
                 SELECT ?, ?, 'queued', ?, ?
                 WHERE EXISTS (
                     SELECT 1 FROM agent_hub_target_bindings b WHERE b.asset_id = ?
                 )",
            )
            .bind(transfer_id)
            .bind(asset_id)
            .bind(&now)
            .bind(&now)
            .bind(asset_id)
            .execute(&mut **tx)
            .await?;
            n += res.rows_affected();
        }
        Ok(n)
    }

    /// 列出某 transfer 仍为 queued 的 projection intent。
    ///
    /// Business Logic: committed 回放必须补偿未完成 intent。
    /// Code Logic: SELECT asset_id WHERE transfer_id AND status=queued。
    pub async fn list_queued_lan_projection_intents(
        &self,
        transfer_id: &str,
    ) -> Result<Vec<String>, AppError> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT asset_id FROM agent_hub_lan_projection_intents
             WHERE transfer_id = ? AND status = 'queued'
             ORDER BY asset_id ASC",
        )
        .bind(transfer_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 全仓 claim 一批 queued LAN projection intent。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     commit 后 spawn 失败或进程崩溃时，owner 启动/周期 worker 必须跨 transfer 排水，
    ///     不能只靠 commit endpoint 按 transfer 查询。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT transfer_id, asset_id WHERE status=queued ORDER BY updated_at LIMIT。
    pub async fn claim_queued_lan_projection_intents(
        &self,
        limit: i64,
    ) -> Result<Vec<(String, String)>, AppError> {
        let rows = sqlx::query(
            "SELECT transfer_id, asset_id FROM agent_hub_lan_projection_intents
             WHERE status = 'queued'
             ORDER BY updated_at ASC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let transfer_id: String = r.try_get("transfer_id")?;
            let asset_id: String = r.try_get("asset_id")?;
            out.push((transfer_id, asset_id));
        }
        Ok(out)
    }

    /// 标记 projection intent 已入队/完成。
    ///
    /// Business Logic: owner 排水后推进 status，避免重复风暴。
    /// Code Logic: UPDATE status。
    pub async fn mark_lan_projection_intent_status(
        &self,
        transfer_id: &str,
        asset_id: &str,
        status: &str,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "UPDATE agent_hub_lan_projection_intents
                 SET status = ?, updated_at = ?
                 WHERE transfer_id = ? AND asset_id = ?",
            )
            .bind(status)
            .bind(&now)
            .bind(transfer_id)
            .bind(asset_id)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    /// 列出已 committed 且 staging 尚未标记清理完成的 transfer。
    ///
    /// Business Logic: 成功 push 不得永久保留 incoming 明文/重复 CAS；
    ///     超过 256 条后必须可推进，禁止永远扫同一批历史行。
    /// Code Logic: SELECT transfer_id WHERE status=committed AND staging_cleaned_at IS NULL。
    pub async fn list_committed_transfer_ids_for_cleanup(
        &self,
        limit: i64,
    ) -> Result<Vec<String>, AppError> {
        // 表在 replication ledger；经 pool 直接查（同 DB）
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT transfer_id FROM agent_hub_push_requests
             WHERE status = 'committed'
               AND (staging_cleaned_at IS NULL OR staging_cleaned_at = '')
             ORDER BY updated_at ASC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 标记 committed transfer 的 staging 清理已完成。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GC 成功删除目录或目录本就不存在后，必须原子推进 cleanup 状态，
    ///     否则第 257 条以后的残留永远饿死。
    ///
    /// Code Logic（这个函数做什么）:
    ///     UPDATE agent_hub_push_requests SET staging_cleaned_at=now WHERE transfer_id。
    pub async fn mark_committed_transfer_staging_cleaned(
        &self,
        transfer_id: &str,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "UPDATE agent_hub_push_requests
                 SET staging_cleaned_at = ?, updated_at = ?
                 WHERE transfer_id = ?",
            )
            .bind(&now)
            .bind(&now)
            .bind(transfer_id)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    /// 读取本机 device-lane Git 导出状态。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     export runtime 需要 last_pushed / pending / attempt 决定是否空提交与退避。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 device_id 查询 `agent_hub_git_export_state`。
    pub async fn get_git_export_state(
        &self,
        device_id: &str,
    ) -> Result<Option<AgentHubGitExportState>, AppError> {
        let row = sqlx::query(
            "SELECT device_id, last_exported_snapshot_hash, last_pushed_snapshot_hash,
                    pending_snapshot_hash, pending_commit_oid, pending_phase, attempt_count, next_attempt_at, last_error
             FROM agent_hub_git_export_state WHERE device_id = ?",
        )
        .bind(device_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| AgentHubGitExportState {
            device_id: r.get("device_id"),
            last_exported_snapshot_hash: r.get("last_exported_snapshot_hash"),
            last_pushed_snapshot_hash: r.get("last_pushed_snapshot_hash"),
            pending_snapshot_hash: r.get("pending_snapshot_hash"),
            pending_commit_oid: r.get("pending_commit_oid"),
            pending_phase: r
                .get::<Option<String>, _>("pending_phase")
                .as_deref()
                .and_then(GitExportPendingPhase::from_str_value),
            attempt_count: r.get::<i64, _>("attempt_count") as u32,
            next_attempt_at: r.get("next_attempt_at"),
            last_error: r.get("last_error"),
        }))
    }

    /// 写入/更新本机 device-lane Git 导出状态。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     push 成功清 pending；失败保留 pending_hash + attempt + next_attempt。
    ///
    /// Code Logic（这个函数做什么）:
    ///     shared write lease + INSERT OR REPLACE。
    pub async fn upsert_git_export_state(
        &self,
        state: &AgentHubGitExportState,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = chrono::Utc::now().to_rfc3339();
            let phase_str = state.pending_phase.as_ref().map(|p| p.as_str());
            sqlx::query(
                "INSERT INTO agent_hub_git_export_state (
                    device_id, last_exported_snapshot_hash, last_pushed_snapshot_hash,
                    pending_snapshot_hash, pending_commit_oid, pending_phase, attempt_count, next_attempt_at, last_error,
                    created_at, updated_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(device_id) DO UPDATE SET
                    last_exported_snapshot_hash=excluded.last_exported_snapshot_hash,
                    last_pushed_snapshot_hash=excluded.last_pushed_snapshot_hash,
                    pending_snapshot_hash=excluded.pending_snapshot_hash,
                    pending_commit_oid=excluded.pending_commit_oid,
                    pending_phase=excluded.pending_phase,
                    attempt_count=excluded.attempt_count,
                    next_attempt_at=excluded.next_attempt_at,
                    last_error=excluded.last_error,
                    updated_at=excluded.updated_at",
            )
            .bind(&state.device_id)
            .bind(&state.last_exported_snapshot_hash)
            .bind(&state.last_pushed_snapshot_hash)
            .bind(&state.pending_snapshot_hash)
            .bind(&state.pending_commit_oid)
            .bind(phase_str)
            .bind(state.attempt_count as i64)
            .bind(&state.next_attempt_at)
            .bind(&state.last_error)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }
}

/// Git device-lane 导出 pending 阶段。
///
/// Business Logic（为什么需要这个枚举）:
///     R3b Finding #7：pending intent 必须在 lane 写入前先持久化，否则崩溃窗口
///     会留下 lane 已写但无 pending row 的脏状态。恢复时根据 phase 区分
///     "未到 lane 写入就崩溃" 与 "lane 已写待 push"。
///
/// Code Logic（这个枚举做什么）:
///     三态：`PreLaneWrite` → `LaneWritten` → `Confirmed`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitExportPendingPhase {
    /// 已写入 pending intent，但 lane 目录尚未被 replace；崩溃后未污染 worktree。
    PreLaneWrite,
    /// lane 目录已写入，但 push 尚未成功（commit 也可能未完成）。
    LaneWritten,
    /// push 成功（终态）。
    Confirmed,
}

impl GitExportPendingPhase {
    /// 稳定字符串序列化（小写 snake_case）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreLaneWrite => "pre_lane_write",
            Self::LaneWritten => "lane_written",
            Self::Confirmed => "confirmed",
        }
    }

    /// 反序列化：未知值保守视为 `None`，避免老库升级时误判阶段。
    pub fn from_str_value(s: &str) -> Option<Self> {
        match s {
            "pre_lane_write" => Some(Self::PreLaneWrite),
            "lane_written" => Some(Self::LaneWritten),
            "confirmed" => Some(Self::Confirmed),
            _ => None,
        }
    }
}

/// 本机 Git device-lane 导出持久状态。
///
/// Business Logic（为什么需要这个结构体）:
///     崩溃后需恢复 pending export；snapshotHash 不变时禁止空 commit。
///
/// Code Logic（这个结构体做什么）:
///     镜像 `agent_hub_git_export_state` 业务列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHubGitExportState {
    /// 本机 device_id
    pub device_id: String,
    /// 最近一次成功构建并写出 lane 的 snapshotHash
    pub last_exported_snapshot_hash: Option<String>,
    /// 最近一次成功 push 的 snapshotHash
    pub last_pushed_snapshot_hash: Option<String>,
    /// 待重试的 snapshotHash
    pub pending_snapshot_hash: Option<String>,
    /// 已本地 commit 但尚未确认远端的 commit OID（防永久 pending）
    pub pending_commit_oid: Option<String>,
    /// pending 阶段（Codex R6）：区分 intent-only / lane-written / confirmed
    pub pending_phase: Option<GitExportPendingPhase>,
    /// 连续失败次数
    pub attempt_count: u32,
    /// 下次尝试 RFC3339
    pub next_attempt_at: Option<String>,
    /// 最近错误摘要（脱敏）
    pub last_error: Option<String>,
}

/// Import 故障注入点。
///
/// Business Logic: 验证 Phase B 崩溃边界。
/// Code Logic: u8 编码的一次消费标志。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentHubImportFault {
    /// 无故障
    #[default]
    None,
    /// head 更新前失败（在 apply 末尾、commit 前）
    BeforeHeadUpdate,
    /// TX commit 前失败
    BeforeTxCommit,
}

impl AgentHubImportFault {
    fn as_u8(self) -> u8 {
        match self {
            Self::None => 0,
            Self::BeforeHeadUpdate => 1,
            Self::BeforeTxCommit => 2,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::BeforeHeadUpdate,
            2 => Self::BeforeTxCommit,
            _ => Self::None,
        }
    }
}

/// 单事务 import 输入（Phase B）。
///
/// Business Logic: importer 已完成 CAS 验证后，把可落库身份打包进一笔 TX。
/// Code Logic: scopes/assets/lineages/revisions/variants/conflicts/head_decisions/mappings +
///     Gate D plugin component/residual 边（与 package revision 同 TX 恢复）。
#[derive(Debug, Clone)]
pub struct ImportBundle {
    /// 需要确保存在的 scopes
    pub scopes: Vec<NewScopeNode>,
    /// 资产 upsert（id 保留 snapshot id；unique key 冲突时挂 lineage）
    pub assets: Vec<ImportAssetRow>,
    /// 额外 lineage 对 (asset_id, lineage_id)
    pub lineages: Vec<(String, String)>,
    /// 按拓扑序 revision（id 保留）
    pub revisions: Vec<ImportRevisionRow>,
    /// variants
    pub variants: Vec<ImportVariantRow>,
    /// 直接落库 conflicts（含 snapshot 携带 + 新开）
    pub conflicts: Vec<ImportConflictRow>,
    /// 每个 local asset 的 head 决策
    pub head_decisions: Vec<ImportHeadDecision>,
    /// 确认后的 project mapping（opted_in 不自动 true）
    pub project_mappings: Vec<UpsertAgentHubProjectMapping>,
    /// Gate D：plugin package revision → component 边（import 校验后写入）
    pub plugin_components: Vec<ImportPluginComponentEdge>,
    /// Gate D：plugin package revision → residual 边
    pub plugin_residuals: Vec<ImportPluginResidualEdge>,
}

/// import 时恢复的 plugin component 边。
///
/// Business Logic: ownership/export 只查边表；import 必须与 package revision 同 TX 重建。
/// Code Logic: 字段对齐 `agent_hub_plugin_components`；component_asset_id 已 remap 到本地。
#[derive(Debug, Clone)]
pub struct ImportPluginComponentEdge {
    /// package revision id（snapshot 保留）
    pub package_revision_id: String,
    /// component kind
    pub component_kind: AssetKind,
    /// 本地 component asset id
    pub component_asset_id: String,
    /// component revision id（snapshot 保留）
    pub component_revision_id: String,
    /// ownership 标签
    pub ownership: ComponentOwnership,
}

/// import 时恢复的 plugin residual 边。
///
/// Business Logic: residual tree 必须继续参与 re-export 闭包。
/// Code Logic: 字段对齐 `agent_hub_plugin_residuals`。
#[derive(Debug, Clone)]
pub struct ImportPluginResidualEdge {
    /// package revision id
    pub package_revision_id: String,
    /// residual target
    pub target: AgentTarget,
    /// residual 类别
    pub residual_kind: ResidualKind,
    /// residual tree manifest hash
    pub tree_manifest_hash: String,
}

/// import 资产行。
#[derive(Debug, Clone)]
pub struct ImportAssetRow {
    pub id: String,
    pub scope_id: String,
    pub kind: AssetKind,
    pub origin_namespace: String,
    pub logical_key: String,
    pub display_name: String,
    pub policy: AssetPolicy,
    pub deleted_at: Option<String>,
}

/// import revision 行（generation 由 parents 重算或采用 snapshot 值）。
#[derive(Debug, Clone)]
pub struct ImportRevisionRow {
    pub id: String,
    pub asset_lineage_id: String,
    pub parents: Vec<String>,
    pub generation: u64,
    pub operation: RevisionOperation,
    pub origin_kind: RevisionOriginKind,
    pub origin_target: Option<AgentTarget>,
    pub origin_replica_id: String,
    pub payload_hash: Option<String>,
    pub tree_manifest_hash: Option<String>,
    pub created_at: String,
}

/// import variant 行。
#[derive(Debug, Clone)]
pub struct ImportVariantRow {
    pub asset_id: String,
    pub target: AgentTarget,
    pub revision_id: String,
}

/// import conflict 行。
#[derive(Debug, Clone)]
pub struct ImportConflictRow {
    pub id: String,
    pub asset_id: String,
    pub target: Option<AgentTarget>,
    pub base_revision_id: Option<String>,
    pub hub_revision_id: Option<String>,
    pub external_revision_id: Option<String>,
    pub detail_json: String,
    pub created_at: String,
}

/// head 收敛决策。
///
/// Business Logic: 禁止 LWW；要么推进到 merge head，要么保留双 head 并 conflict。
#[derive(Debug, Clone)]
pub struct ImportHeadDecision {
    /// 本地 logical asset id
    pub asset_id: String,
    /// 规划时观测到的 local head（含 NULL）；提交阶段 CAS 必须匹配，防并发本地写被静默覆盖
    pub expected_head: Option<String>,
    /// 新 current head；None 表示不改 head（仅开 conflict）
    pub new_head: Option<String>,
    /// 是否 tombstone
    pub deleted_at: Option<String>,
}

/// import TX 结果。
#[derive(Debug, Clone, Default)]
pub struct ImportBundleResult {
    /// 新插入 revision 数
    pub inserted_revisions: u64,
    /// 已存在 dedupe 的 revision 数
    pub deduped_revisions: u64,
    /// 推进 head 的资产数
    pub heads_advanced: u64,
    /// 新开 conflict 数
    pub conflicts_opened: u64,
    /// 导入资产 id（local）
    pub imported_asset_ids: Vec<String>,
}

/// 在已开启的写事务内应用 import bundle。
///
/// Business Logic: Phase B 核心；失败则整笔回滚。
/// Code Logic: scopes → assets/lineages → revisions/parents → plugin edges →
///     variants → conflicts → heads。
pub(crate) async fn apply_import_bundle_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    bundle: &ImportBundle,
) -> Result<ImportBundleResult, AppError> {
    let mut result = ImportBundleResult::default();
    let now = chrono::Utc::now().to_rfc3339();

    // 1) scopes
    for scope in &bundle.scopes {
        let id = scope
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_scopes WHERE id = ?")
            .bind(&id)
            .fetch_one(&mut **tx)
            .await?;
        if exists == 0 {
            // 也按 hub_project_id 查 project scope 是否已有
            if let Some(hub) = scope.hub_project_id.as_deref() {
                let existing: Option<String> = sqlx::query_scalar(
                    "SELECT id FROM agent_hub_scopes WHERE kind = 'project' AND hub_project_id = ? LIMIT 1",
                )
                .bind(hub)
                .fetch_optional(&mut **tx)
                .await?;
                if existing.is_some() {
                    continue;
                }
            }
            sqlx::query(
                "INSERT INTO agent_hub_scopes (id, kind, hub_project_id, relative_path, created_at)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(scope.kind.as_str())
            .bind(&scope.hub_project_id)
            .bind(&scope.relative_path)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
        }
    }

    // 2) project mappings（确认后；opted_in 由调用方决定，默认 false）
    //    按 hub_project_id 或 local_workbench_project_id 去重，避免 UNIQUE(local) 冲突。
    for m in &bundle.project_mappings {
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT id FROM agent_hub_project_mappings WHERE hub_project_id = ? LIMIT 1",
        )
        .bind(&m.hub_project_id)
        .fetch_optional(&mut **tx)
        .await?;
        let existing = match existing {
            Some(id) => Some(id),
            None => {
                if let Some(local) = m.local_workbench_project_id.as_deref() {
                    sqlx::query_scalar(
                        "SELECT id FROM agent_hub_project_mappings
                         WHERE local_workbench_project_id = ? LIMIT 1",
                    )
                    .bind(local)
                    .fetch_optional(&mut **tx)
                    .await?
                } else {
                    None
                }
            }
        };
        if let Some(id) = existing {
            sqlx::query(
                "UPDATE agent_hub_project_mappings
                 SET hub_project_id = ?,
                     local_workbench_project_id = COALESCE(?, local_workbench_project_id),
                     git_remote_fingerprint = COALESCE(?, git_remote_fingerprint),
                     opted_in = CASE WHEN ? = 1 THEN 1 ELSE opted_in END,
                     updated_at = ?
                 WHERE id = ?",
            )
            .bind(&m.hub_project_id)
            .bind(&m.local_workbench_project_id)
            .bind(&m.git_remote_fingerprint)
            .bind(if m.opted_in { 1 } else { 0 })
            .bind(&now)
            .bind(&id)
            .execute(&mut **tx)
            .await?;
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO agent_hub_project_mappings
                 (id, hub_project_id, local_workbench_project_id, git_remote_fingerprint,
                  local_absolute_path, opted_in, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&m.hub_project_id)
            .bind(&m.local_workbench_project_id)
            .bind(&m.git_remote_fingerprint)
            .bind(&m.local_absolute_path)
            .bind(if m.opted_in { 1 } else { 0 })
            .bind(&now)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
        }
    }

    // 3) assets + self lineage + aliases
    let mut imported_assets = std::collections::BTreeSet::new();
    for a in &bundle.assets {
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM agent_hub_assets
             WHERE scope_id = ? AND kind = ? AND origin_namespace = ? AND logical_key = ?
             LIMIT 1",
        )
        .bind(&a.scope_id)
        .bind(a.kind.as_str())
        .bind(&a.origin_namespace)
        .bind(&a.logical_key)
        .fetch_optional(&mut **tx)
        .await?;

        let local_asset_id = if let Some((id,)) = existing {
            // 挂远端 id 为 lineage alias
            sqlx::query(
                "INSERT OR IGNORE INTO agent_hub_asset_lineages (asset_id, lineage_id, created_at)
                 VALUES (?, ?, ?)",
            )
            .bind(&id)
            .bind(&a.id)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
            // self lineage
            sqlx::query(
                "INSERT OR IGNORE INTO agent_hub_asset_lineages (asset_id, lineage_id, created_at)
                 VALUES (?, ?, ?)",
            )
            .bind(&id)
            .bind(&id)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
            id
        } else {
            // 若 id 已存在则只补 lineage
            let by_id: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_assets WHERE id = ?")
                    .bind(&a.id)
                    .fetch_one(&mut **tx)
                    .await?;
            if by_id == 0 {
                sqlx::query(
                    "INSERT INTO agent_hub_assets
                     (id, scope_id, kind, origin_namespace, logical_key, display_name, policy,
                      current_revision_id, deleted_at, created_at, updated_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?)",
                )
                .bind(&a.id)
                .bind(&a.scope_id)
                .bind(a.kind.as_str())
                .bind(&a.origin_namespace)
                .bind(&a.logical_key)
                .bind(&a.display_name)
                .bind(a.policy.as_str())
                .bind(&a.deleted_at)
                .bind(&now)
                .bind(&now)
                .execute(&mut **tx)
                .await?;
            }
            sqlx::query(
                "INSERT OR IGNORE INTO agent_hub_asset_lineages (asset_id, lineage_id, created_at)
                 VALUES (?, ?, ?)",
            )
            .bind(&a.id)
            .bind(&a.id)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
            a.id.clone()
        };
        imported_assets.insert(local_asset_id);
    }

    for (asset_id, lineage_id) in &bundle.lineages {
        sqlx::query(
            "INSERT OR IGNORE INTO agent_hub_asset_lineages (asset_id, lineage_id, created_at)
             VALUES (?, ?, ?)",
        )
        .bind(asset_id)
        .bind(lineage_id)
        .bind(&now)
        .execute(&mut **tx)
        .await?;
    }

    // 4) revisions + parents（ID 命中时校验全部不可变字段；不一致 → conflict）
    for rev in &bundle.revisions {
        if rev.operation == RevisionOperation::Delete
            && (rev.payload_hash.is_some() || rev.tree_manifest_hash.is_some())
        {
            return Err(AppError::validation(
                "agent_hub_delete_revision_rejects_payload_hash".to_string(),
            ));
        }
        // 校验 parents 存在（或在本批更早插入）
        for parent in &rev.parents {
            let exists: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_revisions WHERE id = ?")
                    .bind(parent)
                    .fetch_one(&mut **tx)
                    .await?;
            if exists == 0 {
                return Err(AppError::validation(format!(
                    "agent_hub_import_parent_missing:{parent}"
                )));
            }
        }
        let existing_row = sqlx::query(
            "SELECT id, asset_lineage_id, generation, operation, origin_kind, origin_target,
                    origin_replica_id, payload_hash, tree_manifest_hash, created_at
             FROM agent_hub_revisions WHERE id = ?",
        )
        .bind(&rev.id)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(row) = existing_row {
            // 命中已有 ID：逐项比较不可变字段 + 有序 parents
            let same = revision_immutable_fields_match_on_tx(tx, &row, rev).await?;
            if !same {
                return Err(AppError::conflict(format!(
                    "agent_hub_import_revision_id_content_mismatch:{}",
                    rev.id
                )));
            }
            result.deduped_revisions += 1;
            continue;
        }
        // generation: 使用 snapshot 提供值，但若 parents 在本库，可取 max+1 校正
        let mut max_parent: Option<u64> = None;
        for parent in &rev.parents {
            let g: Option<i64> =
                sqlx::query_scalar("SELECT generation FROM agent_hub_revisions WHERE id = ?")
                    .bind(parent)
                    .fetch_optional(&mut **tx)
                    .await?;
            if let Some(g) = g {
                let g = g as u64;
                max_parent = Some(max_parent.map_or(g, |m| m.max(g)));
            }
        }
        let generation = max_parent
            .map(|m| m.saturating_add(1))
            .unwrap_or(rev.generation);

        sqlx::query(
            "INSERT INTO agent_hub_revisions
             (id, asset_lineage_id, generation, operation, origin_kind, origin_target,
              origin_replica_id, payload_hash, tree_manifest_hash, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&rev.id)
        .bind(&rev.asset_lineage_id)
        .bind(generation as i64)
        .bind(rev.operation.as_str())
        .bind(rev.origin_kind.as_str())
        .bind(rev.origin_target.map(|t| t.as_str()))
        .bind(&rev.origin_replica_id)
        .bind(&rev.payload_hash)
        .bind(&rev.tree_manifest_hash)
        .bind(&rev.created_at)
        .execute(&mut **tx)
        .await?;

        for (pos, parent) in rev.parents.iter().enumerate() {
            sqlx::query(
                "INSERT INTO agent_hub_revision_parents
                 (revision_id, parent_revision_id, parent_order)
                 VALUES (?, ?, ?)",
            )
            .bind(&rev.id)
            .bind(parent)
            .bind(pos as i64)
            .execute(&mut **tx)
            .await?;
        }
        result.inserted_revisions += 1;
    }

    // 4b) plugin package component/residual edges（与 package revision 同 TX；幂等）
    //     边恢复前再次校验 package/component revision lineage 一致，防 ID 复用污染 ownership。
    verify_plugin_edge_lineage_on_tx(tx, bundle).await?;
    restore_plugin_edges_on_tx(tx, bundle).await?;

    // 5) 所有受影响 asset 的 read-set CAS（独立于 new_head）
    //    Conflict/AlreadyAncestor 的 new_head=None 也必须验证 expected_head，
    //    禁止规划后本地 head 推进仍写入陈旧 variants/conflicts。
    {
        use std::collections::{BTreeMap, BTreeSet};
        let mut cas_assets: BTreeSet<String> = BTreeSet::new();
        for d in &bundle.head_decisions {
            cas_assets.insert(d.asset_id.clone());
        }
        for v in &bundle.variants {
            cas_assets.insert(v.asset_id.clone());
        }
        for c in &bundle.conflicts {
            cas_assets.insert(c.asset_id.clone());
        }
        // head_decisions 是权威 expected_head 来源；variants/conflicts 涉及的 asset 必须出现在其中
        let decision_by_asset: BTreeMap<&str, &ImportHeadDecision> = bundle
            .head_decisions
            .iter()
            .map(|d| (d.asset_id.as_str(), d))
            .collect();
        for asset_id in cas_assets {
            let Some(d) = decision_by_asset.get(asset_id.as_str()) else {
                return Err(AppError::validation(format!(
                    "agent_hub_import_missing_head_decision:{asset_id}"
                )));
            };
            let current: Option<String> =
                sqlx::query_scalar("SELECT current_revision_id FROM agent_hub_assets WHERE id = ?")
                    .bind(&asset_id)
                    .fetch_optional(&mut **tx)
                    .await?
                    .flatten();
            let expected = d.expected_head.as_deref();
            let matches = match (current.as_deref(), expected) {
                (None, None) => true,
                (Some(c), Some(e)) => c == e,
                _ => false,
            };
            if !matches {
                return Err(AppError::conflict(format!(
                    "agent_hub_import_head_cas_conflict:{asset_id}"
                )));
            }
        }
    }

    // 6) variants（CAS 通过后才写，避免陈旧 plan 覆盖）
    for v in &bundle.variants {
        sqlx::query("DELETE FROM agent_hub_variants WHERE asset_id = ? AND target = ?")
            .bind(&v.asset_id)
            .bind(v.target.as_str())
            .execute(&mut **tx)
            .await?;
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO agent_hub_variants
             (id, asset_id, target, revision_id, extension_payload_hash, created_at)
             VALUES (?, ?, ?, ?, NULL, ?)",
        )
        .bind(&id)
        .bind(&v.asset_id)
        .bind(v.target.as_str())
        .bind(&v.revision_id)
        .bind(&now)
        .execute(&mut **tx)
        .await?;
    }

    // 7) conflicts
    for c in &bundle.conflicts {
        let exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_conflicts WHERE id = ?")
                .bind(&c.id)
                .fetch_one(&mut **tx)
                .await?;
        if exists > 0 {
            continue;
        }
        sqlx::query(
            "INSERT INTO agent_hub_conflicts (
                id, asset_id, target, base_revision_id, hub_revision_id,
                external_revision_id, detail_json, resolved, created_at, resolved_at
             ) VALUES (?,?,?,?,?,?,?,0,?,NULL)",
        )
        .bind(&c.id)
        .bind(&c.asset_id)
        .bind(c.target.map(|t| t.as_str().to_string()))
        .bind(&c.base_revision_id)
        .bind(&c.hub_revision_id)
        .bind(&c.external_revision_id)
        .bind(&c.detail_json)
        .bind(&c.created_at)
        .execute(&mut **tx)
        .await?;
        result.conflicts_opened += 1;
    }

    // 8) head decisions（仅显式 new_head 才推进；None 保留旧 head）
    //    expected_head 已在步骤 5 验证；此处再 CAS 更新防止同 TX 内竞态（防御性）。
    //    推进前校验 head revision 的 asset_lineage_id 与资产一致，防 ID 复用跨资产污染。
    for d in &bundle.head_decisions {
        if let Some(head) = &d.new_head {
            // 确认 head revision 存在且 lineage 属于该 asset
            let head_lineage: Option<String> =
                sqlx::query_scalar("SELECT asset_lineage_id FROM agent_hub_revisions WHERE id = ?")
                    .bind(head)
                    .fetch_optional(&mut **tx)
                    .await?;
            let Some(head_lineage) = head_lineage else {
                return Err(AppError::validation(format!(
                    "agent_hub_import_head_missing:{head}"
                )));
            };
            // lineage 通常等于 root asset id；若不等则要求属于该 asset 的 lineage 别名
            if head_lineage != d.asset_id {
                let same =
                    lineages_refer_to_same_asset_on_tx(tx, &head_lineage, &d.asset_id).await?;
                if !same {
                    return Err(AppError::conflict(format!(
                        "agent_hub_import_head_lineage_mismatch:{}:{}",
                        d.asset_id, head
                    )));
                }
            }
            let cas = sqlx::query(
                "UPDATE agent_hub_assets
                 SET current_revision_id = ?, deleted_at = ?, updated_at = ?
                 WHERE id = ?
                   AND (
                     (? IS NULL AND current_revision_id IS NULL)
                     OR current_revision_id = ?
                   )",
            )
            .bind(head)
            .bind(&d.deleted_at)
            .bind(&now)
            .bind(&d.asset_id)
            .bind(&d.expected_head)
            .bind(&d.expected_head)
            .execute(&mut **tx)
            .await?;
            if cas.rows_affected() != 1 {
                return Err(AppError::conflict(format!(
                    "agent_hub_import_head_cas_conflict:{}",
                    d.asset_id
                )));
            }
            result.heads_advanced += 1;
        }
        imported_assets.insert(d.asset_id.clone());
    }

    result.imported_asset_ids = imported_assets.into_iter().collect();
    Ok(result)
}

/// Snapshot 身份加载请求（与 builder selection 对齐的最小字段集）。
///
/// Business Logic（为什么需要这个结构体）:
///     单读事务 API 需要与 builder 相同的 mode/id 列表，但不依赖 snapshot 模块反向引用。
///
/// Code Logic（这个结构体做什么）:
///     mode + scope/asset/hub ids + include_history。
#[derive(Debug, Clone)]
pub struct SnapshotIdentityRequest {
    /// full / user / project / explicit
    pub mode: SnapshotIdentityMode,
    /// 显式 scope ids
    pub scope_ids: Vec<String>,
    /// 显式 asset ids
    pub asset_ids: Vec<String>,
    /// project hubProjectId 列表
    pub hub_project_ids: Vec<String>,
    /// 是否闭合完整 ancestry
    pub include_history: bool,
}

/// Snapshot 选择模式（repo 侧镜像，避免 storage→agent_hub::snapshot 反向依赖）。
///
/// Business Logic: full/user/project/explicit 四档。
/// Code Logic: 与 builder::SnapshotSelectionMode 一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotIdentityMode {
    /// 全部 Hub 资产
    FullHub,
    /// 用户 scope
    UserScope,
    /// 一个或多个 project
    Project,
    /// 显式 asset id 列表
    ExplicitAssets,
}

/// 单读事务内冻结的 snapshot 身份集合。
///
/// Business Logic（为什么需要这个结构体）:
///     builder 在 CAS 流式 re-hash 前必须持有一致身份集合；TX 结束后集合只读。
///
/// Code Logic（这个结构体做什么）:
///     保存 assets/lineages/revisions/variants/conflicts/aliases/hub ids/
///     residual tree hashes（Gate D package residual 闭包）。
#[derive(Debug, Clone)]
pub struct SnapshotIdentityBundle {
    /// 选中资产（含 tombstone）
    pub assets: Vec<LogicalAsset>,
    /// (asset_id, lineage_id)
    pub lineages: Vec<(String, String)>,
    /// 闭合 revision 集合
    pub revisions: Vec<Revision>,
    /// 当前 variants
    pub variants: Vec<AgentHubVariantRow>,
    /// 未解决 conflicts
    pub conflicts: Vec<AgentHubConflict>,
    /// 便携 aliases（无绝对路径）
    pub aliases: Vec<AgentHubPortableAliasRow>,
    /// 相关 hub project ids
    pub hub_project_ids: Vec<String>,
    /// Plugin residual tree_manifest_hash（即使非 active head 也闭合）
    pub residual_tree_hashes: Vec<String>,
}

/// Snapshot 用 variant 行。
///
/// Business Logic: envelope variants 字段来源。
/// Code Logic: 镜像 agent_hub_variants 列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHubVariantRow {
    /// 主键
    pub id: String,
    /// 资产 id
    pub asset_id: String,
    /// CLI target
    pub target: AgentTarget,
    /// 变体 head revision
    pub revision_id: String,
    /// 可选扩展 payload hash
    pub extension_payload_hash: Option<String>,
    /// 创建时间
    pub created_at: String,
}

/// Snapshot 用便携别名（无绝对路径）。
///
/// Business Logic: aliases 跨设备映射 portable identity。
/// Code Logic: kind + external_id + local_id。
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd)]
pub struct AgentHubPortableAliasRow {
    /// 别名种类
    pub kind: String,
    /// 外部 id
    pub external_id: String,
    /// 本地 canonical id
    pub local_id: String,
}

/// Agent Hub 项目映射行（本机 portable 身份）。
///
/// Business Logic（为什么需要这个结构体）:
///     hubProjectId 与本机 Workbench project/Git fingerprint 的映射是 opt-in 与跨设备对齐的基础。
///
/// Code Logic（这个结构体做什么）:
///     镜像 agent_hub_project_mappings 列（含 opted_in）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHubProjectMappingRow {
    pub id: String,
    pub hub_project_id: String,
    pub local_workbench_project_id: Option<String>,
    pub git_remote_fingerprint: Option<String>,
    pub local_absolute_path: Option<String>,
    pub opted_in: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 写入项目映射的输入。
///
/// Business Logic（为什么需要这个结构体）:
///     enable_project_scope 需要与完整行分离的 upsert 输入。
///
/// Code Logic（这个结构体做什么）:
///     不含 id/时间戳，由 repo 生成或保留。
#[derive(Debug, Clone)]
pub struct UpsertAgentHubProjectMapping {
    pub hub_project_id: String,
    pub local_workbench_project_id: Option<String>,
    pub git_remote_fingerprint: Option<String>,
    pub local_absolute_path: Option<String>,
    pub opted_in: bool,
}

/// Checkout binding 行（本地绝对路径仅存此处）。
///
/// Business Logic（为什么需要这个结构体）:
///     projection 需要按 checkout 定位路径；绝对路径绝不能进入 portable scope/asset。
///
/// Code Logic（这个结构体做什么）:
///     镜像 agent_hub_checkout_bindings 列（含 status/warning/local_absolute_path）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHubCheckoutBindingRow {
    pub id: String,
    pub hub_project_id: String,
    pub workbench_worktree_id: Option<String>,
    pub checkout_kind: String,
    pub relative_root: Option<String>,
    pub local_absolute_path: Option<String>,
    pub enabled: bool,
    pub status: String,
    pub warning: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 写入 checkout binding 的输入。
///
/// Business Logic（为什么需要这个结构体）:
///     refresh_checkout_bindings 需要 upsert 主/worktree 绑定。
///
/// Code Logic（这个结构体做什么）:
///     不含 id/时间戳。
#[derive(Debug, Clone)]
pub struct UpsertAgentHubCheckoutBinding {
    pub hub_project_id: String,
    pub workbench_worktree_id: Option<String>,
    pub checkout_kind: String,
    pub relative_root: Option<String>,
    pub local_absolute_path: Option<String>,
    pub enabled: bool,
    pub status: String,
    pub warning: Option<String>,
}

/// 幂等建表 SQL 列表。
const AGENT_HUB_SCHEMA_STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS agent_hub_scopes (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        hub_project_id TEXT,
        relative_path TEXT,
        created_at TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_assets (
        id TEXT PRIMARY KEY,
        scope_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        origin_namespace TEXT NOT NULL,
        logical_key TEXT NOT NULL,
        display_name TEXT NOT NULL,
        policy TEXT NOT NULL,
        current_revision_id TEXT,
        deleted_at TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        UNIQUE(scope_id, kind, origin_namespace, logical_key)
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_asset_lineages (
        asset_id TEXT NOT NULL,
        lineage_id TEXT NOT NULL,
        created_at TEXT NOT NULL,
        PRIMARY KEY (asset_id, lineage_id)
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_revisions (
        id TEXT PRIMARY KEY,
        asset_lineage_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        operation TEXT NOT NULL,
        origin_kind TEXT NOT NULL,
        origin_target TEXT,
        origin_replica_id TEXT NOT NULL,
        payload_hash TEXT,
        tree_manifest_hash TEXT,
        created_at TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_revision_parents (
        revision_id TEXT NOT NULL,
        parent_revision_id TEXT NOT NULL,
        parent_order INTEGER NOT NULL,
        PRIMARY KEY (revision_id, parent_order)
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_variants (
        id TEXT PRIMARY KEY,
        asset_id TEXT NOT NULL,
        target TEXT NOT NULL,
        revision_id TEXT NOT NULL,
        extension_payload_hash TEXT,
        created_at TEXT NOT NULL,
        UNIQUE(asset_id, target, revision_id)
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_target_bindings (
        id TEXT PRIMARY KEY,
        asset_id TEXT NOT NULL,
        target TEXT NOT NULL,
        local_scope_mapping_id TEXT,
        checkout_binding_id TEXT,
        desired_presence TEXT NOT NULL,
        desired_enabled INTEGER NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_hub_target_bindings_unique
     ON agent_hub_target_bindings(
        asset_id, target,
        IFNULL(local_scope_mapping_id, ''),
        IFNULL(checkout_binding_id, '')
     )",
    "CREATE TABLE IF NOT EXISTS agent_hub_materializations (
        id TEXT PRIMARY KEY,
        asset_id TEXT NOT NULL,
        target TEXT NOT NULL,
        target_binding_id TEXT NOT NULL,
        native_path TEXT,
        last_projected_revision_id TEXT,
        rendered_hash TEXT,
        observed_external_hash TEXT,
        status TEXT NOT NULL,
        last_error TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_projection_jobs (
        id TEXT PRIMARY KEY,
        asset_id TEXT NOT NULL,
        target TEXT NOT NULL,
        target_binding_id TEXT NOT NULL,
        desired_revision_id TEXT,
        state TEXT NOT NULL,
        attempt INTEGER NOT NULL DEFAULT 0,
        last_error TEXT,
        target_path TEXT,
        expected_external_hash TEXT,
        rendered_hash TEXT,
        rendered_object_hash TEXT,
        write_token TEXT,
        desired_presence TEXT,
        desired_enabled INTEGER NOT NULL DEFAULT 1,
        payload_kind TEXT NOT NULL DEFAULT 'file',
        managed_paths_json TEXT,
        hub_project_id TEXT,
        staging_path TEXT,
        backup_path TEXT,
        base_hash TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_conflicts (
        id TEXT PRIMARY KEY,
        asset_id TEXT NOT NULL,
        target TEXT,
        base_revision_id TEXT,
        hub_revision_id TEXT,
        external_revision_id TEXT,
        detail_json TEXT NOT NULL,
        resolved INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        resolved_at TEXT
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_project_mappings (
        id TEXT PRIMARY KEY,
        hub_project_id TEXT NOT NULL,
        local_workbench_project_id TEXT,
        git_remote_fingerprint TEXT,
        local_absolute_path TEXT,
        opted_in INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_checkout_bindings (
        id TEXT PRIMARY KEY,
        hub_project_id TEXT NOT NULL,
        workbench_worktree_id TEXT,
        checkout_kind TEXT NOT NULL,
        relative_root TEXT,
        local_absolute_path TEXT,
        enabled INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL DEFAULT 'active',
        warning TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_replica_state (
        replica_id TEXT PRIMARY KEY,
        last_seen_at TEXT,
        probe_json TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_agent_hub_revisions_lineage
     ON agent_hub_revisions(asset_lineage_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_agent_hub_revision_parents_parent
     ON agent_hub_revision_parents(parent_revision_id)",
    "CREATE INDEX IF NOT EXISTS idx_agent_hub_assets_scope
     ON agent_hub_assets(scope_id)",
    "CREATE TABLE IF NOT EXISTS agent_hub_adoptions (
        id TEXT PRIMARY KEY,
        asset_id TEXT,
        target TEXT NOT NULL,
        origin_path TEXT NOT NULL,
        origin_tree_hash TEXT NOT NULL,
        archive_tree_hash TEXT,
        materialization_id TEXT,
        package_id TEXT,
        staging_path TEXT,
        state TEXT NOT NULL,
        last_error TEXT,
        confirmed INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_agent_hub_adoptions_state
     ON agent_hub_adoptions(state, updated_at)",
    "CREATE INDEX IF NOT EXISTS idx_agent_hub_adoptions_origin
     ON agent_hub_adoptions(origin_path)",
    "CREATE TABLE IF NOT EXISTS agent_hub_user_instruction_ownership (
        asset_id TEXT NOT NULL,
        target TEXT NOT NULL,
        resolved_path TEXT NOT NULL,
        adopted_hash TEXT,
        adopted_revision_id TEXT,
        adoption_operation TEXT NOT NULL,
        confirmed_plan_token TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (asset_id, target)
    )",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_hub_user_instruction_ownership_path
     ON agent_hub_user_instruction_ownership(target, resolved_path)",
    "CREATE TABLE IF NOT EXISTS agent_hub_user_instruction_plans (
        plan_token TEXT PRIMARY KEY,
        owner_fingerprint TEXT NOT NULL,
        expires_at TEXT NOT NULL,
        base_revision_id TEXT,
        inventory_snapshot_hash TEXT NOT NULL,
        plan_json TEXT NOT NULL,
        client_request_id TEXT,
        claimed_at TEXT,
        consumed_at TEXT,
        result_json TEXT,
        created_at TEXT NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_agent_hub_user_instruction_plans_expiry
     ON agent_hub_user_instruction_plans(expires_at, consumed_at)",
    // Gate C Task 4：LAN push 幂等 ledger（sourceDeviceId+clientRequestId 非认证标签）
    "CREATE TABLE IF NOT EXISTS agent_hub_push_requests (
        source_device_id TEXT NOT NULL,
        client_request_id TEXT NOT NULL,
        transfer_id TEXT NOT NULL UNIQUE,
        selection_hash TEXT NOT NULL,
        snapshot_hash TEXT NOT NULL,
        status TEXT NOT NULL,
        envelope_json TEXT NOT NULL,
        outcome_json TEXT,
        staging_cleaned_at TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (source_device_id, client_request_id)
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_push_objects (
        transfer_id TEXT NOT NULL,
        object_hash TEXT NOT NULL,
        expected_size INTEGER NOT NULL,
        received_bytes INTEGER NOT NULL DEFAULT 0,
        verified INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (transfer_id, object_hash)
    )",
    "CREATE INDEX IF NOT EXISTS idx_agent_hub_push_requests_status
     ON agent_hub_push_requests(status, updated_at)",
    // Gate C Task 5：源侧 multi-target push 进度（GUI reconnect / Attention）
    "CREATE TABLE IF NOT EXISTS agent_hub_source_push_requests (
        request_id TEXT PRIMARY KEY,
        selection_mode TEXT NOT NULL,
        selection_json TEXT NOT NULL,
        selection_hash TEXT NOT NULL,
        snapshot_hash TEXT NOT NULL,
        status TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS agent_hub_source_push_targets (
        request_id TEXT NOT NULL,
        peer_device_id TEXT NOT NULL,
        peer_label TEXT NOT NULL,
        client_request_id TEXT NOT NULL,
        status TEXT NOT NULL,
        retryable INTEGER NOT NULL DEFAULT 0,
        error_code TEXT,
        transfer_id TEXT,
        missing_object_count INTEGER NOT NULL DEFAULT 0,
        transferred_object_count INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (request_id, peer_device_id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_agent_hub_source_push_targets_status
     ON agent_hub_source_push_targets(status, updated_at)",
    // Gate C Task 6：本机 device-lane Git 导出 pending / last-pushed 状态
    "CREATE TABLE IF NOT EXISTS agent_hub_git_export_state (
        device_id TEXT PRIMARY KEY,
        last_exported_snapshot_hash TEXT,
        last_pushed_snapshot_hash TEXT,
        pending_snapshot_hash TEXT,
        pending_commit_oid TEXT,
        pending_phase TEXT,
        attempt_count INTEGER NOT NULL DEFAULT 0,
        next_attempt_at TEXT,
        last_error TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )",
    // Codex R2: LAN commit 后 durable projection/cleanup intent（与 import 同 TX）
    "CREATE TABLE IF NOT EXISTS agent_hub_lan_projection_intents (
        transfer_id TEXT NOT NULL,
        asset_id TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'queued',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (transfer_id, asset_id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_agent_hub_lan_projection_intents_status
     ON agent_hub_lan_projection_intents(status, updated_at)",
    // Gate D Task 1：PluginPackage 固定 component/residual 引用边表 + standalone 引用
    "CREATE TABLE IF NOT EXISTS agent_hub_plugin_components (
        package_revision_id TEXT NOT NULL,
        component_kind TEXT NOT NULL,
        component_asset_id TEXT NOT NULL,
        component_revision_id TEXT NOT NULL,
        ownership TEXT NOT NULL,
        PRIMARY KEY (package_revision_id, component_kind, component_asset_id, component_revision_id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_agent_hub_plugin_components_component
     ON agent_hub_plugin_components(component_asset_id, component_revision_id)",
    "CREATE TABLE IF NOT EXISTS agent_hub_plugin_residuals (
        package_revision_id TEXT NOT NULL,
        target TEXT NOT NULL,
        residual_kind TEXT NOT NULL,
        tree_manifest_hash TEXT NOT NULL,
        PRIMARY KEY (package_revision_id, target, residual_kind, tree_manifest_hash)
    )",
    "CREATE INDEX IF NOT EXISTS idx_agent_hub_plugin_residuals_tree
     ON agent_hub_plugin_residuals(tree_manifest_hash)",
    "CREATE TABLE IF NOT EXISTS agent_hub_component_standalone_refs (
        component_asset_id TEXT NOT NULL,
        standalone_asset_id TEXT NOT NULL,
        created_at TEXT NOT NULL,
        PRIMARY KEY (component_asset_id, standalone_asset_id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_agent_hub_component_standalone_refs_standalone
     ON agent_hub_component_standalone_refs(standalone_asset_id)",
];

/// 升级旧库：为 mappings/bindings 补列与唯一索引。
///
/// Business Logic（为什么需要这个函数）:
///     已有 Gate A Task1 库缺少 opt-in/status/绝对路径列时，必须幂等 ALTER，不能重建丢行。
///
/// Code Logic（这个函数做什么）:
///     PRAGMA table_info 检测缺失列后 ALTER ADD COLUMN；再建 unique index。
async fn migrate_agent_hub_columns(pool: &SqlitePool) -> Result<(), AppError> {
    let mapping_cols = table_column_names(pool, "agent_hub_project_mappings").await?;
    if !mapping_cols.iter().any(|c| c == "opted_in") {
        sqlx::query(
            "ALTER TABLE agent_hub_project_mappings
             ADD COLUMN opted_in INTEGER NOT NULL DEFAULT 0",
        )
        .execute(pool)
        .await?;
    }

    let binding_cols = table_column_names(pool, "agent_hub_checkout_bindings").await?;
    if !binding_cols.iter().any(|c| c == "local_absolute_path") {
        sqlx::query(
            "ALTER TABLE agent_hub_checkout_bindings
             ADD COLUMN local_absolute_path TEXT",
        )
        .execute(pool)
        .await?;
    }
    if !binding_cols.iter().any(|c| c == "status") {
        sqlx::query(
            "ALTER TABLE agent_hub_checkout_bindings
             ADD COLUMN status TEXT NOT NULL DEFAULT 'active'",
        )
        .execute(pool)
        .await?;
    }
    if !binding_cols.iter().any(|c| c == "warning") {
        sqlx::query("ALTER TABLE agent_hub_checkout_bindings ADD COLUMN warning TEXT")
            .execute(pool)
            .await?;
    }

    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_hub_checkout_bindings_unique
         ON agent_hub_checkout_bindings(
            hub_project_id, IFNULL(workbench_worktree_id, '')
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_hub_project_mappings_local
         ON agent_hub_project_mappings(local_workbench_project_id)
         WHERE local_workbench_project_id IS NOT NULL",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_hub_project_mappings_hub
         ON agent_hub_project_mappings(hub_project_id)",
    )
    .execute(pool)
    .await?;

    let job_cols = table_column_names(pool, "agent_hub_projection_jobs").await?;
    for (col, ddl) in [
        ("target_path", "ALTER TABLE agent_hub_projection_jobs ADD COLUMN target_path TEXT"),
        (
            "expected_external_hash",
            "ALTER TABLE agent_hub_projection_jobs ADD COLUMN expected_external_hash TEXT",
        ),
        ("rendered_hash", "ALTER TABLE agent_hub_projection_jobs ADD COLUMN rendered_hash TEXT"),
        (
            "rendered_object_hash",
            "ALTER TABLE agent_hub_projection_jobs ADD COLUMN rendered_object_hash TEXT",
        ),
        ("write_token", "ALTER TABLE agent_hub_projection_jobs ADD COLUMN write_token TEXT"),
        (
            "desired_presence",
            "ALTER TABLE agent_hub_projection_jobs ADD COLUMN desired_presence TEXT",
        ),
        (
            "desired_enabled",
            "ALTER TABLE agent_hub_projection_jobs ADD COLUMN desired_enabled INTEGER NOT NULL DEFAULT 1",
        ),
        (
            "payload_kind",
            "ALTER TABLE agent_hub_projection_jobs ADD COLUMN payload_kind TEXT NOT NULL DEFAULT 'file'",
        ),
        (
            "managed_paths_json",
            "ALTER TABLE agent_hub_projection_jobs ADD COLUMN managed_paths_json TEXT",
        ),
        (
            "hub_project_id",
            "ALTER TABLE agent_hub_projection_jobs ADD COLUMN hub_project_id TEXT",
        ),
        ("staging_path", "ALTER TABLE agent_hub_projection_jobs ADD COLUMN staging_path TEXT"),
        ("backup_path", "ALTER TABLE agent_hub_projection_jobs ADD COLUMN backup_path TEXT"),
        ("base_hash", "ALTER TABLE agent_hub_projection_jobs ADD COLUMN base_hash TEXT"),
    ] {
        if !job_cols.iter().any(|c| c == col) {
            sqlx::query(ddl).execute(pool).await?;
        }
    }
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_agent_hub_projection_jobs_state
         ON agent_hub_projection_jobs(state, updated_at)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_agent_hub_projection_jobs_asset
         ON agent_hub_projection_jobs(asset_id, state)",
    )
    .execute(pool)
    .await?;

    // Codex R2: git pending_commit_oid 列升级
    let git_cols = table_column_names(pool, "agent_hub_git_export_state").await?;
    if !git_cols.iter().any(|c| c == "pending_commit_oid") {
        sqlx::query("ALTER TABLE agent_hub_git_export_state ADD COLUMN pending_commit_oid TEXT")
            .execute(pool)
            .await?;
    }
    // Codex R6: git pending_phase 列升级（pre_lane_write / lane_written / confirmed）
    if !git_cols.iter().any(|c| c == "pending_phase") {
        sqlx::query("ALTER TABLE agent_hub_git_export_state ADD COLUMN pending_phase TEXT")
            .execute(pool)
            .await?;
    }
    // Codex R3: committed staging GC 可推进标记，避免 256 条后饿死
    let push_cols = table_column_names(pool, "agent_hub_push_requests").await?;
    if !push_cols.iter().any(|c| c == "staging_cleaned_at") {
        sqlx::query("ALTER TABLE agent_hub_push_requests ADD COLUMN staging_cleaned_at TEXT")
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// 读取表列名集合。
///
/// Business Logic（为什么需要这个函数）:
///     升级迁移需幂等检测缺列，避免重复 ALTER 失败。
///
/// Code Logic（这个函数做什么）:
///     `PRAGMA table_info(table)` 收集 name 列。
async fn table_column_names(pool: &SqlitePool, table: &str) -> Result<Vec<String>, AppError> {
    // table 名来自本模块常量，非用户输入。
    let rows = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await?;
    let mut names = Vec::with_capacity(rows.len());
    for row in rows {
        names.push(row.try_get::<String, _>("name")?);
    }
    Ok(names)
}

/// 解析 project mapping 行。
///
/// Business Logic（为什么需要这个函数）:
///     读路径需把 SQLite INTEGER opted_in 转为 bool。
///
/// Code Logic（这个函数做什么）:
///     try_get 字段并构造 AgentHubProjectMappingRow。
fn row_to_project_mapping(row: &SqliteRow) -> Result<AgentHubProjectMappingRow, AppError> {
    let opted_in_i: i64 = row.try_get("opted_in")?;
    Ok(AgentHubProjectMappingRow {
        id: row.try_get("id")?,
        hub_project_id: row.try_get("hub_project_id")?,
        local_workbench_project_id: row.try_get("local_workbench_project_id")?,
        git_remote_fingerprint: row.try_get("git_remote_fingerprint")?,
        local_absolute_path: row.try_get("local_absolute_path")?,
        opted_in: opted_in_i != 0,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// 解析 checkout binding 行。
///
/// Business Logic（为什么需要这个函数）:
///     refresh/status 需要完整 binding 字段（含 status/warning/绝对路径）。
///
/// Code Logic（这个函数做什么）:
///     try_get 字段并构造 AgentHubCheckoutBindingRow；缺 status 时默认 active。
fn row_to_checkout_binding(row: &SqliteRow) -> Result<AgentHubCheckoutBindingRow, AppError> {
    let enabled_i: i64 = row.try_get("enabled")?;
    let status: String = row
        .try_get::<Option<String>, _>("status")?
        .unwrap_or_else(|| "active".to_string());
    Ok(AgentHubCheckoutBindingRow {
        id: row.try_get("id")?,
        hub_project_id: row.try_get("hub_project_id")?,
        workbench_worktree_id: row.try_get("workbench_worktree_id")?,
        checkout_kind: row.try_get("checkout_kind")?,
        relative_root: row.try_get("relative_root")?,
        local_absolute_path: row.try_get("local_absolute_path")?,
        enabled: enabled_i != 0,
        status,
        warning: row.try_get("warning")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// 判断 sqlx 错误是否为 UNIQUE 约束冲突。
///
/// Business Logic（为什么需要这个函数）:
///     资产唯一键冲突需映射为 Conflict，而非 500。
///
/// Code Logic（这个函数做什么）:
///     检查 Database 错误码 2067/1555 或消息含 UNIQUE。
fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db) => {
            if let Some(code) = db.code() {
                // SQLite SQLITE_CONSTRAINT_UNIQUE=2067, PRIMARYKEY=1555
                if code == "2067" || code == "1555" {
                    return true;
                }
            }
            let msg = db.message().to_lowercase();
            msg.contains("unique") || msg.contains("constraint")
        }
        _ => false,
    }
}

/// 解析 scope 行。
///
/// Business Logic（为什么需要这个函数）:
///     读路径需 fail-closed 解析 kind。
///
/// Code Logic（这个函数做什么）:
///     try_get 字段 + ScopeKind::parse。
fn row_to_scope(row: &SqliteRow) -> Result<ScopeNode, AppError> {
    let kind_raw: String = row.try_get("kind")?;
    let kind = ScopeKind::parse(&kind_raw)
        .ok_or_else(|| AppError::generic(format!("agent_hub_unknown_scope_kind:{kind_raw}")))?;
    Ok(ScopeNode {
        id: row.try_get("id")?,
        kind,
        hub_project_id: row.try_get("hub_project_id")?,
        relative_path: row.try_get("relative_path")?,
        created_at: row.try_get("created_at")?,
    })
}

/// 解析 asset 行。
///
/// Business Logic（为什么需要这个函数）:
///     枚举损坏必须报错，禁止 silent default。
///
/// Code Logic（这个函数做什么）:
///     解析 kind/policy 与可选 revision id。
fn row_to_asset(row: &SqliteRow) -> Result<LogicalAsset, AppError> {
    let kind_raw: String = row.try_get("kind")?;
    let kind = AssetKind::parse(&kind_raw)
        .ok_or_else(|| AppError::generic(format!("agent_hub_unknown_asset_kind:{kind_raw}")))?;
    let policy_raw: String = row.try_get("policy")?;
    let policy = AssetPolicy::parse(&policy_raw)
        .ok_or_else(|| AppError::generic(format!("agent_hub_unknown_asset_policy:{policy_raw}")))?;
    let current: Option<String> = row.try_get("current_revision_id")?;
    Ok(LogicalAsset {
        id: row.try_get("id")?,
        scope_id: row.try_get("scope_id")?,
        kind,
        origin_namespace: row.try_get("origin_namespace")?,
        logical_key: row.try_get("logical_key")?,
        display_name: row.try_get("display_name")?,
        policy,
        current_revision_id: current.map(RevisionId),
        deleted_at: row.try_get("deleted_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// 解析 revision 行并附加 parents。
///
/// Business Logic（为什么需要这个函数）:
///     round-trip 断言依赖完整 Revision 相等。
///
/// Code Logic（这个函数做什么）:
///     解析枚举与 generation。
fn row_to_revision(row: &SqliteRow, parents: Vec<RevisionId>) -> Result<Revision, AppError> {
    let op_raw: String = row.try_get("operation")?;
    let operation = RevisionOperation::parse(&op_raw).ok_or_else(|| {
        AppError::generic(format!("agent_hub_unknown_revision_operation:{op_raw}"))
    })?;
    let origin_raw: String = row.try_get("origin_kind")?;
    let origin_kind = RevisionOriginKind::parse(&origin_raw).ok_or_else(|| {
        AppError::generic(format!("agent_hub_unknown_revision_origin:{origin_raw}"))
    })?;
    let origin_target_raw: Option<String> = row.try_get("origin_target")?;
    let origin_target = match origin_target_raw {
        None => None,
        Some(s) => Some(
            AgentTarget::parse(&s)
                .ok_or_else(|| AppError::generic(format!("agent_hub_unknown_agent_target:{s}")))?,
        ),
    };
    let generation: i64 = row.try_get("generation")?;
    let id: String = row.try_get("id")?;
    Ok(Revision {
        id: RevisionId(id),
        asset_lineage_id: row.try_get("asset_lineage_id")?,
        parents,
        generation: generation as u64,
        operation,
        origin_kind,
        origin_target,
        origin_replica_id: row.try_get("origin_replica_id")?,
        payload_hash: row.try_get("payload_hash")?,
        tree_manifest_hash: row.try_get("tree_manifest_hash")?,
        created_at: row.try_get("created_at")?,
    })
}

/// 解析 target binding 行。
///
/// Business Logic（为什么需要这个函数）:
///     desired_enabled 以 INTEGER 存储，读出必须是 bool。
///
/// Code Logic（这个函数做什么）:
///     解析 target/presence/enabled。
fn row_to_target_binding(row: &SqliteRow) -> Result<TargetBinding, AppError> {
    let target_raw: String = row.try_get("target")?;
    let target = AgentTarget::parse(&target_raw)
        .ok_or_else(|| AppError::generic(format!("agent_hub_unknown_agent_target:{target_raw}")))?;
    let presence_raw: String = row.try_get("desired_presence")?;
    let desired_presence = DesiredPresence::parse(&presence_raw).ok_or_else(|| {
        AppError::generic(format!("agent_hub_unknown_desired_presence:{presence_raw}"))
    })?;
    let enabled_i: i64 = row.try_get("desired_enabled")?;
    Ok(TargetBinding {
        id: row.try_get("id")?,
        asset_id: row.try_get("asset_id")?,
        target,
        local_scope_mapping_id: row.try_get("local_scope_mapping_id")?,
        checkout_binding_id: row.try_get("checkout_binding_id")?,
        desired_presence,
        desired_enabled: enabled_i != 0,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// 解析 projection job 行。
///
/// Business Logic（为什么需要这个函数）:
///     crash recovery 与调度需完整 job 字段。
///
/// Code Logic（这个函数做什么）:
///     fail-closed 解析枚举与可选字段。
fn row_to_projection_job(row: &SqliteRow) -> Result<ProjectionJob, AppError> {
    let target_raw: String = row.try_get("target")?;
    let target = AgentTarget::parse(&target_raw)
        .ok_or_else(|| AppError::generic(format!("agent_hub_unknown_agent_target:{target_raw}")))?;
    let state_raw: String = row.try_get("state")?;
    let state = ProjectionJobState::parse(&state_raw).ok_or_else(|| {
        AppError::generic(format!("agent_hub_unknown_projection_state:{state_raw}"))
    })?;
    let presence_raw: Option<String> = row.try_get("desired_presence")?;
    let desired_presence = match presence_raw.as_deref() {
        None | Some("") => DesiredPresence::Present,
        Some(s) => DesiredPresence::parse(s)
            .ok_or_else(|| AppError::generic(format!("agent_hub_unknown_desired_presence:{s}")))?,
    };
    let enabled_i: i64 = row.try_get("desired_enabled").unwrap_or(1);
    let kind_raw: String = row
        .try_get::<Option<String>, _>("payload_kind")?
        .unwrap_or_else(|| "file".to_string());
    let payload_kind = ProjectionPayloadKind::parse(&kind_raw)
        .ok_or_else(|| AppError::generic(format!("agent_hub_unknown_payload_kind:{kind_raw}")))?;
    let attempt_i: i64 = row.try_get("attempt")?;
    let rev: Option<String> = row.try_get("desired_revision_id")?;
    let target_path: Option<String> = row.try_get("target_path")?;
    let rendered_hash: Option<String> = row.try_get("rendered_hash")?;
    let rendered_object_hash: Option<String> = row.try_get("rendered_object_hash")?;
    let write_token: Option<String> = row.try_get("write_token")?;
    Ok(ProjectionJob {
        id: row.try_get("id")?,
        asset_id: row.try_get("asset_id")?,
        target,
        target_binding_id: row.try_get("target_binding_id")?,
        desired_revision_id: rev.map(RevisionId),
        state,
        attempt: attempt_i.max(0) as u32,
        last_error: row.try_get("last_error")?,
        target_path: target_path.unwrap_or_default(),
        expected_external_hash: row.try_get("expected_external_hash")?,
        rendered_hash: rendered_hash.unwrap_or_default(),
        rendered_object_hash: rendered_object_hash.unwrap_or_default(),
        write_token: write_token.unwrap_or_default(),
        desired_presence,
        desired_enabled: enabled_i != 0,
        payload_kind,
        managed_paths_json: row.try_get("managed_paths_json")?,
        hub_project_id: row.try_get("hub_project_id")?,
        staging_path: row.try_get("staging_path")?,
        backup_path: row.try_get("backup_path")?,
        base_hash: row.try_get("base_hash")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// 解析 variant 行。
///
/// Business Logic: snapshot builder 需要 target variants。
/// Code Logic: try_get + AgentTarget::parse。
fn row_to_variant(row: &SqliteRow) -> Result<AgentHubVariantRow, AppError> {
    let target_raw: String = row.try_get("target")?;
    let target = AgentTarget::parse(&target_raw)
        .ok_or_else(|| AppError::generic(format!("agent_hub_unknown_target:{target_raw}")))?;
    Ok(AgentHubVariantRow {
        id: row.try_get("id")?,
        asset_id: row.try_get("asset_id")?,
        target,
        revision_id: row.try_get("revision_id")?,
        extension_payload_hash: row.try_get("extension_payload_hash")?,
        created_at: row.try_get("created_at")?,
    })
}

/// 解析 conflict 行。
///
/// Business Logic（为什么需要这个函数）:
///     Attention 与调度冻结需要完整 AgentHubConflict。
///
/// Code Logic（这个函数做什么）:
///     解析 target/revision 可选字段与 resolved 标志。
fn row_to_conflict(row: &SqliteRow) -> Result<AgentHubConflict, AppError> {
    let target_raw: Option<String> = row.try_get("target")?;
    let target =
        match target_raw.as_deref() {
            None => None,
            Some(raw) => Some(AgentTarget::parse(raw).ok_or_else(|| {
                AppError::generic(format!("agent_hub_unknown_agent_target:{raw}"))
            })?),
        };
    let base: Option<String> = row.try_get("base_revision_id")?;
    let hub: Option<String> = row.try_get("hub_revision_id")?;
    let external: Option<String> = row.try_get("external_revision_id")?;
    let resolved_i: i64 = row.try_get("resolved")?;
    Ok(AgentHubConflict {
        id: row.try_get("id")?,
        asset_id: row.try_get("asset_id")?,
        target,
        base_revision_id: base.map(RevisionId),
        hub_revision_id: hub.map(RevisionId),
        external_revision_id: external.map(RevisionId),
        detail_json: row.try_get("detail_json")?,
        resolved: resolved_i != 0,
        created_at: row.try_get("created_at")?,
        resolved_at: row.try_get("resolved_at")?,
    })
}

/// 解析 materialization 行。
///
/// Business Logic（为什么需要这个函数）:
///     观测状态 round-trip 依赖完整 Materialization。
///
/// Code Logic（这个函数做什么）:
///     解析 target/status 与 hash 字段。
fn row_to_materialization(row: &SqliteRow) -> Result<Materialization, AppError> {
    let target_raw: String = row.try_get("target")?;
    let target = AgentTarget::parse(&target_raw)
        .ok_or_else(|| AppError::generic(format!("agent_hub_unknown_agent_target:{target_raw}")))?;
    let status_raw: String = row.try_get("status")?;
    let status = MaterializationStatus::parse(&status_raw).ok_or_else(|| {
        AppError::generic(format!(
            "agent_hub_unknown_materialization_status:{status_raw}"
        ))
    })?;
    let rev: Option<String> = row.try_get("last_projected_revision_id")?;
    Ok(Materialization {
        id: row.try_get("id")?,
        asset_id: row.try_get("asset_id")?,
        target,
        target_binding_id: row.try_get("target_binding_id")?,
        native_path: row.try_get("native_path")?,
        last_projected_revision_id: rev.map(RevisionId),
        rendered_hash: row.try_get("rendered_hash")?,
        observed_external_hash: row.try_get("observed_external_hash")?,
        status,
        last_error: row.try_get("last_error")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// 解析 adoption 行。
///
/// Business Logic: crash recovery / UI 需要完整 AdoptionRecord。
/// Code Logic: 解析 target/state 枚举与 confirmed 整型。
fn row_to_adoption(row: &SqliteRow) -> Result<AdoptionRecord, AppError> {
    let target_raw: String = row.try_get("target")?;
    let target = AgentTarget::parse(&target_raw)
        .ok_or_else(|| AppError::generic(format!("agent_hub_unknown_agent_target:{target_raw}")))?;
    let state_raw: String = row.try_get("state")?;
    let state = AdoptionState::parse(&state_raw).ok_or_else(|| {
        AppError::generic(format!("agent_hub_unknown_adoption_state:{state_raw}"))
    })?;
    let confirmed_i: i64 = row.try_get("confirmed")?;
    Ok(AdoptionRecord {
        id: row.try_get("id")?,
        asset_id: row.try_get("asset_id")?,
        target,
        origin_path: row.try_get("origin_path")?,
        origin_tree_hash: row.try_get("origin_tree_hash")?,
        archive_tree_hash: row.try_get("archive_tree_hash")?,
        materialization_id: row.try_get("materialization_id")?,
        package_id: row.try_get("package_id")?,
        staging_path: row.try_get("staging_path")?,
        state,
        last_error: row.try_get("last_error")?,
        confirmed: confirmed_i != 0,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// 解析用户级指令 ownership 行。
fn row_to_user_instruction_ownership(
    row: &SqliteRow,
) -> Result<UserInstructionOwnershipRecord, AppError> {
    let target_raw: String = row.try_get("target")?;
    let target = AgentTarget::parse(&target_raw)
        .ok_or_else(|| AppError::generic(format!("agent_hub_unknown_agent_target:{target_raw}")))?;
    let adopted_revision_id: Option<String> = row.try_get("adopted_revision_id")?;
    Ok(UserInstructionOwnershipRecord {
        asset_id: row.try_get("asset_id")?,
        target,
        resolved_path: row.try_get("resolved_path")?,
        adopted_hash: row.try_get("adopted_hash")?,
        adopted_revision_id: adopted_revision_id.map(RevisionId),
        adoption_operation: row.try_get("adoption_operation")?,
        confirmed_plan_token: row.try_get("confirmed_plan_token")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// 解析用户级指令 preview plan 行。
fn row_to_user_instruction_plan(row: &SqliteRow) -> Result<UserInstructionPlanRecord, AppError> {
    let base_revision_id: Option<String> = row.try_get("base_revision_id")?;
    Ok(UserInstructionPlanRecord {
        plan_token: row.try_get("plan_token")?,
        owner_fingerprint: row.try_get("owner_fingerprint")?,
        expires_at: row.try_get("expires_at")?,
        base_revision_id: base_revision_id.map(RevisionId),
        inventory_snapshot_hash: row.try_get("inventory_snapshot_hash")?,
        plan_json: row.try_get("plan_json")?,
        client_request_id: row.try_get("client_request_id")?,
        claimed_at: row.try_get("claimed_at")?,
        consumed_at: row.try_get("consumed_at")?,
        result_json: row.try_get("result_json")?,
        created_at: row.try_get("created_at")?,
    })
}

// ── Snapshot 单读事务 on_tx helpers（Gate C Task 2 fix）──────────────────

/// 在事务连接上解析 selection 资产集合。
///
/// Business Logic: 与 builder 四档 selection 语义一致，且全部落在同一 TX。
/// Code Logic: 按 mode 调用对应 on_tx 列表查询。
async fn resolve_selected_assets_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    request: &SnapshotIdentityRequest,
) -> Result<Vec<LogicalAsset>, AppError> {
    match request.mode {
        SnapshotIdentityMode::FullHub => list_all_assets_including_deleted_on_tx(tx).await,
        SnapshotIdentityMode::UserScope => {
            let scope_ids = if !request.scope_ids.is_empty() {
                request.scope_ids.clone()
            } else if let Some(id) = resolve_user_scope_id_on_tx(tx).await? {
                vec![id]
            } else {
                return Ok(Vec::new());
            };
            list_assets_in_scopes_including_deleted_on_tx(tx, &scope_ids).await
        }
        SnapshotIdentityMode::Project => {
            let mut scope_ids = request.scope_ids.clone();
            for hub in &request.hub_project_ids {
                if let Some(sid) = resolve_project_scope_id_on_tx(tx, hub).await? {
                    scope_ids.push(sid);
                }
            }
            scope_ids.sort();
            scope_ids.dedup();
            list_assets_in_scopes_including_deleted_on_tx(tx, &scope_ids).await
        }
        SnapshotIdentityMode::ExplicitAssets => {
            list_assets_by_ids_including_deleted_on_tx(tx, &request.asset_ids).await
        }
    }
}

/// 事务内列出全部资产（含 deleted）。
async fn list_all_assets_including_deleted_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<Vec<LogicalAsset>, AppError> {
    let rows = sqlx::query(
        "SELECT id, scope_id, kind, origin_namespace, logical_key, display_name, policy,
                current_revision_id, deleted_at, created_at, updated_at
         FROM agent_hub_assets
         ORDER BY id ASC",
    )
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(row_to_asset).collect()
}

/// 事务内按 id 列表读资产（含 deleted）。
async fn list_assets_by_ids_including_deleted_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    ids: &[String],
) -> Result<Vec<LogicalAsset>, AppError> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        if !seen.insert(id.clone()) {
            continue;
        }
        let row = sqlx::query(
            "SELECT id, scope_id, kind, origin_namespace, logical_key, display_name, policy,
                    current_revision_id, deleted_at, created_at, updated_at
             FROM agent_hub_assets WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(row) = row {
            out.push(row_to_asset(&row)?);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// 事务内按 scope 列表读资产（含 deleted）。
async fn list_assets_in_scopes_including_deleted_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    scope_ids: &[String],
) -> Result<Vec<LogicalAsset>, AppError> {
    if scope_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for scope_id in scope_ids {
        let rows = sqlx::query(
            "SELECT id, scope_id, kind, origin_namespace, logical_key, display_name, policy,
                    current_revision_id, deleted_at, created_at, updated_at
             FROM agent_hub_assets
             WHERE scope_id = ?
             ORDER BY id ASC",
        )
        .bind(scope_id)
        .fetch_all(&mut **tx)
        .await?;
        for row in rows {
            out.push(row_to_asset(&row)?);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}

/// 事务内读资产 lineage 对。
async fn list_lineages_for_assets_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    asset_ids: &[String],
) -> Result<Vec<(String, String)>, AppError> {
    let mut out = Vec::new();
    for asset_id in asset_ids {
        let rows = sqlx::query(
            "SELECT asset_id, lineage_id FROM agent_hub_asset_lineages
             WHERE asset_id = ? ORDER BY lineage_id ASC",
        )
        .bind(asset_id)
        .fetch_all(&mut **tx)
        .await?;
        for row in rows {
            let a: String = row.try_get("asset_id")?;
            let l: String = row.try_get("lineage_id")?;
            out.push((a, l));
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// 事务内按 id 读取资产（含 deleted）。
///
/// Business Logic: package component 可能不在 selection；闭包扩展 assets 时需同 TX 读。
/// Code Logic: SELECT agent_hub_assets WHERE id = ?。
async fn get_asset_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<LogicalAsset>, AppError> {
    let row = sqlx::query(
        "SELECT id, scope_id, kind, origin_namespace, logical_key, display_name, policy,
                current_revision_id, deleted_at, created_at, updated_at
         FROM agent_hub_assets WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|r| row_to_asset(&r)).transpose()
}

/// 事务内列出 package revision 的 component 边。
///
/// Business Logic: Snapshot 闭包必须钉住固定 component revision，即使非 active head。
/// Code Logic: SELECT agent_hub_plugin_components。
async fn list_plugin_components_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    package_revision_id: &str,
) -> Result<Vec<PluginComponentRef>, AppError> {
    let rows = sqlx::query(
        "SELECT component_kind, component_asset_id, component_revision_id, ownership
         FROM agent_hub_plugin_components
         WHERE package_revision_id = ?
         ORDER BY component_kind ASC, component_asset_id ASC, component_revision_id ASC",
    )
    .bind(package_revision_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let kind_s: String = row.try_get("component_kind")?;
        let kind = AssetKind::parse(&kind_s).ok_or_else(|| {
            AppError::validation(format!("agent_hub_plugin_component_kind_unknown:{kind_s}"))
        })?;
        let asset_id: String = row.try_get("component_asset_id")?;
        let rev: String = row.try_get("component_revision_id")?;
        let own_s: String = row.try_get("ownership")?;
        let ownership = ComponentOwnership::parse(&own_s).ok_or_else(|| {
            AppError::validation(format!("agent_hub_plugin_ownership_unknown:{own_s}"))
        })?;
        out.push(PluginComponentRef {
            kind,
            asset_id,
            revision_id: RevisionId::from(rev),
            ownership,
        });
    }
    Ok(out)
}

/// 比较已存在 revision 行与导入行的不可变字段 + 有序 parents。
///
/// Business Logic（为什么需要这个函数）:
///     仅按 revision UUID 去重会让未经校验的 snapshot 复用已知 ID 污染 DAG。
///     命中 ID 时必须验证 lineage/payload/operation/origin/parents 完全一致。
///
/// Code Logic（这个函数做什么）:
///     读取 local row 的 immutable 列与 ORDER BY parent_order 的 parents；
///     与 ImportRevisionRow 逐项比较；任一不等返回 false。
async fn revision_immutable_fields_match_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    row: &SqliteRow,
    rev: &ImportRevisionRow,
) -> Result<bool, AppError> {
    let lineage: String = row.try_get("asset_lineage_id")?;
    let operation: String = row.try_get("operation")?;
    let origin_kind: String = row.try_get("origin_kind")?;
    let origin_target: Option<String> = row.try_get("origin_target")?;
    let payload: Option<String> = row.try_get("payload_hash")?;
    let tree: Option<String> = row.try_get("tree_manifest_hash")?;
    // generation 在首次插入时可能被 max(parent)+1 校正；用同一规则重算期望值再比。
    let generation: i64 = row.try_get("generation")?;

    // lineage：直接相等，或双方指向同一本地资产（lineage 表 / assets 表可证明）
    if lineage != rev.asset_lineage_id {
        let same_asset =
            lineages_refer_to_same_asset_on_tx(tx, &lineage, &rev.asset_lineage_id).await?;
        if !same_asset {
            return Ok(false);
        }
    }
    if operation != rev.operation.as_str() {
        return Ok(false);
    }
    if origin_kind != rev.origin_kind.as_str() {
        return Ok(false);
    }
    let expected_target = rev.origin_target.map(|t| t.as_str().to_string());
    if origin_target != expected_target {
        return Ok(false);
    }
    // origin_replica_id / created_at：同 ID 的 payload+parents 一致即可 dedupe；
    // multi-replica 共用祖先时可能带不同 origin 标签，不得因此阻断合法 merge。
    // 真正的内容攻击靠 payload/parents/operation 拦截。
    if payload != rev.payload_hash {
        return Ok(false);
    }
    if tree != rev.tree_manifest_hash {
        return Ok(false);
    }
    let mut max_parent: Option<u64> = None;
    for parent in &rev.parents {
        let g: Option<i64> =
            sqlx::query_scalar("SELECT generation FROM agent_hub_revisions WHERE id = ?")
                .bind(parent)
                .fetch_optional(&mut **tx)
                .await?;
        if let Some(g) = g {
            let g = g as u64;
            max_parent = Some(max_parent.map_or(g, |m| m.max(g)));
        }
    }
    let expected_generation = max_parent
        .map(|m| m.saturating_add(1))
        .unwrap_or(rev.generation);
    if generation as u64 != expected_generation {
        return Ok(false);
    }

    let parent_rows = sqlx::query(
        "SELECT parent_revision_id FROM agent_hub_revision_parents
         WHERE revision_id = ? ORDER BY parent_order ASC",
    )
    .bind(&rev.id)
    .fetch_all(&mut **tx)
    .await?;
    let local_parents: Vec<String> = parent_rows
        .iter()
        .map(|r| r.try_get::<String, _>("parent_revision_id"))
        .collect::<Result<Vec<_>, _>>()?;
    if local_parents != rev.parents {
        return Ok(false);
    }
    Ok(true)
}

/// 判断两个 lineage/asset id 是否指向同一逻辑资产。
///
/// Business Logic: multi-replica identity remap 后，同一 revision 可能带着旧/新 asset id。
/// Code Logic: 相等、或双方均为某 asset 的 lineage 别名、或一方是另一方的 asset 行。
async fn lineages_refer_to_same_asset_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    a: &str,
    b: &str,
) -> Result<bool, AppError> {
    if a == b {
        return Ok(true);
    }
    // a 的 lineage 集合是否包含 b，或 b 的集合是否包含 a
    let a_as_asset: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_assets WHERE id = ?")
        .bind(a)
        .fetch_one(&mut **tx)
        .await?;
    let b_as_asset: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_assets WHERE id = ?")
        .bind(b)
        .fetch_one(&mut **tx)
        .await?;
    if a_as_asset > 0 {
        let linked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_hub_asset_lineages
             WHERE asset_id = ? AND lineage_id = ?",
        )
        .bind(a)
        .bind(b)
        .fetch_one(&mut **tx)
        .await?;
        if linked > 0 {
            return Ok(true);
        }
    }
    if b_as_asset > 0 {
        let linked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_hub_asset_lineages
             WHERE asset_id = ? AND lineage_id = ?",
        )
        .bind(b)
        .bind(a)
        .fetch_one(&mut **tx)
        .await?;
        if linked > 0 {
            return Ok(true);
        }
    }
    // 双方都作为 lineage_id 挂到同一 asset_id
    let shared: Option<String> = sqlx::query_scalar(
        "SELECT a.asset_id FROM agent_hub_asset_lineages a
         INNER JOIN agent_hub_asset_lineages b ON a.asset_id = b.asset_id
         WHERE a.lineage_id = ? AND b.lineage_id = ?
         LIMIT 1",
    )
    .bind(a)
    .bind(b)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(shared.is_some())
}

/// 恢复 plugin 边前校验 package/component revision lineage 与资产 kind。
///
/// Business Logic（为什么需要这个函数）:
///     head 或 plugin edge 不得指向另一资产/另一内容的本地 revision，破坏 ownership 完整性。
///
/// Code Logic（这个函数做什么）:
///     对每条 component edge：package revision 与 component revision 的 asset_lineage_id
///     必须分别等于 package/component asset_id；component asset kind 必须匹配。
async fn verify_plugin_edge_lineage_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    bundle: &ImportBundle,
) -> Result<(), AppError> {
    for edge in &bundle.plugin_components {
        let pkg_lineage: Option<String> =
            sqlx::query_scalar("SELECT asset_lineage_id FROM agent_hub_revisions WHERE id = ?")
                .bind(&edge.package_revision_id)
                .fetch_optional(&mut **tx)
                .await?;
        let Some(pkg_lineage) = pkg_lineage else {
            return Err(AppError::validation(format!(
                "agent_hub_import_plugin_package_revision_missing:{}",
                edge.package_revision_id
            )));
        };
        // package lineage 必须指向某 package asset（通过 bundle.assets 或已有 assets 表）
        let pkg_asset_kind: Option<String> =
            sqlx::query_scalar("SELECT kind FROM agent_hub_assets WHERE id = ?")
                .bind(&pkg_lineage)
                .fetch_optional(&mut **tx)
                .await?;
        if pkg_asset_kind.as_deref() != Some(AssetKind::Plugin.as_str()) {
            // 若 lineage 本身就是 package asset id 但本批才插入，从 bundle 查
            let from_bundle = bundle
                .assets
                .iter()
                .find(|a| a.id == pkg_lineage)
                .map(|a| a.kind);
            if from_bundle != Some(AssetKind::Plugin) {
                return Err(AppError::conflict(format!(
                    "agent_hub_import_plugin_package_lineage_mismatch:{}",
                    edge.package_revision_id
                )));
            }
        }

        let comp_lineage: Option<String> =
            sqlx::query_scalar("SELECT asset_lineage_id FROM agent_hub_revisions WHERE id = ?")
                .bind(&edge.component_revision_id)
                .fetch_optional(&mut **tx)
                .await?;
        let Some(comp_lineage) = comp_lineage else {
            return Err(AppError::validation(format!(
                "agent_hub_import_plugin_component_revision_missing:{}",
                edge.component_revision_id
            )));
        };
        if comp_lineage != edge.component_asset_id {
            return Err(AppError::conflict(format!(
                "agent_hub_import_plugin_component_lineage_mismatch:{}:{}",
                edge.component_revision_id, edge.component_asset_id
            )));
        }
    }
    Ok(())
}

/// 在 import TX 内校验并幂等写入 plugin component/residual 边。
///
/// Business Logic（为什么需要这个函数）:
///     Snapshot/LAN/Git import 只恢复 revision/CAS 会让 ownership 边表为空；
///     共享 component 在 package delete 时会被误判为独占并 tombstone。
///
/// Code Logic（这个函数做什么）:
///     对每条边：package revision 存在；component asset/revision 存在；
///     residual hash 非空；INSERT OR IGNORE 幂等。
async fn restore_plugin_edges_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    bundle: &ImportBundle,
) -> Result<(), AppError> {
    for edge in &bundle.plugin_components {
        let package_exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_revisions WHERE id = ?")
                .bind(&edge.package_revision_id)
                .fetch_one(&mut **tx)
                .await?;
        if package_exists == 0 {
            return Err(AppError::validation(format!(
                "agent_hub_import_plugin_package_revision_missing:{}",
                edge.package_revision_id
            )));
        }
        let asset_exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_assets WHERE id = ?")
                .bind(&edge.component_asset_id)
                .fetch_one(&mut **tx)
                .await?;
        if asset_exists == 0 {
            return Err(AppError::validation(format!(
                "agent_hub_import_plugin_component_asset_missing:{}",
                edge.component_asset_id
            )));
        }
        let rev_exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_revisions WHERE id = ?")
                .bind(&edge.component_revision_id)
                .fetch_one(&mut **tx)
                .await?;
        if rev_exists == 0 {
            return Err(AppError::validation(format!(
                "agent_hub_import_plugin_component_revision_missing:{}",
                edge.component_revision_id
            )));
        }
        sqlx::query(
            "INSERT OR IGNORE INTO agent_hub_plugin_components
             (package_revision_id, component_kind, component_asset_id,
              component_revision_id, ownership)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&edge.package_revision_id)
        .bind(edge.component_kind.as_str())
        .bind(&edge.component_asset_id)
        .bind(&edge.component_revision_id)
        .bind(edge.ownership.as_str())
        .execute(&mut **tx)
        .await?;
        // Standalone ownership 必须同步重建 agent_hub_component_standalone_refs，
        // 否则 import 后删除唯一 Plugin 会误判 has_standalone=false 并 tombstone 独立资产。
        if edge.ownership == ComponentOwnership::Standalone {
            let now_sa = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT OR IGNORE INTO agent_hub_component_standalone_refs
                 (component_asset_id, standalone_asset_id, created_at)
                 VALUES (?, ?, ?)",
            )
            .bind(&edge.component_asset_id)
            .bind(&edge.component_asset_id)
            .bind(&now_sa)
            .execute(&mut **tx)
            .await?;
        }
    }

    for edge in &bundle.plugin_residuals {
        if edge.tree_manifest_hash.trim().is_empty() {
            return Err(AppError::validation(
                "agent_hub_import_plugin_residual_tree_empty".to_string(),
            ));
        }
        let package_exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_hub_revisions WHERE id = ?")
                .bind(&edge.package_revision_id)
                .fetch_one(&mut **tx)
                .await?;
        if package_exists == 0 {
            return Err(AppError::validation(format!(
                "agent_hub_import_plugin_package_revision_missing:{}",
                edge.package_revision_id
            )));
        }
        sqlx::query(
            "INSERT OR IGNORE INTO agent_hub_plugin_residuals
             (package_revision_id, target, residual_kind, tree_manifest_hash)
             VALUES (?, ?, ?, ?)",
        )
        .bind(&edge.package_revision_id)
        .bind(edge.target.as_str())
        .bind(edge.residual_kind.as_str())
        .bind(&edge.tree_manifest_hash)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// 事务内：component 是否有 standalone 边。
async fn component_has_standalone_ref_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    component_asset_id: &str,
) -> Result<bool, AppError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_hub_component_standalone_refs
         WHERE component_asset_id = ?",
    )
    .bind(component_asset_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(n > 0)
}

/// 事务内：其它 live package head 对该 component 的引用数。
async fn count_other_live_package_head_refs_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    component_asset_id: &str,
    exclude_package_asset_id: Option<&str>,
) -> Result<u64, AppError> {
    let n: i64 = if let Some(exclude) = exclude_package_asset_id {
        sqlx::query_scalar(
            "SELECT COUNT(*)
             FROM agent_hub_plugin_components c
             INNER JOIN agent_hub_assets a
               ON a.current_revision_id = c.package_revision_id
             WHERE c.component_asset_id = ?
               AND a.kind = 'plugin'
               AND a.deleted_at IS NULL
               AND a.id != ?",
        )
        .bind(component_asset_id)
        .bind(exclude)
        .fetch_one(&mut **tx)
        .await?
    } else {
        sqlx::query_scalar(
            "SELECT COUNT(*)
             FROM agent_hub_plugin_components c
             INNER JOIN agent_hub_assets a
               ON a.current_revision_id = c.package_revision_id
             WHERE c.component_asset_id = ?
               AND a.kind = 'plugin'
               AND a.deleted_at IS NULL",
        )
        .bind(component_asset_id)
        .fetch_one(&mut **tx)
        .await?
    };
    Ok(n as u64)
}

/// 事务内插入 Delete revision 并 CAS 推进 head（用于 multi-asset ownership delete）。
#[allow(clippy::too_many_arguments)]
async fn insert_delete_revision_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    asset_id: &str,
    asset_lineage_id: &str,
    revision_id: &RevisionId,
    parents: &[RevisionId],
    expected_parent_id: Option<RevisionId>,
    origin_kind: RevisionOriginKind,
    origin_replica_id: &str,
    created_at: &str,
) -> Result<Revision, AppError> {
    let mut max_parent_generation: Option<u64> = None;
    for parent in parents {
        let gen: Option<i64> =
            sqlx::query_scalar("SELECT generation FROM agent_hub_revisions WHERE id = ?")
                .bind(parent.as_str())
                .fetch_optional(&mut **tx)
                .await?;
        let Some(g) = gen else {
            return Err(AppError::validation(format!(
                "agent_hub_revision_parent_missing:{}",
                parent.as_str()
            )));
        };
        let g = g as u64;
        max_parent_generation = Some(max_parent_generation.map_or(g, |m| m.max(g)));
    }
    let generation = max_parent_generation.map_or(0, |m| m.saturating_add(1));

    sqlx::query(
        "INSERT INTO agent_hub_revisions
         (id, asset_lineage_id, generation, operation, origin_kind, origin_target,
          origin_replica_id, payload_hash, tree_manifest_hash, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(revision_id.as_str())
    .bind(asset_lineage_id)
    .bind(generation as i64)
    .bind(RevisionOperation::Delete.as_str())
    .bind(origin_kind.as_str())
    .bind::<Option<String>>(None)
    .bind(origin_replica_id)
    .bind::<Option<String>>(None)
    .bind::<Option<String>>(None)
    .bind(created_at)
    .execute(&mut **tx)
    .await?;

    for (pos, parent) in parents.iter().enumerate() {
        sqlx::query(
            "INSERT INTO agent_hub_revision_parents
             (revision_id, parent_revision_id, parent_order)
             VALUES (?, ?, ?)",
        )
        .bind(revision_id.as_str())
        .bind(parent.as_str())
        .bind(pos as i64)
        .execute(&mut **tx)
        .await?;
    }

    let deleted_at = Some(created_at.to_string());
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(expected) = expected_parent_id.as_ref().map(|r| r.as_str().to_string()) {
        let result = sqlx::query(
            "UPDATE agent_hub_assets
             SET current_revision_id = ?, deleted_at = ?, updated_at = ?
             WHERE id = ?
               AND (current_revision_id IS NULL OR current_revision_id = ?)",
        )
        .bind(revision_id.as_str())
        .bind(&deleted_at)
        .bind(&now)
        .bind(asset_id)
        .bind(&expected)
        .execute(&mut **tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(AppError::conflict(
                "agent_hub_revision_conflict".to_string(),
            ));
        }
    } else {
        sqlx::query(
            "UPDATE agent_hub_assets
             SET current_revision_id = ?, deleted_at = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(revision_id.as_str())
        .bind(&deleted_at)
        .bind(&now)
        .bind(asset_id)
        .execute(&mut **tx)
        .await?;
    }

    Ok(Revision {
        id: revision_id.clone(),
        asset_lineage_id: asset_lineage_id.to_string(),
        parents: parents.to_vec(),
        generation,
        operation: RevisionOperation::Delete,
        origin_kind,
        origin_target: None,
        origin_replica_id: origin_replica_id.to_string(),
        payload_hash: None,
        tree_manifest_hash: None,
        created_at: created_at.to_string(),
    })
}

/// 事务内列出 package revision 的 residual 边。
///
/// Business Logic: residual tree 必须进 snapshot objects，即使 package 非 active head。
/// Code Logic: SELECT agent_hub_plugin_residuals。
async fn list_plugin_residuals_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    package_revision_id: &str,
) -> Result<Vec<PluginResidualRef>, AppError> {
    let rows = sqlx::query(
        "SELECT target, residual_kind, tree_manifest_hash
         FROM agent_hub_plugin_residuals
         WHERE package_revision_id = ?
         ORDER BY target ASC, residual_kind ASC, tree_manifest_hash ASC",
    )
    .bind(package_revision_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let target_s: String = row.try_get("target")?;
        let target = AgentTarget::parse(&target_s).ok_or_else(|| {
            AppError::validation(format!(
                "agent_hub_plugin_residual_target_unknown:{target_s}"
            ))
        })?;
        let kind_s: String = row.try_get("residual_kind")?;
        let residual_kind = ResidualKind::parse(&kind_s).ok_or_else(|| {
            AppError::validation(format!("agent_hub_plugin_residual_kind_unknown:{kind_s}"))
        })?;
        let tree_manifest_hash: String = row.try_get("tree_manifest_hash")?;
        out.push(PluginResidualRef {
            target,
            residual_kind,
            tree_manifest_hash,
        });
    }
    Ok(out)
}

/// 事务内 get_revision（含有序 parents）。
async fn get_revision_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<Revision>, AppError> {
    let row = sqlx::query(
        "SELECT id, asset_lineage_id, generation, operation, origin_kind, origin_target,
                origin_replica_id, payload_hash, tree_manifest_hash, created_at
         FROM agent_hub_revisions WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let parent_rows = sqlx::query(
        "SELECT parent_revision_id FROM agent_hub_revision_parents
         WHERE revision_id = ? ORDER BY parent_order ASC",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await?;
    let parents = parent_rows
        .iter()
        .map(|r| {
            let s: String = r.try_get("parent_revision_id")?;
            Ok::<_, AppError>(RevisionId(s))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(row_to_revision(&row, parents)?))
}

/// 事务内从 heads BFS 闭合 ancestry。
async fn collect_revision_ancestry_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    head_ids: &[String],
) -> Result<Vec<Revision>, AppError> {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    for h in head_ids {
        if !h.is_empty() {
            queue.push_back(h.clone());
        }
    }
    let mut by_id: BTreeMap<String, Revision> = BTreeMap::new();
    while let Some(id) = queue.pop_front() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let rev = get_revision_on_tx(tx, &id).await?.ok_or_else(|| {
            AppError::not_found(format!("agent_hub_snapshot_revision_missing:{id}"))
        })?;
        for p in &rev.parents {
            queue.push_back(p.as_str().to_string());
        }
        by_id.insert(id, rev);
    }
    Ok(by_id.into_values().collect())
}

/// 事务内列 variants。
async fn list_variants_for_assets_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    asset_ids: &[String],
) -> Result<Vec<AgentHubVariantRow>, AppError> {
    let mut out = Vec::new();
    for asset_id in asset_ids {
        let rows = sqlx::query(
            "SELECT id, asset_id, target, revision_id, extension_payload_hash, created_at
             FROM agent_hub_variants
             WHERE asset_id = ?
             ORDER BY target ASC, revision_id ASC",
        )
        .bind(asset_id)
        .fetch_all(&mut **tx)
        .await?;
        for row in rows {
            out.push(row_to_variant(&row)?);
        }
    }
    out.sort_by(|a, b| {
        a.asset_id
            .cmp(&b.asset_id)
            .then(a.target.as_str().cmp(b.target.as_str()))
            .then(a.revision_id.cmp(&b.revision_id))
    });
    Ok(out)
}

/// 事务内按资产过滤未解决 conflicts。
async fn list_unresolved_conflicts_for_assets_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    asset_ids: &[String],
) -> Result<Vec<AgentHubConflict>, AppError> {
    let set: std::collections::BTreeSet<&str> = asset_ids.iter().map(String::as_str).collect();
    let rows = sqlx::query(
        "SELECT id, asset_id, target, base_revision_id, hub_revision_id,
                external_revision_id, detail_json, resolved, created_at, resolved_at
         FROM agent_hub_conflicts
         WHERE resolved = 0
         ORDER BY created_at DESC, id ASC",
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::new();
    for row in rows {
        let c = row_to_conflict(&row)?;
        if set.contains(c.asset_id.as_str()) {
            out.push(c);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// 事务内 get_scope。
async fn get_scope_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<ScopeNode>, AppError> {
    let row = sqlx::query(
        "SELECT id, kind, hub_project_id, relative_path, created_at
         FROM agent_hub_scopes WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|r| row_to_scope(&r)).transpose()
}

/// 事务内按 hub_project_id 取 project scope id。
async fn resolve_project_scope_id_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    hub_project_id: &str,
) -> Result<Option<String>, AppError> {
    let row = sqlx::query(
        "SELECT id, kind, hub_project_id, relative_path, created_at
         FROM agent_hub_scopes
         WHERE kind = 'project' AND hub_project_id = ?
         LIMIT 1",
    )
    .bind(hub_project_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| row_to_scope(&r)).transpose()?.map(|s| s.id))
}

/// 事务内取字典序最小 user scope id。
async fn resolve_user_scope_id_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<Option<String>, AppError> {
    let rows = sqlx::query(
        "SELECT id, kind, hub_project_id, relative_path, created_at
         FROM agent_hub_scopes
         ORDER BY id ASC",
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut user_ids = Vec::new();
    for row in rows {
        let scope = row_to_scope(&row)?;
        if scope.kind == ScopeKind::User {
            user_ids.push(scope.id);
        }
    }
    Ok(user_ids.into_iter().min())
}

/// 事务内读 project mapping。
async fn get_project_mapping_by_hub_project_id_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    hub_project_id: &str,
) -> Result<Option<AgentHubProjectMappingRow>, AppError> {
    let row = sqlx::query(
        "SELECT id, hub_project_id, local_workbench_project_id, git_remote_fingerprint,
                local_absolute_path, opted_in, created_at, updated_at
         FROM agent_hub_project_mappings
         WHERE hub_project_id = ?
         LIMIT 1",
    )
    .bind(hub_project_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|r| row_to_project_mapping(&r)).transpose()
}

/// 事务内列 portable aliases（永不导出绝对路径）。
async fn list_portable_project_aliases_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    hub_project_ids: &[String],
) -> Result<Vec<AgentHubPortableAliasRow>, AppError> {
    let mut out = Vec::new();
    for hub_id in hub_project_ids {
        let Some(row) = get_project_mapping_by_hub_project_id_on_tx(tx, hub_id).await? else {
            out.push(AgentHubPortableAliasRow {
                kind: "hubProjectId".into(),
                external_id: hub_id.clone(),
                local_id: hub_id.clone(),
            });
            continue;
        };
        out.push(AgentHubPortableAliasRow {
            kind: "hubProjectId".into(),
            external_id: hub_id.clone(),
            local_id: hub_id.clone(),
        });
        if let Some(fp) = row.git_remote_fingerprint {
            if !fp.is_empty() {
                out.push(AgentHubPortableAliasRow {
                    kind: "gitRemoteFingerprint".into(),
                    external_id: fp,
                    local_id: hub_id.clone(),
                });
            }
        }
        if let Some(local) = row.local_workbench_project_id {
            if !local.is_empty() {
                out.push(AgentHubPortableAliasRow {
                    kind: "workbenchProjectId".into(),
                    external_id: local,
                    local_id: hub_id.clone(),
                });
            }
        }
        let _ = row.local_absolute_path;
    }
    out.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then(a.external_id.cmp(&b.external_id))
            .then(a.local_id.cmp(&b.local_id))
    });
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{
        AgentTarget, AssetKind, AssetPolicy, DesiredPresence, NewLogicalAsset, NewRevision,
        NewScopeNode, NewTargetBinding, RevisionId, RevisionOperation, RevisionOriginKind,
        ScopeKind,
    };
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 创建内存库并 ensure_schema。
    ///
    /// Business Logic: 单测隔离，不触碰用户 data.db。
    /// Code Logic: sqlite::memory: + max_connections(1)。
    async fn test_repo() -> AgentHubRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        AgentHubRepo::new(pool)
    }

    /// 插入默认 user scope。
    ///
    /// Business Logic: 多数测试需要挂载 scope。
    /// Code Logic: insert_scope(User)。
    async fn user_scope(repo: &AgentHubRepo) -> ScopeNode {
        repo.insert_scope(NewScopeNode {
            id: Some("scope-user".into()),
            kind: ScopeKind::User,
            hub_project_id: None,
            relative_path: None,
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn ensure_schema_is_idempotent_and_preserves_rows() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "src-tauri".into(),
                display_name: "src-tauri rules".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();

        AgentHubRepo::ensure_schema(&repo.pool()).await.unwrap();
        AgentHubRepo::ensure_schema(&repo.pool()).await.unwrap();

        let again = repo.get_asset(&asset.id).await.unwrap().unwrap();
        assert_eq!(again.id, asset.id);
        assert_eq!(again.logical_key, "src-tauri");
    }

    #[tokio::test]
    async fn asset_and_revision_round_trip() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "src-tauri".into(),
                display_name: "src-tauri rules".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let revision = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Migration,
                origin_target: Some(AgentTarget::Claude),
                origin_replica_id: "device-a".into(),
                payload_hash: Some("a".repeat(64)),
                tree_manifest_hash: None,
                created_at: "2026-07-29T00:00:00Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        assert_eq!(revision.generation, 0);
        assert_eq!(
            repo.get_revision(&revision.id).await.unwrap().unwrap(),
            revision
        );
        let asset_after = repo.get_asset(&asset.id).await.unwrap().unwrap();
        assert_eq!(
            asset_after.current_revision_id.as_ref().map(|r| r.as_str()),
            Some(revision.id.as_str())
        );
    }

    #[tokio::test]
    async fn asset_unique_key_is_scope_kind_namespace_logical_key() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        repo.insert_asset(NewLogicalAsset {
            scope_id: scope.id.clone(),
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "src-tauri".into(),
            display_name: "a".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();

        let err = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "src-tauri".into(),
                display_name: "b".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));

        // 不同 namespace 允许同 logical_key
        let other = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "plugin:foo".into(),
                logical_key: "src-tauri".into(),
                display_name: "plugin copy".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        assert_eq!(other.origin_namespace, "plugin:foo");
    }

    #[tokio::test]
    async fn revision_parents_preserve_order_and_generation() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "root".into(),
                display_name: "root".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let r1 = repo
            .append_revision(NewRevision {
                id: RevisionId::from("rev-1"),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "d1".into(),
                payload_hash: Some("b".repeat(64)),
                tree_manifest_hash: None,
                created_at: "2026-07-29T00:00:01Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        let r2 = repo
            .append_revision(NewRevision {
                id: RevisionId::from("rev-2"),
                asset_lineage_id: asset.id.clone(),
                parents: vec![r1.id.clone()],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "d1".into(),
                payload_hash: Some("c".repeat(64)),
                tree_manifest_hash: None,
                created_at: "2026-07-29T00:00:02Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        assert_eq!(r2.generation, 1);

        let merge = repo
            .append_revision(NewRevision {
                id: RevisionId::from("rev-merge"),
                asset_lineage_id: asset.id.clone(),
                parents: vec![RevisionId::from("rev-2"), RevisionId::from("rev-1")],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Lan,
                origin_target: Some(AgentTarget::Codex),
                origin_replica_id: "d2".into(),
                payload_hash: Some("d".repeat(64)),
                tree_manifest_hash: None,
                created_at: "2026-07-29T00:00:03Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        assert_eq!(merge.generation, 2);
        let loaded = repo.get_revision(&merge.id).await.unwrap().unwrap();
        assert_eq!(
            loaded.parents,
            vec![RevisionId::from("rev-2"), RevisionId::from("rev-1")]
        );
    }

    #[tokio::test]
    async fn append_revision_rejects_missing_parent_and_delete_payload() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "x".into(),
                display_name: "x".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();

        let missing = repo
            .append_revision(NewRevision {
                id: RevisionId::from("rev-bad-parent"),
                asset_lineage_id: asset.id.clone(),
                parents: vec![RevisionId::from("does-not-exist")],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Filesystem,
                origin_target: None,
                origin_replica_id: "d".into(),
                payload_hash: Some("e".repeat(64)),
                tree_manifest_hash: None,
                created_at: "2026-07-29T00:00:00Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(missing, AppError::Validation(_)));

        let delete_payload = repo
            .append_revision(NewRevision {
                id: RevisionId::from("rev-delete-bad"),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Delete,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "d".into(),
                payload_hash: Some("f".repeat(64)),
                tree_manifest_hash: None,
                created_at: "2026-07-29T00:00:00Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(delete_payload, AppError::Validation(_)));

        let ok_delete = repo
            .append_revision(NewRevision {
                id: RevisionId::from("rev-delete-ok"),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Delete,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "d".into(),
                payload_hash: None,
                tree_manifest_hash: None,
                created_at: "2026-07-29T00:00:00Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        assert_eq!(ok_delete.operation, RevisionOperation::Delete);
        assert!(ok_delete.payload_hash.is_none());
    }

    #[tokio::test]
    async fn desired_enabled_false_is_target_local() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Skill,
                origin_namespace: "standalone".into(),
                logical_key: "demo".into(),
                display_name: "demo".into(),
                policy: AssetPolicy::Adapted,
            })
            .await
            .unwrap();

        let claude = repo
            .upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target: AgentTarget::Claude,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: DesiredPresence::Present,
                desired_enabled: false,
            })
            .await
            .unwrap();
        let codex = repo
            .upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target: AgentTarget::Codex,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
            })
            .await
            .unwrap();

        assert!(!claude.desired_enabled);
        assert!(codex.desired_enabled);

        let listed = repo
            .list_target_bindings_for_asset(&asset.id)
            .await
            .unwrap();
        assert_eq!(listed.len(), 2);
        let claude_loaded = listed
            .iter()
            .find(|b| b.target == AgentTarget::Claude)
            .unwrap();
        let codex_loaded = listed
            .iter()
            .find(|b| b.target == AgentTarget::Codex)
            .unwrap();
        assert!(!claude_loaded.desired_enabled);
        assert!(codex_loaded.desired_enabled);
        // 关闭 Claude 不影响 Codex
        assert!(
            repo.get_target_binding(&codex.id)
                .await
                .unwrap()
                .unwrap()
                .desired_enabled
        );
    }

    /// Business Logic: disable 一 target 后其它 binding + canonical revision 不变。
    /// Code Logic: Claude enabled=false；Codex present/enabled 与 head revision 保持。
    #[tokio::test]
    async fn disable_one_target_leaves_other_bindings_and_revision_untouched() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "rev-keep".into(),
                display_name: "rev-keep".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let rev = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "dev".into(),
                payload_hash: Some("aa".repeat(32)),
                tree_manifest_hash: None,
                created_at: "t0".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        let head_before = rev.id.clone();
        repo.upsert_target_binding(NewTargetBinding {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
        })
        .await
        .unwrap();
        repo.upsert_target_binding(NewTargetBinding {
            asset_id: asset.id.clone(),
            target: AgentTarget::Codex,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
        })
        .await
        .unwrap();

        // disable Claude only
        repo.upsert_target_binding(NewTargetBinding {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Present,
            desired_enabled: false,
        })
        .await
        .unwrap();

        let listed = repo
            .list_target_bindings_for_asset(&asset.id)
            .await
            .unwrap();
        let claude = listed
            .iter()
            .find(|b| b.target == AgentTarget::Claude)
            .unwrap();
        let codex = listed
            .iter()
            .find(|b| b.target == AgentTarget::Codex)
            .unwrap();
        assert!(!claude.desired_enabled);
        assert_eq!(claude.desired_presence, DesiredPresence::Present);
        assert!(codex.desired_enabled);
        assert_eq!(codex.desired_presence, DesiredPresence::Present);
        let asset_after = repo.get_asset(&asset.id).await.unwrap().unwrap();
        assert_eq!(
            asset_after.current_revision_id.as_ref().map(|r| r.as_str()),
            Some(head_before.as_str())
        );
        assert!(asset_after.deleted_at.is_none());
    }

    /// Business Logic: delete_everywhere 语义下 fan-out Absent + 一条 delete tombstone。
    /// Code Logic: 两个 present binding → Absent/disabled；append 一次 Delete revision。
    #[tokio::test]
    async fn delete_everywhere_fans_out_absent_and_one_tombstone() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Skill,
                origin_namespace: "standalone".into(),
                logical_key: "everywhere".into(),
                display_name: "everywhere".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let r1 = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "dev".into(),
                payload_hash: Some("bb".repeat(32)),
                tree_manifest_hash: None,
                created_at: "t0".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        for target in [AgentTarget::Claude, AgentTarget::Codex] {
            repo.upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
            })
            .await
            .unwrap();
        }

        // fan-out absent
        for target in [AgentTarget::Claude, AgentTarget::Codex] {
            repo.upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: DesiredPresence::Absent,
                desired_enabled: false,
            })
            .await
            .unwrap();
        }
        // one tombstone
        let tomb = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![r1.id.clone()],
                operation: RevisionOperation::Delete,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "dev".into(),
                payload_hash: None,
                tree_manifest_hash: None,
                created_at: "t1".into(),
                expected_parent_id: Some(r1.id.clone()),
            })
            .await
            .unwrap();
        assert_eq!(tomb.operation, RevisionOperation::Delete);

        let listed = repo
            .list_target_bindings_for_asset(&asset.id)
            .await
            .unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed
            .iter()
            .all(|b| { b.desired_presence == DesiredPresence::Absent && !b.desired_enabled }));
        let asset_after = repo.get_asset(&asset.id).await.unwrap().unwrap();
        assert_eq!(
            asset_after.current_revision_id.as_ref().map(|r| r.as_str()),
            Some(tomb.id.as_str())
        );
        assert!(asset_after.deleted_at.is_some());
    }

    /// Business Logic: 并发写同一 head 时后写必须 CAS 失败，不得静默覆盖。
    /// Code Logic: 两次 append 相同 expected_parent_id，第二次 conflict。
    #[tokio::test]
    async fn append_revision_head_cas_rejects_stale_parent() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "cas".into(),
                display_name: "cas".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let r1 = repo
            .append_revision(NewRevision {
                id: RevisionId::from("cas-r1"),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "d".into(),
                payload_hash: Some("a".repeat(64)),
                tree_manifest_hash: None,
                created_at: "2026-07-29T00:00:00Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        let _r2 = repo
            .append_revision(NewRevision {
                id: RevisionId::from("cas-r2"),
                asset_lineage_id: asset.id.clone(),
                parents: vec![r1.id.clone()],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "d".into(),
                payload_hash: Some("b".repeat(64)),
                tree_manifest_hash: None,
                created_at: "2026-07-29T00:00:01Z".into(),
                expected_parent_id: Some(r1.id.clone()),
            })
            .await
            .unwrap();
        let stale = repo
            .append_revision(NewRevision {
                id: RevisionId::from("cas-r3-stale"),
                asset_lineage_id: asset.id.clone(),
                parents: vec![r1.id.clone()],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "d".into(),
                payload_hash: Some("c".repeat(64)),
                tree_manifest_hash: None,
                created_at: "2026-07-29T00:00:02Z".into(),
                expected_parent_id: Some(r1.id.clone()),
            })
            .await;
        assert!(stale.is_err(), "stale parent must CAS fail");
        let err = stale.unwrap_err();
        assert_eq!(err.code(), "agent_hub_revision_conflict");
        let head = repo.get_asset(&asset.id).await.unwrap().unwrap();
        assert_eq!(head.current_revision_id.unwrap().as_str(), "cas-r2");
    }

    #[tokio::test]
    async fn portable_mcp_revision_round_trip_preserves_secrets_in_cas() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Mcp,
                origin_namespace: "standalone".into(),
                logical_key: "private-api".into(),
                display_name: "private-api".into(),
                policy: AssetPolicy::Adapted,
            })
            .await
            .unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let store = ObjectStore::open(tmp.path()).unwrap();
        let payload = PortableAssetPayload::Mcp(crate::agent_hub::assets::PortableMcpServer {
            key: "private-api".into(),
            transport: crate::agent_hub::assets::McpTransport::Http {
                url: "https://example.invalid/mcp?token=plain-fixture".into(),
                headers: std::collections::BTreeMap::from([(
                    "Authorization".into(),
                    "Bearer plain-fixture".into(),
                )]),
            },
            env: std::collections::BTreeMap::from([("API_TOKEN".into(), "plain-fixture".into())]),
            enabled: true,
            tool_allow: vec![],
            tool_deny: vec![],
            target_extensions: std::collections::BTreeMap::new(),
        });

        let rev = repo
            .append_portable_asset_revision(
                &asset.id,
                &payload,
                &store,
                RevisionOriginKind::Ui,
                None,
                "device-1",
                None,
            )
            .await
            .unwrap();
        assert!(rev.payload_hash.is_some());
        assert!(rev.tree_manifest_hash.is_none());

        let loaded = repo
            .load_portable_asset(&asset.id, &store)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded, payload);

        // CAS blob still holds exact secrets
        let bytes = store
            .get_blob(rev.payload_hash.as_deref().unwrap())
            .await
            .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("plain-fixture"));
        assert!(text.contains("Bearer plain-fixture"));
    }

    #[tokio::test]
    async fn portable_kind_mismatch_rejected_before_write() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Command,
                origin_namespace: "standalone".into(),
                logical_key: "ship".into(),
                display_name: "ship".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let store = ObjectStore::open(tmp.path()).unwrap();
        let skill = PortableAssetPayload::Skill(crate::agent_hub::assets::PortableSkill {
            name: "review".into(),
            description: "d".into(),
            skill_markdown_hash: "a".repeat(64),
            tree_manifest_hash: "b".repeat(64),
            target_extensions: std::collections::BTreeMap::new(),
        });
        let err = repo
            .append_portable_asset_revision(
                &asset.id,
                &skill,
                &store,
                RevisionOriginKind::Ui,
                None,
                "d",
                None,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("kind_mismatch") || err.code().contains("kind_mismatch"));
        // no revision head
        let a = repo.get_asset(&asset.id).await.unwrap().unwrap();
        assert!(a.current_revision_id.is_none());
    }

    #[tokio::test]
    async fn portable_skill_binary_supporting_file_round_trip() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Skill,
                origin_namespace: "standalone".into(),
                logical_key: "review".into(),
                display_name: "review".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let store = ObjectStore::open(tmp.path()).unwrap();

        // binary supporting file exact bytes
        let bin = vec![0u8, 1, 2, 255, 0, 128];
        let bin_obj = store.put_blob(&bin).await.unwrap();
        let md = b"# Review\n\nDo review.\n".to_vec();
        let md_obj = store.put_blob(&md).await.unwrap();
        let tree = crate::agent_hub::object_store::TreeManifest {
            entries: vec![
                crate::agent_hub::object_store::TreeEntry {
                    path: "SKILL.md".into(),
                    blob_hash: md_obj.hash.clone(),
                    entry_type: crate::agent_hub::object_store::TreeEntryType::File,
                    executable: false,
                },
                crate::agent_hub::object_store::TreeEntry {
                    path: "assets/icon.bin".into(),
                    blob_hash: bin_obj.hash.clone(),
                    entry_type: crate::agent_hub::object_store::TreeEntryType::File,
                    executable: false,
                },
            ],
        };
        let tree_obj = store.put_tree(&tree).await.unwrap();

        let payload = PortableAssetPayload::Skill(crate::agent_hub::assets::PortableSkill {
            name: "review".into(),
            description: "Review skill".into(),
            skill_markdown_hash: md_obj.hash.clone(),
            tree_manifest_hash: tree_obj.hash.clone(),
            target_extensions: std::collections::BTreeMap::new(),
        });
        let rev = repo
            .append_portable_asset_revision(
                &asset.id,
                &payload,
                &store,
                RevisionOriginKind::Filesystem,
                Some(AgentTarget::Claude),
                "d",
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            rev.tree_manifest_hash.as_deref(),
            Some(tree_obj.hash.as_str())
        );

        let loaded = repo
            .load_portable_asset(&asset.id, &store)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded, payload);

        // binary unchanged
        let again = store.get_blob(&bin_obj.hash).await.unwrap();
        assert_eq!(again, bin);
    }

    /// 创建 temp ObjectStore。
    async fn test_store() -> (tempfile::TempDir, ObjectStore) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = ObjectStore::open(dir.path()).unwrap();
        (dir, store)
    }

    /// 插入 Skill 组件 revision 并返回 (asset, revision)。
    async fn seed_skill_component(
        repo: &AgentHubRepo,
        store: &ObjectStore,
        scope_id: &str,
        key: &str,
        body: &str,
    ) -> (LogicalAsset, Revision) {
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope_id.into(),
                kind: AssetKind::Skill,
                origin_namespace: "plugin:demo".into(),
                logical_key: key.into(),
                display_name: key.into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let md = store.put_blob(body.as_bytes()).await.unwrap();
        let tree = store
            .put_tree(&crate::agent_hub::object_store::TreeManifest {
                entries: vec![crate::agent_hub::object_store::TreeEntry {
                    path: "SKILL.md".into(),
                    blob_hash: md.hash.clone(),
                    entry_type: crate::agent_hub::object_store::TreeEntryType::File,
                    executable: false,
                }],
            })
            .await
            .unwrap();
        let payload = PortableAssetPayload::Skill(crate::agent_hub::assets::PortableSkill {
            name: key.into(),
            description: "d".into(),
            skill_markdown_hash: md.hash,
            tree_manifest_hash: tree.hash,
            target_extensions: std::collections::BTreeMap::new(),
        });
        let rev = repo
            .append_portable_asset_revision(
                &asset.id,
                &payload,
                store,
                RevisionOriginKind::Ui,
                Some(AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                None,
            )
            .await
            .unwrap();
        (asset, rev)
    }

    #[tokio::test]
    async fn plugin_package_immutable_refs_survive_component_update() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let (_dir, store) = test_store().await;

        let (skill, s1) =
            seed_skill_component(&repo, &store, &scope.id, "review", "skill-v1").await;
        let residual_body = b"runtime-v1";
        let residual_blob = store.put_blob(residual_body).await.unwrap();
        let residual_tree = store
            .put_tree(&crate::agent_hub::object_store::TreeManifest {
                entries: vec![crate::agent_hub::object_store::TreeEntry {
                    path: "index.js".into(),
                    blob_hash: residual_blob.hash.clone(),
                    entry_type: crate::agent_hub::object_store::TreeEntryType::File,
                    executable: false,
                }],
            })
            .await
            .unwrap();

        let plugin = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Plugin,
                origin_namespace: "standalone".into(),
                logical_key: "demo.plugin".into(),
                display_name: "Demo Plugin".into(),
                policy: AssetPolicy::TargetOnly,
            })
            .await
            .unwrap();

        let p1_payload = PluginPackagePayload {
            plugin_id: "demo.plugin".into(),
            name: "Demo".into(),
            version: Some("1".into()),
            description: None,
            source_target: AgentTarget::Claude,
            component_refs: vec![PluginComponentRef {
                kind: AssetKind::Skill,
                asset_id: skill.id.clone(),
                revision_id: s1.id.clone(),
                ownership: ComponentOwnership::PackageOwned,
            }],
            residual_refs: vec![PluginResidualRef {
                target: AgentTarget::Claude,
                residual_kind: ResidualKind::Runtime,
                tree_manifest_hash: residual_tree.hash.clone(),
            }],
            target_extensions: std::collections::BTreeMap::new(),
        };
        let p1 = repo
            .append_plugin_package_revision(
                &plugin.id,
                &p1_payload,
                &store,
                RevisionOriginKind::Ui,
                Some(AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                None,
            )
            .await
            .unwrap();

        // append skill S2
        let s2_md = store.put_blob(b"skill-v2-body").await.unwrap();
        let s2_tree = store
            .put_tree(&crate::agent_hub::object_store::TreeManifest {
                entries: vec![crate::agent_hub::object_store::TreeEntry {
                    path: "SKILL.md".into(),
                    blob_hash: s2_md.hash.clone(),
                    entry_type: crate::agent_hub::object_store::TreeEntryType::File,
                    executable: false,
                }],
            })
            .await
            .unwrap();
        let s2_payload = PortableAssetPayload::Skill(crate::agent_hub::assets::PortableSkill {
            name: "review".into(),
            description: "v2".into(),
            skill_markdown_hash: s2_md.hash,
            tree_manifest_hash: s2_tree.hash,
            target_extensions: std::collections::BTreeMap::new(),
        });
        let s2 = repo
            .append_portable_asset_revision(
                &skill.id,
                &s2_payload,
                &store,
                RevisionOriginKind::Ui,
                Some(AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                Some(s1.id.clone()),
            )
            .await
            .unwrap();

        // P1 still pins S1
        let comps = repo
            .list_plugin_components_for_revision(p1.id.as_str())
            .await
            .unwrap();
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].revision_id, s1.id);
        let loaded_p1 = repo
            .load_plugin_package_revision(&p1.id, &store)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded_p1.component_refs[0].revision_id, s1.id);

        // P2 pins S2
        let p2_payload = PluginPackagePayload {
            plugin_id: "demo.plugin".into(),
            name: "Demo".into(),
            version: Some("2".into()),
            description: None,
            source_target: AgentTarget::Claude,
            component_refs: vec![PluginComponentRef {
                kind: AssetKind::Skill,
                asset_id: skill.id.clone(),
                revision_id: s2.id.clone(),
                ownership: ComponentOwnership::PackageOwned,
            }],
            residual_refs: vec![PluginResidualRef {
                target: AgentTarget::Claude,
                residual_kind: ResidualKind::Runtime,
                tree_manifest_hash: residual_tree.hash.clone(),
            }],
            target_extensions: std::collections::BTreeMap::new(),
        };
        let p2 = repo
            .append_plugin_package_revision(
                &plugin.id,
                &p2_payload,
                &store,
                RevisionOriginKind::Ui,
                Some(AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                Some(p1.id.clone()),
            )
            .await
            .unwrap();
        assert_ne!(p1.id, p2.id);
        let p1_again = repo
            .list_plugin_components_for_revision(p1.id.as_str())
            .await
            .unwrap();
        assert_eq!(p1_again[0].revision_id, s1.id);
        let p2_comps = repo
            .list_plugin_components_for_revision(p2.id.as_str())
            .await
            .unwrap();
        assert_eq!(p2_comps[0].revision_id, s2.id);

        // residual exact bytes
        let restored = store.get_blob(&residual_blob.hash).await.unwrap();
        assert_eq!(restored, residual_body);
    }

    #[tokio::test]
    async fn plugin_package_rejects_missing_component_revision_and_residual_tree() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let (_dir, store) = test_store().await;
        let (skill, s1) = seed_skill_component(&repo, &store, &scope.id, "x", "body").await;
        let plugin = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Plugin,
                origin_namespace: "standalone".into(),
                logical_key: "p".into(),
                display_name: "P".into(),
                policy: AssetPolicy::TargetOnly,
            })
            .await
            .unwrap();

        let missing_rev = PluginPackagePayload {
            plugin_id: "p".into(),
            name: "P".into(),
            version: None,
            description: None,
            source_target: AgentTarget::Claude,
            component_refs: vec![PluginComponentRef {
                kind: AssetKind::Skill,
                asset_id: skill.id.clone(),
                revision_id: RevisionId::from("does-not-exist"),
                ownership: ComponentOwnership::PackageOwned,
            }],
            residual_refs: vec![],
            target_extensions: std::collections::BTreeMap::new(),
        };
        let err = repo
            .append_plugin_package_revision(
                &plugin.id,
                &missing_rev,
                &store,
                RevisionOriginKind::Ui,
                Some(AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                None,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("component_revision_missing"));

        let kind_mismatch = PluginPackagePayload {
            plugin_id: "p".into(),
            name: "P".into(),
            version: None,
            description: None,
            source_target: AgentTarget::Claude,
            component_refs: vec![PluginComponentRef {
                kind: AssetKind::Command,
                asset_id: skill.id.clone(),
                revision_id: s1.id.clone(),
                ownership: ComponentOwnership::PackageOwned,
            }],
            residual_refs: vec![],
            target_extensions: std::collections::BTreeMap::new(),
        };
        let err = repo
            .append_plugin_package_revision(
                &plugin.id,
                &kind_mismatch,
                &store,
                RevisionOriginKind::Ui,
                Some(AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                None,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("component_kind_mismatch"));

        let missing_tree = PluginPackagePayload {
            plugin_id: "p".into(),
            name: "P".into(),
            version: None,
            description: None,
            source_target: AgentTarget::Claude,
            component_refs: vec![PluginComponentRef {
                kind: AssetKind::Skill,
                asset_id: skill.id.clone(),
                revision_id: s1.id.clone(),
                ownership: ComponentOwnership::PackageOwned,
            }],
            residual_refs: vec![PluginResidualRef {
                target: AgentTarget::Claude,
                residual_kind: ResidualKind::Runtime,
                tree_manifest_hash: "f".repeat(64),
            }],
            target_extensions: std::collections::BTreeMap::new(),
        };
        let err = repo
            .append_plugin_package_revision(
                &plugin.id,
                &missing_tree,
                &store,
                RevisionOriginKind::Ui,
                Some(AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                None,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("residual_tree_missing"));
    }

    #[tokio::test]
    async fn snapshot_closure_includes_historical_plugin_component_refs() {
        use crate::agent_hub::snapshot::builder::{
            build_snapshot, SnapshotSelectionMode, SnapshotSelectionRequest,
        };

        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let (_dir, store) = test_store().await;
        let (skill, s1) = seed_skill_component(&repo, &store, &scope.id, "hist", "s1").await;
        let s2_md = store.put_blob(b"s2").await.unwrap();
        let s2_tree = store
            .put_tree(&crate::agent_hub::object_store::TreeManifest {
                entries: vec![crate::agent_hub::object_store::TreeEntry {
                    path: "SKILL.md".into(),
                    blob_hash: s2_md.hash.clone(),
                    entry_type: crate::agent_hub::object_store::TreeEntryType::File,
                    executable: false,
                }],
            })
            .await
            .unwrap();
        let s2 = repo
            .append_portable_asset_revision(
                &skill.id,
                &PortableAssetPayload::Skill(crate::agent_hub::assets::PortableSkill {
                    name: "hist".into(),
                    description: "s2".into(),
                    skill_markdown_hash: s2_md.hash,
                    tree_manifest_hash: s2_tree.hash,
                    target_extensions: std::collections::BTreeMap::new(),
                }),
                &store,
                RevisionOriginKind::Ui,
                Some(AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                Some(s1.id.clone()),
            )
            .await
            .unwrap();

        let residual_blob = store.put_blob(b"res-hist").await.unwrap();
        let residual_tree = store
            .put_tree(&crate::agent_hub::object_store::TreeManifest {
                entries: vec![crate::agent_hub::object_store::TreeEntry {
                    path: "r.js".into(),
                    blob_hash: residual_blob.hash.clone(),
                    entry_type: crate::agent_hub::object_store::TreeEntryType::File,
                    executable: false,
                }],
            })
            .await
            .unwrap();

        let plugin = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Plugin,
                origin_namespace: "standalone".into(),
                logical_key: "hist.plugin".into(),
                display_name: "Hist".into(),
                policy: AssetPolicy::TargetOnly,
            })
            .await
            .unwrap();
        let p1 = repo
            .append_plugin_package_revision(
                &plugin.id,
                &PluginPackagePayload {
                    plugin_id: "hist.plugin".into(),
                    name: "Hist".into(),
                    version: None,
                    description: None,
                    source_target: AgentTarget::Claude,
                    component_refs: vec![PluginComponentRef {
                        kind: AssetKind::Skill,
                        asset_id: skill.id.clone(),
                        revision_id: s1.id.clone(),
                        ownership: ComponentOwnership::PackageOwned,
                    }],
                    residual_refs: vec![PluginResidualRef {
                        target: AgentTarget::Claude,
                        residual_kind: ResidualKind::Runtime,
                        tree_manifest_hash: residual_tree.hash.clone(),
                    }],
                    target_extensions: std::collections::BTreeMap::new(),
                },
                &store,
                RevisionOriginKind::Ui,
                Some(AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                None,
            )
            .await
            .unwrap();
        let _p2 = repo
            .append_plugin_package_revision(
                &plugin.id,
                &PluginPackagePayload {
                    plugin_id: "hist.plugin".into(),
                    name: "Hist".into(),
                    version: Some("2".into()),
                    description: None,
                    source_target: AgentTarget::Claude,
                    component_refs: vec![PluginComponentRef {
                        kind: AssetKind::Skill,
                        asset_id: skill.id.clone(),
                        revision_id: s2.id.clone(),
                        ownership: ComponentOwnership::PackageOwned,
                    }],
                    residual_refs: vec![PluginResidualRef {
                        target: AgentTarget::Claude,
                        residual_kind: ResidualKind::Runtime,
                        tree_manifest_hash: residual_tree.hash.clone(),
                    }],
                    target_extensions: std::collections::BTreeMap::new(),
                },
                &store,
                RevisionOriginKind::Ui,
                Some(AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                Some(p1.id.clone()),
            )
            .await
            .unwrap();

        // skill head is S2; historical P1 still references S1 — snapshot of plugin with history
        // must include S1 payload + residual tree
        let built = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::ExplicitAssets,
                scope_ids: vec![],
                asset_ids: vec![plugin.id.clone()],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
                limits: None,
            },
        )
        .await
        .unwrap();

        let rev_ids: std::collections::BTreeSet<_> = built
            .envelope
            .revisions
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert!(
            rev_ids.contains(s1.id.as_str()),
            "historical component revision S1 must be in snapshot closure"
        );
        assert!(
            rev_ids.contains(s2.id.as_str()),
            "current component revision S2 must be in snapshot closure"
        );
        assert!(
            rev_ids.contains(p1.id.as_str()),
            "historical package P1 must be retained with history"
        );
        let obj_hashes: std::collections::BTreeSet<_> = built
            .envelope
            .objects
            .iter()
            .map(|o| o.hash.as_str())
            .collect();
        assert!(
            obj_hashes.contains(residual_tree.hash.as_str())
                || obj_hashes.contains(residual_blob.hash.as_str()),
            "residual tree/blob must be in snapshot objects"
        );
        // residual blob must be present after tree expansion
        assert!(obj_hashes.contains(residual_blob.hash.as_str()));
    }

    /// Business Logic: import head 必须 CAS 规划时的 expected head，并发本地写不得静默覆盖。
    /// Code Logic: 准备 asset head H0，bundle expected H0→H1；先把 head 改到 L1，再 apply 应 conflict。
    #[tokio::test]
    async fn import_head_cas_rejects_stale_expected_head() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "cas.md".into(),
                display_name: "cas".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let h0 = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "d1".into(),
                payload_hash: Some("a".repeat(64)),
                tree_manifest_hash: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        let l1 = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![h0.id.clone()],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "d1".into(),
                payload_hash: Some("b".repeat(64)),
                tree_manifest_hash: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                expected_parent_id: Some(h0.id.clone()),
            })
            .await
            .unwrap();
        // remote planned head H_remote based on expected H0, but local already L1
        let h_remote = RevisionId::new_v7();
        let now = chrono::Utc::now().to_rfc3339();
        let bundle = ImportBundle {
            scopes: vec![],
            assets: vec![],
            lineages: vec![],
            revisions: vec![ImportRevisionRow {
                id: h_remote.as_str().to_string(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![h0.id.as_str().to_string()],
                generation: 2,
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Lan,
                origin_target: None,
                origin_replica_id: "peer".into(),
                payload_hash: Some("c".repeat(64)),
                tree_manifest_hash: None,
                created_at: now.clone(),
            }],
            variants: vec![],
            conflicts: vec![],
            head_decisions: vec![ImportHeadDecision {
                asset_id: asset.id.clone(),
                expected_head: Some(h0.id.as_str().to_string()),
                new_head: Some(h_remote.as_str().to_string()),
                deleted_at: None,
            }],
            project_mappings: vec![],
            plugin_components: vec![],
            plugin_residuals: vec![],
        };
        let err = repo.commit_import_bundle(bundle).await.unwrap_err();
        assert_eq!(err.ipc_category_code(), "conflict");
        let head = repo.get_asset(&asset.id).await.unwrap().unwrap();
        assert_eq!(
            head.current_revision_id.as_ref().map(|r| r.as_str()),
            Some(l1.id.as_str()),
            "local L1 head must survive failed CAS import"
        );
    }

    /// Business Logic: Conflict plan（new_head=None）也必须 read-set CAS；失败时无 conflict/variant 副作用。
    /// Code Logic: local head L1、plan expected H0 + conflict/variant → conflict，无新 conflict 行。
    #[tokio::test]
    async fn import_conflict_plan_requires_expected_head_cas() {
        let repo = test_repo().await;
        let scope = repo
            .insert_scope(NewScopeNode {
                id: Some("s-cas-c".into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "ns".into(),
                logical_key: "k-cas-c".into(),
                display_name: "n".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let h0 = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "d1".into(),
                payload_hash: Some("a".repeat(64)),
                tree_manifest_hash: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        let l1 = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![h0.id.clone()],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "d1".into(),
                payload_hash: Some("b".repeat(64)),
                tree_manifest_hash: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                expected_parent_id: Some(h0.id.clone()),
            })
            .await
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let conflict_id = uuid::Uuid::new_v4().to_string();
        let remote_rev = RevisionId::new_v7();
        let bundle = ImportBundle {
            scopes: vec![],
            assets: vec![],
            lineages: vec![],
            revisions: vec![ImportRevisionRow {
                id: remote_rev.as_str().to_string(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![h0.id.as_str().to_string()],
                generation: 2,
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Lan,
                origin_target: None,
                origin_replica_id: "peer".into(),
                payload_hash: Some("c".repeat(64)),
                tree_manifest_hash: None,
                created_at: now.clone(),
            }],
            variants: vec![ImportVariantRow {
                asset_id: asset.id.clone(),
                target: crate::agent_hub::models::AgentTarget::Claude,
                revision_id: remote_rev.as_str().to_string(),
            }],
            conflicts: vec![ImportConflictRow {
                id: conflict_id.clone(),
                asset_id: asset.id.clone(),
                target: None,
                base_revision_id: Some(h0.id.as_str().to_string()),
                hub_revision_id: Some(l1.id.as_str().to_string()),
                external_revision_id: Some(remote_rev.as_str().to_string()),
                detail_json: "{}".into(),
                created_at: now.clone(),
            }],
            head_decisions: vec![ImportHeadDecision {
                asset_id: asset.id.clone(),
                expected_head: Some(h0.id.as_str().to_string()),
                new_head: None,
                deleted_at: None,
            }],
            project_mappings: vec![],
            plugin_components: vec![],
            plugin_residuals: vec![],
        };
        let err = repo.commit_import_bundle(bundle).await.unwrap_err();
        assert_eq!(err.ipc_category_code(), "conflict");
        let head = repo.get_asset(&asset.id).await.unwrap().unwrap();
        assert_eq!(
            head.current_revision_id.as_ref().map(|r| r.as_str()),
            Some(l1.id.as_str())
        );
        // no conflict side effect
        let c =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_hub_conflicts WHERE id = ?")
                .bind(&conflict_id)
                .fetch_one(&repo.pool())
                .await
                .unwrap();
        assert_eq!(c, 0, "stale conflict plan must not insert conflicts");
        let v = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_hub_variants WHERE asset_id = ? AND revision_id = ?",
        )
        .bind(&asset.id)
        .bind(remote_rev.as_str())
        .fetch_one(&repo.pool())
        .await
        .unwrap();
        assert_eq!(v, 0, "stale conflict plan must not write variants");
    }

    /// Business Logic: import 必须重建 Standalone ownership 边，删除唯一 plugin 不得 tombstone 独立 skill。
    /// Code Logic: 写 plugin edge ownership=Standalone → import → has_standalone_ref true。
    #[tokio::test]
    async fn import_restores_standalone_component_refs() {
        let repo = test_repo().await;
        let scope = repo
            .insert_scope(NewScopeNode {
                id: Some("s-sa".into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let pkg = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Plugin,
                origin_namespace: "ns".into(),
                logical_key: "plug-sa".into(),
                display_name: "p".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let skill = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Skill,
                origin_namespace: "ns".into(),
                logical_key: "skill-sa".into(),
                display_name: "s".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let pkg_rev = RevisionId::new_v7();
        let skill_rev = RevisionId::new_v7();
        let now = chrono::Utc::now().to_rfc3339();
        let bundle = ImportBundle {
            scopes: vec![],
            assets: vec![],
            lineages: vec![],
            revisions: vec![
                ImportRevisionRow {
                    id: pkg_rev.as_str().to_string(),
                    asset_lineage_id: pkg.id.clone(),
                    parents: vec![],
                    generation: 0,
                    operation: RevisionOperation::Upsert,
                    origin_kind: RevisionOriginKind::Lan,
                    origin_target: None,
                    origin_replica_id: "peer".into(),
                    payload_hash: Some("d".repeat(64)),
                    tree_manifest_hash: None,
                    created_at: now.clone(),
                },
                ImportRevisionRow {
                    id: skill_rev.as_str().to_string(),
                    asset_lineage_id: skill.id.clone(),
                    parents: vec![],
                    generation: 0,
                    operation: RevisionOperation::Upsert,
                    origin_kind: RevisionOriginKind::Lan,
                    origin_target: None,
                    origin_replica_id: "peer".into(),
                    payload_hash: Some("e".repeat(64)),
                    tree_manifest_hash: None,
                    created_at: now.clone(),
                },
            ],
            variants: vec![],
            conflicts: vec![],
            head_decisions: vec![
                ImportHeadDecision {
                    asset_id: pkg.id.clone(),
                    expected_head: None,
                    new_head: Some(pkg_rev.as_str().to_string()),
                    deleted_at: None,
                },
                ImportHeadDecision {
                    asset_id: skill.id.clone(),
                    expected_head: None,
                    new_head: Some(skill_rev.as_str().to_string()),
                    deleted_at: None,
                },
            ],
            project_mappings: vec![],
            plugin_components: vec![ImportPluginComponentEdge {
                package_revision_id: pkg_rev.as_str().to_string(),
                component_kind: AssetKind::Skill,
                component_asset_id: skill.id.clone(),
                component_revision_id: skill_rev.as_str().to_string(),
                ownership: ComponentOwnership::Standalone,
            }],
            plugin_residuals: vec![],
        };
        // assets must exist with expected heads for CAS - we already inserted empty assets;
        // apply expects assets rows exist. head expected None matches.
        repo.commit_import_bundle(bundle).await.unwrap();
        assert!(
            repo.component_has_standalone_ref(&skill.id).await.unwrap(),
            "standalone ownership must rebuild agent_hub_component_standalone_refs"
        );
    }

    /// Business Logic: 同 revision UUID 但 payload 不同必须 conflict，禁止静默 dedupe。
    /// Code Logic: 先 append 真 revision，再 import 同 id 不同 payload → content mismatch。
    #[tokio::test]
    async fn import_revision_id_content_mismatch_conflicts() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "mismatch".into(),
                display_name: "Mismatch".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let rev = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "local".into(),
                payload_hash: Some("a".repeat(64)),
                tree_manifest_hash: None,
                created_at: "2026-07-29T10:00:00Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        let now = "2026-07-29T11:00:00Z".to_string();
        let bundle = ImportBundle {
            scopes: vec![],
            assets: vec![ImportAssetRow {
                id: asset.id.clone(),
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "mismatch".into(),
                display_name: "Mismatch".into(),
                policy: AssetPolicy::Shared,
                deleted_at: None,
            }],
            lineages: vec![(asset.id.clone(), asset.id.clone())],
            revisions: vec![ImportRevisionRow {
                id: rev.id.as_str().to_string(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                generation: 0,
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "local".into(),
                payload_hash: Some("b".repeat(64)), // different payload
                tree_manifest_hash: None,
                created_at: "2026-07-29T10:00:00Z".into(),
            }],
            variants: vec![],
            conflicts: vec![],
            head_decisions: vec![ImportHeadDecision {
                asset_id: asset.id.clone(),
                expected_head: Some(rev.id.as_str().to_string()),
                new_head: None,
                deleted_at: None,
            }],
            project_mappings: vec![],
            plugin_components: vec![],
            plugin_residuals: vec![],
        };
        let err = repo.commit_import_bundle(bundle).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("agent_hub_import_revision_id_content_mismatch"),
            "got {err}"
        );
        let _ = now;
    }

    /// Business Logic: staging GC 必须可推进，避免永远扫前 256 条。
    /// Code Logic: mark cleaned 后 list_committed 不再返回该 transfer。
    #[tokio::test]
    async fn staging_cleanup_mark_advances_keyset() {
        let repo = test_repo().await;
        let now = chrono::Utc::now().to_rfc3339();
        // 直接写 push_requests 行（与 ledger 同 schema）
        for i in 0..3 {
            sqlx::query(
                "INSERT INTO agent_hub_push_requests
                 (source_device_id, client_request_id, transfer_id, selection_hash, snapshot_hash,
                  status, envelope_json, outcome_json, staging_cleaned_at, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, 'committed', '{}', NULL, NULL, ?, ?)",
            )
            .bind("src")
            .bind(format!("req-{i}"))
            .bind(format!("xfer-{i}"))
            .bind("s".repeat(64))
            .bind("h".repeat(64))
            .bind(&now)
            .bind(&now)
            .execute(&repo.pool())
            .await
            .unwrap();
        }
        let first = repo
            .list_committed_transfer_ids_for_cleanup(10)
            .await
            .unwrap();
        assert_eq!(first.len(), 3);
        repo.mark_committed_transfer_staging_cleaned("xfer-0")
            .await
            .unwrap();
        let second = repo
            .list_committed_transfer_ids_for_cleanup(10)
            .await
            .unwrap();
        assert_eq!(second.len(), 2);
        assert!(!second.iter().any(|t| t == "xfer-0"));
    }

    /// Business Logic: outbox worker 需跨 transfer claim queued intents。
    /// Code Logic: insert_on_tx + claim + mark done。
    #[tokio::test]
    async fn lan_projection_intent_claim_and_mark() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let mut asset_ids = Vec::new();
        for index in 1..=2 {
            let asset = repo
                .insert_asset(NewLogicalAsset {
                    scope_id: scope.id.clone(),
                    kind: AssetKind::Instruction,
                    origin_namespace: "test".into(),
                    logical_key: format!("asset-{index}"),
                    display_name: format!("Asset {index}"),
                    policy: AssetPolicy::Shared,
                })
                .await
                .unwrap();
            repo.upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target: AgentTarget::Claude,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
            })
            .await
            .unwrap();
            asset_ids.push(asset.id);
        }
        let mut tx = repo.pool().begin().await.unwrap();
        let n = repo
            .insert_lan_projection_intents_on_tx(&mut tx, "xfer-a", &asset_ids)
            .await
            .unwrap();
        assert_eq!(n, 2);
        tx.commit().await.unwrap();
        let claimed = repo.claim_queued_lan_projection_intents(10).await.unwrap();
        assert_eq!(claimed.len(), 2);
        repo.mark_lan_projection_intent_status("xfer-a", &asset_ids[0], "done")
            .await
            .unwrap();
        let remaining = repo.claim_queued_lan_projection_intents(10).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].1, asset_ids[1]);
    }

    /// Business Logic: 仅导入 canonical、未选择本机 target 时不得伪报 projection queued。
    /// Code Logic: 无 binding 的 asset 不插入 LAN projection intent。
    #[tokio::test]
    async fn lan_projection_intent_skips_assets_without_local_target_binding() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id,
                kind: AssetKind::Instruction,
                origin_namespace: "test".into(),
                logical_key: "canonical-only".into(),
                display_name: "Canonical only".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let mut tx = repo.pool().begin().await.unwrap();
        let inserted = repo
            .insert_lan_projection_intents_on_tx(&mut tx, "xfer-no-target", &[asset.id])
            .await
            .unwrap();
        assert_eq!(inserted, 0);
        tx.commit().await.unwrap();
        assert!(repo
            .list_queued_lan_projection_intents("xfer-no-target")
            .await
            .unwrap()
            .is_empty());
    }

    /// Business Logic: preview token 同时只能由一个幂等请求 claim；同 id 完成后必须回放原结果。
    /// Code Logic: insert → Claimed → Pending → complete → Replay；异 id 返回 conflict。
    #[tokio::test]
    async fn user_instruction_plan_claim_is_atomic_and_replayable() {
        let repo = test_repo().await;
        let now = chrono::Utc::now();
        repo.insert_user_instruction_plan(UserInstructionPlanRecord {
            plan_token: "plan-1".into(),
            owner_fingerprint: "owner".into(),
            expires_at: (now + chrono::Duration::minutes(10)).to_rfc3339(),
            base_revision_id: None,
            inventory_snapshot_hash: "snapshot".into(),
            plan_json: "{}".into(),
            client_request_id: None,
            claimed_at: None,
            consumed_at: None,
            result_json: None,
            created_at: now.to_rfc3339(),
        })
        .await
        .unwrap();
        assert!(matches!(
            repo.claim_user_instruction_plan("plan-1", "request-a")
                .await
                .unwrap(),
            UserInstructionPlanClaim::Claimed(_)
        ));
        assert!(matches!(
            repo.claim_user_instruction_plan("plan-1", "request-a")
                .await
                .unwrap(),
            UserInstructionPlanClaim::Pending
        ));
        let conflict = repo
            .claim_user_instruction_plan("plan-1", "request-b")
            .await
            .unwrap_err();
        assert_eq!(conflict.ipc_category_code(), "conflict");
        repo.complete_user_instruction_plan("plan-1", "request-a", "{\"ok\":true}")
            .await
            .unwrap();
        assert_eq!(
            repo.claim_user_instruction_plan("plan-1", "request-a")
                .await
                .unwrap(),
            UserInstructionPlanClaim::Replay("{\"ok\":true}".into())
        );
    }

    /// Business Logic: ownership 必须是独立持久事实，不能从 materialization hash 猜测。
    /// Code Logic: upsert 后按 asset/target 读取并列出同一记录。
    #[tokio::test]
    async fn user_instruction_ownership_round_trip() {
        let repo = test_repo().await;
        let now = chrono::Utc::now().to_rfc3339();
        let record = UserInstructionOwnershipRecord {
            asset_id: "asset-owned".into(),
            target: AgentTarget::Codex,
            resolved_path: "/tmp/codex/AGENTS.md".into(),
            adopted_hash: Some("a".repeat(64)),
            adopted_revision_id: None,
            adoption_operation: "adopt".into(),
            confirmed_plan_token: "plan-owned".into(),
            created_at: now.clone(),
            updated_at: now,
        };
        repo.upsert_user_instruction_ownership(record.clone())
            .await
            .unwrap();
        assert_eq!(
            repo.get_user_instruction_ownership("asset-owned", AgentTarget::Codex)
                .await
                .unwrap(),
            Some(record.clone())
        );
        assert_eq!(
            repo.list_user_instruction_ownerships("asset-owned")
                .await
                .unwrap(),
            vec![record]
        );
    }
}
