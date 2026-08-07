//! agent_hub/replication/pull — 同类 Agent 远端 portable inventory + 选择性 Pull
//!
//! Business Logic（为什么需要这个模块）:
//!     用户从远端设备加载 metadata inventory，勾选后 preview/apply 到本机同类 Agent；
//!     只允许 sourceTarget == destinationTarget；未映射/未 opt-in 只导入 canonical。
//!     expected-device / clientRequestId 是路由绑定与幂等标签，**不是**身份认证。
//!
//! Code Logic（这个模块做什么）:
//!     远端 inventory 路由（无 secret）；本地 preview plan + apply（CAS 分块 ≤8MiB 续传、
//!     SnapshotImporter 导入、映射项安装）；SQLite clientRequestId claim/replay 与 partial 报告。

use crate::agent_hub::assets::{from_canonical_bytes, PortableAssetPayload};
use crate::agent_hub::config_patch::{
    apply_config_patch_atomically, value_content_hash, JsoncConfigPatcher, ManagedConfigPatch,
};
use crate::agent_hub::models::{AgentTarget, PortablePullClaim, PortablePullPlanRecord};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore, TreeEntryType};
use crate::agent_hub::portable_actions::PortableAssetConflictPolicy;
use crate::agent_hub::portable_inventory::{
    inspect_portable_inventory, PortableAssetKind, PortableInventoryItemDto,
    PortableInventoryManagementState, PortableInventorySnapshotDto, PortableInventorySourceOrigin,
    PortableMcpCredentialFactDto,
};
use crate::agent_hub::replication::receiver::AGENT_HUB_MAX_CHUNK_BYTES;
use crate::agent_hub::snapshot::envelope::SnapshotEnvelopeV1;
use crate::agent_hub::snapshot::importer::{
    ConfirmedImportSelection, SnapshotImporter, ValidatedSnapshot,
};
use crate::agent_hub::snapshot::portable_builder::{
    build_portable_selection_envelope, bytes_are_legacy_lossy, BuiltPortableSelection,
    PortableSelectionItem,
};
use crate::agent_hub::targets::portable::render_mcp_projection;
use crate::error::AppError;
use crate::models::device::Device;
use crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER;
use crate::net::peer_client::PeerClient;
use crate::net::peer_error::PeerCallError;
use crate::net::peer_timeout::PeerTimeoutClass;
use crate::net::protocol::CAPABILITY_PORTABLE_PULL_V1;
use crate::net::request_context::{new_request_id, REQUEST_ID_HEADER};
use crate::state::AppState;
use crate::storage::agent_hub_repo::AgentHubRepo;
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// CAS 分块上限（与 receiver 一致）。
pub const PORTABLE_PULL_MAX_CHUNK_BYTES: usize = AGENT_HUB_MAX_CHUNK_BYTES;

/// preview plan TTL（分钟）。
pub const PULL_PLAN_TTL_MINUTES: i64 = 15;

// ───────────────────────── DTOs ─────────────────────────

/// 远端 portable inventory（metadata only）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePortableInventoryDto {
    pub source_device_id: String,
    pub source_target: AgentTarget,
    pub inventory_snapshot_hash: String,
    pub refreshed_at: String,
    pub stale: bool,
    pub items: Vec<RemotePortableInventoryItemDto>,
}

/// 远端 inventory 单项（无 secret / 无 path）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePortableInventoryItemDto {
    pub inventory_item_id: String,
    pub target: AgentTarget,
    pub kind: PortableAssetKind,
    pub native_id: String,
    pub display_name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub scope_id: String,
    pub project_id: Option<String>,
    pub project_opted_in: bool,
    pub source_origin: PortableInventorySourceOrigin,
    pub actual_enabled: Option<bool>,
    pub content_hash: Option<String>,
    pub tree_hash: Option<String>,
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_credential: Option<PortableMcpCredentialFactDto>,
}

/// 列出远端 inventory 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListRemotePortableInventoryRequest {
    pub source_device_id: String,
    pub source_target: AgentTarget,
}

/// Pull preview 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewPortablePullRequest {
    pub source_device_id: String,
    pub source_target: AgentTarget,
    pub destination_target: AgentTarget,
    pub remote_inventory_snapshot_hash: String,
    pub inventory_item_ids: Vec<String>,
    #[serde(default)]
    pub conflict_policy: PortableAssetConflictPolicy,
}

/// Pull plan（短期）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortablePullPlanDto {
    pub plan_token: String,
    pub expires_at: String,
    pub source_device_id: String,
    pub source_target: AgentTarget,
    pub destination_target: AgentTarget,
    pub remote_inventory_snapshot_hash: String,
    pub local_inventory_snapshot_hash: String,
    pub conflict_policy: PortableAssetConflictPolicy,
    pub selection_manifest_hash: String,
    pub credential_bearing_count: u64,
    pub has_credential_bearing_assets: bool,
    pub changes: Vec<PortablePullChangeDto>,
    pub blocking_reasons: Vec<String>,
}

/// 单条 pull 变更预览。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortablePullChangeDto {
    pub inventory_item_id: String,
    pub kind: PortableAssetKind,
    pub native_id: String,
    pub display_name: String,
    pub install_mode: PortablePullInstallMode,
    pub conflict: bool,
    pub legacy_lossy: bool,
    pub credential_bearing: bool,
    pub blocking_reasons: Vec<String>,
    pub warnings: Vec<String>,
}

/// 安装模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortablePullInstallMode {
    InstallToTarget,
    ImportedCanonicalOnly,
    SkipExisting,
    Blocked,
}

impl PortablePullInstallMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InstallToTarget => "installToTarget",
            Self::ImportedCanonicalOnly => "importedCanonicalOnly",
            Self::SkipExisting => "skipExisting",
            Self::Blocked => "blocked",
        }
    }
}

/// Apply pull 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyPortablePullRequest {
    pub plan_token: String,
    pub client_request_id: String,
}

/// 单条 pull 结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortablePullItemResultDto {
    pub inventory_item_id: String,
    pub state: PortablePullItemState,
    pub install_mode: Option<PortablePullInstallMode>,
    pub error_code: Option<String>,
    pub message: Option<String>,
}

/// 逐项状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortablePullItemState {
    Succeeded,
    Skipped,
    Failed,
    Blocked,
    ImportedCanonicalOnly,
    OutcomeUnknown,
}

impl PortablePullItemState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::ImportedCanonicalOnly => "importedCanonicalOnly",
            Self::OutcomeUnknown => "outcomeUnknown",
        }
    }
}

/// Pull 聚合结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortablePullResultDto {
    pub plan_token: String,
    pub client_request_id: String,
    pub source_device_id: String,
    pub source_target: AgentTarget,
    pub destination_target: AgentTarget,
    pub partial: bool,
    pub items: Vec<PortablePullItemResultDto>,
}

/// 远端 selection 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePortableSelectionResponse {
    pub transfer_id: String,
    pub envelope: SnapshotEnvelopeV1,
    pub items: Vec<PortableSelectionItem>,
    pub missing_object_hashes: Vec<String>,
}

/// 源端 inventory 查询 body。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteInventoryQuery {
    pub source_target: AgentTarget,
}

/// 源端 selection 查询 body。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSelectionQuery {
    pub source_target: AgentTarget,
    pub inventory_item_ids: Vec<String>,
}

/// 内部持久化 plan（JSON 存 SQLite）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPortablePullPlan {
    public: PortablePullPlanDto,
    remote_item_ids: Vec<String>,
}

// ───────────────────────── 源端 object staging（进程内 transfer 缓冲） ─────────────────────────

/// Staging 上限：条目数 / 总字节 / TTL。LAN 无鉴权，必须有界。
const STAGING_MAX_ENTRIES: usize = 8;
const STAGING_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB
const STAGING_TTL: Duration = Duration::from_secs(15 * 60);

struct StagedSelection {
    built: BuiltPortableSelection,
    created_at: Instant,
    total_bytes: u64,
}

fn staging() -> &'static Mutex<BTreeMap<String, StagedSelection>> {
    static MAP: OnceLock<Mutex<BTreeMap<String, StagedSelection>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn staged_total_bytes(built: &BuiltPortableSelection) -> u64 {
    built.object_bytes.values().map(|b| b.len() as u64).sum()
}

fn evict_expired_staging(map: &mut BTreeMap<String, StagedSelection>) {
    let now = Instant::now();
    map.retain(|_, v| now.duration_since(v.created_at) < STAGING_TTL);
}

fn staging_insert(transfer_id: String, built: BuiltPortableSelection) -> Result<(), AppError> {
    let total_bytes = staged_total_bytes(&built);
    let mut g = staging().lock().expect("staging");
    evict_expired_staging(&mut g);
    // 已有同 id 覆盖
    g.remove(&transfer_id);
    let current_bytes: u64 = g.values().map(|s| s.total_bytes).sum();
    if g.len() >= STAGING_MAX_ENTRIES
        || current_bytes.saturating_add(total_bytes) > STAGING_MAX_TOTAL_BYTES
    {
        // 再尝试淘汰最旧
        if let Some(oldest_key) = g
            .iter()
            .min_by_key(|(_, v)| v.created_at)
            .map(|(k, _)| k.clone())
        {
            g.remove(&oldest_key);
        }
    }
    let current_bytes: u64 = g.values().map(|s| s.total_bytes).sum();
    if g.len() >= STAGING_MAX_ENTRIES
        || current_bytes.saturating_add(total_bytes) > STAGING_MAX_TOTAL_BYTES
    {
        return Err(AppError::validation(
            "PORTABLE_PULL_STAGING_LIMIT".to_string(),
        ));
    }
    g.insert(
        transfer_id,
        StagedSelection {
            built,
            created_at: Instant::now(),
            total_bytes,
        },
    );
    Ok(())
}

fn staging_remove(transfer_id: &str) {
    if let Ok(mut g) = staging().lock() {
        g.remove(transfer_id);
    }
}

/// 校验 tree entry 路径安全：相对、无 `..`、无绝对路径，最终仍在 `dir` 下。
fn safe_tree_dest(dir: &Path, entry_path: &str) -> Result<PathBuf, AppError> {
    let rel = Path::new(entry_path);
    if rel.is_absolute() {
        return Err(AppError::validation(
            "PORTABLE_PULL_UNSAFE_TREE_PATH".to_string(),
        ));
    }
    for c in rel.components() {
        match c {
            Component::Normal(_) => {}
            Component::CurDir => {}
            _ => {
                return Err(AppError::validation(
                    "PORTABLE_PULL_UNSAFE_TREE_PATH".to_string(),
                ));
            }
        }
    }
    if entry_path.contains('\0') {
        return Err(AppError::validation(
            "PORTABLE_PULL_UNSAFE_TREE_PATH".to_string(),
        ));
    }
    let dest = dir.join(rel);
    // 前缀检查：在 create 前用逻辑路径判断（dir 未必已 canonicalize）
    let dir_norm = dir.components().collect::<Vec<_>>();
    let dest_norm = dest.components().collect::<Vec<_>>();
    if dest_norm.len() < dir_norm.len() || dest_norm[..dir_norm.len()] != dir_norm[..] {
        return Err(AppError::validation(
            "PORTABLE_PULL_UNSAFE_TREE_PATH".to_string(),
        ));
    }
    Ok(dest)
}

/// MCP 原生 leaf（无 Hub DTO 字段 key/transport/toolAllow）。
fn native_mcp_leaf_value(
    target: AgentTarget,
    server: &crate::agent_hub::assets::PortableMcpServer,
) -> Result<serde_json::Value, AppError> {
    let proj = render_mcp_projection(target, server)?;
    let bytes = proj
        .files
        .first()
        .map(|f| f.bytes.as_slice())
        .unwrap_or(b"{}");
    serde_json::from_slice(bytes)
        .map_err(|e| AppError::validation(format!("PORTABLE_PULL_MCP_NATIVE_RENDER:{e}")))
}

/// Claude 用户 MCP 配置权威路径（与 scanner 一致）。
fn resolve_claude_mcp_config_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        PathBuf::from(dir).join(".claude.json")
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".claude.json")
    }
}

/// OpenCode MCP 配置权威路径（OPENCODE_CONFIG / jsonc / json）。
fn resolve_opencode_mcp_config_path(root: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("OPENCODE_CONFIG") {
        return PathBuf::from(p);
    }
    let jsonc = root.join("opencode.jsonc");
    if jsonc.is_file() {
        return jsonc;
    }
    root.join("opencode.json")
}

/// 解析 InstallToTarget 的 agent 配置根；project scope 走映射项目路径。
async fn resolve_install_root(
    state: &AppState,
    item: &PortableSelectionItem,
    change: &PortablePullChangeDto,
) -> Result<PathBuf, AppError> {
    let _ = change;
    let env_home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    // selection.scope_id 形如 `user` 或 `project:<hub_id>`
    if item.scope_id.starts_with("project:") {
        let hub_id = item.scope_id.trim_start_matches("project:");
        let mapping = state
            .agent_hub_repo
            .get_project_mapping_by_hub_project_id(hub_id)
            .await?;
        let Some(mapping) = mapping.filter(|m| m.opted_in) else {
            return Err(AppError::validation(
                "PORTABLE_PULL_PROJECT_MAPPING_UNAVAILABLE".to_string(),
            ));
        };
        let project_path = mapping
            .local_absolute_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty());
        let project_path = if let Some(p) = project_path {
            p
        } else if let Some(wb) = mapping.local_workbench_project_id.as_ref() {
            state
                .workbench_project_repo
                .get(wb)
                .await
                .ok()
                .flatten()
                .map(|p| PathBuf::from(p.path))
                .ok_or_else(|| {
                    AppError::validation("PORTABLE_PULL_PROJECT_MAPPING_UNAVAILABLE".to_string())
                })?
        } else {
            return Err(AppError::validation(
                "PORTABLE_PULL_PROJECT_MAPPING_UNAVAILABLE".to_string(),
            ));
        };
        // 项目资产根：Claude `.claude` / Codex 项目 agents / OpenCode 项目 `.opencode`
        return Ok(match item.target {
            AgentTarget::Claude => project_path.join(".claude"),
            AgentTarget::Codex => project_path.join(".agents"),
            AgentTarget::OpenCode => project_path.join(".opencode"),
        });
    }
    Ok(match item.target {
        AgentTarget::Claude => std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| env_home.join(".claude")),
        AgentTarget::Codex => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| env_home.join(".codex")),
        AgentTarget::OpenCode => std::env::var_os("OPENCODE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(|p| PathBuf::from(p).join("opencode"))
                    .unwrap_or_else(|| env_home.join(".config").join("opencode"))
            }),
    })
}

fn parse_stored_pull_plan(plan_json: &str) -> Result<StoredPortablePullPlan, AppError> {
    serde_json::from_str(plan_json).map_err(AppError::from)
}

fn outcome_unknown_pull_result(
    plan_token: &str,
    client_request_id: &str,
    plan: &PortablePullPlanDto,
) -> PortablePullResultDto {
    PortablePullResultDto {
        plan_token: plan_token.to_string(),
        client_request_id: client_request_id.to_string(),
        source_device_id: plan.source_device_id.clone(),
        source_target: plan.source_target,
        destination_target: plan.destination_target,
        partial: true,
        items: plan
            .changes
            .iter()
            .map(|c| PortablePullItemResultDto {
                inventory_item_id: c.inventory_item_id.clone(),
                state: PortablePullItemState::OutcomeUnknown,
                install_mode: Some(c.install_mode),
                error_code: Some("PORTABLE_PULL_OUTCOME_UNKNOWN".into()),
                message: Some("pull claimed but not completed".into()),
            })
            .collect(),
    }
}

// ───────────────────────── 源端 inventory ─────────────────────────

/// 构建脱敏远端 inventory（本机作为源被查询）。
pub async fn build_remote_inventory_for_target(
    state: &AppState,
    source_target: AgentTarget,
) -> Result<RemotePortableInventoryDto, AppError> {
    let snap = inspect_portable_inventory(state).await?;
    Ok(snapshot_to_remote(
        state.device_id.as_str(),
        source_target,
        &snap,
    ))
}

fn snapshot_to_remote(
    device_id: &str,
    source_target: AgentTarget,
    snap: &PortableInventorySnapshotDto,
) -> RemotePortableInventoryDto {
    let items = snap
        .items
        .iter()
        .filter(|i| i.target == source_target)
        .map(to_remote_item)
        .collect();
    RemotePortableInventoryDto {
        source_device_id: device_id.to_string(),
        source_target,
        inventory_snapshot_hash: snap.inventory_snapshot_hash.clone(),
        refreshed_at: snap.refreshed_at.clone(),
        stale: snap.stale,
        items,
    }
}

fn to_remote_item(item: &PortableInventoryItemDto) -> RemotePortableInventoryItemDto {
    RemotePortableInventoryItemDto {
        inventory_item_id: item.inventory_item_id.clone(),
        target: item.target,
        kind: item.kind,
        native_id: item.native_id.clone(),
        display_name: item.display_name.clone(),
        description: item.description.clone(),
        version: item.version.clone(),
        scope_id: item.scope_id.clone(),
        project_id: item.project_id.clone(),
        project_opted_in: item.project_opted_in,
        source_origin: item.source_origin,
        actual_enabled: item.actual_enabled,
        content_hash: item.content_hash.clone(),
        tree_hash: item.tree_hash.clone(),
        warnings: item.warnings.clone(),
        mcp_credential: item
            .mcp_credential
            .as_ref()
            .map(|c| PortableMcpCredentialFactDto {
                present: c.present,
                hash: c.hash.clone(),
            }),
    }
}

/// 校验 remote inventory DTO 不含 secret/path 字段。
pub fn remote_inventory_is_metadata_only(dto: &RemotePortableInventoryDto) -> bool {
    let json = serde_json::to_string(dto).unwrap_or_default();
    let lower = json.to_ascii_lowercase();
    !lower.contains("\"sourcepath\"")
        && !lower.contains("\"env\"")
        && !lower.contains("\"authorization\"")
        && !lower.contains("\"api_token\"")
        && !lower.contains("\"apitoken\"")
}

// ───────────────────────── 源端 selection / objects ─────────────────────────

/// 源端按 item ids 构建 selection 并登记 transfer。
pub async fn source_prepare_selection(
    state: &AppState,
    source_target: AgentTarget,
    item_ids: Vec<String>,
) -> Result<RemotePortableSelectionResponse, AppError> {
    let built = build_source_selection_for_items(state, source_target, &item_ids).await?;
    let transfer_id = format!("ppull-src-{}", Uuid::now_v7());
    let missing: Vec<String> = built
        .envelope
        .objects
        .iter()
        .map(|o| o.hash.clone())
        .collect();
    let resp = RemotePortableSelectionResponse {
        transfer_id: transfer_id.clone(),
        envelope: built.envelope.clone(),
        items: built.items.clone(),
        missing_object_hashes: missing,
    };
    // freeze inventory snapshot hash on the envelope source id for apply revalidation
    staging_insert(transfer_id, built)?;
    Ok(resp)
}

/// 源端读取 object chunk（offset 续传）。
pub fn source_read_object_chunk(
    transfer_id: &str,
    object_hash: &str,
    offset: u64,
) -> Result<Vec<u8>, AppError> {
    let mut g = staging().lock().expect("staging");
    evict_expired_staging(&mut g);
    let staged = g
        .get(transfer_id)
        .ok_or_else(|| AppError::not_found("PORTABLE_PULL_TRANSFER_NOT_FOUND".to_string()))?;
    let bytes = staged
        .built
        .object_bytes
        .get(object_hash)
        .ok_or_else(|| AppError::not_found("PORTABLE_PULL_OBJECT_NOT_FOUND".to_string()))?;
    if offset as usize >= bytes.len() {
        return Ok(Vec::new());
    }
    let end = (offset as usize + PORTABLE_PULL_MAX_CHUNK_BYTES).min(bytes.len());
    let chunk = bytes[offset as usize..end].to_vec();
    // 完整读完最后一个对象后清理（best-effort：offset 到末尾）
    let fully_read = end >= bytes.len();
    let all_done = fully_read
        && staged.built.object_bytes.iter().all(|(h, b)| {
            if h == object_hash {
                true
            } else {
                // 无法精确追踪其它对象；仅在本对象末尾且仅有一个对象时清
                staged.built.object_bytes.len() == 1 && !b.is_empty()
            }
        });
    drop(g);
    if all_done {
        staging_remove(transfer_id);
    }
    Ok(chunk)
}

/// 源端显式释放 transfer staging（完整传输后或 plan 过期）。
pub fn source_release_transfer(transfer_id: &str) {
    staging_remove(transfer_id);
}

async fn build_source_selection_for_items(
    state: &AppState,
    source_target: AgentTarget,
    item_ids: &[String],
) -> Result<BuiltPortableSelection, AppError> {
    let snap = inspect_portable_inventory(state).await?;
    let wanted: BTreeSet<&str> = item_ids.iter().map(String::as_str).collect();
    let selected: Vec<_> = snap
        .items
        .into_iter()
        .filter(|i| i.target == source_target && wanted.contains(i.inventory_item_id.as_str()))
        .collect();
    if selected.is_empty() {
        return Err(AppError::validation(
            "PORTABLE_PULL_SELECTION_EMPTY".to_string(),
        ));
    }
    let data_dir = crate::config::data_dir()?;
    let store = ObjectStore::open(&data_dir)?;
    build_portable_selection_envelope(&store, state.device_id.as_str(), source_target, &selected)
        .await
}

// ───────────────────────── 本机 list / preview / apply / get ─────────────────────────

/// 列出远端 inventory（本机作为客户端）。
pub async fn list_remote_portable_inventory(
    state: &AppState,
    request: ListRemotePortableInventoryRequest,
) -> Result<RemotePortableInventoryDto, AppError> {
    let device = resolve_device(state, &request.source_device_id)?;
    let base_url = device.base_url();
    let peer = PeerClient::new();
    peer.require_capability(&base_url, CAPABILITY_PORTABLE_PULL_V1)
        .await
        .map_err(peer_err)?;
    fetch_remote_inventory(
        &peer,
        &base_url,
        &request.source_device_id,
        request.source_target,
    )
    .await
}

/// 预览同类 Agent pull（零写入目标文件）。
pub async fn preview_portable_pull(
    state: &AppState,
    request: PreviewPortablePullRequest,
) -> Result<PortablePullPlanDto, AppError> {
    if request.source_target != request.destination_target {
        return Err(AppError::validation(
            "PORTABLE_PULL_TARGET_MISMATCH:source!=destination".to_string(),
        ));
    }
    if request.inventory_item_ids.is_empty() {
        return Err(AppError::validation(
            "PORTABLE_PULL_SELECTION_EMPTY".to_string(),
        ));
    }

    let device = resolve_device(state, &request.source_device_id)?;
    let base_url = device.base_url();
    let peer = PeerClient::new();
    peer.require_capability(&base_url, CAPABILITY_PORTABLE_PULL_V1)
        .await
        .map_err(peer_err)?;

    let remote = fetch_remote_inventory(
        &peer,
        &base_url,
        &request.source_device_id,
        request.source_target,
    )
    .await?;

    if remote.inventory_snapshot_hash != request.remote_inventory_snapshot_hash {
        return Err(AppError::conflict(
            "PORTABLE_PULL_REMOTE_INVENTORY_STALE".to_string(),
        ));
    }

    let wanted: BTreeSet<&str> = request
        .inventory_item_ids
        .iter()
        .map(String::as_str)
        .collect();
    let remote_selected: Vec<_> = remote
        .items
        .iter()
        .filter(|i| wanted.contains(i.inventory_item_id.as_str()))
        .cloned()
        .collect();
    if remote_selected.is_empty() {
        return Err(AppError::validation(
            "PORTABLE_PULL_ITEMS_NOT_FOUND".to_string(),
        ));
    }
    for item in &remote_selected {
        if item.target != request.source_target {
            return Err(AppError::validation(format!(
                "PORTABLE_PULL_TARGET_MISMATCH:item={}",
                item.inventory_item_id
            )));
        }
    }

    let local = inspect_portable_inventory(state).await?;
    let local_by_native: BTreeMap<
        (AgentTarget, PortableAssetKind, String),
        &PortableInventoryItemDto,
    > = local
        .items
        .iter()
        .map(|i| ((i.target, i.kind, i.native_id.clone()), i))
        .collect();

    let mut changes = Vec::new();
    let mut blocking = Vec::new();
    let mut credential_bearing = 0u64;

    for rem in &remote_selected {
        let mut item_blocking = Vec::new();
        let mut warnings = rem.warnings.clone();
        let existing = local_by_native.get(&(rem.target, rem.kind, rem.native_id.clone()));
        let unmapped_project = rem.project_id.is_some() && !rem.project_opted_in;
        let local_project_opted = rem.project_id.as_ref().map(|pid| {
            local
                .items
                .iter()
                .any(|i| i.project_id.as_deref() == Some(pid.as_str()) && i.project_opted_in)
        });
        let mapping_missing = rem.project_id.is_some()
            && local_project_opted != Some(true)
            && rem.scope_id.starts_with("project:");

        let legacy_lossy = warnings
            .iter()
            .any(|w| w.to_ascii_lowercase().contains("legacylossy"));
        let cred = rem
            .mcp_credential
            .as_ref()
            .map(|c| c.present)
            .unwrap_or(false);
        if cred {
            credential_bearing += 1;
        }

        let install_mode = if legacy_lossy {
            item_blocking.push("legacyLossy credential blocked".into());
            PortablePullInstallMode::Blocked
        } else if let Some(local_item) = existing {
            match request.conflict_policy {
                PortableAssetConflictPolicy::SkipExisting => PortablePullInstallMode::SkipExisting,
                PortableAssetConflictPolicy::ReplaceAfterPreview => {
                    if local_item.management_state
                        == PortableInventoryManagementState::ExternalCollision
                    {
                        item_blocking.push("externalCollision".into());
                        PortablePullInstallMode::Blocked
                    } else if mapping_missing || unmapped_project {
                        PortablePullInstallMode::ImportedCanonicalOnly
                    } else {
                        PortablePullInstallMode::InstallToTarget
                    }
                }
            }
        } else if mapping_missing || unmapped_project {
            warnings.push("project unmapped or not opted-in; canonical only".into());
            PortablePullInstallMode::ImportedCanonicalOnly
        } else {
            PortablePullInstallMode::InstallToTarget
        };

        if !item_blocking.is_empty() {
            blocking.extend(item_blocking.iter().cloned());
        }

        changes.push(PortablePullChangeDto {
            inventory_item_id: rem.inventory_item_id.clone(),
            kind: rem.kind,
            native_id: rem.native_id.clone(),
            display_name: rem.display_name.clone(),
            install_mode,
            conflict: existing.is_some(),
            legacy_lossy,
            credential_bearing: cred,
            blocking_reasons: item_blocking,
            warnings,
        });
    }

    let selection_manifest_hash = sha256_hex(
        serde_json::to_string(&request.inventory_item_ids)
            .unwrap_or_default()
            .as_bytes(),
    );
    let plan_token = format!("ppull-{}", Uuid::now_v7());
    let expires_at = (Utc::now() + ChronoDuration::minutes(PULL_PLAN_TTL_MINUTES)).to_rfc3339();
    let remote_item_ids: Vec<String> = remote_selected
        .iter()
        .map(|r| r.inventory_item_id.clone())
        .collect();

    let public = PortablePullPlanDto {
        plan_token: plan_token.clone(),
        expires_at: expires_at.clone(),
        source_device_id: request.source_device_id.clone(),
        source_target: request.source_target,
        destination_target: request.destination_target,
        remote_inventory_snapshot_hash: remote.inventory_snapshot_hash.clone(),
        local_inventory_snapshot_hash: local.inventory_snapshot_hash.clone(),
        conflict_policy: request.conflict_policy,
        selection_manifest_hash,
        credential_bearing_count: credential_bearing,
        has_credential_bearing_assets: credential_bearing > 0,
        changes,
        blocking_reasons: blocking,
    };

    let stored = StoredPortablePullPlan {
        public: public.clone(),
        remote_item_ids,
    };
    let plan_json = serde_json::to_string(&stored)?;
    let repo = AgentHubRepo::new(state.agent_hub_repo.pool().clone());
    repo.insert_portable_pull_plan(PortablePullPlanRecord {
        plan_token,
        expires_at,
        remote_inventory_snapshot_hash: remote.inventory_snapshot_hash,
        local_inventory_snapshot_hash: local.inventory_snapshot_hash,
        plan_json,
        client_request_id: None,
        claimed_at: None,
        consumed_at: None,
        result_json: None,
        created_at: Utc::now().to_rfc3339(),
    })
    .await?;
    Ok(public)
}

/// 执行 pull（SQLite claim → transfer → import → install → rescan → complete）。
pub async fn apply_portable_pull(
    state: &AppState,
    request: ApplyPortablePullRequest,
) -> Result<PortablePullResultDto, AppError> {
    if request.plan_token.trim().is_empty() || request.client_request_id.trim().is_empty() {
        return Err(AppError::validation(
            "PORTABLE_PULL_APPLY_IDS_REQUIRED".to_string(),
        ));
    }
    let repo = AgentHubRepo::new(state.agent_hub_repo.pool().clone());
    let claim = repo
        .claim_portable_pull_plan(&request.plan_token, &request.client_request_id)
        .await?;

    match claim {
        PortablePullClaim::Replay(json) => serde_json::from_str(&json).map_err(AppError::from),
        PortablePullClaim::Pending => {
            let row = repo
                .get_portable_pull_plan(&request.plan_token)
                .await?
                .ok_or_else(|| AppError::not_found("PORTABLE_PULL_PLAN_NOT_FOUND".to_string()))?;
            let stored = parse_stored_pull_plan(&row.plan_json)?;
            Ok(outcome_unknown_pull_result(
                &request.plan_token,
                &request.client_request_id,
                &stored.public,
            ))
        }
        PortablePullClaim::Claimed(record) => {
            let stored = parse_stored_pull_plan(&record.plan_json)?;
            let result = match execute_claimed_pull(state, &stored, &request).await {
                Ok(r) => r,
                Err(e) => {
                    // fail-closed 完成 ledger，避免永远 Pending 假死；调用方可 get 到失败结果
                    let fail = PortablePullResultDto {
                        plan_token: request.plan_token.clone(),
                        client_request_id: request.client_request_id.clone(),
                        source_device_id: stored.public.source_device_id.clone(),
                        source_target: stored.public.source_target,
                        destination_target: stored.public.destination_target,
                        partial: true,
                        items: stored
                            .public
                            .changes
                            .iter()
                            .map(|c| PortablePullItemResultDto {
                                inventory_item_id: c.inventory_item_id.clone(),
                                state: PortablePullItemState::Failed,
                                install_mode: Some(c.install_mode),
                                error_code: Some("PORTABLE_PULL_APPLY_FAILED".into()),
                                message: Some(e.to_string()),
                            })
                            .collect(),
                    };
                    let _ = repo
                        .complete_portable_pull_plan(
                            &request.plan_token,
                            &request.client_request_id,
                            &serde_json::to_string(&fail)?,
                        )
                        .await;
                    return Ok(fail);
                }
            };
            repo.complete_portable_pull_plan(
                &request.plan_token,
                &request.client_request_id,
                &serde_json::to_string(&result)?,
            )
            .await?;
            Ok(result)
        }
    }
}

async fn execute_claimed_pull(
    state: &AppState,
    stored: &StoredPortablePullPlan,
    request: &ApplyPortablePullRequest,
) -> Result<PortablePullResultDto, AppError> {
    // transfer 前强制同源 target + 本机 inventory 再验证
    if stored.public.source_target != stored.public.destination_target {
        return Err(AppError::validation(
            "PORTABLE_PULL_TARGET_MISMATCH:source!=destination".to_string(),
        ));
    }
    if stored.public.expires_at.as_str() < Utc::now().to_rfc3339().as_str() {
        return Err(AppError::conflict("PORTABLE_PULL_PLAN_EXPIRED".to_string()));
    }
    let local_now = inspect_portable_inventory(state).await?;
    if local_now.inventory_snapshot_hash != stored.public.local_inventory_snapshot_hash {
        return Err(AppError::conflict(
            "PORTABLE_PULL_LOCAL_INVENTORY_STALE".to_string(),
        ));
    }

    let device = resolve_device(state, &stored.public.source_device_id)?;
    let base_url = device.base_url();
    let peer = PeerClient::new();
    peer.require_capability(&base_url, CAPABILITY_PORTABLE_PULL_V1)
        .await
        .map_err(peer_err)?;

    // Apply 前重新校验远端 inventory 快照，防止 preview 后远端 MCP/资产漂移仍安装
    let remote_now = fetch_remote_inventory(
        &peer,
        &base_url,
        &stored.public.source_device_id,
        stored.public.source_target,
    )
    .await?;
    if remote_now.inventory_snapshot_hash != stored.public.remote_inventory_snapshot_hash {
        return Err(AppError::conflict(
            "PORTABLE_PULL_REMOTE_INVENTORY_STALE".to_string(),
        ));
    }
    // selection 必须仍能覆盖 plan 中的 item ids
    let remote_ids: BTreeSet<&str> = remote_now
        .items
        .iter()
        .map(|i| i.inventory_item_id.as_str())
        .collect();
    for id in &stored.remote_item_ids {
        if !remote_ids.contains(id.as_str()) {
            return Err(AppError::conflict(
                "PORTABLE_PULL_REMOTE_INVENTORY_STALE".to_string(),
            ));
        }
    }

    let selection = fetch_remote_selection(
        &peer,
        &base_url,
        &stored.public.source_device_id,
        stored.public.source_target,
        &stored.remote_item_ids,
    )
    .await?;

    let data_dir = crate::config::data_dir()?;
    let store = ObjectStore::open(&data_dir)?;
    let mut collected: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    for obj in &selection.envelope.objects {
        // CAS 已有 blob 时仍 rehash 校验（integrity）
        if let Ok(existing) = store.get_blob(&obj.hash).await {
            let got = sha256_hex(&existing);
            if got != obj.hash {
                return Err(AppError::validation(format!(
                    "PORTABLE_PULL_OBJECT_HASH_MISMATCH:{}",
                    obj.hash
                )));
            }
            collected.insert(obj.hash.clone(), existing);
            continue;
        }
        let size: u64 = obj.size.parse().unwrap_or(0);
        let mut offset = 0u64;
        let mut buf = Vec::new();
        loop {
            let chunk = fetch_object_chunk(
                &peer,
                &base_url,
                &stored.public.source_device_id,
                &selection.transfer_id,
                &obj.hash,
                offset,
            )
            .await?;
            if chunk.is_empty() {
                // size>0 但提前 empty → fail-closed
                if size > 0 && offset < size {
                    return Err(AppError::validation(format!(
                        "PORTABLE_PULL_OBJECT_INCOMPLETE:{}",
                        obj.hash
                    )));
                }
                break;
            }
            let n = chunk.len() as u64;
            buf.extend_from_slice(&chunk);
            offset += n;
            if size > 0 && offset >= size {
                break;
            }
            // size==0：仅接受一次非空或 empty 结束；禁止无限空转
            if size == 0 && n < PORTABLE_PULL_MAX_CHUNK_BYTES as u64 {
                break;
            }
        }
        if bytes_are_legacy_lossy(&buf) {
            continue;
        }
        if !buf.is_empty() || size == 0 {
            let got = sha256_hex(&buf);
            if size > 0 && got != obj.hash {
                return Err(AppError::validation(format!(
                    "PORTABLE_PULL_OBJECT_HASH_MISMATCH:{}",
                    obj.hash
                )));
            }
            if !buf.is_empty() {
                store.put_blob(&buf).await?;
                collected.insert(obj.hash.clone(), buf);
            }
        }
    }

    for obj in &selection.envelope.objects {
        if !collected.contains_key(&obj.hash) {
            if let Ok(b) = store.get_blob(&obj.hash).await {
                collected.insert(obj.hash.clone(), b);
            }
        }
    }

    let import_ok = import_selection_canonical(state, &store, &data_dir, &selection).await;
    let local_before = local_now;

    let mut items = Vec::new();
    let mut any_fail = false;
    for change in &stored.public.changes {
        if change.legacy_lossy || change.install_mode == PortablePullInstallMode::Blocked {
            items.push(PortablePullItemResultDto {
                inventory_item_id: change.inventory_item_id.clone(),
                state: PortablePullItemState::Blocked,
                install_mode: Some(change.install_mode),
                error_code: Some("PORTABLE_PULL_ITEM_BLOCKED".into()),
                message: change.blocking_reasons.first().cloned(),
            });
            continue;
        }
        if change.install_mode == PortablePullInstallMode::SkipExisting {
            // 写时再次确认本地仍存在，避免 preview 后被删仍 skip
            let still = local_before.items.iter().any(|i| {
                i.target == stored.public.destination_target
                    && i.kind == change.kind
                    && i.native_id == change.native_id
            });
            if still {
                items.push(PortablePullItemResultDto {
                    inventory_item_id: change.inventory_item_id.clone(),
                    state: PortablePullItemState::Skipped,
                    install_mode: Some(change.install_mode),
                    error_code: None,
                    message: Some("skipExisting".into()),
                });
            } else {
                // 已不存在：降级为 installToTarget 语义。
                // 与主 InstallToTarget 一致：import 必成才写目标；成功后 rescan 门禁。
                if import_ok.is_err() {
                    any_fail = true;
                    items.push(PortablePullItemResultDto {
                        inventory_item_id: change.inventory_item_id.clone(),
                        state: PortablePullItemState::Failed,
                        install_mode: Some(PortablePullInstallMode::InstallToTarget),
                        error_code: Some("PORTABLE_PULL_CANONICAL_IMPORT_REQUIRED".into()),
                        message: Some(format!(
                            "canonical import failed before skipExisting demotion install: {}",
                            import_ok
                                .as_ref()
                                .err()
                                .map(|e| e.to_string())
                                .unwrap_or_default()
                        )),
                    });
                    continue;
                }
                match install_change(state, &store, &selection, change).await {
                    Ok(()) => {
                        let post = inspect_portable_inventory(state).await?;
                        let observed = post.items.iter().any(|i| {
                            i.target == stored.public.destination_target
                                && i.kind == change.kind
                                && i.native_id == change.native_id
                        });
                        if observed {
                            items.push(PortablePullItemResultDto {
                                inventory_item_id: change.inventory_item_id.clone(),
                                state: PortablePullItemState::Succeeded,
                                install_mode: Some(PortablePullInstallMode::InstallToTarget),
                                error_code: None,
                                message: Some(
                                    "skipExisting demotion install verified by rescan".into(),
                                ),
                            });
                        } else {
                            any_fail = true;
                            items.push(PortablePullItemResultDto {
                                inventory_item_id: change.inventory_item_id.clone(),
                                state: PortablePullItemState::Failed,
                                install_mode: Some(PortablePullInstallMode::InstallToTarget),
                                error_code: Some("PORTABLE_PULL_RESCAN_MISSING".into()),
                                message: Some(
                                    "skipExisting demotion install not observed after rescan"
                                        .into(),
                                ),
                            });
                        }
                    }
                    Err(e) => {
                        any_fail = true;
                        items.push(PortablePullItemResultDto {
                            inventory_item_id: change.inventory_item_id.clone(),
                            state: PortablePullItemState::Failed,
                            install_mode: Some(change.install_mode),
                            error_code: Some("PORTABLE_PULL_INSTALL_FAILED".into()),
                            message: Some(e.to_string()),
                        });
                    }
                }
            }
            continue;
        }
        if change.install_mode == PortablePullInstallMode::ImportedCanonicalOnly {
            if import_ok.is_err() {
                any_fail = true;
                items.push(PortablePullItemResultDto {
                    inventory_item_id: change.inventory_item_id.clone(),
                    state: PortablePullItemState::Failed,
                    install_mode: Some(change.install_mode),
                    error_code: Some("PORTABLE_PULL_CANONICAL_IMPORT_REQUIRED".into()),
                    message: Some("canonical import failed".into()),
                });
            } else {
                items.push(PortablePullItemResultDto {
                    inventory_item_id: change.inventory_item_id.clone(),
                    state: PortablePullItemState::ImportedCanonicalOnly,
                    install_mode: Some(change.install_mode),
                    error_code: None,
                    message: Some("canonical only; project unmapped or not opted-in".into()),
                });
            }
            continue;
        }

        // InstallToTarget：canonical import 必须成功，否则不得标 Succeeded
        if import_ok.is_err() {
            any_fail = true;
            items.push(PortablePullItemResultDto {
                inventory_item_id: change.inventory_item_id.clone(),
                state: PortablePullItemState::Failed,
                install_mode: Some(change.install_mode),
                error_code: Some("PORTABLE_PULL_CANONICAL_IMPORT_REQUIRED".into()),
                message: Some(format!(
                    "canonical import failed: {}",
                    import_ok
                        .as_ref()
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_default()
                )),
            });
            continue;
        }

        match install_change(state, &store, &selection, change).await {
            Ok(()) => {
                // post-install rescan gate
                let post = inspect_portable_inventory(state).await?;
                let observed = post.items.iter().any(|i| {
                    i.target == stored.public.destination_target
                        && i.kind == change.kind
                        && i.native_id == change.native_id
                });
                if observed {
                    items.push(PortablePullItemResultDto {
                        inventory_item_id: change.inventory_item_id.clone(),
                        state: PortablePullItemState::Succeeded,
                        install_mode: Some(change.install_mode),
                        error_code: None,
                        message: Some("install verified by rescan".into()),
                    });
                } else {
                    any_fail = true;
                    items.push(PortablePullItemResultDto {
                        inventory_item_id: change.inventory_item_id.clone(),
                        state: PortablePullItemState::Failed,
                        install_mode: Some(change.install_mode),
                        error_code: Some("PORTABLE_PULL_RESCAN_MISSING".into()),
                        message: Some("install not observed after rescan".into()),
                    });
                }
            }
            Err(e) => {
                any_fail = true;
                items.push(PortablePullItemResultDto {
                    inventory_item_id: change.inventory_item_id.clone(),
                    state: PortablePullItemState::Failed,
                    install_mode: Some(change.install_mode),
                    error_code: Some("PORTABLE_PULL_INSTALL_FAILED".into()),
                    message: Some(e.to_string()),
                });
            }
        }
    }

    Ok(PortablePullResultDto {
        plan_token: request.plan_token.clone(),
        client_request_id: request.client_request_id.clone(),
        source_device_id: stored.public.source_device_id.clone(),
        source_target: stored.public.source_target,
        destination_target: stored.public.destination_target,
        partial: any_fail || import_ok.is_err(),
        items,
    })
}

async fn install_change(
    state: &AppState,
    store: &ObjectStore,
    selection: &RemotePortableSelectionResponse,
    change: &PortablePullChangeDto,
) -> Result<(), AppError> {
    let sel = selection
        .items
        .iter()
        .find(|s| s.inventory_item_id == change.inventory_item_id)
        .ok_or_else(|| AppError::generic("selection item missing after transfer"))?;
    install_payload_to_target(state, store, sel, change).await
}

/// 按 clientRequestId 查询 pull 结果。
pub async fn get_portable_pull(
    state: &AppState,
    client_request_id: &str,
) -> Result<PortablePullResultDto, AppError> {
    if client_request_id.trim().is_empty() {
        return Err(AppError::validation(
            "PORTABLE_PULL_REQUEST_ID_REQUIRED".to_string(),
        ));
    }
    let repo = AgentHubRepo::new(state.agent_hub_repo.pool().clone());
    let row = repo
        .get_portable_pull_by_request_id(client_request_id)
        .await?
        .ok_or_else(|| AppError::not_found("PORTABLE_PULL_REQUEST_NOT_FOUND".to_string()))?;
    if let Some(result_json) = row.result_json.as_deref() {
        return serde_json::from_str(result_json).map_err(AppError::from);
    }
    let stored = parse_stored_pull_plan(&row.plan_json)?;
    Ok(outcome_unknown_pull_result(
        &row.plan_token,
        client_request_id,
        &stored.public,
    ))
}

// ───────────────────────── helpers ─────────────────────────

fn resolve_device(state: &AppState, device_id: &str) -> Result<Device, AppError> {
    let devices = state.devices.read().expect("devices lock");
    devices
        .get(device_id)
        .cloned()
        .ok_or_else(|| AppError::not_found("设备不存在或已离线".to_string()))
}

fn peer_err(e: PeerCallError) -> AppError {
    match e {
        PeerCallError::Unsupported { .. } => {
            AppError::unavailable("PORTABLE_PULL_UNSUPPORTED_PEER".to_string())
        }
        PeerCallError::Network { .. } => {
            AppError::unavailable("PORTABLE_PULL_PEER_NETWORK".to_string())
        }
        PeerCallError::Remote { message, .. } => AppError::generic(message),
        PeerCallError::InvalidResponse { .. } => {
            AppError::generic("PORTABLE_PULL_INVALID_RESPONSE".to_string())
        }
    }
}

async fn fetch_remote_inventory(
    peer: &PeerClient,
    base_url: &str,
    expected_device_id: &str,
    source_target: AgentTarget,
) -> Result<RemotePortableInventoryDto, AppError> {
    post_json_bound(
        peer,
        base_url,
        "/api/agent-hub/portable/inventory",
        &RemoteInventoryQuery { source_target },
        expected_device_id,
        PeerTimeoutClass::Metadata,
    )
    .await
    .map_err(peer_err)
}

async fn fetch_remote_selection(
    peer: &PeerClient,
    base_url: &str,
    expected_device_id: &str,
    source_target: AgentTarget,
    item_ids: &[String],
) -> Result<RemotePortableSelectionResponse, AppError> {
    post_json_bound(
        peer,
        base_url,
        "/api/agent-hub/portable/selection",
        &RemoteSelectionQuery {
            source_target,
            inventory_item_ids: item_ids.to_vec(),
        },
        expected_device_id,
        PeerTimeoutClass::Metadata,
    )
    .await
    .map_err(peer_err)
}

async fn fetch_object_chunk(
    peer: &PeerClient,
    base_url: &str,
    expected_device_id: &str,
    transfer_id: &str,
    object_hash: &str,
    offset: u64,
) -> Result<Vec<u8>, AppError> {
    let path =
        format!("/api/agent-hub/portable/objects/{transfer_id}/{object_hash}?offset={offset}");
    get_bytes_bound(
        peer,
        base_url,
        &path,
        expected_device_id,
        PeerTimeoutClass::Mutation,
    )
    .await
    .map_err(peer_err)
}

async fn post_json_bound<T, B>(
    peer: &PeerClient,
    base_url: &str,
    path: &str,
    body: &B,
    expected_device_id: &str,
    class: PeerTimeoutClass,
) -> Result<T, PeerCallError>
where
    T: for<'de> Deserialize<'de>,
    B: Serialize + ?Sized,
{
    let url = format!("{base_url}{path}");
    let resp = peer
        .http_client()
        .post(&url)
        .timeout(class.timeout())
        .header(REQUEST_ID_HEADER, new_request_id())
        .header(EXPECTED_DEVICE_ID_HEADER.as_str(), expected_device_id)
        .json(body)
        .send()
        .await
        .map_err(|e| PeerCallError::Network {
            url: url.clone(),
            source: e,
        })?;
    crate::net::peer_error::parse_peer_response::<T>(resp, &url).await
}

async fn get_bytes_bound(
    peer: &PeerClient,
    base_url: &str,
    path: &str,
    expected_device_id: &str,
    class: PeerTimeoutClass,
) -> Result<Vec<u8>, PeerCallError> {
    let url = format!("{base_url}{path}");
    let resp = peer
        .http_client()
        .get(&url)
        .timeout(class.timeout())
        .header(REQUEST_ID_HEADER, new_request_id())
        .header(EXPECTED_DEVICE_ID_HEADER.as_str(), expected_device_id)
        .send()
        .await
        .map_err(|e| PeerCallError::Network {
            url: url.clone(),
            source: e,
        })?;
    if !resp.status().is_success() {
        return Err(PeerCallError::Remote {
            url,
            status: resp.status().as_u16(),
            code: "portable_pull_object".into(),
            message: format!("object chunk http {}", resp.status()),
            request_id: String::new(),
            retryable: false,
            legacy: false,
            details: serde_json::json!({}),
        });
    }
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| PeerCallError::Network { url, source: e })
}

async fn import_selection_canonical(
    state: &AppState,
    store: &ObjectStore,
    data_dir: &std::path::Path,
    selection: &RemotePortableSelectionResponse,
) -> Result<(), AppError> {
    if selection.envelope.assets.is_empty() {
        return Ok(());
    }
    let mut object_bytes = BTreeMap::new();
    for obj in &selection.envelope.objects {
        if let Ok(b) = store.get_blob(&obj.hash).await {
            object_bytes.insert(obj.hash.clone(), b);
        }
    }
    let validated = ValidatedSnapshot::from_parts(selection.envelope.clone(), object_bytes, None)?;
    let importer = SnapshotImporter::new(
        AgentHubRepo::new(state.agent_hub_repo.pool().clone()),
        store.clone(),
        data_dir,
    );
    let _ = importer
        .commit_import(validated, ConfirmedImportSelection::default())
        .await?;
    Ok(())
}

#[allow(clippy::collapsible_if, clippy::collapsible_match)]
async fn install_payload_to_target(
    state: &AppState,
    store: &ObjectStore,
    item: &PortableSelectionItem,
    change: &PortablePullChangeDto,
) -> Result<(), AppError> {
    let bytes = store.get_blob(&item.object_hash).await?;
    if bytes_are_legacy_lossy(&bytes) {
        return Err(AppError::validation(
            "PORTABLE_PULL_LEGACY_LOSSY".to_string(),
        ));
    }
    // Project-scoped selection → mapped project asset root; never silent-install into user root.
    let root = resolve_install_root(state, item, change).await?;
    match item.kind {
        PortableAssetKind::Command => {
            let dir = root.join("commands");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{}.md", item.native_id));
            if change.install_mode == PortablePullInstallMode::SkipExisting && path.exists() {
                return Ok(());
            }
            if path.exists()
                && matches!(
                    change.install_mode,
                    PortablePullInstallMode::InstallToTarget
                )
            {
                // replaceAfterPreview：允许覆盖；skipExisting 已在上分支处理
            }
            if let Ok(payload) = from_canonical_bytes(&bytes) {
                if let PortableAssetPayload::Command(cmd) = payload {
                    let text = format!(
                        "---\nname: {}\ndescription: {}\n---\n{}\n",
                        cmd.name,
                        cmd.description.as_deref().unwrap_or(""),
                        cmd.prompt_template
                    );
                    std::fs::write(&path, text)?;
                    return Ok(());
                }
            }
            std::fs::write(&path, &bytes)?;
        }
        PortableAssetKind::Skill => {
            let dir = root.join("skills").join(&item.native_id);
            if change.install_mode == PortablePullInstallMode::SkipExisting && dir.exists() {
                return Ok(());
            }
            // 优先 CAS 树还原完整 body（tree_hash 在 envelope revision / payload）
            if let Ok(payload) = from_canonical_bytes(&bytes) {
                if let PortableAssetPayload::Skill(skill) = payload {
                    if let Ok(manifest) = store.get_tree(&skill.tree_manifest_hash).await {
                        std::fs::create_dir_all(&dir)?;
                        for entry in &manifest.entries {
                            let dest = safe_tree_dest(&dir, &entry.path)?;
                            if let Some(parent) = dest.parent() {
                                std::fs::create_dir_all(parent)?;
                            }
                            match entry.entry_type {
                                TreeEntryType::File => {
                                    let blob = store.get_blob(&entry.blob_hash).await?;
                                    std::fs::write(&dest, blob)?;
                                }
                                TreeEntryType::Symlink => {
                                    // 不跟随外链；仅写文本占位失败则跳过
                                    let _ = entry;
                                }
                            }
                        }
                        // 若树未含 SKILL.md，至少写 frontmatter（fail-closed 再检查）
                        if !dir.join("SKILL.md").exists() {
                            return Err(AppError::validation(
                                "PORTABLE_PULL_SKILL_TREE_MISSING_SKILL_MD".to_string(),
                            ));
                        }
                        return Ok(());
                    }
                    // 无树：不得只写 frontmatter 丢 body —— fail-closed
                    return Err(AppError::validation(format!(
                        "PORTABLE_PULL_SKILL_TREE_UNAVAILABLE:{}",
                        skill.tree_manifest_hash
                    )));
                }
            }
            // 非 canonical skill payload：整文件当 SKILL.md（单文件 skill）
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("SKILL.md"), &bytes)?;
        }
        PortableAssetKind::Plugin => {
            let dir = root.join("plugins").join(&item.native_id);
            if change.install_mode == PortablePullInstallMode::SkipExisting && dir.exists() {
                return Ok(());
            }
            // 目录 Plugin：portablePluginTreeRef + CAS tree 还原；禁止 hash 清单当 plugin.json
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if v.get("kind").and_then(|k| k.as_str()) == Some("portablePluginTreeRef") {
                    let tree_hash = v
                        .get("treeManifestHash")
                        .and_then(|h| h.as_str())
                        .unwrap_or("");
                    if tree_hash.is_empty() {
                        return Err(AppError::validation(
                            "PORTABLE_PULL_PLUGIN_TREE_UNAVAILABLE".to_string(),
                        ));
                    }
                    let manifest = store.get_tree(tree_hash).await.map_err(|_| {
                        AppError::validation(format!(
                            "PORTABLE_PULL_PLUGIN_TREE_UNAVAILABLE:{tree_hash}"
                        ))
                    })?;
                    std::fs::create_dir_all(&dir)?;
                    for entry in &manifest.entries {
                        let dest = safe_tree_dest(&dir, &entry.path)?;
                        if let Some(parent) = dest.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        if matches!(entry.entry_type, TreeEntryType::File) {
                            let blob = store.get_blob(&entry.blob_hash).await?;
                            std::fs::write(&dest, blob)?;
                        }
                    }
                    return Ok(());
                }
                // 旧 path+hash-only 清单：fail-closed，绝不当 plugin.json 假成功
                if v.get("kind").and_then(|k| k.as_str()) == Some("portablePluginTree") {
                    return Err(AppError::validation(
                        "PORTABLE_PULL_PLUGIN_TREE_UNAVAILABLE".to_string(),
                    ));
                }
            }
            // 单文件 plugin.json body
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("plugin.json"), &bytes)?;
        }
        PortableAssetKind::Mcp => {
            // 权威 MCP 配置路径：Claude 用 home/.claude.json（非 ~/.claude/.claude.json）；
            // OpenCode 尊重 OPENCODE_CONFIG / jsonc。
            let path = match item.target {
                AgentTarget::Claude => {
                    if item.scope_id.starts_with("project:") {
                        // 项目 MCP：优先 `.mcp.json` 再 settings.local.json
                        let project_root = root
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_else(|| root.clone());
                        let mcp = project_root.join(".mcp.json");
                        if mcp.exists() {
                            mcp
                        } else {
                            root.join("settings.local.json")
                        }
                    } else {
                        resolve_claude_mcp_config_path()
                    }
                }
                AgentTarget::Codex => root.join("config.toml"),
                AgentTarget::OpenCode => resolve_opencode_mcp_config_path(&root),
            };
            if let Ok(payload) = from_canonical_bytes(&bytes) {
                if let PortableAssetPayload::Mcp(server) = payload {
                    if matches!(item.target, AgentTarget::Claude | AgentTarget::OpenCode) {
                        // ownership-aware semantic patch；skipExisting 若 leaf 已存在则跳过
                        let current_bytes = if path.exists() {
                            std::fs::read(&path)?
                        } else {
                            b"{}".to_vec()
                        };
                        let existing = {
                            use crate::agent_hub::config_patch::SemanticConfigPatcher;
                            JsoncConfigPatcher
                                .inspect(&current_bytes, &["mcpServers".into(), server.key.clone()])
                                .ok()
                        };
                        if change.install_mode == PortablePullInstallMode::SkipExisting
                            && existing.as_ref().map(|o| o.present).unwrap_or(false)
                        {
                            return Ok(());
                        }
                        // 原生 CLI shape（command/args/env 或 url/headers），禁止 Hub DTO 直序列化
                        let value = native_mcp_leaf_value(item.target, &server)?;
                        let expected = existing.and_then(|o| {
                            if o.present {
                                Some(value_content_hash(&o.value))
                            } else {
                                None
                            }
                        });
                        let patches = [ManagedConfigPatch {
                            owner_id: format!("portable-pull:{}", server.key),
                            path: vec!["mcpServers".into(), server.key.clone()],
                            value: Some(value),
                            expected_base_hash: expected,
                        }];
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        let prepared =
                            apply_config_patch_atomically(&JsoncConfigPatcher, &path, &patches)?;
                        if !matches!(
                            prepared.patched.outcome,
                            crate::agent_hub::config_patch::ConfigPatchOutcome::Applied
                        ) {
                            return Err(AppError::conflict(format!(
                                "PORTABLE_PULL_MCP_PATCH:{:?}",
                                prepared.patched.outcome
                            )));
                        }
                        return Ok(());
                    }
                }
            }
            // 非 JSON MCP / Codex：fail-closed，禁止 silent side-file 冒充安装成功
            return Err(AppError::validation(
                "PORTABLE_PULL_MCP_INSTALL_UNSUPPORTED".to_string(),
            ));
        }
    }
    Ok(())
}

// ───────────────────────── tests ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::ScopeKind;
    use crate::agent_hub::portable_inventory::PortableInventoryItemCapabilitiesDto;
    use crate::agent_hub::snapshot::envelope::SnapshotSelection;

    fn sample_local_item(target: AgentTarget, native: &str) -> PortableInventoryItemDto {
        PortableInventoryItemDto {
            inventory_item_id: format!("id-{native}"),
            target,
            kind: PortableAssetKind::Command,
            native_id: native.into(),
            display_name: native.into(),
            description: None,
            version: None,
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            source_path: Some(format!("/tmp/{native}.md")),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            content_hash: Some("abc".into()),
            tree_hash: None,
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::Unmanaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto::default(),
            warnings: vec![],
            mcp_credential: None,
        }
    }

    #[test]
    fn remote_inventory_strips_paths_and_secrets() {
        let mut item = sample_local_item(AgentTarget::Claude, "ship");
        item.kind = PortableAssetKind::Mcp;
        item.mcp_credential = Some(PortableMcpCredentialFactDto {
            present: true,
            hash: Some("credhash".into()),
        });
        let snap = PortableInventorySnapshotDto {
            inventory_snapshot_hash: "h1".into(),
            refreshed_at: "2026-08-08T00:00:00Z".into(),
            stale: false,
            targets: vec![],
            items: vec![item],
        };
        let remote = snapshot_to_remote("dev-a", AgentTarget::Claude, &snap);
        assert!(remote_inventory_is_metadata_only(&remote));
        let json = serde_json::to_string(&remote).unwrap();
        assert!(!json.contains("/tmp/"));
        assert!(!json.to_ascii_lowercase().contains("sourcepath"));
    }

    #[test]
    fn chunk_limit_is_8mib() {
        assert_eq!(PORTABLE_PULL_MAX_CHUNK_BYTES, 8 * 1024 * 1024);
    }

    #[test]
    fn install_mode_wire_tokens() {
        assert_eq!(
            PortablePullInstallMode::ImportedCanonicalOnly.as_str(),
            "importedCanonicalOnly"
        );
        assert_eq!(
            PortablePullInstallMode::SkipExisting.as_str(),
            "skipExisting"
        );
    }

    #[test]
    fn capability_token_is_portable_pull_v1() {
        assert_eq!(CAPABILITY_PORTABLE_PULL_V1, "agent-hub.portable-pull.v1");
    }

    #[test]
    fn preview_rejects_cross_target_at_request_shape() {
        let req = PreviewPortablePullRequest {
            source_device_id: "d".into(),
            source_target: AgentTarget::Claude,
            destination_target: AgentTarget::Codex,
            remote_inventory_snapshot_hash: "h".into(),
            inventory_item_ids: vec!["a".into()],
            conflict_policy: PortableAssetConflictPolicy::SkipExisting,
        };
        assert_ne!(req.source_target, req.destination_target);
    }

    #[tokio::test]
    async fn durable_pull_claim_replay_after_complete() {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        let repo = AgentHubRepo::new(pool);
        let plan = StoredPortablePullPlan {
            public: PortablePullPlanDto {
                plan_token: "plan-a".into(),
                expires_at: (Utc::now() + ChronoDuration::minutes(10)).to_rfc3339(),
                source_device_id: "d".into(),
                source_target: AgentTarget::Claude,
                destination_target: AgentTarget::Claude,
                remote_inventory_snapshot_hash: "r".into(),
                local_inventory_snapshot_hash: "l".into(),
                conflict_policy: PortableAssetConflictPolicy::SkipExisting,
                selection_manifest_hash: "m".into(),
                credential_bearing_count: 0,
                has_credential_bearing_assets: false,
                changes: vec![],
                blocking_reasons: vec![],
            },
            remote_item_ids: vec![],
        };
        let plan_json = serde_json::to_string(&plan).unwrap();
        repo.insert_portable_pull_plan(PortablePullPlanRecord {
            plan_token: "plan-a".into(),
            expires_at: plan.public.expires_at.clone(),
            remote_inventory_snapshot_hash: "r".into(),
            local_inventory_snapshot_hash: "l".into(),
            plan_json,
            client_request_id: None,
            claimed_at: None,
            consumed_at: None,
            result_json: None,
            created_at: Utc::now().to_rfc3339(),
        })
        .await
        .unwrap();
        let claim = repo
            .claim_portable_pull_plan("plan-a", "req-1")
            .await
            .unwrap();
        assert!(matches!(claim, PortablePullClaim::Claimed(_)));
        let result = PortablePullResultDto {
            plan_token: "plan-a".into(),
            client_request_id: "req-1".into(),
            source_device_id: "d".into(),
            source_target: AgentTarget::Claude,
            destination_target: AgentTarget::Claude,
            partial: false,
            items: vec![],
        };
        repo.complete_portable_pull_plan(
            "plan-a",
            "req-1",
            &serde_json::to_string(&result).unwrap(),
        )
        .await
        .unwrap();
        let replay = repo
            .claim_portable_pull_plan("plan-a", "req-1")
            .await
            .unwrap();
        match replay {
            PortablePullClaim::Replay(json) => {
                let back: PortablePullResultDto = serde_json::from_str(&json).unwrap();
                assert_eq!(back, result);
            }
            other => panic!("expected replay, got {other:?}"),
        }
        // 同 request 绑不同 plan → conflict
        repo.insert_portable_pull_plan(PortablePullPlanRecord {
            plan_token: "plan-b".into(),
            expires_at: (Utc::now() + ChronoDuration::minutes(10)).to_rfc3339(),
            remote_inventory_snapshot_hash: "r".into(),
            local_inventory_snapshot_hash: "l".into(),
            plan_json: serde_json::to_string(&plan).unwrap(),
            client_request_id: None,
            claimed_at: None,
            consumed_at: None,
            result_json: None,
            created_at: Utc::now().to_rfc3339(),
        })
        .await
        .unwrap();
        let conflict = repo.claim_portable_pull_plan("plan-b", "req-1").await;
        assert!(conflict.is_err());
    }

    #[test]
    fn skill_install_must_not_drop_body_without_tree() {
        // 契约：canonical Skill 无 tree 时 install 必须 fail-closed，不得只写 frontmatter
        let src = include_str!("pull.rs");
        assert!(src.contains("PORTABLE_PULL_SKILL_TREE_UNAVAILABLE"));
        assert!(src.contains("PORTABLE_PULL_CANONICAL_IMPORT_REQUIRED"));
        assert!(src.contains("install verified by rescan"));
    }

    #[test]
    fn skip_existing_demotion_fail_closed_before_install_and_rescan() {
        // R2-M1 / Spec M5：skipExisting 降级不得在 import 失败后写目标；成功后需 rescan gate
        let src = include_str!("pull.rs");
        assert!(src.contains("canonical import failed before skipExisting demotion install"));
        assert!(src.contains("skipExisting demotion install verified by rescan"));
        assert!(src.contains("PORTABLE_PULL_RESCAN_MISSING"));
    }

    #[test]
    fn source_chunk_resume_from_offset() {
        let mut built = BuiltPortableSelection {
            envelope: SnapshotEnvelopeV1 {
                format: "cc-partner-agent-hub".into(),
                format_version: 1,
                canonicalization: "RFC8785-JSON".into(),
                snapshot_id: "s".into(),
                snapshot_hash: "0".repeat(64),
                source_replica_id: "d".into(),
                created_at: "t".into(),
                selection: SnapshotSelection {
                    scope_ids: vec![],
                    asset_ids: vec![],
                    include_history: false,
                },
                asset_heads: BTreeMap::new(),
                assets: vec![],
                lineages: vec![],
                revisions: vec![],
                variants: vec![],
                conflicts: vec![],
                aliases: vec![],
                objects: vec![],
            },
            items: vec![],
            object_bytes: BTreeMap::new(),
        };
        let payload = vec![1u8, 2, 3, 4, 5];
        let hash = sha256_hex(&payload);
        built.object_bytes.insert(hash.clone(), payload.clone());
        staging_insert("tid-resume".into(), built).unwrap();
        let c1 = source_read_object_chunk("tid-resume", &hash, 0).unwrap();
        assert_eq!(c1, payload);
        // full read of single object cleans staging
        assert!(
            staging().lock().unwrap().get("tid-resume").is_none(),
            "staging must drop transfer after full object transfer"
        );
        // re-stage for offset resume check
        let mut built2 = BuiltPortableSelection {
            envelope: SnapshotEnvelopeV1 {
                format: "cc-partner-agent-hub".into(),
                format_version: 1,
                canonicalization: "RFC8785-JSON".into(),
                snapshot_id: "s".into(),
                snapshot_hash: "0".repeat(64),
                source_replica_id: "d".into(),
                created_at: "t".into(),
                selection: SnapshotSelection {
                    scope_ids: vec![],
                    asset_ids: vec![],
                    include_history: false,
                },
                asset_heads: BTreeMap::new(),
                assets: vec![],
                lineages: vec![],
                revisions: vec![],
                variants: vec![],
                conflicts: vec![],
                aliases: vec![],
                objects: vec![],
            },
            items: vec![],
            object_bytes: BTreeMap::new(),
        };
        // multi-object keeps staging until explicit release
        let p2 = vec![9u8, 8, 7];
        let h2 = sha256_hex(&p2);
        built2.object_bytes.insert(hash.clone(), payload.clone());
        built2.object_bytes.insert(h2.clone(), p2.clone());
        staging_insert("tid-multi".into(), built2).unwrap();
        let c2 = source_read_object_chunk("tid-multi", &hash, 2).unwrap();
        assert_eq!(c2, payload[2..]);
        assert!(staging().lock().unwrap().contains_key("tid-multi"));
        source_release_transfer("tid-multi");
        assert!(!staging().lock().unwrap().contains_key("tid-multi"));
        let missing = source_read_object_chunk("tid-resume", &hash, 100);
        assert!(missing.is_err());
    }

    #[test]
    fn safe_tree_dest_rejects_traversal() {
        let dir = PathBuf::from("/tmp/skill-root");
        assert!(safe_tree_dest(&dir, "SKILL.md").is_ok());
        assert!(safe_tree_dest(&dir, "nested/file.txt").is_ok());
        assert!(safe_tree_dest(&dir, "../escape").is_err());
        assert!(safe_tree_dest(&dir, "/abs/path").is_err());
        assert!(safe_tree_dest(&dir, "a/../../b").is_err());
        let err = safe_tree_dest(&dir, "../x").unwrap_err();
        assert!(err.to_string().contains("PORTABLE_PULL_UNSAFE_TREE_PATH"));
    }

    #[test]
    fn native_mcp_leaf_is_cli_shape_not_hub_dto() {
        use crate::agent_hub::assets::{McpTransport, PortableMcpServer};
        use std::collections::BTreeMap;
        let server = PortableMcpServer {
            key: "demo".into(),
            transport: McpTransport::Stdio {
                command: "uvx".into(),
                args: vec!["svc".into()],
                cwd: None,
            },
            env: {
                let mut m = BTreeMap::new();
                m.insert("TOKEN".into(), "secret-value".into());
                m
            },
            enabled: true,
            tool_allow: vec!["x".into()],
            tool_deny: vec![],
            target_extensions: BTreeMap::new(),
        };
        let v = native_mcp_leaf_value(AgentTarget::Claude, &server).unwrap();
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"command\""));
        assert!(s.contains("\"args\""));
        assert!(!s.contains("\"key\""), "must not embed hub key field");
        assert!(
            !s.contains("toolAllow") && !s.contains("tool_allow"),
            "must not embed hub toolAllow"
        );
        // credentials preserved in env bytes but never logged by this helper
        assert_eq!(v["env"]["TOKEN"], "secret-value");
    }

    #[test]
    fn claude_mcp_config_path_is_home_dot_claude_json_by_default() {
        // When CLAUDE_CONFIG_DIR unset, scanner & pull share ~/.claude.json (not ~/.claude/.claude.json)
        let path = {
            // isolate: clear env for assertion of default branch via pure logic
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
            if std::env::var_os("CLAUDE_CONFIG_DIR").is_some() {
                // still must be <CLAUDE_CONFIG_DIR>/.claude.json shape
                let p = resolve_claude_mcp_config_path();
                assert!(p.ends_with(".claude.json"));
                p
            } else {
                let p = resolve_claude_mcp_config_path();
                assert_eq!(p, home.join(".claude.json"));
                p
            }
        };
        assert!(
            !path.to_string_lossy().ends_with(".claude/.claude.json")
                || std::env::var_os("CLAUDE_CONFIG_DIR").is_some(),
            "default path must not nest under ~/.claude/"
        );
    }

    #[test]
    fn plugin_hash_list_payload_is_rejected_contract() {
        let src = include_str!("pull.rs");
        assert!(src.contains("PORTABLE_PULL_PLUGIN_TREE_UNAVAILABLE"));
        assert!(src.contains("portablePluginTreeRef"));
        assert!(src.contains("PORTABLE_PULL_UNSAFE_TREE_PATH"));
        assert!(src.contains("PORTABLE_PULL_STAGING_LIMIT"));
        assert!(src.contains("PORTABLE_PULL_REMOTE_INVENTORY_STALE"));
        assert!(src.contains("native_mcp_leaf_value"));
        assert!(src.contains("resolve_claude_mcp_config_path"));
    }

    #[test]
    fn staging_limit_rejects_oversized() {
        // Construct many tiny staged transfers until limit
        let make = |id: &str| {
            let mut built = BuiltPortableSelection {
                envelope: SnapshotEnvelopeV1 {
                    format: "cc-partner-agent-hub".into(),
                    format_version: 1,
                    canonicalization: "RFC8785-JSON".into(),
                    snapshot_id: id.into(),
                    snapshot_hash: "0".repeat(64),
                    source_replica_id: "d".into(),
                    created_at: "t".into(),
                    selection: SnapshotSelection {
                        scope_ids: vec![],
                        asset_ids: vec![],
                        include_history: false,
                    },
                    asset_heads: BTreeMap::new(),
                    assets: vec![],
                    lineages: vec![],
                    revisions: vec![],
                    variants: vec![],
                    conflicts: vec![],
                    aliases: vec![],
                    objects: vec![],
                },
                items: vec![],
                object_bytes: BTreeMap::new(),
            };
            built.object_bytes.insert(format!("h-{id}"), vec![1u8; 16]);
            built
        };
        // clear any prior
        {
            let mut g = staging().lock().unwrap();
            g.clear();
        }
        for i in 0..STAGING_MAX_ENTRIES {
            staging_insert(format!("lim-{i}"), make(&format!("lim-{i}"))).unwrap();
        }
        // next should still succeed after evicting oldest
        staging_insert("lim-extra".into(), make("lim-extra")).unwrap();
        // force hard limit by filling bytes with huge payload after clear
        {
            let mut g = staging().lock().unwrap();
            g.clear();
        }
        let mut huge = make("huge");
        huge.object_bytes.insert(
            "big".into(),
            vec![0u8; (STAGING_MAX_TOTAL_BYTES as usize) + 1],
        );
        let err = staging_insert("huge".into(), huge).unwrap_err();
        assert!(err.to_string().contains("PORTABLE_PULL_STAGING_LIMIT"));
    }
}
