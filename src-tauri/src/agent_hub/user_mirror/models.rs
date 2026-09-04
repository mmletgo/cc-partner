//! user_mirror/models — 用户级镜像 DTO、TTL 与稳定错误码
//!
//! Business Logic（为什么需要这个模块）:
//!     preview/apply 与 LAN 传输必须共用同一份 camelCase 合同；错误码写入协议与前端 decoder，
//!     不得随实现漂移。本文件只定义形状，不执行 inventory/apply。
//!
//! Code Logic（这个模块做什么）:
//!     导出 TTL、512 MiB 上限、稳定 error code 字符串与 inventory/plan/result DTO。

use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::portable_inventory::PortableAssetKind;
use serde::{Deserialize, Serialize};

/// Preview plan 有效期（分钟）。
///
/// Business Logic: 过期 plan 不得覆盖原生文件；任一侧 inventory 漂移必须重新预览。
/// Code Logic: apply 用 `expires_at` 与本常量对齐的 TTL 墙钟校验。
pub const USER_MIRROR_PLAN_TTL_MINUTES: i64 = 15;

/// 镜像传输累计上限（字节）。
///
/// Business Logic: 全 Agent + Plugin 超过现 portable-pull 的 64 MiB；超限 fail-closed。
/// Code Logic: 512 MiB；突破返回 `USER_MIRROR_TRANSFER_LIMIT`。
pub const USER_MIRROR_DEST_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

/// 对端未宣告 `agent-hub.user-mirror.v1`。
///
/// Business Logic: 缺能力时整次失败，禁止回落逐项 portable-pull / agent-hub.v1 push。
/// Code Logic: 稳定 transport/control 错误码。
pub const USER_MIRROR_CAPABILITY_UNSUPPORTED: &str = "USER_MIRROR_CAPABILITY_UNSUPPORTED";

/// 所选对端离线。
///
/// Business Logic: 镜像必须打到当前在线 owning process，不得对离线设备宣称成功。
/// Code Logic: 稳定错误码。
pub const USER_MIRROR_PEER_OFFLINE: &str = "USER_MIRROR_PEER_OFFLINE";

/// 源/目标 inventory 或 plan 绑定已漂移。
///
/// Business Logic: 过期预览不得写盘；必须重新 preview。
/// Code Logic: 稳定错误码。
pub const USER_MIRROR_STALE: &str = "USER_MIRROR_STALE";

/// 未预览或预览与当前选择不一致。
///
/// Business Logic: 强制预览 + 破坏性确认，禁止跳过 plan 直接 apply。
/// Code Logic: 稳定错误码。
pub const USER_MIRROR_PREVIEW_REQUIRED: &str = "USER_MIRROR_PREVIEW_REQUIRED";

/// 对象累计超过 512 MiB。
///
/// Business Logic: 超限不得部分下载后静默截断。
/// Code Logic: 稳定错误码。
pub const USER_MIRROR_TRANSFER_LIMIT: &str = "USER_MIRROR_TRANSFER_LIMIT";

/// 解析结果落到白名单外路径。
///
/// Business Logic: 禁止写仓库根 `AGENTS.md` 或把 OpenCode fallback 写成 Claude `CLAUDE.md`。
/// Code Logic: 稳定错误码。
pub const USER_MIRROR_NATIVE_PATH_FORBIDDEN: &str = "USER_MIRROR_NATIVE_PATH_FORBIDDEN";

/// MCP 占位凭据不得覆盖目标已有真凭据。
///
/// Business Logic: `legacyLossy` 该 server 标失败并继续其他项。
/// Code Logic: 稳定错误码。
pub const USER_MIRROR_LEGACY_LOSSY_BLOCKED: &str = "USER_MIRROR_LEGACY_LOSSY_BLOCKED";

/// 镜像方向：Pull 对端覆盖本机，Push 本机覆盖所选对端。
///
/// Business Logic: apply 端永远是 destination；UI 方向决定谁是 source。
/// Code Logic: camelCase wire。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserMirrorDirection {
    Pull,
    Push,
}

/// 预览中单条文件/资产将执行的动作。
///
/// Business Logic: 用户必须在确认框看到写/替换/清空/删除/停用，而不是笼统「同步」。
/// Code Logic: camelCase；Plugin 多余为 Disable，Skill/MCP 多余为 Delete，空原生文件为 Clear。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserMirrorChangeOp {
    Write,
    Replace,
    Clear,
    Delete,
    Disable,
}

/// MCP 凭据仅暴露 present/hash，永不回显 secret。
///
/// Business Logic: inventory/UI/log 不得包含明文 env；凭据只在 CAS 对象里。
/// Code Logic: `present` + 可选内容 hash。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorMcpCredentialFactDto {
    pub present: bool,
    pub hash: Option<String>,
}

/// 用户级原生提示词文件的元数据事实（无绝对路径）。
///
/// Business Logic: 预览按逻辑 id 对号入座，禁止把路径泄漏到 LAN JSON。
/// Code Logic: `logical_id` 形如 `claude.native.CLAUDE.md` / `cursor.slot.adapted`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorNativeFileFactDto {
    /// 稳定逻辑 id，例如 `claude.native.CLAUDE.md` / `cursor.slot.adapted`
    pub logical_id: String,
    pub content_hash: Option<String>,
    pub exists: bool,
    pub size: u64,
}

/// 用户级 portable 条目元数据。
///
/// Business Logic: Skill/Command/Plugin/MCP 进入镜像选择；MCP 凭据只给 present/hash。
/// Code Logic: 复用 `PortableAssetKind`；warnings 为扫描诊断，不含 secret。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorPortableItemDto {
    pub kind: PortableAssetKind,
    pub native_id: String,
    pub display_name: String,
    pub content_hash: Option<String>,
    pub tree_hash: Option<String>,
    pub actual_enabled: Option<bool>,
    pub mcp_credential: Option<UserMirrorMcpCredentialFactDto>,
    pub warnings: Vec<String>,
}

/// 三槽 canonical 内容 hash。
///
/// Business Logic: inventory 只传 hash，正文走 CAS，避免把整份指令放进元数据快照。
/// Code Logic: 空槽为 None。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorSlotHashesDto {
    pub common: Option<String>,
    pub adapted: Option<String>,
    pub exclusive: Option<String>,
}

/// 单个已登记 Agent 的用户级 inventory。
///
/// Business Logic: 同名 Agent 对号入座；一次覆盖 catalog 全部 Hub Agent。
/// Code Logic: slots + 原生文件事实 + portable 条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorAgentInventoryDto {
    pub target: AgentTarget,
    pub slots: UserMirrorSlotHashesDto,
    pub native_files: Vec<UserMirrorNativeFileFactDto>,
    pub items: Vec<UserMirrorPortableItemDto>,
}

/// 全 Agent 用户级元数据快照。
///
/// Business Logic: 源端暴露 inventory；无 path、无 secret、无 env。
/// Code Logic: `inventory_snapshot_hash` 绑定 preview/apply，漂移则 STALE。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorInventoryDto {
    pub source_device_id: String,
    pub inventory_snapshot_hash: String,
    pub refreshed_at: String,
    pub agents: Vec<UserMirrorAgentInventoryDto>,
    pub credential_bearing_count: u64,
}

/// 预览镜像请求。
///
/// Business Logic: Pull 选一台源设备；Push 可多选对端；没有条目勾选或 mode。
/// Code Logic: `source_device_id` 仅 Pull；`peer_device_ids` 仅 Push。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewUserMirrorRequest {
    pub direction: UserMirrorDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_device_id: Option<String>,
    #[serde(default)]
    pub peer_device_ids: Vec<String>,
}

/// 原生提示词文件将发生的变更。
///
/// Business Logic: 预览列出写/替换/清空，用户确认后才 apply。
/// Code Logic: 只含逻辑 id 与两侧 hash，无绝对路径。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorFileChangeDto {
    pub logical_id: String,
    pub op: UserMirrorChangeOp,
    pub source_hash: Option<String>,
    pub dest_hash: Option<String>,
}

/// portable 资产将发生的变更。
///
/// Business Logic: 预览按新增/替换/删除/停用列出，并标是否含凭据。
/// Code Logic: MCP 删除与 Plugin disable 分列于 plan 不同字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorPortableChangeDto {
    pub kind: PortableAssetKind,
    pub native_id: String,
    pub display_name: String,
    pub op: UserMirrorChangeOp,
    pub credential_bearing: bool,
}

/// 单个 Agent 的镜像 plan。
///
/// Business Logic: 用户按 Agent 看到指令文件与资产变更数量。
/// Code Logic: 指令写、portable upsert/delete、plugin disable、MCP 删除分列。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorAgentPlanDto {
    pub target: AgentTarget,
    pub instruction_writes: Vec<UserMirrorFileChangeDto>,
    pub portable_upserts: Vec<UserMirrorPortableChangeDto>,
    pub portable_deletes: Vec<UserMirrorPortableChangeDto>,
    pub plugin_disables: Vec<UserMirrorPortableChangeDto>,
    pub mcp_deletes: Vec<UserMirrorPortableChangeDto>,
}

/// portable 资产选择键：跨 Agent 联动（同名 Skill 在多个 Agent 上算同一资产）。
///
/// Business Logic: 用户按 `(kind, nativeId)` 勾选要同步的资产；同名资产跨 Agent 一起同步。
/// Code Logic: camelCase wire；与 inventory/plan 的 `(kind, native_id)` 对号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorPortableKeyDto {
    pub kind: PortableAssetKind,
    pub native_id: String,
}

/// 镜像选择过滤器；None / 缺省 = 全部同步（默认行为不变）。
///
/// Business Logic: pull/push 时用户可选择只同步部分资产；缺省必须等价旧的全量镜像。
/// Code Logic: `include_instructions` 缺省 true；`portable_keys=None` 表示全部 portable 资产。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UserMirrorSelectionFilterDto {
    /// 原生指令文件 + Hub 三槽是否同步（默认 true）
    #[serde(default = "default_true")]
    pub include_instructions: bool,
    /// None = 全部 portable 资产；Some(list) = 仅选中的键
    pub portable_keys: Option<Vec<UserMirrorPortableKeyDto>>,
}

impl Default for UserMirrorSelectionFilterDto {
    /// Business Logic: 缺省过滤器必须等价「全量镜像」，不得让 Default 意外关闭指令。
    /// Code Logic: include_instructions=true、portable_keys=None。
    fn default() -> Self {
        Self {
            include_instructions: true,
            portable_keys: None,
        }
    }
}

/// `include_instructions` 的 serde 缺省值（true）。
fn default_true() -> bool {
    true
}

/// 绑定源/目标 inventory 的镜像 preview plan。
///
/// Business Logic: apply 必须带本 plan；TTL 15 分钟；凭据条数用于确认框披露。
/// Code Logic: `plan_token` + 两侧 snapshot hash + per-agent 变更。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorPlanDto {
    pub plan_token: String,
    pub expires_at: String,
    pub direction: UserMirrorDirection,
    pub source_device_id: String,
    pub destination_device_id: String,
    pub remote_inventory_snapshot_hash: String,
    pub local_inventory_snapshot_hash: String,
    pub credential_bearing_count: u64,
    pub has_credential_bearing_assets: bool,
    pub agents: Vec<UserMirrorAgentPlanDto>,
    pub blocking_reasons: Vec<String>,
    /// Push 所选对端；空则 apply 回落到 `destination_device_id`。
    #[serde(default)]
    pub peer_device_ids: Vec<String>,
    /// 用户选择的同步范围；None = 全量。preview 时为空，apply 时由 request 合并进内存 plan，
    /// push fan-out 经 dest_plan 序列化携带到对端。
    #[serde(default)]
    pub selection: Option<UserMirrorSelectionFilterDto>,
}

/// 应用已预览镜像的请求。
///
/// Business Logic: 同 `clientRequestId` 重放同一结果；不同 plan 冲突。
/// Code Logic: planToken + clientRequestId；`selection` 缺省 None = 全量同步（旧客户端兼容）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyUserMirrorRequest {
    pub plan_token: String,
    pub client_request_id: String,
    /// 本机 apply 的同步范围选择；push-dest 经 plan 携带（缺省 None = 全量）。
    #[serde(default)]
    pub selection: Option<UserMirrorSelectionFilterDto>,
}

/// 单 Agent / 条目落地结果。
///
/// Business Logic: 崩溃后未知不得标成功；部分成功不回滚。
/// Code Logic: camelCase；`outcomeUnknown` 表示未完成。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserMirrorItemState {
    Succeeded,
    Failed,
    Skipped,
    OutcomeUnknown,
}

/// 单个 Agent 的 apply 结果。
///
/// Business Logic: 分项展示 succeeded/failed/unknown，已成功项保留。
/// Code Logic: 失败时带稳定 error_code。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorAgentResultDto {
    pub target: AgentTarget,
    pub state: UserMirrorItemState,
    pub error_code: Option<String>,
    pub message: Option<String>,
}

/// 一次镜像 apply 的整次结果。
///
/// Business Logic: `partial=true` 当且仅当存在失败或 unknown。
/// Code Logic: 绑定 plan/request 与源/目标 device，按 Agent 列出状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorResultDto {
    pub plan_token: String,
    pub client_request_id: String,
    pub source_device_id: String,
    pub destination_device_id: String,
    pub partial: bool,
    pub agents: Vec<UserMirrorAgentResultDto>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     前端 decoder 与协议文档按精确字符串识别镜像失败原因；改名会把失败显示成未知错误。
    ///
    /// Code Logic（这个测试做什么）:
    ///     锁定六条稳定 error code 字面量。
    #[test]
    fn user_mirror_error_codes_are_stable() {
        assert_eq!(
            USER_MIRROR_CAPABILITY_UNSUPPORTED,
            "USER_MIRROR_CAPABILITY_UNSUPPORTED"
        );
        assert_eq!(USER_MIRROR_STALE, "USER_MIRROR_STALE");
        assert_eq!(USER_MIRROR_PREVIEW_REQUIRED, "USER_MIRROR_PREVIEW_REQUIRED");
        assert_eq!(USER_MIRROR_TRANSFER_LIMIT, "USER_MIRROR_TRANSFER_LIMIT");
        assert_eq!(
            USER_MIRROR_NATIVE_PATH_FORBIDDEN,
            "USER_MIRROR_NATIVE_PATH_FORBIDDEN"
        );
        assert_eq!(
            USER_MIRROR_LEGACY_LOSSY_BLOCKED,
            "USER_MIRROR_LEGACY_LOSSY_BLOCKED"
        );
    }
}
