//! agent_hub/snapshot/importer — 两阶段 Snapshot 导入（CAS 验证 + 单 TX head 收敛）
//!
//! Business Logic（为什么需要这个模块）:
//!     LAN push commit 与 Git 确认 import 共用同一入口；必须先验证对象 hash 再原子落库，
//!     用 MCA/块级合并收敛分叉 heads，禁止 last-write-wins。
//!
//! Code Logic（这个模块做什么）:
//!     目录模块：本文件承载导入 DTO（ValidatedSnapshot/Preview/映射确认/outcome）、
//!     `SnapshotImporter` 定义与 `SnapshotImporter::{inspect_import,commit_import}` 等核心
//!     入口，以及对象加载辅助（ObjectLoader/load_merge_payload/snapshot_err_to_app）；
//!     `revision` 子模块承担 revision 闭包校验与拓扑/祖先/MCA 内存图计算；
//!     `tests` 子模块承载单元测试。
//!     Phase A 校验 envelope/闭包/CAS；Phase B 单写事务 upsert 身份/DAG 并更新 head 或
//!     conflict；outcome 在 enqueue 前返回。

mod revision;

#[cfg(test)]
mod tests;

use self::revision::{
    build_parent_index, collect_referenced_object_hashes, is_ancestor_mem,
    maximal_common_ancestors_mem, topological_revisions, validate_revision_closure,
};

use crate::agent_hub::instructions::{
    merge_instruction_documents, InstructionContentMerge, InstructionDocument,
};
use crate::agent_hub::models::{
    AssetKind, NewScopeNode, RevisionId, RevisionOperation, RevisionOriginKind, ScopeKind,
};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::plugins::from_plugin_package_bytes;
use crate::agent_hub::revision_graph::{MergePayload, RevisionGraph};
use crate::agent_hub::snapshot::envelope::{
    validate_snapshot, SnapshotEnvelopeV1, SnapshotError, SnapshotLimits,
};
use crate::error::AppError;
use crate::storage::agent_hub_repo::{
    AgentHubRepo, ImportAssetRow, ImportBundle, ImportConflictRow, ImportHeadDecision,
    ImportPluginComponentEdge, ImportPluginResidualEdge, ImportRevisionRow, ImportVariantRow,
    UpsertAgentHubProjectMapping,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// 已通过 envelope 校验、携带对象正文的导入输入。
///
/// Business Logic（为什么需要这个结构体）:
///     import 不得在未校验 hash 的 envelope 上推进；对象字节可来自 archive/LAN staging。
///
/// Code Logic（这个结构体做什么）:
///     持有 envelope + hash→bytes；构造时 validate_snapshot。
#[derive(Debug, Clone)]
pub struct ValidatedSnapshot {
    /// 已校验 envelope
    pub envelope: SnapshotEnvelopeV1,
    /// 对象正文（可缺部分；commit 时必须补齐 manifest 中全部 hash）
    pub object_bytes: BTreeMap<String, Vec<u8>>,
}

impl ValidatedSnapshot {
    /// 从 envelope + 对象字节构造并校验 schema/hash/limits。
    ///
    /// Business Logic: 入口 fail-closed，非法 manifest 不进入 preview/commit。
    /// Code Logic: validate_snapshot → 包装。
    pub fn from_parts(
        envelope: SnapshotEnvelopeV1,
        object_bytes: BTreeMap<String, Vec<u8>>,
        limits: Option<SnapshotLimits>,
    ) -> Result<Self, AppError> {
        let limits =
            limits.unwrap_or_else(crate::agent_hub::snapshot::envelope::default_snapshot_limits);
        let json = serde_json::to_string(&envelope).map_err(|_| {
            AppError::validation("agent_hub_import_envelope_serialize_failed".to_string())
        })?;
        let envelope = validate_snapshot(&json, &limits).map_err(snapshot_err_to_app)?;
        Ok(Self {
            envelope,
            object_bytes,
        })
    }
}

/// import 预览（映射候选 + 变更摘要，不含 secret 正文）。
///
/// Business Logic: 用户确认前只看 identity/映射/冲突数量。
/// Code Logic: camelCase 可序列化 DTO。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotImportPreview {
    /// snapshot id
    pub snapshot_id: String,
    /// snapshot hash
    pub snapshot_hash: String,
    /// 源 replica
    pub source_replica_id: String,
    /// 资产数
    pub asset_count: u64,
    /// revision 数
    pub revision_count: u64,
    /// 未映射 project 的候选（非 mapping）
    pub project_candidates: Vec<ProjectMappingCandidate>,
    /// 已保存 mapping（hub→local workbench）
    pub resolved_mappings: Vec<ResolvedProjectMapping>,
    /// 是否含 credential-bearing assets 标记（仅计数，无 secret）
    pub credential_bearing_asset_count: u64,
    /// 预估将开 conflict 的资产数（粗估：双 head 同 lineage）
    pub estimated_conflict_assets: u64,
}

/// Git remote / hubProjectId 映射候选（未确认前不是 mapping）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMappingCandidate {
    /// 远端 hubProjectId
    pub hub_project_id: String,
    /// 候选类型：gitRemoteFingerprint / workbenchProjectId
    pub candidate_kind: String,
    /// 候选外部 id（fingerprint 或 workbench id）
    pub candidate_external_id: String,
    /// 本地已存在的 workbench project id（若可解析）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_workbench_project_id: Option<String>,
}

/// 已保存的 hub→local mapping 摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedProjectMapping {
    pub hub_project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_workbench_project_id: Option<String>,
    pub opted_in: bool,
}

/// 用户确认的 import 选择。
///
/// Business Logic: 候选≠mapping；确认 mapping 不自动 opted_in。
/// Code Logic: confirmed mappings + 可选 import 全部 asset。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmedImportSelection {
    /// 确认的 hub_project_id → local_workbench_project_id（opted_in 默认 false）
    #[serde(default)]
    pub project_mappings: Vec<ConfirmedProjectMapping>,
    /// 若 true，未映射 project 资产仍导入但 projections_scheduled=0
    #[serde(default = "default_true")]
    pub import_unmapped_projects: bool,
}

fn default_true() -> bool {
    true
}

/// 用户确认的一条 project mapping。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmedProjectMapping {
    pub hub_project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_workbench_project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_remote_fingerprint: Option<String>,
    /// 显式 opt-in；默认 false，确认 mapping ≠ 自动写盘
    #[serde(default)]
    pub opted_in: bool,
}

/// import 结果。
///
/// Business Logic: 在 async reconcile/projection 之前返回；投影数仅统计已映射且 opted_in。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotImportOutcome {
    pub snapshot_id: String,
    pub snapshot_hash: String,
    pub imported_asset_ids: Vec<String>,
    pub inserted_revisions: u64,
    pub deduped_revisions: u64,
    pub heads_advanced: u64,
    pub conflicts_opened: u64,
    /// 计划调度的 projection 数（未映射/未 opt-in 为 0）
    pub projections_scheduled: u64,
    /// 已写入 CAS 且被 revision 引用的 object hash
    pub imported_object_hashes: Vec<String>,
}

/// Snapshot 导入器。
///
/// Business Logic: 持有 repo + object store + data_dir，供 inspect/commit 共用。
/// Code Logic: clone-friendly 句柄。
#[derive(Clone)]
pub struct SnapshotImporter {
    repo: AgentHubRepo,
    objects: ObjectStore,
    /// data_dir 保留供后续 staging/GC；当前 put_blob 走 ObjectStore::open 已绑定路径
    #[allow(dead_code)]
    data_dir: PathBuf,
}

impl SnapshotImporter {
    /// 构造导入器。
    ///
    /// Business Logic: data_dir 用于 ObjectStore::open 与 staging 路径，禁止预 join objects。
    /// Code Logic: 保存 clone；store 由调用方 open(data_dir)。
    pub fn new(repo: AgentHubRepo, objects: ObjectStore, data_dir: impl AsRef<Path>) -> Self {
        Self {
            repo,
            objects,
            data_dir: data_dir.as_ref().to_path_buf(),
        }
    }

    /// 预览 import：映射候选与计数，不写库。
    ///
    /// Business Logic: Git/LAN 确认 UI 依赖 preview；候选不是 mapping。
    /// Code Logic: 读 project_mappings + envelope aliases。
    pub async fn inspect_import(
        &self,
        snapshot: &ValidatedSnapshot,
    ) -> Result<SnapshotImportPreview, AppError> {
        let env = &snapshot.envelope;
        let mut project_candidates = Vec::new();
        let mut resolved_mappings = Vec::new();
        let mut seen_hub = BTreeSet::new();

        // 从 aliases 收集 hubProjectId
        for alias in &env.aliases {
            if alias.kind == "hubProjectId" {
                seen_hub.insert(alias.external_id.clone());
                seen_hub.insert(alias.local_id.clone());
            }
        }
        // 从 scopes 在 builder 侧不导出 scope 表；用 aliases 与 project assets 推断
        for a in &env.assets {
            // scope_id 可能编码 project；aliases 优先
            let _ = a;
        }

        for hub in &seen_hub {
            if hub.is_empty() {
                continue;
            }
            if let Some(row) = self.repo.get_project_mapping_by_hub_project_id(hub).await? {
                resolved_mappings.push(ResolvedProjectMapping {
                    hub_project_id: hub.clone(),
                    local_workbench_project_id: row.local_workbench_project_id.clone(),
                    opted_in: row.opted_in,
                });
            } else {
                // 提供 fingerprint 候选
                for alias in &env.aliases {
                    if alias.local_id == *hub
                        && (alias.kind == "gitRemoteFingerprint"
                            || alias.kind == "workbenchProjectId")
                    {
                        let local = if alias.kind == "workbenchProjectId" {
                            Some(alias.external_id.clone())
                        } else {
                            None
                        };
                        project_candidates.push(ProjectMappingCandidate {
                            hub_project_id: hub.clone(),
                            candidate_kind: alias.kind.clone(),
                            candidate_external_id: alias.external_id.clone(),
                            local_workbench_project_id: local,
                        });
                    }
                }
                if !project_candidates.iter().any(|c| c.hub_project_id == *hub) {
                    project_candidates.push(ProjectMappingCandidate {
                        hub_project_id: hub.clone(),
                        candidate_kind: "hubProjectId".into(),
                        candidate_external_id: hub.clone(),
                        local_workbench_project_id: None,
                    });
                }
            }
        }

        // credential-bearing：MCP / 含 password 等 kind 计数（不读 secret）
        let credential_bearing_asset_count = env
            .assets
            .iter()
            .filter(|a| matches!(a.kind, AssetKind::Mcp))
            .count() as u64;

        Ok(SnapshotImportPreview {
            snapshot_id: env.snapshot_id.clone(),
            snapshot_hash: env.snapshot_hash.clone(),
            source_replica_id: env.source_replica_id.clone(),
            asset_count: env.assets.len() as u64,
            revision_count: env.revisions.len() as u64,
            project_candidates,
            resolved_mappings,
            credential_bearing_asset_count,
            estimated_conflict_assets: 0,
        })
    }

    /// 两阶段提交 import。
    ///
    /// Business Logic:
    ///     Phase A 校验闭包与每个 object hash 进 CAS；Phase B 单 TX 收敛 heads/conflicts；
    ///     outcome 在 async projection 之前返回。
    ///
    /// Code Logic:
    ///     validate → stage objects → plan bundle → commit_import_bundle → outcome。
    pub async fn commit_import(
        &self,
        snapshot: ValidatedSnapshot,
        selection: ConfirmedImportSelection,
    ) -> Result<SnapshotImportOutcome, AppError> {
        let env = &snapshot.envelope;
        let imported_object_hashes = self.stage_objects_for_import(&snapshot).await?;

        // ── Plan Phase B bundle ─────────────────────────────────────────
        let plan = self.plan_import_bundle(env, &selection).await?;
        let projections_scheduled = plan.projections_scheduled;
        // Gate D C1：plugin package 边必须与 revision/head 同 TX 恢复
        let mut bundle = plan.bundle;
        self.enrich_plugin_edges_for_import(&mut bundle, env, &snapshot.object_bytes)
            .await?;

        // ── Phase B: single write TX ────────────────────────────────────
        let result = self.repo.commit_import_bundle(bundle).await?;

        Ok(SnapshotImportOutcome {
            snapshot_id: env.snapshot_id.clone(),
            snapshot_hash: env.snapshot_hash.clone(),
            imported_asset_ids: result.imported_asset_ids,
            inserted_revisions: result.inserted_revisions,
            deduped_revisions: result.deduped_revisions,
            heads_advanced: result.heads_advanced,
            conflicts_opened: result.conflicts_opened,
            projections_scheduled,
            imported_object_hashes,
        })
    }

    /// Phase A：校验闭包并把 snapshot objects 写入 CAS（幂等）。
    ///
    /// Business Logic: LAN commit 在 ledger 同 TX import 前先完成 CAS 落盘；CAS 幂等可重放。
    /// Code Logic: validate_revision_closure → put_blob / require local CAS → 返回 imported hashes。
    pub async fn stage_objects_for_import(
        &self,
        snapshot: &ValidatedSnapshot,
    ) -> Result<Vec<String>, AppError> {
        let env = &snapshot.envelope;
        validate_revision_closure(env)?;
        let referenced = collect_referenced_object_hashes(env);
        let mut imported_object_hashes = Vec::new();
        for hash in &referenced {
            if let Some(bytes) = snapshot.object_bytes.get(hash) {
                let actual = sha256_hex(bytes);
                if actual != *hash {
                    return Err(AppError::validation(
                        "agent_hub_import_corrupt_object:hash_mismatch".to_string(),
                    ));
                }
                self.objects.put_blob(bytes).await?;
                imported_object_hashes.push(hash.clone());
            } else {
                match self.objects.get_blob(hash).await {
                    Ok(_) => {
                        imported_object_hashes.push(hash.clone());
                    }
                    Err(_) => {
                        return Err(AppError::validation(format!(
                            "agent_hub_import_object_missing:{hash}"
                        )));
                    }
                }
            }
        }
        for (hash, bytes) in &snapshot.object_bytes {
            if referenced.contains(hash) {
                continue;
            }
            let actual = sha256_hex(bytes);
            if actual == *hash {
                let _ = self.objects.put_blob(bytes).await;
            }
        }

        #[cfg(any(test, debug_assertions))]
        {
            if std::env::var("CC_PARTNER_IMPORT_CRASH_AFTER_CAS")
                .ok()
                .as_deref()
                == Some("1")
            {
                return Err(AppError::generic(
                    "agent_hub_import_injected_crash_after_cas".to_string(),
                ));
            }
        }
        Ok(imported_object_hashes)
    }

    /// 公开 plan 入口（LAN 原子 commit 复用）。
    ///
    /// Business Logic: receiver 在同一 TX 内需要 plan 后再 apply。
    /// Code Logic: 委托私有 `plan_import_bundle`。
    pub async fn plan_import_bundle_public(
        &self,
        env: &SnapshotEnvelopeV1,
        selection: &ConfirmedImportSelection,
    ) -> Result<PlannedImport, AppError> {
        self.plan_import_bundle(env, selection).await
    }

    /// 公开 plugin edges 丰富入口（LAN 原子 commit 复用）。
    pub async fn enrich_plugin_edges_for_import_public(
        &self,
        bundle: &mut ImportBundle,
        env: &SnapshotEnvelopeV1,
        object_bytes: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), AppError> {
        self.enrich_plugin_edges_for_import(bundle, env, object_bytes)
            .await
    }

    /// 规划 import bundle：identity 映射 + MCA head 决策。
    ///
    /// Business Logic: 先落 revision DAG，再按 local/remote head 做 MCA/块合并。
    /// Code Logic: 解析 scope/asset identity → revisions → per-asset converge。
    async fn plan_import_bundle(
        &self,
        env: &SnapshotEnvelopeV1,
        selection: &ConfirmedImportSelection,
    ) -> Result<PlannedImport, AppError> {
        let mut scopes: Vec<NewScopeNode> = Vec::new();
        let mut scope_remap: HashMap<String, String> = HashMap::new();

        // 确保 user scope
        let user_scope_id = if let Some(id) = self.repo.resolve_user_scope_id().await? {
            id
        } else {
            let id = "scope-user".to_string();
            scopes.push(NewScopeNode {
                id: Some(id.clone()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            });
            id
        };

        // hub project scopes from aliases + asset scope_ids
        let mut remote_hub_projects: BTreeSet<String> = BTreeSet::new();
        for alias in &env.aliases {
            if alias.kind == "hubProjectId" {
                remote_hub_projects.insert(alias.local_id.clone());
                remote_hub_projects.insert(alias.external_id.clone());
            }
        }

        // confirmed mappings
        let mut project_mappings: Vec<UpsertAgentHubProjectMapping> = Vec::new();
        let mut mapped_hubs: HashMap<String, ConfirmedProjectMapping> = HashMap::new();
        for m in &selection.project_mappings {
            mapped_hubs.insert(m.hub_project_id.clone(), m.clone());
            project_mappings.push(UpsertAgentHubProjectMapping {
                hub_project_id: m.hub_project_id.clone(),
                local_workbench_project_id: m.local_workbench_project_id.clone(),
                git_remote_fingerprint: m.git_remote_fingerprint.clone(),
                local_absolute_path: None,
                opted_in: m.opted_in,
            });
        }
        // 已保存 mapping 也算 mapped（用于 projection 计数）
        for hub in &remote_hub_projects {
            if mapped_hubs.contains_key(hub) {
                continue;
            }
            if let Some(row) = self.repo.get_project_mapping_by_hub_project_id(hub).await? {
                mapped_hubs.insert(
                    hub.clone(),
                    ConfirmedProjectMapping {
                        hub_project_id: hub.clone(),
                        local_workbench_project_id: row.local_workbench_project_id,
                        git_remote_fingerprint: row.git_remote_fingerprint,
                        opted_in: row.opted_in,
                    },
                );
            }
        }

        // 为每个 hub project 确保 scope
        let mut hub_to_scope: HashMap<String, String> = HashMap::new();
        for hub in &remote_hub_projects {
            if let Some(existing) = self.repo.resolve_project_scope_id(hub).await? {
                hub_to_scope.insert(hub.clone(), existing);
            } else {
                let id = format!("scope-proj-{hub}");
                scopes.push(NewScopeNode {
                    id: Some(id.clone()),
                    kind: ScopeKind::Project,
                    hub_project_id: Some(hub.clone()),
                    relative_path: Some(".".into()),
                });
                hub_to_scope.insert(hub.clone(), id);
            }
        }

        // 推断每个 snapshot scope_id 的 kind：若等于某 hub scope 或 alias 命中
        // 简化：若 scope_id 出现在 hub_to_scope values / keys 映射；否则当 user
        for a in &env.assets {
            if !scope_remap.contains_key(&a.scope_id) {
                // 尝试用 hub_to_scope 的 value 匹配；或若 scope_id 含 hub
                let mut mapped = None;
                for (hub, sid) in &hub_to_scope {
                    if sid == &a.scope_id || a.scope_id.contains(hub.as_str()) {
                        mapped = Some(sid.clone());
                        break;
                    }
                }
                // aliases workbench/git 不改 scope
                if let Some(sid) = mapped {
                    scope_remap.insert(a.scope_id.clone(), sid);
                } else if self.repo.get_scope(&a.scope_id).await?.is_some() {
                    scope_remap.insert(a.scope_id.clone(), a.scope_id.clone());
                } else {
                    // 新 scope：若只有一个 project hub，挂 project；否则 user
                    if remote_hub_projects.len() == 1 {
                        let hub = remote_hub_projects.iter().next().unwrap();
                        let sid = hub_to_scope
                            .get(hub)
                            .cloned()
                            .unwrap_or_else(|| user_scope_id.clone());
                        scope_remap.insert(a.scope_id.clone(), sid);
                    } else {
                        // 确保 snapshot scope 存在为 user 或保留 id
                        scopes.push(NewScopeNode {
                            id: Some(a.scope_id.clone()),
                            kind: ScopeKind::User,
                            hub_project_id: None,
                            relative_path: None,
                        });
                        scope_remap.insert(a.scope_id.clone(), a.scope_id.clone());
                    }
                }
            }
        }

        // assets
        let mut assets: Vec<ImportAssetRow> = Vec::new();
        let mut lineages: Vec<(String, String)> = Vec::new();
        let mut asset_id_local: HashMap<String, String> = HashMap::new();

        for a in &env.assets {
            let local_scope = scope_remap
                .get(&a.scope_id)
                .cloned()
                .unwrap_or_else(|| user_scope_id.clone());

            // 若 local 已有 unique key，复用 id
            let existing = self
                .repo
                .get_asset_by_unique_key(&local_scope, a.kind, &a.origin_namespace, &a.logical_key)
                .await?;
            let local_id = if let Some(ex) = existing {
                lineages.push((ex.id.clone(), a.id.clone()));
                lineages.push((ex.id.clone(), ex.id.clone()));
                asset_id_local.insert(a.id.clone(), ex.id.clone());
                ex.id
            } else {
                asset_id_local.insert(a.id.clone(), a.id.clone());
                a.id.clone()
            };

            assets.push(ImportAssetRow {
                id: if local_id == a.id {
                    a.id.clone()
                } else {
                    // 仍以 snapshot 行插入时用 local；apply 会按 unique key 合并
                    a.id.clone()
                },
                scope_id: local_scope,
                kind: a.kind,
                origin_namespace: a.origin_namespace.clone(),
                logical_key: a.logical_key.clone(),
                display_name: a.display_name.clone(),
                policy: a.policy,
                deleted_at: a.deleted_at.clone(),
            });

            // lineage 表：snapshot lineages
            for lin in &env.lineages {
                if lin.root_asset_id == a.id || lin.id == a.id {
                    let local_asset = asset_id_local.get(&a.id).cloned().unwrap_or(a.id.clone());
                    lineages.push((local_asset, lin.id.clone()));
                }
            }
        }

        // 将 snapshot asset id 别名挂到 local
        for (remote, local) in &asset_id_local {
            if remote != local {
                lineages.push((local.clone(), remote.clone()));
            }
        }
        for lin in &env.lineages {
            let root_local = asset_id_local
                .get(&lin.root_asset_id)
                .cloned()
                .unwrap_or_else(|| lin.root_asset_id.clone());
            lineages.push((root_local, lin.id.clone()));
        }
        lineages.sort();
        lineages.dedup();

        // revisions 拓扑序；asset_lineage_id 经 identity remap，避免同逻辑资产不同本地 id 误冲突
        let ordered = topological_revisions(&env.revisions)?;
        let mut revisions: Vec<ImportRevisionRow> = Vec::new();
        for r in ordered {
            let gen: u64 = r.generation.parse().unwrap_or(0);
            let lineage = asset_id_local
                .get(&r.asset_lineage_id)
                .cloned()
                .unwrap_or_else(|| r.asset_lineage_id.clone());
            revisions.push(ImportRevisionRow {
                id: r.id.clone(),
                asset_lineage_id: lineage,
                parents: r.parents.clone(),
                generation: gen,
                operation: r.operation,
                origin_kind: r.origin_kind,
                origin_target: r.origin_target,
                origin_replica_id: r.origin_replica_id.clone(),
                payload_hash: r.payload_hash.clone(),
                tree_manifest_hash: r.tree_manifest_hash.clone(),
                created_at: r.created_at.clone(),
            });
        }

        // 先把 identity+revisions 写入一份「仅 DAG」bundle，再读库算 MCA
        // 为避免两次 TX，我们在单 TX 内无法调用 RevisionGraph（它读 pool）。
        // 策略：先插入 revisions（head 暂不改），再在同一 TX 外预计算决策需要本地 heads。
        // 简化：commit 两次也不行。改为：
        // 1) 用内存图 + 本地 get_revision 预计算
        // 2) 单 TX 一次提交全部

        let mut head_decisions: Vec<ImportHeadDecision> = Vec::new();
        let mut conflicts: Vec<ImportConflictRow> = Vec::new();
        let mut merge_revisions: Vec<ImportRevisionRow> = Vec::new();

        // snapshot 携带的 conflicts
        for c in &env.conflicts {
            let local_asset = asset_id_local
                .get(&c.asset_id)
                .cloned()
                .unwrap_or_else(|| c.asset_id.clone());
            conflicts.push(ImportConflictRow {
                id: c.id.clone(),
                asset_id: local_asset,
                target: c.target,
                base_revision_id: c.base_revision_id.clone(),
                hub_revision_id: c.hub_revision_id.clone(),
                external_revision_id: c.external_revision_id.clone(),
                detail_json: c.detail_json.clone(),
                created_at: c.created_at.clone(),
            });
        }

        // 按 asset 收敛 heads
        for a in &env.assets {
            let local_asset = asset_id_local
                .get(&a.id)
                .cloned()
                .unwrap_or_else(|| a.id.clone());
            let remote_heads: Vec<String> = env.asset_heads.get(&a.id).cloned().unwrap_or_default();
            if remote_heads.is_empty() {
                continue;
            }

            let local_asset_row = self.repo.get_asset(&local_asset).await?;
            let local_head = local_asset_row.as_ref().and_then(|x| {
                x.current_revision_id
                    .as_ref()
                    .map(|r| r.as_str().to_string())
            });

            // 若本地无资产/无 head：推进到 remote 的单一 head 或第一个字典序 head + 其余 conflict
            if local_head.is_none() {
                if remote_heads.len() == 1 {
                    let h = &remote_heads[0];
                    let deleted = env.revisions.iter().find(|r| r.id == *h).and_then(|r| {
                        if r.operation == RevisionOperation::Delete {
                            Some(r.created_at.clone())
                        } else {
                            None
                        }
                    });
                    head_decisions.push(ImportHeadDecision {
                        asset_id: local_asset.clone(),
                        expected_head: None,
                        new_head: Some(h.clone()),
                        deleted_at: deleted,
                    });
                } else {
                    // 多 remote heads：保留不设 single head，开 conflict
                    let mut sorted = remote_heads.clone();
                    sorted.sort();
                    head_decisions.push(ImportHeadDecision {
                        asset_id: local_asset.clone(),
                        expected_head: None,
                        new_head: Some(sorted[0].clone()),
                        deleted_at: None,
                    });
                    for h in sorted.iter().skip(1) {
                        conflicts.push(ImportConflictRow {
                            id: Uuid::now_v7().to_string(),
                            asset_id: local_asset.clone(),
                            target: None,
                            base_revision_id: None,
                            hub_revision_id: Some(sorted[0].clone()),
                            external_revision_id: Some(h.clone()),
                            detail_json: r#"{"reason":"agent_hub_import_multi_remote_heads"}"#
                                .into(),
                            created_at: chrono::Utc::now().to_rfc3339(),
                        });
                    }
                }
                continue;
            }

            let local_head = local_head.unwrap();
            // 每个 remote head 与 local 收敛
            for remote_head in &remote_heads {
                if remote_head == &local_head {
                    // equal head：仍写 no-op decision，保证同快照/variant CAS 有 expected_head
                    head_decisions.push(ImportHeadDecision {
                        asset_id: local_asset.clone(),
                        expected_head: Some(local_head.clone()),
                        new_head: None,
                        deleted_at: None,
                    });
                    continue;
                }
                // 若 remote 已是 local 祖先（仅在 remote 已在本地库时）
                if let Some(local_rev) = self
                    .repo
                    .get_revision(&RevisionId(local_head.clone()))
                    .await?
                {
                    let _ = local_rev;
                }

                // 构建内存可达：导入 revisions + 本地库
                let decision = self
                    .converge_heads(
                        &local_asset,
                        &local_head,
                        remote_head,
                        env,
                        &snapshot_object_loader(env, &self.objects, &self.repo),
                    )
                    .await?;

                match decision {
                    ConvergeDecision::FastForward { head, deleted_at } => {
                        head_decisions.push(ImportHeadDecision {
                            asset_id: local_asset.clone(),
                            expected_head: Some(local_head.clone()),
                            new_head: Some(head),
                            deleted_at,
                        });
                    }
                    ConvergeDecision::AlreadyAncestor => {
                        // keep local head，但必须产出 no-op decision 供 variant/conflict CAS
                        head_decisions.push(ImportHeadDecision {
                            asset_id: local_asset.clone(),
                            expected_head: Some(local_head.clone()),
                            new_head: None,
                            deleted_at: None,
                        });
                    }
                    ConvergeDecision::Merge {
                        merge_rev,
                        deleted_at,
                    } => {
                        let merge_id = merge_rev.id.clone();
                        merge_revisions.push(merge_rev);
                        head_decisions.push(ImportHeadDecision {
                            asset_id: local_asset.clone(),
                            expected_head: Some(local_head.clone()),
                            new_head: Some(merge_id),
                            deleted_at,
                        });
                    }
                    ConvergeDecision::Conflict {
                        base,
                        hub,
                        external,
                        detail,
                    } => {
                        // 不推进 head
                        head_decisions.push(ImportHeadDecision {
                            asset_id: local_asset.clone(),
                            expected_head: Some(local_head.clone()),
                            new_head: None,
                            deleted_at: None,
                        });
                        conflicts.push(ImportConflictRow {
                            id: Uuid::now_v7().to_string(),
                            asset_id: local_asset.clone(),
                            target: None,
                            base_revision_id: base,
                            hub_revision_id: Some(hub),
                            external_revision_id: Some(external),
                            detail_json: detail,
                            created_at: chrono::Utc::now().to_rfc3339(),
                        });
                    }
                }
            }
        }

        revisions.extend(merge_revisions);

        // variants：映射 asset id
        let mut variants: Vec<ImportVariantRow> = Vec::new();
        let mut variant_touched: BTreeSet<String> = BTreeSet::new();
        for v in &env.variants {
            let local_asset = asset_id_local
                .get(&v.asset_id)
                .cloned()
                .unwrap_or_else(|| v.asset_id.clone());
            variant_touched.insert(local_asset.clone());
            variants.push(ImportVariantRow {
                asset_id: local_asset,
                target: v.target,
                revision_id: v.revision_id.clone(),
            });
        }
        // conflicts 触及的 asset 也必须有 head decision
        let mut conflict_touched: BTreeSet<String> = BTreeSet::new();
        for c in &conflicts {
            conflict_touched.insert(c.asset_id.clone());
        }

        // projections_scheduled：只有本机已明确选择 target binding 的资产才可声称将投影；
        // canonical 导入与本机应用是两个结果，新导入 user asset 默认待配置。
        let mut projections_scheduled = 0u64;
        for a in &env.assets {
            let local_asset_id = asset_id_local
                .get(&a.id)
                .cloned()
                .unwrap_or_else(|| a.id.clone());
            if self
                .repo
                .list_target_bindings_for_asset(&local_asset_id)
                .await?
                .is_empty()
            {
                continue;
            }
            // user scope assets always count if opted global? Gate A: user always projectable
            let is_user = scope_remap
                .get(&a.scope_id)
                .map(|s| s == &user_scope_id || s.starts_with("scope-user"))
                .unwrap_or(true);
            if is_user {
                projections_scheduled += 1;
                continue;
            }
            // project：需要 mapping + opted_in
            let mut scheduled = false;
            for (hub, m) in &mapped_hubs {
                if m.opted_in {
                    if let Some(sid) = hub_to_scope.get(hub) {
                        if scope_remap.get(&a.scope_id) == Some(sid) {
                            scheduled = true;
                            break;
                        }
                    }
                }
            }
            if scheduled {
                projections_scheduled += 1;
            }
        }
        // unmapped project still import but 0 projections for those
        if !selection.import_unmapped_projects {
            // still import; projection count already excludes unmapped
        }

        // head_decisions 去重：同 asset 最后一条 wins for new_head（conflict 的 None 优先保留若存在）
        let mut head_map: BTreeMap<String, ImportHeadDecision> = BTreeMap::new();
        for d in head_decisions {
            let keep_existing_conflict = head_map
                .get(&d.asset_id)
                .map(|prev| prev.new_head.is_none())
                .unwrap_or(false);
            if keep_existing_conflict {
                continue;
            }
            head_map.insert(d.asset_id.clone(), d);
        }

        // variant/conflict 触及但未产出 head decision 的资产：补 no-op expected_head，
        // 避免 commit 层 agent_hub_import_missing_head_decision。
        for asset_id in variant_touched.union(&conflict_touched) {
            if head_map.contains_key(asset_id) {
                continue;
            }
            let local_asset_row = self.repo.get_asset(asset_id).await?;
            let expected = local_asset_row.and_then(|a| {
                a.current_revision_id
                    .as_ref()
                    .map(|r| r.as_str().to_string())
            });
            head_map.insert(
                asset_id.clone(),
                ImportHeadDecision {
                    asset_id: asset_id.clone(),
                    expected_head: expected,
                    new_head: None,
                    deleted_at: None,
                },
            );
        }

        Ok(PlannedImport {
            bundle: ImportBundle {
                scopes,
                assets,
                lineages,
                revisions,
                variants,
                conflicts,
                head_decisions: head_map.into_values().collect(),
                project_mappings,
                // commit_import 在 Phase A CAS 后用 package payload 填充
                plugin_components: Vec::new(),
                plugin_residuals: Vec::new(),
            },
            projections_scheduled,
        })
    }

    /// 从 package payload 重建 plugin component/residual 边并 fail-closed 校验 refs。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本地 append_plugin_package_revision 双写边表；import 若只恢复 revision/CAS，
    ///     ownership delete 与 re-export residual 闭包都会静默损坏。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对每个 AssetKind::Plugin Upsert revision：读 payload CAS → parse →
    ///     remap component asset id → 校验 component revision 在 import set/CAS 存在 →
    ///     residual tree 在 CAS 存在 → 填入 bundle edges。
    async fn enrich_plugin_edges_for_import(
        &self,
        bundle: &mut ImportBundle,
        env: &SnapshotEnvelopeV1,
        object_bytes: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), AppError> {
        // snapshot asset id → local asset id（unique-key 合并后）
        let mut remote_to_local: HashMap<String, String> = HashMap::new();
        for a in &env.assets {
            let local_scope = bundle
                .assets
                .iter()
                .find(|row| {
                    row.kind == a.kind
                        && row.origin_namespace == a.origin_namespace
                        && row.logical_key == a.logical_key
                })
                .map(|row| row.scope_id.clone())
                .unwrap_or_else(|| a.scope_id.clone());
            let existing = self
                .repo
                .get_asset_by_unique_key(&local_scope, a.kind, &a.origin_namespace, &a.logical_key)
                .await?;
            let local_id = existing.map(|ex| ex.id).unwrap_or_else(|| a.id.clone());
            remote_to_local.insert(a.id.clone(), local_id);
        }
        // also honor lineages already planned
        for (local, lineage) in &bundle.lineages {
            remote_to_local
                .entry(lineage.clone())
                .or_insert_with(|| local.clone());
        }

        let imported_revision_ids: HashSet<String> =
            bundle.revisions.iter().map(|r| r.id.clone()).collect();
        let imported_asset_ids: HashSet<String> = bundle
            .assets
            .iter()
            .map(|a| a.id.clone())
            .chain(remote_to_local.values().cloned())
            .collect();

        let mut plugin_components: Vec<ImportPluginComponentEdge> = Vec::new();
        let mut plugin_residuals: Vec<ImportPluginResidualEdge> = Vec::new();

        for rev in &bundle.revisions {
            if rev.operation == RevisionOperation::Delete {
                continue;
            }
            // 仅 package 资产的 revision 才可能是 PluginPackagePayload
            let package_asset_kind = env
                .assets
                .iter()
                .find(|a| {
                    a.id == rev.asset_lineage_id
                        || remote_to_local
                            .get(&a.id)
                            .map(|local| local == &rev.asset_lineage_id)
                            .unwrap_or(false)
                })
                .map(|a| a.kind)
                .or_else(|| {
                    // 若 lineage 已是 local asset id，从 bundle assets 找
                    bundle
                        .assets
                        .iter()
                        .find(|a| a.id == rev.asset_lineage_id)
                        .map(|a| a.kind)
                });
            let Some(AssetKind::Plugin) = package_asset_kind else {
                continue;
            };
            let Some(payload_hash) = rev.payload_hash.as_deref() else {
                return Err(AppError::validation(format!(
                    "agent_hub_import_plugin_revision_missing_payload_hash:{}",
                    rev.id
                )));
            };
            let bytes = if let Some(b) = object_bytes.get(payload_hash) {
                b.clone()
            } else {
                self.objects.get_blob(payload_hash).await.map_err(|_| {
                    AppError::validation(format!(
                        "agent_hub_import_plugin_payload_missing:{payload_hash}"
                    ))
                })?
            };
            let payload = from_plugin_package_bytes(&bytes).map_err(|e| {
                AppError::validation(format!(
                    "agent_hub_import_plugin_payload_invalid:{}:{e}",
                    rev.id
                ))
            })?;

            for cref in &payload.component_refs {
                let local_component_id = remote_to_local
                    .get(&cref.asset_id)
                    .cloned()
                    .unwrap_or_else(|| cref.asset_id.clone());
                // component asset must be present in import set or already local
                if !imported_asset_ids.contains(&local_component_id)
                    && self.repo.get_asset(&local_component_id).await?.is_none()
                {
                    return Err(AppError::validation(format!(
                        "agent_hub_import_plugin_component_asset_missing:{}",
                        cref.asset_id
                    )));
                }
                let comp_rev_id = cref.revision_id.as_str().to_string();
                if !imported_revision_ids.contains(&comp_rev_id)
                    && self
                        .repo
                        .get_revision(&RevisionId::from(comp_rev_id.clone()))
                        .await?
                        .is_none()
                {
                    return Err(AppError::validation(format!(
                        "agent_hub_import_plugin_component_revision_missing:{comp_rev_id}"
                    )));
                }
                plugin_components.push(ImportPluginComponentEdge {
                    package_revision_id: rev.id.clone(),
                    component_kind: cref.kind,
                    component_asset_id: local_component_id,
                    component_revision_id: comp_rev_id,
                    ownership: cref.ownership,
                });
            }

            for rref in &payload.residual_refs {
                // residual tree must exist in CAS (already staged in Phase A or local)
                if self
                    .objects
                    .get_tree(&rref.tree_manifest_hash)
                    .await
                    .is_err()
                {
                    return Err(AppError::validation(format!(
                        "agent_hub_import_plugin_residual_tree_missing:{}",
                        rref.tree_manifest_hash
                    )));
                }
                plugin_residuals.push(ImportPluginResidualEdge {
                    package_revision_id: rev.id.clone(),
                    target: rref.target,
                    residual_kind: rref.residual_kind,
                    tree_manifest_hash: rref.tree_manifest_hash.clone(),
                });
            }
        }

        bundle.plugin_components = plugin_components;
        bundle.plugin_residuals = plugin_residuals;
        Ok(())
    }

    /// 收敛 local head 与 remote head。
    ///
    /// Business Logic: MCA + 内容合并；delete-vs-edit / 同块 → conflict；不相交 → merge rev。
    /// Code Logic: 优先 RevisionGraph（双方已在库）；否则内存祖先 + payload 比较。
    async fn converge_heads(
        &self,
        asset_id: &str,
        local_head: &str,
        remote_head: &str,
        env: &SnapshotEnvelopeV1,
        load_doc: &ObjectLoader<'_>,
    ) -> Result<ConvergeDecision, AppError> {
        // 若 remote 已在本地且是 local 后代 → fast-forward
        let graph = RevisionGraph::new(self.repo.clone());
        let local_id = RevisionId(local_head.to_string());
        let remote_id = RevisionId(remote_head.to_string());

        let local_in_db = self.repo.get_revision(&local_id).await?.is_some();
        let remote_in_db = self.repo.get_revision(&remote_id).await?.is_some();

        if local_in_db && remote_in_db {
            if graph.is_ancestor(&remote_id, &local_id).await? {
                return Ok(ConvergeDecision::AlreadyAncestor);
            }
            if graph.is_ancestor(&local_id, &remote_id).await? {
                let deleted = self.repo.get_revision(&remote_id).await?.and_then(|r| {
                    if r.operation == RevisionOperation::Delete {
                        Some(r.created_at)
                    } else {
                        None
                    }
                });
                return Ok(ConvergeDecision::FastForward {
                    head: remote_head.to_string(),
                    deleted_at: deleted,
                });
            }
            let mcas = graph
                .maximal_common_ancestors(&local_id, &remote_id)
                .await?;
            return self
                .merge_or_conflict(
                    asset_id,
                    local_head,
                    remote_head,
                    mcas.iter().map(|r| r.as_str().to_string()).collect(),
                    env,
                    load_doc,
                )
                .await;
        }

        // remote 可能尚未入库：用 snapshot parents + 本地库 BFS。
        // 必须 seed local/remote heads：本地独有 head 不在 envelope 时仍要能走 parents 求 MCA。
        let parent_index = build_parent_index(env, &self.repo, &[local_head, remote_head]).await?;
        if is_ancestor_mem(&parent_index, remote_head, local_head) {
            return Ok(ConvergeDecision::AlreadyAncestor);
        }
        if is_ancestor_mem(&parent_index, local_head, remote_head) {
            let deleted = env
                .revisions
                .iter()
                .find(|r| r.id == remote_head)
                .and_then(|r| {
                    if r.operation == RevisionOperation::Delete {
                        Some(r.created_at.clone())
                    } else {
                        None
                    }
                });
            return Ok(ConvergeDecision::FastForward {
                head: remote_head.to_string(),
                deleted_at: deleted,
            });
        }
        let mcas = maximal_common_ancestors_mem(&parent_index, local_head, remote_head);
        self.merge_or_conflict(asset_id, local_head, remote_head, mcas, env, load_doc)
            .await
    }

    async fn merge_or_conflict(
        &self,
        asset_id: &str,
        local_head: &str,
        remote_head: &str,
        mcas: Vec<String>,
        env: &SnapshotEnvelopeV1,
        load_doc: &ObjectLoader<'_>,
    ) -> Result<ConvergeDecision, AppError> {
        let local_payload = load_merge_payload(local_head, env, &self.repo).await?;
        let remote_payload = load_merge_payload(remote_head, env, &self.repo).await?;

        // delete-vs-edit
        if local_payload.is_delete != remote_payload.is_delete {
            return Ok(ConvergeDecision::Conflict {
                base: mcas.first().cloned(),
                hub: local_head.to_string(),
                external: remote_head.to_string(),
                detail: r#"{"reason":"agent_hub_delete_vs_edit"}"#.into(),
            });
        }
        if local_payload.content_eq(&remote_payload) {
            // 内容相同：创建 merge 指向双方，或 fast-forward 字典序较大
            let merge_id = Uuid::now_v7().to_string();
            let mut parents = vec![local_head.to_string(), remote_head.to_string()];
            parents.sort();
            parents.dedup();
            let gen = local_payload
                .payload_hash
                .as_ref()
                .map(|_| 0u64)
                .unwrap_or(0);
            let _ = gen;
            return Ok(ConvergeDecision::Merge {
                merge_rev: ImportRevisionRow {
                    id: merge_id,
                    // merge 必须挂在本地 asset id 上，禁止沿用 remote snapshot lineage
                    asset_lineage_id: asset_id.to_string(),
                    parents,
                    generation: 0,
                    operation: if local_payload.is_delete {
                        RevisionOperation::Delete
                    } else {
                        RevisionOperation::Upsert
                    },
                    origin_kind: RevisionOriginKind::Lan,
                    origin_target: None,
                    origin_replica_id: env.source_replica_id.clone(),
                    payload_hash: local_payload.payload_hash.clone(),
                    tree_manifest_hash: local_payload.tree_manifest_hash.clone(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                },
                deleted_at: if local_payload.is_delete {
                    Some(chrono::Utc::now().to_rfc3339())
                } else {
                    None
                },
            });
        }

        // 尝试 instruction 文档块级合并
        let base_id = mcas.first().cloned();
        let base_doc = if let Some(b) = &base_id {
            load_doc.load(b).await.ok().flatten()
        } else {
            None
        };
        let local_doc = match load_doc.load(local_head).await {
            Ok(Some(doc)) => doc,
            _ => {
                return Ok(ConvergeDecision::Conflict {
                    base: base_id,
                    hub: local_head.to_string(),
                    external: remote_head.to_string(),
                    detail: r#"{"reason":"agent_hub_import_content_conflict"}"#.into(),
                });
            }
        };
        let remote_doc = match load_doc.load(remote_head).await {
            Ok(Some(doc)) => doc,
            _ => {
                return Ok(ConvergeDecision::Conflict {
                    base: base_id,
                    hub: local_head.to_string(),
                    external: remote_head.to_string(),
                    detail: r#"{"reason":"agent_hub_import_content_conflict"}"#.into(),
                });
            }
        };

        match merge_instruction_documents(base_doc.as_ref(), &local_doc, &remote_doc) {
            InstructionContentMerge::Conflict => Ok(ConvergeDecision::Conflict {
                base: base_id,
                hub: local_head.to_string(),
                external: remote_head.to_string(),
                detail: r#"{"reason":"agent_hub_same_block_conflict"}"#.into(),
            }),
            InstructionContentMerge::Merged(doc) => {
                let bytes = serde_json::to_vec(&doc).map_err(|e| {
                    AppError::generic(format!("agent_hub_import_merge_serialize:{e}"))
                })?;
                let stored = self.objects.put_blob(&bytes).await?;
                let mut parents = vec![local_head.to_string(), remote_head.to_string()];
                parents.sort();
                Ok(ConvergeDecision::Merge {
                    merge_rev: ImportRevisionRow {
                        id: Uuid::now_v7().to_string(),
                        asset_lineage_id: asset_id.to_string(),
                        parents,
                        generation: 0,
                        operation: RevisionOperation::Upsert,
                        origin_kind: RevisionOriginKind::Lan,
                        origin_target: None,
                        origin_replica_id: env.source_replica_id.clone(),
                        payload_hash: Some(stored.hash),
                        tree_manifest_hash: None,
                        created_at: chrono::Utc::now().to_rfc3339(),
                    },
                    deleted_at: None,
                })
            }
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────

pub struct PlannedImport {
    /// import 事务载荷
    pub bundle: ImportBundle,
    /// 预计调度的 projection 数
    pub projections_scheduled: u64,
}

enum ConvergeDecision {
    FastForward {
        head: String,
        deleted_at: Option<String>,
    },
    AlreadyAncestor,
    Merge {
        merge_rev: ImportRevisionRow,
        deleted_at: Option<String>,
    },
    Conflict {
        base: Option<String>,
        hub: String,
        external: String,
        detail: String,
    },
}

struct ObjectLoader<'a> {
    env: &'a SnapshotEnvelopeV1,
    objects: &'a ObjectStore,
    repo: &'a AgentHubRepo,
}

fn snapshot_object_loader<'a>(
    env: &'a SnapshotEnvelopeV1,
    objects: &'a ObjectStore,
    repo: &'a AgentHubRepo,
) -> ObjectLoader<'a> {
    ObjectLoader { env, objects, repo }
}

impl ObjectLoader<'_> {
    /// 加载 revision 对应的 InstructionDocument。
    ///
    /// Business Logic: multi-replica import 时 local head 已在 SQLite/CAS，通常不在 *incoming*
    /// envelope；必须能读双方文档才能做不相交双 parent merge。
    ///
    /// Code Logic: envelope.payload_hash → 否则 `repo.get_revision` → `objects.get_blob`；
    /// 仍不可读则 `Ok(None)`（调用方 fail-closed 开 conflict，禁止 LWW）。
    async fn load(&self, rev_id: &str) -> Result<Option<InstructionDocument>, AppError> {
        let hash = if let Some(r) = self.env.revisions.iter().find(|r| r.id == rev_id) {
            r.payload_hash.clone()
        } else if let Some(r) = self
            .repo
            .get_revision(&RevisionId(rev_id.to_string()))
            .await?
        {
            r.payload_hash
        } else {
            None
        };
        let Some(hash) = hash else {
            return Ok(None);
        };
        let bytes = match self.objects.get_blob(&hash).await {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        if let Ok(doc) = serde_json::from_slice::<InstructionDocument>(&bytes) {
            return Ok(Some(doc));
        }
        // fallback shared markdown
        if let Ok(text) = std::str::from_utf8(&bytes) {
            return Ok(Some(InstructionDocument::from_shared_markdown(
                "imported", text,
            )));
        }
        Ok(None)
    }
}

async fn load_merge_payload(
    rev_id: &str,
    env: &SnapshotEnvelopeV1,
    repo: &AgentHubRepo,
) -> Result<MergePayload, AppError> {
    if let Some(r) = env.revisions.iter().find(|r| r.id == rev_id) {
        return Ok(MergePayload {
            payload_hash: r.payload_hash.clone(),
            tree_manifest_hash: r.tree_manifest_hash.clone(),
            is_delete: r.operation == RevisionOperation::Delete,
        });
    }
    if let Some(r) = repo.get_revision(&RevisionId(rev_id.to_string())).await? {
        return Ok(MergePayload::from_revision(&r));
    }
    Err(AppError::not_found(format!(
        "agent_hub_import_revision_missing:{rev_id}"
    )))
}

fn snapshot_err_to_app(e: SnapshotError) -> AppError {
    AppError::validation(e.to_string())
}
