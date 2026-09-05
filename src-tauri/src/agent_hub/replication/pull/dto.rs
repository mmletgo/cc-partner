//! agent_hub/replication/pull/dto — Pull 线协议 DTO / 请求响应定义
//!
//! Business Logic（为什么需要这个模块）:
//!     跨设备 portable Pull 的全部 wire 契约集中在此：远端 inventory 浏览、preview plan、
//!     apply/replay、源端 selection 查询以及远端项目代理请求，供路由层 / backend CLI /
//!     本机 GUI 共用同一份形状（serde camelCase，敏感字段永不出现）。
//!
//! Code Logic（这个模块做什么）:
//!     纯数据定义：DTO struct / enum 及其 as_str 序列化辅助；无 I/O、无状态。

use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::portable_actions::{
    ApplyPortableAssetActionRequest, PortableAssetConflictPolicy, PreviewPortableAssetActionRequest,
};
use crate::agent_hub::portable_inventory::{
    PortableAssetKind, PortableInventorySourceOrigin, PortableMcpCredentialFactDto,
};
use crate::agent_hub::snapshot::envelope::SnapshotEnvelopeV1;
use crate::agent_hub::snapshot::portable_builder::PortableSelectionItem;
use serde::{Deserialize, Serialize};

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
    /// 对端 Workbench 的本地项目 id；None 表示 user scope。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_local_project_id: Option<String>,
    /// 本机保存的 remote shortcut id；存在时由 owner 解析为对端 local project id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_project_ref: Option<String>,
}

/// Pull preview 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewPortablePullRequest {
    pub source_device_id: String,
    pub source_target: AgentTarget,
    pub destination_target: AgentTarget,
    /// 对端 Workbench 的本地项目 id；None 表示从对端 user scope 拉取。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_local_project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_project_ref: Option<String>,
    /// 本机 Workbench 的本地项目 id；None 表示写入本机 user scope。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_local_project_id: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_local_project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_project_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_local_project_id: Option<String>,
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
    /// Resolved destination scope identity (user / project:<id>); required for conflict + rescan.
    #[serde(default)]
    pub scope_id: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_local_project_id: Option<String>,
}

/// 源端 selection 查询 body。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSelectionQuery {
    pub source_target: AgentTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_local_project_id: Option<String>,
    pub inventory_item_ids: Vec<String>,
}

/// 本机 GUI 使用的远端项目 inventory 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InspectRemoteProjectPortableInventoryRequest {
    /// 本机保存的 Workbench remote shortcut id。
    pub project_ref: String,
    pub target: AgentTarget,
    #[serde(default)]
    pub kind: Option<PortableAssetKind>,
}

/// Peer project inventory wire body；local_project_id 属于 owning peer。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteProjectPortableInventoryQuery {
    pub local_project_id: String,
    pub target: AgentTarget,
    #[serde(default)]
    pub kind: Option<PortableAssetKind>,
}

/// 本机 GUI 使用的远端项目 action preview 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewRemoteProjectPortableActionRequest {
    pub project_ref: String,
    pub request: PreviewPortableAssetActionRequest,
}

/// 本机 GUI 使用的远端项目 action apply 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyRemoteProjectPortableActionRequest {
    pub project_ref: String,
    pub request: ApplyPortableAssetActionRequest,
}

/// 本机 GUI 使用的远端项目 action 对账请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetRemoteProjectPortableActionRequest {
    pub project_ref: String,
    pub client_request_id: String,
}

/// 本机 GUI 使用的远端项目 opt-in 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteProjectRefRequest {
    pub project_ref: String,
}
