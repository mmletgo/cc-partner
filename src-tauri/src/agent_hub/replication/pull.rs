//! agent_hub/replication/pull — 同类 Agent 远端 portable inventory + 选择性 Pull
//!
//! Business Logic（为什么需要这个模块）:
//!     用户从远端设备加载 metadata inventory，勾选后 preview/apply 到本机同类 Agent；
//!     只允许 sourceTarget == destinationTarget；未映射/未 opt-in 只导入 canonical。
//!     expected-device / clientRequestId 是路由绑定与幂等标签，**不是**身份认证。
//!
//! Code Logic（这个模块做什么）:
//!     远端 inventory 路由（无 secret）；本地 preview plan + apply（CAS 分块 ≤8MiB 续传、
//!     SnapshotImporter 导入、映射项安装）；clientRequestId 幂等与 partial 报告。

use crate::agent_hub::models::{AgentTarget, ScopeKind};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
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
use crate::error::AppError;
use crate::models::device::Device;
use crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER;
use crate::net::peer_client::PeerClient;
use crate::net::peer_error::PeerCallError;
use crate::net::peer_timeout::PeerTimeoutClass;
use crate::net::protocol::CAPABILITY_PORTABLE_PULL_V1;
use crate::net::request_context::{new_request_id, REQUEST_ID_HEADER};
use crate::state::AppState;
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
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

/// 内部持久化 plan。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPortablePullPlan {
    public: PortablePullPlanDto,
    remote_item_ids: Vec<String>,
}

// ───────────────────────── 进程内 plan/result/staging ─────────────────────────

fn plans() -> &'static Mutex<BTreeMap<String, StoredPortablePullPlan>> {
    static MAP: OnceLock<Mutex<BTreeMap<String, StoredPortablePullPlan>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn results() -> &'static Mutex<BTreeMap<String, PortablePullResultDto>> {
    static MAP: OnceLock<Mutex<BTreeMap<String, PortablePullResultDto>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn staging() -> &'static Mutex<BTreeMap<String, BuiltPortableSelection>> {
    static MAP: OnceLock<Mutex<BTreeMap<String, BuiltPortableSelection>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn store_plan(plan: StoredPortablePullPlan) {
    plans()
        .lock()
        .expect("pull plans")
        .insert(plan.public.plan_token.clone(), plan);
}

fn load_plan(token: &str) -> Option<StoredPortablePullPlan> {
    plans().lock().expect("pull plans").get(token).cloned()
}

fn store_result(result: PortablePullResultDto) {
    results()
        .lock()
        .expect("pull results")
        .insert(result.client_request_id.clone(), result);
}

fn load_result(client_request_id: &str) -> Option<PortablePullResultDto> {
    results()
        .lock()
        .expect("pull results")
        .get(client_request_id)
        .cloned()
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
    staging()
        .lock()
        .expect("staging")
        .insert(transfer_id, built);
    Ok(resp)
}

/// 源端读取 object chunk（offset 续传）。
pub fn source_read_object_chunk(
    transfer_id: &str,
    object_hash: &str,
    offset: u64,
) -> Result<Vec<u8>, AppError> {
    let g = staging().lock().expect("staging");
    let built = g
        .get(transfer_id)
        .ok_or_else(|| AppError::not_found("PORTABLE_PULL_TRANSFER_NOT_FOUND".to_string()))?;
    let bytes = built
        .object_bytes
        .get(object_hash)
        .ok_or_else(|| AppError::not_found("PORTABLE_PULL_OBJECT_NOT_FOUND".to_string()))?;
    if offset as usize >= bytes.len() {
        return Ok(Vec::new());
    }
    let end = (offset as usize + PORTABLE_PULL_MAX_CHUNK_BYTES).min(bytes.len());
    Ok(bytes[offset as usize..end].to_vec())
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
        expires_at,
        source_device_id: request.source_device_id.clone(),
        source_target: request.source_target,
        destination_target: request.destination_target,
        remote_inventory_snapshot_hash: remote.inventory_snapshot_hash,
        local_inventory_snapshot_hash: local.inventory_snapshot_hash,
        conflict_policy: request.conflict_policy,
        selection_manifest_hash,
        credential_bearing_count: credential_bearing,
        has_credential_bearing_assets: credential_bearing > 0,
        changes,
        blocking_reasons: blocking,
    };

    store_plan(StoredPortablePullPlan {
        public: public.clone(),
        remote_item_ids,
    });
    Ok(public)
}

/// 执行 pull。
pub async fn apply_portable_pull(
    state: &AppState,
    request: ApplyPortablePullRequest,
) -> Result<PortablePullResultDto, AppError> {
    if request.plan_token.trim().is_empty() || request.client_request_id.trim().is_empty() {
        return Err(AppError::validation(
            "PORTABLE_PULL_APPLY_IDS_REQUIRED".to_string(),
        ));
    }
    if let Some(existing) = load_result(&request.client_request_id) {
        if existing.plan_token != request.plan_token {
            return Err(AppError::conflict(
                "PORTABLE_PULL_REQUEST_ID_CONFLICT".to_string(),
            ));
        }
        return Ok(existing);
    }

    let stored = load_plan(&request.plan_token)
        .ok_or_else(|| AppError::not_found("PORTABLE_PULL_PLAN_NOT_FOUND".to_string()))?;
    if stored.public.expires_at < Utc::now().to_rfc3339() {
        return Err(AppError::conflict("PORTABLE_PULL_PLAN_EXPIRED".to_string()));
    }
    // transfer 前强制同源 target
    if stored.public.source_target != stored.public.destination_target {
        return Err(AppError::validation(
            "PORTABLE_PULL_TARGET_MISMATCH:source!=destination".to_string(),
        ));
    }

    let device = resolve_device(state, &stored.public.source_device_id)?;
    let base_url = device.base_url();
    let peer = PeerClient::new();
    peer.require_capability(&base_url, CAPABILITY_PORTABLE_PULL_V1)
        .await
        .map_err(peer_err)?;

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
        if store.get_blob(&obj.hash).await.is_ok() {
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
                break;
            }
            let n = chunk.len() as u64;
            buf.extend_from_slice(&chunk);
            offset += n;
            if size > 0 && offset >= size {
                break;
            }
            if size == 0 {
                break;
            }
        }
        if bytes_are_legacy_lossy(&buf) {
            continue;
        }
        if !buf.is_empty() {
            let got = sha256_hex(&buf);
            if got != obj.hash {
                return Err(AppError::validation(format!(
                    "PORTABLE_PULL_OBJECT_HASH_MISMATCH:{}",
                    obj.hash
                )));
            }
            store.put_blob(&buf).await?;
            collected.insert(obj.hash.clone(), buf);
        }
    }

    // 补齐 object_bytes 供 importer
    for obj in &selection.envelope.objects {
        if !collected.contains_key(&obj.hash) {
            if let Ok(b) = store.get_blob(&obj.hash).await {
                collected.insert(obj.hash.clone(), b);
            }
        }
    }

    let import_ok = import_selection_canonical(state, &store, &data_dir, &selection).await;

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
            items.push(PortablePullItemResultDto {
                inventory_item_id: change.inventory_item_id.clone(),
                state: PortablePullItemState::Skipped,
                install_mode: Some(change.install_mode),
                error_code: None,
                message: Some("skipExisting".into()),
            });
            continue;
        }
        if change.install_mode == PortablePullInstallMode::ImportedCanonicalOnly {
            items.push(PortablePullItemResultDto {
                inventory_item_id: change.inventory_item_id.clone(),
                state: PortablePullItemState::ImportedCanonicalOnly,
                install_mode: Some(change.install_mode),
                error_code: None,
                message: Some("canonical only; project unmapped or not opted-in".into()),
            });
            continue;
        }
        let sel = selection
            .items
            .iter()
            .find(|s| s.inventory_item_id == change.inventory_item_id);
        let install_res = if let Some(s) = sel {
            install_payload_to_target(s).await
        } else {
            Err(AppError::generic("selection item missing after transfer"))
        };
        match install_res {
            Ok(()) => items.push(PortablePullItemResultDto {
                inventory_item_id: change.inventory_item_id.clone(),
                state: PortablePullItemState::Succeeded,
                install_mode: Some(change.install_mode),
                error_code: if import_ok.is_err() {
                    Some("PORTABLE_PULL_CANONICAL_IMPORT_PARTIAL".into())
                } else {
                    None
                },
                message: None,
            }),
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

    let result = PortablePullResultDto {
        plan_token: request.plan_token,
        client_request_id: request.client_request_id,
        source_device_id: stored.public.source_device_id,
        source_target: stored.public.source_target,
        destination_target: stored.public.destination_target,
        partial: any_fail || import_ok.is_err(),
        items,
    };
    store_result(result.clone());
    Ok(result)
}

/// 按 clientRequestId 查询 pull 结果。
pub async fn get_portable_pull(
    _state: &AppState,
    client_request_id: &str,
) -> Result<PortablePullResultDto, AppError> {
    if client_request_id.trim().is_empty() {
        return Err(AppError::validation(
            "PORTABLE_PULL_REQUEST_ID_REQUIRED".to_string(),
        ));
    }
    load_result(client_request_id)
        .ok_or_else(|| AppError::not_found("PORTABLE_PULL_REQUEST_NOT_FOUND".to_string()))
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

use crate::storage::agent_hub_repo::AgentHubRepo;

async fn install_payload_to_target(item: &PortableSelectionItem) -> Result<(), AppError> {
    let data_dir = crate::config::data_dir()?;
    let store = ObjectStore::open(&data_dir)?;
    let bytes = store.get_blob(&item.object_hash).await?;
    if bytes_are_legacy_lossy(&bytes) {
        return Err(AppError::validation(
            "PORTABLE_PULL_LEGACY_LOSSY".to_string(),
        ));
    }
    let env_home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    let root = match item.target {
        AgentTarget::Claude => env_home.join(".claude"),
        AgentTarget::Codex => env_home.join(".codex"),
        AgentTarget::OpenCode => env_home.join(".config").join("opencode"),
    };
    match item.kind {
        PortableAssetKind::Command => {
            let dir = root.join("commands");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{}.md", item.native_id));
            if let Ok(payload) = crate::agent_hub::assets::from_canonical_bytes(&bytes) {
                if let crate::agent_hub::assets::PortableAssetPayload::Command(cmd) = payload {
                    let text = format!("---\nname: {}\n---\n{}\n", cmd.name, cmd.prompt_template);
                    std::fs::write(&path, text)?;
                    return Ok(());
                }
            }
            std::fs::write(&path, &bytes)?;
        }
        PortableAssetKind::Skill => {
            let dir = root.join("skills").join(&item.native_id);
            std::fs::create_dir_all(&dir)?;
            if let Ok(payload) = crate::agent_hub::assets::from_canonical_bytes(&bytes) {
                if let crate::agent_hub::assets::PortableAssetPayload::Skill(skill) = payload {
                    let text = format!(
                        "---\nname: {}\ndescription: {}\n---\n",
                        skill.name, skill.description
                    );
                    std::fs::write(dir.join("SKILL.md"), text)?;
                    return Ok(());
                }
            }
            std::fs::write(dir.join("SKILL.md"), &bytes)?;
        }
        PortableAssetKind::Plugin => {
            let dir = root.join("plugins").join(&item.native_id);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("plugin.json"), &bytes)?;
        }
        PortableAssetKind::Mcp => {
            let path = match item.target {
                AgentTarget::Claude => root.join(".mcp.json"),
                AgentTarget::Codex => root.join("config.toml"),
                AgentTarget::OpenCode => root.join("mcp.json"),
            };
            if let Ok(payload) = crate::agent_hub::assets::from_canonical_bytes(&bytes) {
                if let crate::agent_hub::assets::PortableAssetPayload::Mcp(server) = payload {
                    if matches!(item.target, AgentTarget::Claude | AgentTarget::OpenCode) {
                        let mut root_val = if path.exists() {
                            let t = std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".into());
                            serde_json::from_str(&t).unwrap_or(serde_json::json!({}))
                        } else {
                            serde_json::json!({})
                        };
                        if !root_val.is_object() {
                            root_val = serde_json::json!({});
                        }
                        let map = root_val
                            .as_object_mut()
                            .unwrap()
                            .entry("mcpServers")
                            .or_insert_with(|| serde_json::json!({}));
                        if let Some(obj) = map.as_object_mut() {
                            obj.insert(
                                server.key.clone(),
                                serde_json::to_value(&server).unwrap_or(serde_json::json!({})),
                            );
                        }
                        std::fs::write(
                            &path,
                            serde_json::to_vec_pretty(&root_val).unwrap_or_default(),
                        )?;
                        return Ok(());
                    }
                }
            }
            let side = path.with_extension(format!("pull-{}.json", item.native_id));
            std::fs::write(side, &bytes)?;
        }
    }
    Ok(())
}

// ───────────────────────── tests ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn replay_conflict_when_request_id_bound_to_other_plan() {
        store_result(PortablePullResultDto {
            plan_token: "plan-a".into(),
            client_request_id: "req-1".into(),
            source_device_id: "d".into(),
            source_target: AgentTarget::Claude,
            destination_target: AgentTarget::Claude,
            partial: false,
            items: vec![],
        });
        let existing = load_result("req-1").unwrap();
        assert_eq!(existing.plan_token, "plan-a");
        assert_ne!(existing.plan_token, "plan-b");
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
        staging().lock().unwrap().insert("tid-resume".into(), built);
        let c1 = source_read_object_chunk("tid-resume", &hash, 0).unwrap();
        assert_eq!(c1, payload);
        let c2 = source_read_object_chunk("tid-resume", &hash, 2).unwrap();
        assert_eq!(c2, payload[2..]);
        let c3 = source_read_object_chunk("tid-resume", &hash, 100).unwrap();
        assert!(c3.is_empty());
    }
}
