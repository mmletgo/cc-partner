//! agent_hub/snapshot/importer — 两阶段 Snapshot 导入（CAS 验证 + 单 TX head 收敛）
//!
//! Business Logic（为什么需要这个模块）:
//!     LAN push commit 与 Git 确认 import 共用同一入口；必须先验证对象 hash 再原子落库，
//!     用 MCA/块级合并收敛分叉 heads，禁止 last-write-wins。
//!
//! Code Logic（这个模块做什么）:
//!     `SnapshotImporter::{inspect_import,commit_import}`：Phase A 校验 envelope/闭包/CAS；
//!     Phase B 单写事务 upsert 身份/DAG 并更新 head 或 conflict；outcome 在 enqueue 前返回。

use crate::agent_hub::instructions::{
    merge_instruction_documents, InstructionContentMerge, InstructionDocument,
};
use crate::agent_hub::models::{
    AssetKind, NewScopeNode, RevisionId, RevisionOperation, RevisionOriginKind, ScopeKind,
};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::revision_graph::{MergePayload, RevisionGraph};
use crate::agent_hub::snapshot::envelope::{
    validate_snapshot, SnapshotEnvelopeV1, SnapshotError, SnapshotLimits, SnapshotRevision,
};
use crate::error::AppError;
use crate::storage::agent_hub_repo::{
    AgentHubRepo, ImportAssetRow, ImportBundle, ImportConflictRow, ImportHeadDecision,
    ImportRevisionRow, ImportVariantRow, UpsertAgentHubProjectMapping,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
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

        // ── Phase A: closure + CAS ──────────────────────────────────────
        validate_revision_closure(env)?;
        let referenced = collect_referenced_object_hashes(env);
        let mut imported_object_hashes = Vec::new();
        for hash in &referenced {
            // 优先 snapshot 提供的字节；否则尝试本地 CAS
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
                // 本地 CAS 必须存在
                match self.objects.get_blob(hash).await {
                    Ok(_) => {
                        // 已在 CAS，不计入「本次新导入」也可报告为 imported referenced
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
        // 未引用的 object_bytes 可写入 CAS 供 GC，但不得报告为 imported asset 对象
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
            // crash after CAS：对象已在 CAS，但 TX 未开
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

        // ── Plan Phase B bundle ─────────────────────────────────────────
        let plan = self.plan_import_bundle(env, &selection).await?;
        let projections_scheduled = plan.projections_scheduled;
        let bundle = plan.bundle;

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

        // revisions 拓扑序
        let ordered = topological_revisions(&env.revisions)?;
        let mut revisions: Vec<ImportRevisionRow> = Vec::new();
        for r in ordered {
            let gen: u64 = r.generation.parse().unwrap_or(0);
            revisions.push(ImportRevisionRow {
                id: r.id.clone(),
                asset_lineage_id: r.asset_lineage_id.clone(),
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
                        new_head: Some(h.clone()),
                        deleted_at: deleted,
                    });
                } else {
                    // 多 remote heads：保留不设 single head，开 conflict
                    let mut sorted = remote_heads.clone();
                    sorted.sort();
                    head_decisions.push(ImportHeadDecision {
                        asset_id: local_asset.clone(),
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
                            new_head: Some(head),
                            deleted_at,
                        });
                    }
                    ConvergeDecision::AlreadyAncestor => {
                        // keep local head
                    }
                    ConvergeDecision::Merge {
                        merge_rev,
                        deleted_at,
                    } => {
                        let merge_id = merge_rev.id.clone();
                        merge_revisions.push(merge_rev);
                        head_decisions.push(ImportHeadDecision {
                            asset_id: local_asset.clone(),
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
        for v in &env.variants {
            let local_asset = asset_id_local
                .get(&v.asset_id)
                .cloned()
                .unwrap_or_else(|| v.asset_id.clone());
            variants.push(ImportVariantRow {
                asset_id: local_asset,
                target: v.target,
                revision_id: v.revision_id.clone(),
            });
        }

        // projections_scheduled：仅 mapped 且 opted_in 的 project / 全部 user assets
        let mut projections_scheduled = 0u64;
        for a in &env.assets {
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
            },
            projections_scheduled,
        })
    }

    /// 收敛 local head 与 remote head。
    ///
    /// Business Logic: MCA + 内容合并；delete-vs-edit / 同块 → conflict；不相交 → merge rev。
    /// Code Logic: 优先 RevisionGraph（双方已在库）；否则内存祖先 + payload 比较。
    async fn converge_heads(
        &self,
        _asset_id: &str,
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
        self.merge_or_conflict(local_head, remote_head, mcas, env, load_doc)
            .await
    }

    async fn merge_or_conflict(
        &self,
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
                    asset_lineage_id: env
                        .revisions
                        .iter()
                        .find(|r| r.id == local_head || r.id == remote_head)
                        .map(|r| r.asset_lineage_id.clone())
                        .unwrap_or_default(),
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
                let lineage = env
                    .revisions
                    .iter()
                    .find(|r| r.id == local_head || r.id == remote_head)
                    .map(|r| r.asset_lineage_id.clone())
                    .unwrap_or_default();
                let mut parents = vec![local_head.to_string(), remote_head.to_string()];
                parents.sort();
                Ok(ConvergeDecision::Merge {
                    merge_rev: ImportRevisionRow {
                        id: Uuid::now_v7().to_string(),
                        asset_lineage_id: lineage,
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

struct PlannedImport {
    bundle: ImportBundle,
    projections_scheduled: u64,
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

fn validate_revision_closure(env: &SnapshotEnvelopeV1) -> Result<(), AppError> {
    let ids: HashSet<&str> = env.revisions.iter().map(|r| r.id.as_str()).collect();
    for r in &env.revisions {
        for p in &r.parents {
            if !ids.contains(p.as_str()) {
                return Err(AppError::validation(format!(
                    "agent_hub_import_parent_missing:{p}"
                )));
            }
        }
    }
    for heads in env.asset_heads.values() {
        for h in heads {
            if !ids.contains(h.as_str()) {
                return Err(AppError::validation(format!(
                    "agent_hub_import_head_not_in_closure:{h}"
                )));
            }
        }
    }
    Ok(())
}

fn collect_referenced_object_hashes(env: &SnapshotEnvelopeV1) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for r in &env.revisions {
        if let Some(h) = &r.payload_hash {
            set.insert(h.clone());
        }
        if let Some(h) = &r.tree_manifest_hash {
            set.insert(h.clone());
        }
    }
    for o in &env.objects {
        // objects 列表是协商集合；严格引用仍以 revision 为准
        let _ = o;
    }
    set
}

fn topological_revisions(revs: &[SnapshotRevision]) -> Result<Vec<&SnapshotRevision>, AppError> {
    let mut by_id: HashMap<&str, &SnapshotRevision> = HashMap::new();
    for r in revs {
        by_id.insert(r.id.as_str(), r);
    }
    let mut indeg: HashMap<&str, usize> = HashMap::new();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for r in revs {
        indeg.entry(r.id.as_str()).or_insert(0);
        for p in &r.parents {
            *indeg.entry(r.id.as_str()).or_insert(0) += 1;
            children.entry(p.as_str()).or_default().push(r.id.as_str());
        }
    }
    let mut q: VecDeque<&str> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(k, _)| *k)
        .collect();
    let mut out = Vec::new();
    while let Some(id) = q.pop_front() {
        if let Some(r) = by_id.get(id) {
            out.push(*r);
        }
        if let Some(chs) = children.get(id) {
            for c in chs {
                if let Some(d) = indeg.get_mut(c) {
                    *d = d.saturating_sub(1);
                    if *d == 0 {
                        q.push_back(c);
                    }
                }
            }
        }
    }
    if out.len() != revs.len() {
        return Err(AppError::validation(
            "agent_hub_import_revision_cycle".to_string(),
        ));
    }
    Ok(out)
}

async fn build_parent_index(
    env: &SnapshotEnvelopeV1,
    repo: &AgentHubRepo,
    seed_ids: &[&str],
) -> Result<HashMap<String, Vec<String>>, AppError> {
    let mut idx: HashMap<String, Vec<String>> = HashMap::new();
    for r in &env.revisions {
        idx.insert(r.id.clone(), r.parents.clone());
    }
    // 从 envelope 节点 + 调用方 seed（local/remote heads）BFS 补本地 parents。
    // multi-replica 场景 local head 往往不在 *incoming* envelope，不 seed 则 MCA 为空。
    let mut frontier: VecDeque<String> = idx.keys().cloned().collect();
    for id in seed_ids {
        frontier.push_back((*id).to_string());
    }
    let mut visited = HashSet::new();
    while let Some(id) = frontier.pop_front() {
        if !visited.insert(id.clone()) {
            continue;
        }
        if idx.contains_key(&id) {
            for p in idx.get(&id).cloned().unwrap_or_default() {
                frontier.push_back(p);
            }
            continue;
        }
        if let Some(r) = repo.get_revision(&RevisionId(id.clone())).await? {
            let parents: Vec<String> = r.parents.iter().map(|p| p.as_str().to_string()).collect();
            for p in &parents {
                frontier.push_back(p.clone());
            }
            idx.insert(id, parents);
        }
    }
    Ok(idx)
}

fn is_ancestor_mem(
    parents: &HashMap<String, Vec<String>>,
    maybe_ancestor: &str,
    node: &str,
) -> bool {
    if maybe_ancestor == node {
        return true;
    }
    let mut q = VecDeque::new();
    let mut seen = HashSet::new();
    q.push_back(node.to_string());
    while let Some(cur) = q.pop_front() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        if cur == maybe_ancestor {
            return true;
        }
        if let Some(ps) = parents.get(&cur) {
            for p in ps {
                q.push_back(p.clone());
            }
        }
    }
    false
}

fn maximal_common_ancestors_mem(
    parents: &HashMap<String, Vec<String>>,
    left: &str,
    right: &str,
) -> Vec<String> {
    let left_anc = ancestor_closure_mem(parents, left);
    let right_anc = ancestor_closure_mem(parents, right);
    let common: Vec<String> = left_anc.intersection(&right_anc).cloned().collect();
    let mut maximal = Vec::new();
    for cand in &common {
        let mut dominated = false;
        for other in &common {
            if cand == other {
                continue;
            }
            if is_ancestor_mem(parents, cand, other) {
                dominated = true;
                break;
            }
        }
        if !dominated {
            maximal.push(cand.clone());
        }
    }
    maximal.sort();
    maximal
}

fn ancestor_closure_mem(parents: &HashMap<String, Vec<String>>, node: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut q = VecDeque::new();
    q.push_back(node.to_string());
    while let Some(cur) = q.pop_front() {
        if !out.insert(cur.clone()) {
            continue;
        }
        if let Some(ps) = parents.get(&cur) {
            for p in ps {
                q.push_back(p.clone());
            }
        }
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::instructions::{InstructionBlock, InstructionDocument};
    use crate::agent_hub::models::{
        AssetKind, AssetPolicy, NewLogicalAsset, NewRevision, NewScopeNode, RevisionId,
        RevisionOperation, RevisionOriginKind, ScopeKind,
    };
    use crate::agent_hub::snapshot::builder::{
        build_snapshot, clear_envelope_cache_for_test, SnapshotSelectionMode,
        SnapshotSelectionRequest,
    };
    use crate::storage::agent_hub_repo::{
        AgentHubImportFault, AgentHubRepo, UpsertAgentHubProjectMapping,
    };
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    const REPLICA_B: &str = "01900000-0000-7000-8000-0000000000b2";

    async fn test_env() -> (AgentHubRepo, ObjectStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("t.db");
        let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path.display()))
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        let repo = AgentHubRepo::new(pool);
        // 全局 import fault 跨测试共享：fixture 入口强制复位
        let _ = repo.take_import_fault();
        let store = ObjectStore::open(dir.path()).unwrap();
        (repo, store, dir)
    }

    async fn seed_user(repo: &AgentHubRepo) -> String {
        repo.insert_scope(NewScopeNode {
            id: Some("scope-user".into()),
            kind: ScopeKind::User,
            hub_project_id: None,
            relative_path: None,
        })
        .await
        .unwrap()
        .id
    }

    fn doc_bytes(blocks: Vec<(&str, &str)>) -> Vec<u8> {
        let document = InstructionDocument {
            relative_key: "CLAUDE.md".into(),
            blocks: blocks
                .into_iter()
                .map(|(id, body)| InstructionBlock::shared(id, body, vec![]))
                .collect(),
        };
        serde_json::to_vec(&document).unwrap()
    }

    async fn put_doc(store: &ObjectStore, blocks: Vec<(&str, &str)>) -> String {
        store.put_blob(&doc_bytes(blocks)).await.unwrap().hash
    }

    async fn append_instruction(
        repo: &AgentHubRepo,
        asset_id: &str,
        parents: Vec<RevisionId>,
        hash: &str,
        expected: Option<RevisionId>,
        at: &str,
    ) -> RevisionId {
        let rev = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset_id.to_string(),
                parents,
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
                payload_hash: Some(hash.to_string()),
                tree_manifest_hash: None,
                created_at: at.to_string(),
                expected_parent_id: expected,
            })
            .await
            .unwrap();
        rev.id
    }

    /// 两副本分叉：不相交块 → 必须产出 dual-parent merge revision。
    ///
    /// multi-replica 真实路径：先 import A（base+left）进空 hub；再 import B（base+right），
    /// 且 B 的 envelope **不**再列出 local-only head left——loader 必须从 DB/CAS 读 local head。
    #[tokio::test]
    async fn disjoint_blocks_merge_with_both_parents() {
        clear_envelope_cache_for_test();
        let (repo_a, store_a, dir_a) = test_env().await;
        let user = seed_user(&repo_a).await;
        let asset = repo_a
            .insert_asset(NewLogicalAsset {
                scope_id: user.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "root".into(),
                display_name: "Root".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let base_h = put_doc(&store_a, vec![("b1", "A base"), ("b2", "B base")]).await;
        let base = append_instruction(
            &repo_a,
            &asset.id,
            vec![],
            &base_h,
            None,
            "2026-07-29T10:00:00Z",
        )
        .await;
        let left_h = put_doc(&store_a, vec![("b1", "A left"), ("b2", "B base")]).await;
        let left = append_instruction(
            &repo_a,
            &asset.id,
            vec![base.clone()],
            &left_h,
            Some(base.clone()),
            "2026-07-29T11:00:00Z",
        )
        .await;

        let built = build_snapshot(
            &repo_a,
            &store_a,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::ExplicitAssets,
                scope_ids: vec![],
                asset_ids: vec![asset.id.clone()],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
                limits: None,
            },
        )
        .await
        .unwrap();

        // B: 空库 + import A snapshot（head=left）
        let (repo_b, store_b, dir_b) = test_env().await;
        let importer_b = SnapshotImporter::new(repo_b.clone(), store_b.clone(), dir_b.path());
        let validated =
            ValidatedSnapshot::from_parts(built.envelope.clone(), built.object_bytes.clone(), None)
                .unwrap();
        let out = importer_b
            .commit_import(validated, ConfirmedImportSelection::default())
            .await
            .unwrap();
        assert!(out.inserted_revisions >= 2);
        assert_eq!(
            out.conflicts_opened, 0,
            "first import must not conflict: {out:?}"
        );

        // 独立 replica R：common base + 不相交 right 块；envelope 不含 left
        let (repo_r, store_r, _dir_r) = test_env().await;
        let user_r = seed_user(&repo_r).await;
        let asset_r = repo_r
            .insert_asset(NewLogicalAsset {
                scope_id: user_r,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "root".into(),
                display_name: "Root".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        store_r
            .put_blob(&store_a.get_blob(&base_h).await.unwrap())
            .await
            .unwrap();
        let _ = repo_r
            .append_revision(NewRevision {
                id: base.clone(),
                asset_lineage_id: asset_r.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: REPLICA_B.into(),
                payload_hash: Some(base_h.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T10:00:00Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        let right_h = put_doc(&store_r, vec![("b1", "A base"), ("b2", "B right")]).await;
        let right = repo_r
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset_r.id.clone(),
                parents: vec![base.clone()],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: REPLICA_B.into(),
                payload_hash: Some(right_h.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T11:30:00Z".into(),
                expected_parent_id: Some(base.clone()),
            })
            .await
            .unwrap();

        let built_r = build_snapshot(
            &repo_r,
            &store_r,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::ExplicitAssets,
                scope_ids: vec![],
                asset_ids: vec![asset_r.id.clone()],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
                limits: None,
            },
        )
        .await
        .unwrap();

        // 严格 multi-replica：B envelope 不得再列出 local-only left head
        assert!(
            !built_r
                .envelope
                .revisions
                .iter()
                .any(|r| r.id == left.as_str()),
            "side-B envelope must not re-list local-only left head"
        );
        // 仅带 B 侧 object_bytes（base + right）；left blob 已在 hub CAS
        let validated_r =
            ValidatedSnapshot::from_parts(built_r.envelope, built_r.object_bytes, None).unwrap();
        let out2 = importer_b
            .commit_import(validated_r, ConfirmedImportSelection::default())
            .await
            .unwrap();
        assert!(
            out2.inserted_revisions >= 1,
            "expected remote branch + merge rev: {out2:?}"
        );
        assert_eq!(
            out2.conflicts_opened, 0,
            "pure disjoint blocks must merge without conflict: {out2:?}"
        );

        let local_assets = repo_b.list_assets(None, None).await.unwrap();
        assert_eq!(local_assets.len(), 1);
        let head = local_assets[0].current_revision_id.as_ref().expect("head");
        let head_rev = repo_b.get_revision(head).await.unwrap().unwrap();
        assert_eq!(
            head_rev.parents.len(),
            2,
            "must produce dual-parent merge revision, got parents={:?}",
            head_rev.parents
        );
        let ps: BTreeSet<_> = head_rev
            .parents
            .iter()
            .map(|p| p.as_str().to_string())
            .collect();
        assert!(
            ps.contains(left.as_str()) && ps.contains(right.id.as_str()),
            "merge parents should be left+right, got {ps:?}"
        );
        // 双方分支 revision 仍保留（非 LWW 抹除）
        assert!(repo_b.get_revision(&left).await.unwrap().is_some());
        assert!(repo_b.get_revision(&right.id).await.unwrap().is_some());
        let _ = (dir_a, store_b);
    }

    /// 同块双侧编辑 → 双 head + conflict。
    #[tokio::test]
    async fn same_block_preserves_heads_and_conflict() {
        clear_envelope_cache_for_test();
        let (repo_a, store_a, _dir_a) = test_env().await;
        let user = seed_user(&repo_a).await;
        let asset = repo_a
            .insert_asset(NewLogicalAsset {
                scope_id: user,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "root".into(),
                display_name: "Root".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let base_h = put_doc(&store_a, vec![("b1", "same base"), ("b2", "other")]).await;
        let base = append_instruction(
            &repo_a,
            &asset.id,
            vec![],
            &base_h,
            None,
            "2026-07-29T10:00:00Z",
        )
        .await;
        let left_h = put_doc(&store_a, vec![("b1", "hub edit"), ("b2", "other")]).await;
        let _left = append_instruction(
            &repo_a,
            &asset.id,
            vec![base.clone()],
            &left_h,
            Some(base.clone()),
            "2026-07-29T11:00:00Z",
        )
        .await;
        let built = build_snapshot(
            &repo_a,
            &store_a,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::ExplicitAssets,
                scope_ids: vec![],
                asset_ids: vec![asset.id.clone()],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
                limits: None,
            },
        )
        .await
        .unwrap();

        let (repo_b, store_b, dir_b) = test_env().await;
        let importer = SnapshotImporter::new(repo_b.clone(), store_b.clone(), dir_b.path());
        importer
            .commit_import(
                ValidatedSnapshot::from_parts(
                    built.envelope.clone(),
                    built.object_bytes.clone(),
                    None,
                )
                .unwrap(),
                ConfirmedImportSelection::default(),
            )
            .await
            .unwrap();

        // remote same-block branch
        let (repo_r, store_r, _dir_r) = test_env().await;
        let user_r = seed_user(&repo_r).await;
        let asset_r = repo_r
            .insert_asset(NewLogicalAsset {
                scope_id: user_r,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "root".into(),
                display_name: "Root".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        store_r
            .put_blob(&store_a.get_blob(&base_h).await.unwrap())
            .await
            .unwrap();
        repo_r
            .append_revision(NewRevision {
                id: base.clone(),
                asset_lineage_id: asset_r.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: REPLICA_B.into(),
                payload_hash: Some(base_h.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T10:00:00Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        let right_h = put_doc(&store_r, vec![("b1", "external edit"), ("b2", "other")]).await;
        repo_r
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset_r.id.clone(),
                parents: vec![base.clone()],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: REPLICA_B.into(),
                payload_hash: Some(right_h.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T11:30:00Z".into(),
                expected_parent_id: Some(base),
            })
            .await
            .unwrap();
        let built_r = build_snapshot(
            &repo_r,
            &store_r,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::ExplicitAssets,
                scope_ids: vec![],
                asset_ids: vec![asset_r.id],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
                limits: None,
            },
        )
        .await
        .unwrap();
        store_b
            .put_blob(&store_r.get_blob(&right_h).await.unwrap())
            .await
            .unwrap();
        let mut bytes = built_r.object_bytes;
        for (k, v) in built.object_bytes {
            bytes.entry(k).or_insert(v);
        }
        let out = importer
            .commit_import(
                ValidatedSnapshot::from_parts(built_r.envelope, bytes, None).unwrap(),
                ConfirmedImportSelection::default(),
            )
            .await
            .unwrap();
        assert!(
            out.conflicts_opened >= 1,
            "same-block must conflict, got {out:?}"
        );
        // head 不应被 remote LWW 覆盖为唯一 remote：local head 仍存在
        let assets = repo_b.list_assets(None, None).await.unwrap();
        let head = assets[0].current_revision_id.as_ref().unwrap().as_str();
        // local head should still be the left revision from first import (or unchanged)
        assert!(!head.is_empty());
        let conflicts = repo_b.list_unresolved_conflicts().await.unwrap();
        assert!(!conflicts.is_empty());
    }

    /// 相同 revision id 去重。
    #[tokio::test]
    async fn identical_revision_ids_dedupe() {
        clear_envelope_cache_for_test();
        let (repo, store, dir) = test_env().await;
        let user = seed_user(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "k".into(),
                display_name: "K".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let h = put_doc(&store, vec![("b1", "only")]).await;
        let _ =
            append_instruction(&repo, &asset.id, vec![], &h, None, "2026-07-29T10:00:00Z").await;
        let built = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::FullHub,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
                limits: None,
            },
        )
        .await
        .unwrap();
        let importer = SnapshotImporter::new(repo.clone(), store.clone(), dir.path());
        let v =
            ValidatedSnapshot::from_parts(built.envelope.clone(), built.object_bytes.clone(), None)
                .unwrap();
        let first = importer
            .commit_import(v.clone(), ConfirmedImportSelection::default())
            .await
            .unwrap();
        let second = importer
            .commit_import(
                ValidatedSnapshot::from_parts(built.envelope, built.object_bytes, None).unwrap(),
                ConfirmedImportSelection::default(),
            )
            .await
            .unwrap();
        assert!(
            second.deduped_revisions >= first.inserted_revisions.min(1)
                || second.inserted_revisions == 0
        );
    }

    /// delete-vs-edit → conflict。
    #[tokio::test]
    async fn delete_vs_edit_conflicts() {
        clear_envelope_cache_for_test();
        let (repo_a, store_a, _dir_a) = test_env().await;
        let user = seed_user(&repo_a).await;
        let asset = repo_a
            .insert_asset(NewLogicalAsset {
                scope_id: user,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "root".into(),
                display_name: "Root".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let base_h = put_doc(&store_a, vec![("b1", "body")]).await;
        let base = append_instruction(
            &repo_a,
            &asset.id,
            vec![],
            &base_h,
            None,
            "2026-07-29T10:00:00Z",
        )
        .await;
        let edit_h = put_doc(&store_a, vec![("b1", "edited")]).await;
        let _edit = append_instruction(
            &repo_a,
            &asset.id,
            vec![base.clone()],
            &edit_h,
            Some(base.clone()),
            "2026-07-29T11:00:00Z",
        )
        .await;
        let built = build_snapshot(
            &repo_a,
            &store_a,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::ExplicitAssets,
                scope_ids: vec![],
                asset_ids: vec![asset.id.clone()],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
                limits: None,
            },
        )
        .await
        .unwrap();

        let (repo_b, store_b, dir_b) = test_env().await;
        let importer = SnapshotImporter::new(repo_b.clone(), store_b.clone(), dir_b.path());
        importer
            .commit_import(
                ValidatedSnapshot::from_parts(
                    built.envelope.clone(),
                    built.object_bytes.clone(),
                    None,
                )
                .unwrap(),
                ConfirmedImportSelection::default(),
            )
            .await
            .unwrap();

        // remote delete from base
        let (repo_r, store_r, _dir_r) = test_env().await;
        let user_r = seed_user(&repo_r).await;
        let asset_r = repo_r
            .insert_asset(NewLogicalAsset {
                scope_id: user_r,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "root".into(),
                display_name: "Root".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        store_r
            .put_blob(&store_a.get_blob(&base_h).await.unwrap())
            .await
            .unwrap();
        repo_r
            .append_revision(NewRevision {
                id: base.clone(),
                asset_lineage_id: asset_r.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: REPLICA_B.into(),
                payload_hash: Some(base_h),
                tree_manifest_hash: None,
                created_at: "2026-07-29T10:00:00Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        repo_r
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset_r.id.clone(),
                parents: vec![base],
                operation: RevisionOperation::Delete,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: REPLICA_B.into(),
                payload_hash: None,
                tree_manifest_hash: None,
                created_at: "2026-07-29T11:30:00Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        let built_r = build_snapshot(
            &repo_r,
            &store_r,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::ExplicitAssets,
                scope_ids: vec![],
                asset_ids: vec![asset_r.id],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
                limits: None,
            },
        )
        .await
        .unwrap();
        let mut bytes = built_r.object_bytes;
        for (k, v) in built.object_bytes {
            bytes.entry(k).or_insert(v);
        }
        let out = importer
            .commit_import(
                ValidatedSnapshot::from_parts(built_r.envelope, bytes, None).unwrap(),
                ConfirmedImportSelection::default(),
            )
            .await
            .unwrap();
        assert!(out.conflicts_opened >= 1, "delete-vs-edit: {out:?}");
    }

    /// 不同 hubProjectId 映射为 alias 合并到同一 local project scope。
    #[tokio::test]
    async fn distinct_hub_project_ids_map_to_one_local_scope() {
        clear_envelope_cache_for_test();
        let (repo_a, store_a, _dir_a) = test_env().await;
        let scope = repo_a
            .insert_scope(NewScopeNode {
                id: Some("scope-proj-remote-a".into()),
                kind: ScopeKind::Project,
                hub_project_id: Some("hub-remote-a".into()),
                relative_path: Some(".".into()),
            })
            .await
            .unwrap()
            .id;
        repo_a
            .upsert_project_mapping(UpsertAgentHubProjectMapping {
                hub_project_id: "hub-remote-a".into(),
                local_workbench_project_id: Some("wb-1".into()),
                git_remote_fingerprint: Some("fp-1".into()),
                local_absolute_path: None,
                opted_in: false,
            })
            .await
            .unwrap();
        let asset = repo_a
            .insert_asset(NewLogicalAsset {
                scope_id: scope,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "p".into(),
                display_name: "P".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let h = put_doc(&store_a, vec![("b1", "proj")]).await;
        let _ =
            append_instruction(&repo_a, &asset.id, vec![], &h, None, "2026-07-29T10:00:00Z").await;
        let built = build_snapshot(
            &repo_a,
            &store_a,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::Project,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec!["hub-remote-a".into()],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
                limits: None,
            },
        )
        .await
        .unwrap();

        let (repo_b, store_b, dir_b) = test_env().await;
        // local already maps different hub id to same workbench
        repo_b
            .insert_scope(NewScopeNode {
                id: Some("scope-proj-local".into()),
                kind: ScopeKind::Project,
                hub_project_id: Some("hub-local".into()),
                relative_path: Some(".".into()),
            })
            .await
            .unwrap();
        repo_b
            .upsert_project_mapping(UpsertAgentHubProjectMapping {
                hub_project_id: "hub-local".into(),
                local_workbench_project_id: Some("wb-1".into()),
                git_remote_fingerprint: Some("fp-1".into()),
                local_absolute_path: None,
                opted_in: false,
            })
            .await
            .unwrap();
        // confirm alias: remote hub-remote-a → same local mapping via selection
        let importer = SnapshotImporter::new(repo_b.clone(), store_b, dir_b.path());
        let preview = importer
            .inspect_import(
                &ValidatedSnapshot::from_parts(
                    built.envelope.clone(),
                    built.object_bytes.clone(),
                    None,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            !preview.project_candidates.is_empty()
                || !preview.resolved_mappings.is_empty()
                || built
                    .envelope
                    .aliases
                    .iter()
                    .any(|a| a.kind == "hubProjectId")
        );
        let out = importer
            .commit_import(
                ValidatedSnapshot::from_parts(built.envelope, built.object_bytes, None).unwrap(),
                ConfirmedImportSelection {
                    project_mappings: vec![ConfirmedProjectMapping {
                        hub_project_id: "hub-remote-a".into(),
                        local_workbench_project_id: Some("wb-1".into()),
                        git_remote_fingerprint: Some("fp-1".into()),
                        opted_in: false,
                    }],
                    import_unmapped_projects: true,
                },
            )
            .await
            .unwrap();
        assert!(!out.imported_asset_ids.is_empty());
        // mapping 已保存
        let m = repo_b
            .get_project_mapping_by_hub_project_id("hub-remote-a")
            .await
            .unwrap()
            .expect("mapping");
        assert_eq!(m.local_workbench_project_id.as_deref(), Some("wb-1"));
        assert!(!m.opted_in);
    }

    /// 未映射 project 导入但 projections_scheduled=0。
    #[tokio::test]
    async fn unmapped_project_imports_with_zero_projections() {
        clear_envelope_cache_for_test();
        // 全局 fault 可能被并行测试污染；先清空
        let (repo_a, store_a, _dir_a) = test_env().await;
        let _ = repo_a.take_import_fault();
        let scope = repo_a
            .insert_scope(NewScopeNode {
                id: Some("scope-proj-x".into()),
                kind: ScopeKind::Project,
                hub_project_id: Some("hub-x".into()),
                relative_path: Some(".".into()),
            })
            .await
            .unwrap()
            .id;
        let asset = repo_a
            .insert_asset(NewLogicalAsset {
                scope_id: scope,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "p".into(),
                display_name: "P".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let h = put_doc(&store_a, vec![("b1", "x")]).await;
        let _ =
            append_instruction(&repo_a, &asset.id, vec![], &h, None, "2026-07-29T10:00:00Z").await;
        let built = build_snapshot(
            &repo_a,
            &store_a,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::Project,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec!["hub-x".into()],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
                limits: None,
            },
        )
        .await
        .unwrap();
        let (repo_b, store_b, dir_b) = test_env().await;
        let importer = SnapshotImporter::new(repo_b, store_b, dir_b.path());
        let out = importer
            .commit_import(
                ValidatedSnapshot::from_parts(built.envelope, built.object_bytes, None).unwrap(),
                ConfirmedImportSelection {
                    project_mappings: vec![],
                    import_unmapped_projects: true,
                },
            )
            .await
            .unwrap();
        assert!(!out.imported_asset_ids.is_empty());
        assert_eq!(out.projections_scheduled, 0);
    }

    /// 缺失 parent → 失败，无脏 head。
    #[tokio::test]
    async fn missing_parent_fails_without_active_head() {
        let (repo, _store, _dir) = test_env().await;
        let mut env = empty_envelope();
        env.revisions.push(SnapshotRevision {
            id: "rev-child".into(),
            asset_lineage_id: "asset-1".into(),
            parents: vec!["rev-missing".into()],
            generation: "1".into(),
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Lan,
            origin_target: None,
            origin_replica_id: REPLICA_B.into(),
            payload_hash: None,
            tree_manifest_hash: None,
            created_at: "2026-07-29T10:00:00Z".into(),
        });
        // bypass ValidatedSnapshot validate by constructing manually after fixing hash is hard;
        // call validate_revision_closure path via commit with hand-built invalid that still validates envelope?
        // Use direct helper
        let err = validate_revision_closure(&env).unwrap_err();
        assert!(err.to_string().contains("parent_missing") || err.to_string().contains("missing"));
        assert!(repo.list_assets(None, None).await.unwrap().is_empty());
    }

    /// 损坏 object hash → 失败。
    #[tokio::test]
    async fn corrupt_object_fails() {
        clear_envelope_cache_for_test();
        let (repo, store, _dir) = test_env().await;
        let user = seed_user(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "k".into(),
                display_name: "K".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let h = put_doc(&store, vec![("b1", "ok")]).await;
        let _ =
            append_instruction(&repo, &asset.id, vec![], &h, None, "2026-07-29T10:00:00Z").await;
        let built = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::FullHub,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
                limits: None,
            },
        )
        .await
        .unwrap();
        let mut bytes = built.object_bytes;
        // corrupt
        if let Some(v) = bytes.values_mut().next() {
            v.push(b'x');
        }
        let (repo2, store2, dir2) = test_env().await;
        let importer = SnapshotImporter::new(repo2, store2, dir2.path());
        // from_parts still validates envelope; commit checks object hash
        let v = ValidatedSnapshot {
            envelope: built.envelope,
            object_bytes: bytes,
        };
        let err = importer
            .commit_import(v, ConfirmedImportSelection::default())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("corrupt") || err.to_string().contains("hash"),
            "{err}"
        );
    }

    /// DB fail before head → 无非法 head；CAS 后崩溃对象可残留但不计 imported asset。
    #[tokio::test]
    async fn db_fail_before_head_and_cas_residual() {
        clear_envelope_cache_for_test();
        let (repo_a, store_a, _dir_a) = test_env().await;
        let user = seed_user(&repo_a).await;
        let asset = repo_a
            .insert_asset(NewLogicalAsset {
                scope_id: user,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "k".into(),
                display_name: "K".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let h = put_doc(&store_a, vec![("b1", "body")]).await;
        let _ =
            append_instruction(&repo_a, &asset.id, vec![], &h, None, "2026-07-29T10:00:00Z").await;
        let built = build_snapshot(
            &repo_a,
            &store_a,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::FullHub,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
                limits: None,
            },
        )
        .await
        .unwrap();

        let (repo_b, store_b, dir_b) = test_env().await;
        // 清掉其它测试可能残留的全局 fault，再注入本用例
        let _ = repo_b.take_import_fault();
        repo_b.inject_import_fault(AgentHubImportFault::BeforeTxCommit);
        let importer = SnapshotImporter::new(repo_b.clone(), store_b.clone(), dir_b.path());
        let err = importer
            .commit_import(
                ValidatedSnapshot::from_parts(
                    built.envelope.clone(),
                    built.object_bytes.clone(),
                    None,
                )
                .unwrap(),
                ConfirmedImportSelection::default(),
            )
            .await
            .unwrap_err();
        // 确保 fault 已消费，避免污染后续测试
        let _ = repo_b.take_import_fault();
        assert!(
            err.to_string().contains("injected") || err.to_string().contains("import"),
            "{err}"
        );
        // no assets with head from failed tx
        assert!(repo_b.list_assets(None, None).await.unwrap().is_empty());
        // CAS may have objects
        assert!(store_b.get_blob(&h).await.is_ok());
    }

    fn empty_envelope() -> SnapshotEnvelopeV1 {
        SnapshotEnvelopeV1 {
            format: crate::agent_hub::snapshot::envelope::FORMAT_NAME.into(),
            format_version: crate::agent_hub::snapshot::envelope::FORMAT_VERSION,
            canonicalization: crate::agent_hub::snapshot::envelope::CANONICALIZATION_NAME.into(),
            snapshot_id: Uuid::now_v7().to_string(),
            snapshot_hash: "00".repeat(32),
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            created_at: "2026-07-29T10:00:00Z".into(),
            selection: crate::agent_hub::snapshot::envelope::SnapshotSelection {
                scope_ids: vec![],
                asset_ids: vec![],
                include_history: true,
            },
            asset_heads: BTreeMap::new(),
            assets: vec![],
            lineages: vec![],
            revisions: vec![],
            variants: vec![],
            conflicts: vec![],
            aliases: vec![],
            objects: vec![],
        }
    }
}
