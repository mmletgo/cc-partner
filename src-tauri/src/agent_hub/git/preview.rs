//! agent_hub/git/preview — Git device-lane 只读检查 / 确认导入
//!
//! Business Logic（为什么需要这个模块）:
//!     远端 device lane 经 fetch 后只可 inventory + preview；必须用户 confirm 且 hash
//!     精确匹配后才进入 Hub。preview 不得产生 Hub revision / projection；凭据只以
//!     boolean/label 暴露，禁止日志/DTO 打印 secret 正文。
//!
//! Code Logic（这个模块做什么）:
//!     `inspect_git_lanes` 枚举 workdir lanes 并校验 snapshot.json；
//!     `preview_git_import` repack + 与本地 head 比较（added/modified/deleted/conflict）；
//!     `confirm_git_import` 重验 hash，stale → previewStale，否则 SnapshotImporter::commit_import。

use crate::agent_hub::git::lane::{device_lane_abs_path, inventory_agent_hub_device_lanes};
use crate::agent_hub::models::AssetKind;
use crate::agent_hub::object_store::ObjectStore;
use crate::agent_hub::snapshot::archive::repack_readable_archive;
use crate::agent_hub::snapshot::envelope::{
    compute_snapshot_hash, default_snapshot_limits, SnapshotEnvelopeV1,
};
use crate::agent_hub::snapshot::importer::{
    ConfirmedImportSelection, ConfirmedProjectMapping, ProjectMappingCandidate,
    ResolvedProjectMapping, SnapshotImportOutcome, SnapshotImporter, ValidatedSnapshot,
};
use crate::cloud_sync::engine::cloud_sync_workdir;
use crate::error::AppError;
use crate::state::AppState;
use crate::storage::{AgentHubRepo, UpsertAgentHubProjectMapping};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// 单条 device lane 清单摘要（inspect 阶段）。
///
/// Business Logic: 用户先看有哪些远端 lane 与是否可预览，不触发 import。
/// Code Logic: camelCase DTO；corrupt 独立标记。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLaneSummary {
    /// lane device id（目录名）
    pub lane_device_id: String,
    /// 校验通过的 snapshotHash；corrupt 时为空串
    pub snapshot_hash: String,
    /// snapshotId；corrupt 时为空
    pub snapshot_id: String,
    /// 源 replica
    pub source_replica_id: String,
    /// 资产数（corrupt=0）
    pub asset_count: u64,
    /// revision 数
    pub revision_count: u64,
    /// ok | corrupt | missing
    pub status: String,
    /// 错误码（脱敏，无 path/secret）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// Git 资产 diff 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GitAssetChangeKind {
    /// 远端有、本地无
    Added,
    /// 同一 asset 不同 head
    Modified,
    /// 远端 tombstone 或本地有远端无
    Deleted,
    /// 本地 unresolved conflict 或双 head 冲突预估
    Conflict,
    /// 一致
    Unchanged,
}

impl GitAssetChangeKind {
    /// wire token。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Conflict => "conflict",
            Self::Unchanged => "unchanged",
        }
    }
}

/// 单资产预览条目（仅计数/hash/label，无 secret）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitAssetDiffEntry {
    pub asset_id: String,
    pub kind: String,
    pub logical_key: String,
    pub display_name: String,
    pub change_kind: GitAssetChangeKind,
    /// 是否 credential-bearing（仅 boolean，无 secret）
    pub has_credential: bool,
    /// 本地 current head revision（若有）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_head: Option<String>,
    /// 远端 asset head（若有）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_head: Option<String>,
    /// 远端是否 tombstone
    pub remote_deleted: bool,
}

/// 资产变更计数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitAssetChangeCounts {
    pub added: u64,
    pub modified: u64,
    pub deleted: u64,
    pub conflict: u64,
    pub unchanged: u64,
    pub credential_bearing: u64,
}

/// Git import 预览（用户 confirm 前）。
///
/// Business Logic: 展示 counts/hashes/mappings；零 Hub 写入。
/// Code Logic: camelCase；含 SnapshotImportPreview 的 mapping 部分。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitImportPreview {
    pub lane_device_id: String,
    pub snapshot_id: String,
    pub snapshot_hash: String,
    pub source_replica_id: String,
    pub asset_count: u64,
    pub revision_count: u64,
    pub change_counts: GitAssetChangeCounts,
    pub assets: Vec<GitAssetDiffEntry>,
    pub project_candidates: Vec<ProjectMappingCandidate>,
    pub resolved_mappings: Vec<ResolvedProjectMapping>,
    /// 明文备份披露（无 secret 值）
    pub plaintext_backup_disclosure: String,
    /// 是否含 credential-bearing assets
    pub has_credential_bearing_assets: bool,
}

/// inspect 总报告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLaneInspectReport {
    /// cloud workdir 是否存在
    pub workdir_present: bool,
    pub lanes: Vec<GitLaneSummary>,
    /// 本机 device id（用于 UI 标注）
    pub local_device_id: String,
}

/// 确认 import 请求。
///
/// Business Logic: 必须携带 preview 的 exact hash；selectedAssetIds 限制导入子集。
/// Code Logic: camelCase deny 未知字段由 serde 控制。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmGitImportRequest {
    pub lane_device_id: String,
    pub snapshot_hash: String,
    #[serde(default)]
    pub selected_asset_ids: Vec<String>,
    #[serde(default)]
    pub project_mappings: Vec<ConfirmedProjectMapping>,
    /// 未映射 project 是否仍导入 canonical（默认 true，不自动 opt-in）
    #[serde(default = "default_true")]
    pub import_unmapped_projects: bool,
}

fn default_true() -> bool {
    true
}

/// 确认 import 结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmGitImportOutcome {
    pub lane_device_id: String,
    pub snapshot_hash: String,
    pub import: SnapshotImportOutcome,
    /// 导入后 mapping 状态（可能仍 unmapped / not opted-in）
    pub resolved_mappings: Vec<ResolvedProjectMapping>,
}

/// 确认 project mapping（可不 opt-in）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmProjectMappingRequest {
    pub hub_project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_workbench_project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_remote_fingerprint: Option<String>,
    /// 显式 opt-in；默认 false
    #[serde(default)]
    pub opted_in: bool,
}

/// 明文备份披露文案（中/英混合稳定 token，前端再 i18n）。
pub const PLAINTEXT_BACKUP_DISCLOSURE: &str =
    "Hub snapshots store credential-bearing assets as plaintext bytes in CAS/archive. Secrets are never shown in this preview.";

/// 枚举 cloud workdir 中全部 device lane（只读）。
///
/// Business Logic（为什么需要这个函数）:
///     用户需在 confirm 前看到远端 lane 列表；corrupt lane 不得阻断其它 lane。
///
/// Code Logic（这个函数做什么）:
///     inventory 目录 → 逐 lane repack/validate → DTO；不写 Hub。
pub async fn inspect_git_lanes_for_state(
    state: &AppState,
) -> Result<GitLaneInspectReport, AppError> {
    let workdir = cloud_sync_workdir();
    inspect_git_lanes_in_workdir(&workdir, state.device_id.as_str())
}

/// 纯 workdir inspect（可测）。
///
/// Business Logic: 测试与生产共用路径逻辑。
/// Code Logic: 不依赖 AppState。
pub fn inspect_git_lanes_in_workdir(
    workdir: &Path,
    local_device_id: &str,
) -> Result<GitLaneInspectReport, AppError> {
    let present = workdir.is_dir();
    if !present {
        return Ok(GitLaneInspectReport {
            workdir_present: false,
            lanes: Vec::new(),
            local_device_id: local_device_id.to_string(),
        });
    }
    let ids = inventory_agent_hub_device_lanes(workdir)?;
    let mut lanes = Vec::with_capacity(ids.len());
    for id in ids {
        lanes.push(summarize_lane(workdir, &id));
    }
    Ok(GitLaneInspectReport {
        workdir_present: true,
        lanes,
        local_device_id: local_device_id.to_string(),
    })
}

/// 预览单 lane import（零 Hub 写入）。
///
/// Business Logic: 展示 add/mod/del/conflict 计数与 mapping 候选；secret 仅 boolean。
/// Code Logic: repack → compare local assets → SnapshotImporter::inspect_import。
pub async fn preview_git_import_for_state(
    state: &AppState,
    lane_device_id: &str,
) -> Result<GitImportPreview, AppError> {
    let workdir = cloud_sync_workdir();
    preview_git_import_in_workdir(&workdir, &state.agent_hub_repo, lane_device_id).await
}

/// 纯 workdir + repo preview（可测）。
pub async fn preview_git_import_in_workdir(
    workdir: &Path,
    repo: &AgentHubRepo,
    lane_device_id: &str,
) -> Result<GitImportPreview, AppError> {
    let built = load_lane_snapshot(workdir, lane_device_id)?;
    let env = &built.envelope;
    let validated = ValidatedSnapshot::from_parts(
        env.clone(),
        built.object_bytes.clone(),
        Some(default_snapshot_limits()),
    )?;

    // inspect_import 只读 mappings，不写库；ObjectStore 仅用于构造 importer 句柄
    let data_dir = crate::config::data_dir().unwrap_or_else(|_| {
        std::env::temp_dir().join(format!("cc-partner-preview-{}", std::process::id()))
    });
    let _ = std::fs::create_dir_all(&data_dir);
    let objects = ObjectStore::open(&data_dir)?;
    let importer = SnapshotImporter::new(repo.clone(), objects, &data_dir);
    let import_preview = importer.inspect_import(&validated).await?;

    let (assets, change_counts) = diff_remote_against_local(repo, env).await?;
    let credential_bearing = change_counts.credential_bearing;
    Ok(GitImportPreview {
        lane_device_id: lane_device_id.to_string(),
        snapshot_id: env.snapshot_id.clone(),
        snapshot_hash: env.snapshot_hash.clone(),
        source_replica_id: env.source_replica_id.clone(),
        asset_count: env.assets.len() as u64,
        revision_count: env.revisions.len() as u64,
        change_counts,
        assets,
        project_candidates: import_preview.project_candidates,
        resolved_mappings: import_preview.resolved_mappings,
        plaintext_backup_disclosure: PLAINTEXT_BACKUP_DISCLOSURE.to_string(),
        has_credential_bearing_assets: credential_bearing > 0,
    })
}

/// 确认 import：hash 精确匹配后调用 SnapshotImporter。
///
/// Business Logic: fetch 后 lane hash 变化 → previewStale，永不在旧确认下导入新快照。
/// Code Logic: load → compare full-lane hash → 可选过滤 asset 并重算 selection-local hash → commit_import。
pub async fn confirm_git_import_for_state(
    state: &AppState,
    request: ConfirmGitImportRequest,
) -> Result<ConfirmGitImportOutcome, AppError> {
    let workdir = cloud_sync_workdir();
    let data_dir = crate::config::data_dir()?;
    confirm_git_import_in_workdir(&workdir, state.agent_hub_repo.as_ref(), &data_dir, request).await
}

/// workdir + repo confirm（生产与单测共用；无需完整 AppState）。
///
/// Business Logic: lane-level `snapshotHash` 门闩永远对照未过滤 full-lane；子集仅影响 importer 载荷。
/// Code Logic: load → full-lane hash gate → 可选 filter 后 recompute selection-local hash → validate → commit。
pub async fn confirm_git_import_in_workdir(
    workdir: &Path,
    repo: &AgentHubRepo,
    data_dir: &Path,
    request: ConfirmGitImportRequest,
) -> Result<ConfirmGitImportOutcome, AppError> {
    let expected = request.snapshot_hash.trim();
    if expected.is_empty() {
        return Err(AppError::validation(
            "agent_hub_git_import_missing_snapshot_hash".to_string(),
        ));
    }
    let built = load_lane_snapshot(workdir, &request.lane_device_id)?;
    // Lane-level gate: 用户确认的是整 lane preview hash；不得在 stale lane 上导入。
    if built.envelope.snapshot_hash != expected {
        return Err(AppError::conflict("previewStale"));
    }

    // 可选子集：仅导入 selectedAssetIds（空=全部）。
    // 过滤后 envelope 内容变化，必须 recompute selection-local snapshotHash 才能通过
    // ValidatedSnapshot / validate_snapshot 完整性校验；lane gate 已在上面完成。
    let mut envelope = built.envelope;
    let mut object_bytes = built.object_bytes;
    if !request.selected_asset_ids.is_empty() {
        let selected: BTreeSet<String> = request
            .selected_asset_ids
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if selected.is_empty() {
            return Err(AppError::validation(
                "agent_hub_git_import_empty_selected_assets".to_string(),
            ));
        }
        // 未知 id 不静默吞掉：防止 UI 勾选了不存在的资产却导入全量
        let available: BTreeSet<String> = envelope.assets.iter().map(|a| a.id.clone()).collect();
        for id in &selected {
            if !available.contains(id) {
                return Err(AppError::validation(format!(
                    "agent_hub_git_import_unknown_asset:{id}"
                )));
            }
        }
        filter_envelope_to_assets(&mut envelope, &mut object_bytes, &selected)?;
        envelope.snapshot_hash = compute_snapshot_hash(&envelope).map_err(|e| {
            AppError::validation(format!("agent_hub_git_import_selection_hash_failed:{e}"))
        })?;
    }

    let validated =
        ValidatedSnapshot::from_parts(envelope, object_bytes, Some(default_snapshot_limits()))?;
    let objects = ObjectStore::open(data_dir)?;
    let importer = SnapshotImporter::new(repo.clone(), objects, data_dir);
    let selection = ConfirmedImportSelection {
        project_mappings: request.project_mappings.clone(),
        import_unmapped_projects: request.import_unmapped_projects,
    };
    let outcome = importer.commit_import(validated, selection).await?;

    // 回报 mapping 状态（import 后）
    let mut resolved = Vec::new();
    for m in &request.project_mappings {
        if let Some(row) = repo
            .get_project_mapping_by_hub_project_id(&m.hub_project_id)
            .await?
        {
            resolved.push(ResolvedProjectMapping {
                hub_project_id: m.hub_project_id.clone(),
                local_workbench_project_id: row.local_workbench_project_id,
                opted_in: row.opted_in,
            });
        } else {
            resolved.push(ResolvedProjectMapping {
                hub_project_id: m.hub_project_id.clone(),
                local_workbench_project_id: m.local_workbench_project_id.clone(),
                opted_in: m.opted_in,
            });
        }
    }

    Ok(ConfirmGitImportOutcome {
        lane_device_id: request.lane_device_id,
        // 对外回报用户确认的 lane-level hash（非 selection-local recompute）
        snapshot_hash: expected.to_string(),
        import: outcome,
        resolved_mappings: resolved,
    })
}

/// 保存确认的 project mapping（不自动 opt-in）。
///
/// Business Logic: 未映射 project 可导入 canonical，但 mapping 必须用户显式确认；
///     opted_in 默认 false，需另走 enable preview。
///
/// Code Logic: upsert_project_mapping。
pub async fn confirm_project_mapping_for_state(
    state: &AppState,
    request: ConfirmProjectMappingRequest,
) -> Result<ResolvedProjectMapping, AppError> {
    let hub = request.hub_project_id.trim();
    if hub.is_empty() {
        return Err(AppError::validation(
            "agent_hub_project_mapping_missing_hub_id".to_string(),
        ));
    }
    // 若 opted_in=true 必须有 local workbench id（禁止无路径 opt-in）
    let missing_local = request
        .local_workbench_project_id
        .as_ref()
        .map(|s| s.trim().is_empty())
        .unwrap_or(true);
    if request.opted_in && missing_local {
        return Err(AppError::validation(
            "agent_hub_project_mapping_opt_in_requires_local_project".to_string(),
        ));
    }
    let local_path = if let Some(ref wb) = request.local_workbench_project_id {
        // best-effort 读 workbench path；失败仍保存 mapping 无 path
        match state.workbench_project_repo.get(wb).await {
            Ok(Some(p)) => Some(p.path),
            _ => None,
        }
    } else {
        None
    };
    let row = state
        .agent_hub_repo
        .upsert_project_mapping(UpsertAgentHubProjectMapping {
            hub_project_id: hub.to_string(),
            local_workbench_project_id: request
                .local_workbench_project_id
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            git_remote_fingerprint: request.git_remote_fingerprint.clone(),
            local_absolute_path: local_path,
            opted_in: request.opted_in,
        })
        .await?;
    Ok(ResolvedProjectMapping {
        hub_project_id: row.hub_project_id,
        local_workbench_project_id: row.local_workbench_project_id,
        opted_in: row.opted_in,
    })
}

// ── internal helpers ────────────────────────────────────────────────────────

/// 汇总单 lane。
fn summarize_lane(workdir: &Path, lane_device_id: &str) -> GitLaneSummary {
    match load_lane_snapshot(workdir, lane_device_id) {
        Ok(built) => GitLaneSummary {
            lane_device_id: lane_device_id.to_string(),
            snapshot_hash: built.envelope.snapshot_hash,
            snapshot_id: built.envelope.snapshot_id,
            source_replica_id: built.envelope.source_replica_id,
            asset_count: built.envelope.assets.len() as u64,
            revision_count: built.envelope.revisions.len() as u64,
            status: "ok".into(),
            error_code: None,
        },
        Err(e) => {
            let code = e.code().to_string();
            let status = if code.contains("missing") {
                "missing"
            } else {
                "corrupt"
            };
            GitLaneSummary {
                lane_device_id: lane_device_id.to_string(),
                snapshot_hash: String::new(),
                snapshot_id: String::new(),
                source_replica_id: String::new(),
                asset_count: 0,
                revision_count: 0,
                status: status.into(),
                error_code: Some(stable_error_code(&code)),
            }
        }
    }
}

/// 加载并校验 lane snapshot（repack）。
fn load_lane_snapshot(
    workdir: &Path,
    lane_device_id: &str,
) -> Result<crate::agent_hub::snapshot::builder::BuiltSnapshot, AppError> {
    ensure_valid_device_id(lane_device_id)?;
    let lane = device_lane_abs_path(workdir, lane_device_id)?;
    if !lane.is_dir() {
        return Err(AppError::not_found(format!(
            "agent_hub_git_lane_missing:{lane_device_id}"
        )));
    }
    repack_readable_archive(&lane, &default_snapshot_limits())
}

/// 远端 envelope 与本地 Hub 比较。
async fn diff_remote_against_local(
    repo: &AgentHubRepo,
    env: &SnapshotEnvelopeV1,
) -> Result<(Vec<GitAssetDiffEntry>, GitAssetChangeCounts), AppError> {
    let local_assets = repo.list_all_assets_including_deleted().await?;
    let mut local_by_id: HashMap<String, crate::agent_hub::models::LogicalAsset> = HashMap::new();
    for a in local_assets {
        local_by_id.insert(a.id.clone(), a);
    }

    // unresolved conflicts
    let conflicts = repo.list_unresolved_conflicts().await.unwrap_or_default();
    let conflict_assets: BTreeSet<String> = conflicts.into_iter().map(|c| c.asset_id).collect();

    let mut counts = GitAssetChangeCounts::default();
    let mut entries = Vec::new();
    let mut remote_ids = BTreeSet::new();

    for remote in &env.assets {
        remote_ids.insert(remote.id.clone());
        let remote_head = env
            .asset_heads
            .get(&remote.id)
            .and_then(|h| h.first())
            .cloned();
        let has_credential = matches!(remote.kind, AssetKind::Mcp);
        if has_credential {
            counts.credential_bearing += 1;
        }
        let local = local_by_id.get(&remote.id);
        let change = if let Some(local) = local {
            let local_head = local.current_revision_id.as_ref().map(|r| r.0.clone());
            if conflict_assets.contains(&remote.id) {
                counts.conflict += 1;
                GitAssetChangeKind::Conflict
            } else if remote.deleted_at.is_some() && local.deleted_at.is_none() {
                counts.deleted += 1;
                GitAssetChangeKind::Deleted
            } else if remote.deleted_at.is_none() && local.deleted_at.is_some() {
                // 远端复活 / 本地 tombstone — 视为 modified
                counts.modified += 1;
                GitAssetChangeKind::Modified
            } else if local_head != remote_head {
                counts.modified += 1;
                GitAssetChangeKind::Modified
            } else {
                counts.unchanged += 1;
                GitAssetChangeKind::Unchanged
            }
        } else if remote.deleted_at.is_some() {
            // 远端 tombstone 且本地无 — 可忽略为 deleted 语义
            counts.deleted += 1;
            GitAssetChangeKind::Deleted
        } else {
            counts.added += 1;
            GitAssetChangeKind::Added
        };

        let local_head = local.and_then(|l| l.current_revision_id.as_ref().map(|r| r.0.clone()));
        entries.push(GitAssetDiffEntry {
            asset_id: remote.id.clone(),
            kind: remote.kind.as_str().to_string(),
            logical_key: remote.logical_key.clone(),
            display_name: remote.display_name.clone(),
            change_kind: change,
            has_credential,
            local_head,
            remote_head,
            remote_deleted: remote.deleted_at.is_some(),
        });
    }

    // 本地有、远端无：仅当远端与本地有交集时才报告 deleted（避免 partial selection 误伤）
    let overlap = local_by_id.keys().any(|id| remote_ids.contains(id));
    if overlap {
        for (id, local) in &local_by_id {
            if remote_ids.contains(id) {
                continue;
            }
            if local.deleted_at.is_some() {
                continue;
            }
            counts.deleted += 1;
            let has_credential = matches!(local.kind, AssetKind::Mcp);
            if has_credential {
                counts.credential_bearing += 1;
            }
            entries.push(GitAssetDiffEntry {
                asset_id: id.clone(),
                kind: local.kind.as_str().to_string(),
                logical_key: local.logical_key.clone(),
                display_name: local.display_name.clone(),
                change_kind: GitAssetChangeKind::Deleted,
                has_credential,
                local_head: local.current_revision_id.as_ref().map(|r| r.0.clone()),
                remote_head: None,
                remote_deleted: true,
            });
        }
    }

    entries.sort_by(|a, b| a.asset_id.cmp(&b.asset_id));
    Ok((entries, counts))
}

/// 按 selected asset ids 过滤 envelope（保持对象闭包）。
fn filter_envelope_to_assets(
    envelope: &mut SnapshotEnvelopeV1,
    object_bytes: &mut BTreeMap<String, Vec<u8>>,
    selected: &BTreeSet<String>,
) -> Result<(), AppError> {
    envelope.assets.retain(|a| selected.contains(&a.id));
    envelope
        .lineages
        .retain(|l| selected.contains(&l.root_asset_id) || selected.contains(&l.id));
    envelope.revisions.retain(|r| {
        selected.contains(&r.asset_lineage_id) || {
            // lineage id 常等于 asset id
            envelope
                .lineages
                .iter()
                .any(|l| l.id == r.asset_lineage_id && selected.contains(&l.root_asset_id))
        }
    });
    // 更稳妥：用 asset_heads key
    envelope
        .asset_heads
        .retain(|asset_id, _| selected.contains(asset_id));
    envelope.variants.retain(|v| selected.contains(&v.asset_id));
    envelope
        .conflicts
        .retain(|c| selected.contains(&c.asset_id));
    // 收集仍引用的 object hashes
    let mut keep: BTreeSet<String> = BTreeSet::new();
    for r in &envelope.revisions {
        if let Some(h) = &r.payload_hash {
            keep.insert(h.clone());
        }
        if let Some(h) = &r.tree_manifest_hash {
            keep.insert(h.clone());
        }
    }
    envelope.objects.retain(|o| keep.contains(&o.hash));
    object_bytes.retain(|h, _| keep.contains(h));
    envelope.selection.asset_ids = selected.iter().cloned().collect();
    Ok(())
}

/// 脱敏错误码（截断/剥 path）。
fn stable_error_code(raw: &str) -> String {
    let s = raw.split(':').next().unwrap_or(raw);
    let s = s.trim();
    if s.is_empty() {
        "agent_hub_git_lane_error".into()
    } else {
        s.chars().take(96).collect()
    }
}

/// 校验 device id 单路径段（与 lane.rs 一致）。
fn ensure_valid_device_id(device_id: &str) -> Result<(), AppError> {
    let id = device_id.trim();
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
    {
        return Err(AppError::validation(
            "agent_hub_git_invalid_device_id".to_string(),
        ));
    }
    Ok(())
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{
        AssetKind, AssetPolicy, NewLogicalAsset, NewRevision, NewScopeNode, RevisionId,
        RevisionOperation, RevisionOriginKind, ScopeKind,
    };
    use crate::agent_hub::object_store::sha256_hex;
    use crate::agent_hub::snapshot::archive::expand_readable_archive;
    use crate::agent_hub::snapshot::builder::BuiltSnapshot;
    use crate::agent_hub::snapshot::envelope::{
        compute_snapshot_hash, SnapshotAlias, SnapshotAsset, SnapshotEnvelopeV1, SnapshotLineage,
        SnapshotObjectDescriptor, SnapshotRevision, SnapshotSelection, CANONICALIZATION_NAME,
        FORMAT_NAME, FORMAT_VERSION,
    };
    use crate::storage::agent_hub_repo::AgentHubRepo;
    use crate::storage::maintenance_gate::DatabaseMaintenanceGate;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::{fs, io::Write};

    async fn test_repo() -> (tempfile::TempDir, AgentHubRepo) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}?mode=rwc", db.display()))
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON;")
            .execute(&pool)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        let gate = Arc::new(DatabaseMaintenanceGate::new());
        let repo = AgentHubRepo::with_gate(pool, gate);
        (dir, repo)
    }

    fn write_file(path: &Path, body: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    fn make_lane_snapshot(
        asset_id: &str,
        payload: &[u8],
        kind: AssetKind,
        secret_label: &str,
        rev_suffix: u8,
    ) -> BuiltSnapshot {
        let mut object_bytes = BTreeMap::new();
        let hash = sha256_hex(payload);
        object_bytes.insert(hash.clone(), payload.to_vec());
        // 固定合法 UUID（仅 hex 字符）
        let rev_id = format!("01900000-0000-7000-8000-0000000000{rev_suffix:02x}");
        let snap_id = format!("01900000-0000-7000-8000-0000000001{rev_suffix:02x}");
        let replica = "01900000-0000-7000-8000-0000000000b1";
        let mut env = SnapshotEnvelopeV1 {
            format: FORMAT_NAME.into(),
            format_version: FORMAT_VERSION,
            canonicalization: CANONICALIZATION_NAME.into(),
            snapshot_id: snap_id,
            snapshot_hash: String::new(),
            source_replica_id: replica.into(),
            created_at: "2026-07-29T12:00:00Z".into(),
            selection: SnapshotSelection {
                scope_ids: vec!["scope-user".into()],
                asset_ids: vec![asset_id.into()],
                include_history: true,
            },
            asset_heads: BTreeMap::from([(asset_id.into(), vec![rev_id.clone()])]),
            assets: vec![SnapshotAsset {
                id: asset_id.into(),
                scope_id: "scope-user".into(),
                kind,
                origin_namespace: "standalone".into(),
                logical_key: secret_label.into(),
                display_name: secret_label.into(),
                policy: AssetPolicy::Shared,
                deleted_at: None,
            }],
            lineages: vec![SnapshotLineage {
                id: asset_id.into(),
                root_asset_id: asset_id.into(),
            }],
            revisions: vec![SnapshotRevision {
                id: rev_id,
                asset_lineage_id: asset_id.into(),
                parents: vec![],
                generation: "1".into(),
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: replica.into(),
                payload_hash: Some(hash.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T12:00:00Z".into(),
            }],
            variants: vec![],
            conflicts: vec![],
            aliases: vec![SnapshotAlias {
                kind: "hubProjectId".into(),
                external_id: "hub-remote-proj".into(),
                local_id: "hub-remote-proj".into(),
            }],
            objects: vec![SnapshotObjectDescriptor {
                hash: hash.clone(),
                size: payload.len().to_string(),
            }],
        };
        env.snapshot_hash = compute_snapshot_hash(&env).unwrap();
        BuiltSnapshot {
            envelope: env,
            object_bytes,
            selection_hash: "sel".into(),
            selection_state_hash: "state".into(),
        }
    }

    /// Business Logic: corrupt lane 独立 blocked，不阻止其它 lane inventory。
    #[test]
    fn inspect_isolates_corrupt_lane() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        write_file(
            &workdir
                .join("agent-hub")
                .join("devices")
                .join("device-bad")
                .join("snapshot.json"),
            "{not-json",
        );
        write_file(
            &workdir
                .join("agent-hub")
                .join("devices")
                .join("device-empty")
                .join("snapshot.json"),
            r#"{"format":"cc-partner-agent-hub"}"#,
        );
        let report = inspect_git_lanes_in_workdir(workdir, "local-me").unwrap();
        assert!(report.workdir_present);
        assert_eq!(report.lanes.len(), 2);
        assert!(report.lanes.iter().all(|l| l.status != "ok"));
        assert!(report
            .lanes
            .iter()
            .any(|l| l.lane_device_id == "device-bad"));
        assert!(report
            .lanes
            .iter()
            .any(|l| l.lane_device_id == "device-empty"));
    }

    /// Business Logic: preview 展示 add + credential boolean，零 Hub revision。
    #[tokio::test]
    async fn preview_remote_lane_counts_without_hub_writes() {
        let (dir, repo) = test_repo().await;
        let objects = ObjectStore::open(dir.path()).unwrap();
        let scope = repo
            .insert_scope(NewScopeNode {
                id: Some("scope-user".into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let local_asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "local-only".into(),
                display_name: "Local Only".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let local_bytes = br#"{"blocks":[]}"#;
        let local_hash = objects.put_blob(local_bytes).await.unwrap().hash;
        repo.append_revision(NewRevision {
            id: RevisionId("01900000-0000-7000-8000-000000000001".into()),
            asset_lineage_id: local_asset.id.clone(),
            parents: vec![],
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: "local".into(),
            payload_hash: Some(local_hash),
            tree_manifest_hash: None,
            created_at: "2026-07-29T10:00:00Z".into(),
            expected_parent_id: None,
        })
        .await
        .unwrap();

        // remote MCP lane with secret payload + second instruction asset
        let secret = br#"{"servers":{"s":{"env":{"TOKEN":"super-secret-value"}}}}"#;
        let built = make_lane_snapshot("remote-mcp-1", secret, AssetKind::Mcp, "mcp-1", 0x11);
        let built2 = make_lane_snapshot(
            "remote-added-1",
            br#"{"blocks":[{"id":"b1"}]}"#,
            AssetKind::Instruction,
            "remote-new",
            0x22,
        );
        // merge assets into one lane envelope（revisions/objects 需排序唯一）
        let mut object_bytes = built.object_bytes.clone();
        object_bytes.extend(built2.object_bytes.clone());
        let mut env = built.envelope.clone();
        env.snapshot_id = "01900000-0000-7000-8000-0000000000c1".into();
        env.assets.extend(built2.envelope.assets.clone());
        env.lineages.extend(built2.envelope.lineages.clone());
        env.revisions.extend(built2.envelope.revisions.clone());
        env.revisions.sort_by(|a, b| a.id.cmp(&b.id));
        env.asset_heads.extend(built2.envelope.asset_heads.clone());
        env.objects.extend(built2.envelope.objects.clone());
        env.objects.sort_by(|a, b| a.hash.cmp(&b.hash));
        env.objects.dedup_by(|a, b| a.hash == b.hash);
        env.selection.asset_ids = env.assets.iter().map(|a| a.id.clone()).collect();
        env.snapshot_hash = compute_snapshot_hash(&env).unwrap();
        let combined = BuiltSnapshot {
            envelope: env,
            object_bytes,
            selection_hash: "s".into(),
            selection_state_hash: "t".into(),
        };

        let workdir = dir.path().join("cloud-sync");
        let lane = workdir
            .join("agent-hub")
            .join("devices")
            .join("device-remote");
        expand_readable_archive(&combined, &lane).unwrap();

        let before = repo
            .list_all_assets_including_deleted()
            .await
            .unwrap()
            .len();
        let preview = preview_git_import_in_workdir(&workdir, &repo, "device-remote")
            .await
            .unwrap();
        assert_eq!(preview.lane_device_id, "device-remote");
        assert!(!preview.snapshot_hash.is_empty());
        assert!(preview.change_counts.added >= 2);
        assert!(preview.has_credential_bearing_assets);
        assert!(preview.change_counts.credential_bearing >= 1);
        let ser = serde_json::to_string(&preview).unwrap();
        assert!(
            !ser.contains("super-secret-value"),
            "preview must not leak secret"
        );
        let after = repo
            .list_all_assets_including_deleted()
            .await
            .unwrap()
            .len();
        assert_eq!(before, after, "preview must write zero hub assets");
        let _ = built2;
    }

    /// Business Logic: 错误 snapshotHash 经生产 confirm 路径 fail-closed → previewStale，零 revision 写入。
    #[tokio::test]
    async fn confirm_rejects_stale_preview_hash() {
        let (dir, repo) = test_repo().await;
        let _ = repo
            .insert_scope(NewScopeNode {
                id: Some("scope-user".into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let built =
            make_lane_snapshot("a1", br#"{"blocks":[]}"#, AssetKind::Instruction, "k", 0x33);
        let workdir = dir.path().join("cloud-sync");
        let data_dir = dir.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        let lane = workdir.join("agent-hub").join("devices").join("device-x");
        expand_readable_archive(&built, &lane).unwrap();
        let real = built.envelope.snapshot_hash.clone();
        assert!(!real.is_empty());

        let before_assets = repo
            .list_all_assets_including_deleted()
            .await
            .unwrap()
            .len();

        let err = confirm_git_import_in_workdir(
            &workdir,
            &repo,
            &data_dir,
            ConfirmGitImportRequest {
                lane_device_id: "device-x".into(),
                snapshot_hash: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
                    .into(),
                selected_asset_ids: vec![],
                project_mappings: vec![],
                import_unmapped_projects: true,
            },
        )
        .await
        .expect_err("stale hash must fail closed");
        assert_eq!(err.code(), "previewStale");
        assert_eq!(err.ipc_category_code(), "conflict");

        let after_assets = repo
            .list_all_assets_including_deleted()
            .await
            .unwrap()
            .len();
        assert_eq!(
            before_assets, after_assets,
            "stale confirm must write zero hub assets"
        );

        // Optional path: mutate on-disk lane hash between preview and confirm.
        let mut on_disk = load_lane_snapshot(&workdir, "device-x").unwrap();
        assert_eq!(on_disk.envelope.snapshot_hash, real);
        on_disk.envelope.snapshot_id = "01900000-0000-7000-8000-00000000beef".into();
        on_disk.envelope.snapshot_hash = compute_snapshot_hash(&on_disk.envelope).unwrap();
        // 覆盖 lane 文件：expand 会写新 snapshot.json/objects
        expand_readable_archive(&on_disk, &lane).unwrap();
        assert_ne!(on_disk.envelope.snapshot_hash, real);

        let err2 = confirm_git_import_in_workdir(
            &workdir,
            &repo,
            &data_dir,
            ConfirmGitImportRequest {
                lane_device_id: "device-x".into(),
                snapshot_hash: real,
                selected_asset_ids: vec![],
                project_mappings: vec![],
                import_unmapped_projects: true,
            },
        )
        .await
        .expect_err("mutated lane must reject old preview hash");
        assert_eq!(err2.code(), "previewStale");
        let after_mut = repo
            .list_all_assets_including_deleted()
            .await
            .unwrap()
            .len();
        assert_eq!(before_assets, after_mut);
    }

    /// Business Logic: 非空 proper subset selectedAssetIds 必须成功导入，不得因 self-filter 触发 HashMismatch。
    #[tokio::test]
    async fn confirm_subset_selected_assets_succeeds() {
        let (dir, repo) = test_repo().await;
        let _ = repo
            .insert_scope(NewScopeNode {
                id: Some("scope-user".into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();

        // two-asset lane
        let built_a = make_lane_snapshot(
            "remote-a",
            br#"{"blocks":[{"id":"a"}]}"#,
            AssetKind::Instruction,
            "asset-a",
            0x41,
        );
        let built_b = make_lane_snapshot(
            "remote-b",
            br#"{"blocks":[{"id":"b"}]}"#,
            AssetKind::Instruction,
            "asset-b",
            0x42,
        );
        let mut object_bytes = built_a.object_bytes.clone();
        object_bytes.extend(built_b.object_bytes.clone());
        let mut env = built_a.envelope.clone();
        env.snapshot_id = "01900000-0000-7000-8000-0000000000ab".into();
        env.assets.extend(built_b.envelope.assets.clone());
        env.lineages.extend(built_b.envelope.lineages.clone());
        env.revisions.extend(built_b.envelope.revisions.clone());
        env.revisions.sort_by(|a, b| a.id.cmp(&b.id));
        env.asset_heads.extend(built_b.envelope.asset_heads.clone());
        env.objects.extend(built_b.envelope.objects.clone());
        env.objects.sort_by(|a, b| a.hash.cmp(&b.hash));
        env.objects.dedup_by(|a, b| a.hash == b.hash);
        env.selection.asset_ids = env.assets.iter().map(|a| a.id.clone()).collect();
        env.snapshot_hash = compute_snapshot_hash(&env).unwrap();
        let combined = BuiltSnapshot {
            envelope: env,
            object_bytes,
            selection_hash: "s".into(),
            selection_state_hash: "t".into(),
        };
        let full_hash = combined.envelope.snapshot_hash.clone();

        let workdir = dir.path().join("cloud-sync");
        let data_dir = dir.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        let lane = workdir
            .join("agent-hub")
            .join("devices")
            .join("device-subset");
        expand_readable_archive(&combined, &lane).unwrap();

        // Proper subset: only remote-a
        let outcome = confirm_git_import_in_workdir(
            &workdir,
            &repo,
            &data_dir,
            ConfirmGitImportRequest {
                lane_device_id: "device-subset".into(),
                snapshot_hash: full_hash.clone(),
                selected_asset_ids: vec!["remote-a".into()],
                project_mappings: vec![],
                import_unmapped_projects: true,
            },
        )
        .await
        .expect("subset confirm must succeed without HashMismatch");

        assert_eq!(outcome.lane_device_id, "device-subset");
        assert_eq!(
            outcome.snapshot_hash, full_hash,
            "response must echo lane-level confirmed hash"
        );
        assert!(
            outcome
                .import
                .imported_asset_ids
                .contains(&"remote-a".into()),
            "selected asset must import: {:?}",
            outcome.import.imported_asset_ids
        );
        assert!(
            !outcome
                .import
                .imported_asset_ids
                .contains(&"remote-b".into()),
            "unselected asset must not import: {:?}",
            outcome.import.imported_asset_ids
        );
        assert!(
            outcome.import.inserted_revisions >= 1,
            "subset import must insert revisions: {:?}",
            outcome.import
        );

        let assets = repo.list_all_assets_including_deleted().await.unwrap();
        let ids: BTreeSet<_> = assets.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains("remote-a"));
        assert!(!ids.contains("remote-b"));
    }

    /// Business Logic: mapping 默认 not opted-in。
    #[tokio::test]
    async fn confirm_mapping_defaults_not_opted_in() {
        let (_dir, repo) = test_repo().await;
        let row = repo
            .upsert_project_mapping(UpsertAgentHubProjectMapping {
                hub_project_id: "hub-1".into(),
                local_workbench_project_id: Some("wb-1".into()),
                git_remote_fingerprint: Some("fp".into()),
                local_absolute_path: None,
                opted_in: false,
            })
            .await
            .unwrap();
        assert!(!row.opted_in);
        assert_eq!(row.local_workbench_project_id.as_deref(), Some("wb-1"));
    }
}
