//! storage/agent_hub_repo — Agent Hub canonical SQLite 持久化
//!
//! Business Logic（为什么需要这个模块）:
//!     Multi-CLI Agent Hub 需要可崩溃恢复的 canonical 状态：scope、资产、DAG revision、
//!     target binding、materialization 与 conflict。写入必须经 maintenance gate，旧库升级幂等。
//!
//! Code Logic（这个模块做什么）:
//!     `ensure_schema` 建 13 张表与索引，并对 project_mappings/checkout_bindings/projection_jobs
//!     做 PRAGMA 列升级；`insert_scope/insert_asset/append_revision/upsert_target_binding`、
//!     mapping/binding/materialization/projection_job 写路径走 `with_shared_write_lease`；
//!     revision 多行更新同事务。

use crate::agent_hub::models::{
    AgentHubConflict, AgentTarget, AssetKind, AssetPolicy, DesiredPresence, LogicalAsset,
    Materialization, MaterializationStatus, NewLogicalAsset, NewMaterialization, NewProjectionJob,
    NewRevision, NewScopeNode, NewTargetBinding, ProjectionJob, ProjectionJobState,
    ProjectionPayloadKind, Revision, RevisionId, RevisionOperation, RevisionOriginKind, ScopeKind,
    ScopeNode, TargetBinding,
};
use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::sync::Arc;

/// Agent Hub SQLite 仓库。
///
/// Business Logic（为什么需要这个结构体）:
///     sidecar owner 与单测 fixture 共用同一 schema/语义；生产路径共享 restore 写屏障。
///
/// Code Logic（这个结构体做什么）:
///     持有 SqlitePool + DatabaseMaintenanceGate。
#[derive(Clone)]
pub struct AgentHubRepo {
    pool: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
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
    ///     保存 pool + Arc gate。
    pub fn with_gate(pool: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { pool, gate }
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
            // state CAS：仅 writing/prepared 可提交，防止已 committed/failed 的旧 job 覆盖
            // （attempt 在 writing 路径已 +1，内存 job.attempt 可能落后，故不强制 attempt 等值）
            let result = sqlx::query(
                "UPDATE agent_hub_projection_jobs
                 SET state = ?, last_error = NULL, updated_at = ?
                 WHERE id = ?
                   AND state IN ('writing', 'prepared')",
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
}
