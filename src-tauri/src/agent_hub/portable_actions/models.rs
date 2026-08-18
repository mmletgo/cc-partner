//! portable_actions/models — 本机 portable 资产动作 preview/apply DTO
//!
//! Business Logic（为什么需要这个模块）:
//!     所有本机 enable/disable/uninstall/adopt/install 必须经短期 preview plan；
//!     UI 与 apply 只消费 camelCase 合同，不得自造字段或静默 adoption。
//!
//! Code Logic（这个模块做什么）:
//!     定义 action kind/item state、preview/apply 请求、plan/change/result DTO；
//!     MCP 相关字段禁止携带 secret 原文。

use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::portable_inventory::PortableAssetKind;
use serde::{Deserialize, Serialize};

/// 本机 portable 资产动作类型。
///
/// Business Logic（为什么需要这个枚举）:
///     启停/卸载/纳入/安装到源 target 是固定合同；禁止自由字符串动作。
///
/// Code Logic（这个枚举做什么）:
///     camelCase wire：`adopt`/`enable`/`disable`/`uninstall`/`installToSourceTarget`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableAssetActionKind {
    /// 显式纳入 Agent Hub（唯一创建长期 ownership 的动作）
    Adopt,
    /// 启用
    Enable,
    /// 禁用
    Disable,
    /// 卸载/移除（单目标，不删 canonical）
    Uninstall,
    /// 安装到源同类 target
    InstallToSourceTarget,
    /// 把当前 Agent 的 native 根挂到 store（建软链 / upsert MCP leaf）
    Attach,
    /// 只拆当前 Agent 的 store 软链 / 只删该 Agent MCP leaf
    Detach,
    /// 本机彻底删除 store 真树及剩余附加
    DestroyStore,
    /// 把非 store 的 native Skill/Command/MCP 迁入 store
    MigrateToStore,
    /// 把当前磁盘内容记为一致基准（只改 Hub 账本，不改文件）
    ConfirmCurrentVersion,
}

impl PortableAssetActionKind {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Adopt => "adopt",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Uninstall => "uninstall",
            Self::InstallToSourceTarget => "installToSourceTarget",
            Self::Attach => "attach",
            Self::Detach => "detach",
            Self::DestroyStore => "destroyStore",
            Self::MigrateToStore => "migrateToStore",
            Self::ConfirmCurrentVersion => "confirmCurrentVersion",
        }
    }

    /// 是否只写 Hub 账本、不改 Agent 磁盘/CLI。
    ///
    /// Business Logic（为什么需要）:
    ///     资产可能被 CLI 自己更新；确认当前版本只重记哈希，不得被「无 L3 写入」挡住。
    ///
    /// Code Logic（做什么）:
    ///     `confirmCurrentVersion` 为真；其余动作仍走 CLI/文件门禁。
    pub fn is_hub_ledger_only(self) -> bool {
        matches!(self, Self::ConfirmCurrentVersion)
    }
}

/// 单条动作结果状态。
///
/// Business Logic（为什么需要这个枚举）:
///     禁止用单一 success 掩盖逐项失败；未决 claim 必须诚实 `outcomeUnknown`。
///
/// Code Logic（这个枚举做什么）:
///     camelCase：`succeeded`/`skipped`/`failed`/`blocked`/`outcomeUnknown`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableAssetActionItemState {
    /// 执行成功且 rescan 对齐预期
    Succeeded,
    /// 幂等跳过
    Skipped,
    /// 执行失败
    Failed,
    /// 前置条件阻断
    Blocked,
    /// claim 后未完成或传输/spawn 不确定
    OutcomeUnknown,
}

impl PortableAssetActionItemState {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::OutcomeUnknown => "outcomeUnknown",
        }
    }
}

/// Canonical/ownership 影响。
///
/// Business Logic（为什么需要这个枚举）:
///     未纳管启停/卸载不得创建长期 ownership；只有 adopt 创建 ownership。
///
/// Code Logic（这个枚举做什么）:
///     camelCase：`none`/`createOwnership`/`updateDesired`/`tombstoneComponents`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableAssetCanonicalEffect {
    /// 不改 canonical/ownership
    None,
    /// 显式 adopt 创建 ownership
    CreateOwnership,
    /// 更新 desired presence/enabled
    UpdateDesired,
    /// Plugin 独占 component tombstone
    TombstoneComponents,
}

impl PortableAssetCanonicalEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::CreateOwnership => "createOwnership",
            Self::UpdateDesired => "updateDesired",
            Self::TombstoneComponents => "tombstoneComponents",
        }
    }
}

/// 文件/CLI 操作类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableAssetPlanOperation {
    /// 不写入
    Leave,
    /// 启用
    Enable,
    /// 禁用
    Disable,
    /// 卸载
    Uninstall,
    /// 安装
    Install,
    /// 纳入
    Adopt,
    /// 附加到当前 Agent
    Attach,
    /// 从当前 Agent 卸下
    Detach,
    /// 彻底删除 store
    DestroyStore,
    /// 迁入 portable-store
    MigrateToStore,
    /// 确认当前版本（重记账本哈希）
    ConfirmCurrentVersion,
}

impl PortableAssetPlanOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Leave => "leave",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Uninstall => "uninstall",
            Self::Install => "install",
            Self::Adopt => "adopt",
            Self::Attach => "attach",
            Self::Detach => "detach",
            Self::DestroyStore => "destroyStore",
            Self::MigrateToStore => "migrateToStore",
            Self::ConfirmCurrentVersion => "confirmCurrentVersion",
        }
    }
}

/// 备份策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableAssetBackupPolicy {
    /// 不备份
    None,
    /// 删除前写可恢复备份（Skill/Command）
    RecoverableBeforeDelete,
}

impl PortableAssetBackupPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RecoverableBeforeDelete => "recoverableBeforeDelete",
        }
    }
}

/// 覆盖/冲突策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PortableAssetConflictPolicy {
    /// 已存在则跳过
    #[default]
    SkipExisting,
    /// preview 后替换
    ReplaceAfterPreview,
}

impl PortableAssetConflictPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SkipExisting => "skipExisting",
            Self::ReplaceAfterPreview => "replaceAfterPreview",
        }
    }
}

/// Preview 请求。
///
/// Business Logic（为什么需要这个结构体）:
///     Apply 前必须绑定 inventory hash 与动作参数；uninstall 需要 keepData。
///
/// Code Logic（这个结构体做什么）:
///     camelCase + deny_unknown_fields。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewPortableAssetActionRequest {
    /// 当前 inventory 快照 hash
    pub inventory_snapshot_hash: String,
    /// 生成该快照时使用的扫描过滤条件；apply 必须按同一条件重新校验。
    #[serde(default)]
    pub inventory_query: crate::agent_hub::portable_inventory::PortableInventoryQuery,
    /// 参与动作的 inventory item id
    pub inventory_item_ids: Vec<String>,
    /// 动作类型
    pub action: PortableAssetActionKind,
    /// uninstall 是否保留数据
    #[serde(default)]
    pub keep_data: bool,
    /// 覆盖/冲突策略
    #[serde(default)]
    pub conflict_policy: PortableAssetConflictPolicy,
    /// 可选：期望 canonical revision（整批）
    #[serde(default)]
    pub expected_canonical_revision_id: Option<String>,
}

/// 单条 preview 变更。
///
/// Business Logic（为什么需要这个结构体）:
///     用户确认前必须看到 target/kind/path、操作、备份、hash、ownership/canonical 影响。
///
/// Code Logic（这个结构体做什么）:
///     camelCase change 行；无 MCP secret 字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableAssetActionChangeDto {
    /// inventory item id
    pub inventory_item_id: String,
    /// target
    pub target: AgentTarget,
    /// 四类 kind
    pub kind: PortableAssetKind,
    /// 目标路径
    pub path: Option<String>,
    /// 文件/CLI 操作
    pub operation: PortableAssetPlanOperation,
    /// 当前源内容 hash
    pub expected_source_hash: Option<String>,
    /// 当前目录树 hash
    pub expected_tree_hash: Option<String>,
    /// 期望 canonical revision
    pub expected_canonical_revision_id: Option<String>,
    /// 备份策略
    pub backup_policy: PortableAssetBackupPolicy,
    /// ownership 影响（仅 adopt 为 true）
    pub creates_ownership: bool,
    /// canonical 影响
    pub canonical_effect: PortableAssetCanonicalEffect,
    /// 该项阻断原因
    pub blocking_reasons: Vec<String>,
    /// 警告（无 secret）
    pub warnings: Vec<String>,
}

/// 短期 preview plan。
///
/// Business Logic（为什么需要这个结构体）:
///     Apply 只接受 planToken；计划有 TTL，过期必须重新 preview。
///
/// Code Logic（这个结构体做什么）:
///     camelCase public plan；含 changes 与 blocking_reasons。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableAssetActionPlanDto {
    /// 不可猜短期 token
    pub plan_token: String,
    /// 过期时间 RFC3339
    pub expires_at: String,
    /// 绑定的 inventory hash
    pub inventory_snapshot_hash: String,
    /// 动作类型
    pub action: PortableAssetActionKind,
    /// uninstall keepData
    pub keep_data: bool,
    /// 冲突策略
    pub conflict_policy: PortableAssetConflictPolicy,
    /// 逐项变更
    pub changes: Vec<PortableAssetActionChangeDto>,
    /// 计划级阻断原因
    pub blocking_reasons: Vec<String>,
}

/// Apply 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyPortableAssetActionRequest {
    /// preview plan token
    pub plan_token: String,
    /// 非空幂等键
    pub client_request_id: String,
}

/// 单条 apply 结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableAssetActionItemResultDto {
    /// inventory item id
    pub inventory_item_id: String,
    /// 结果状态
    pub state: PortableAssetActionItemState,
    /// 稳定错误码
    pub error_code: Option<String>,
    /// 证据/说明（无 secret）
    pub message: Option<String>,
}

/// Apply 聚合结果。
///
/// Business Logic（为什么需要这个结构体）:
///     同 clientRequestId 完成后必须可精确回放；未决返回 outcomeUnknown。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 聚合 + 逐项结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableAssetActionResultDto {
    /// plan token
    pub plan_token: String,
    /// 幂等键
    pub client_request_id: String,
    /// 逐项结果
    pub items: Vec<PortableAssetActionItemResultDto>,
}

/// 内部持久化包装（不对外暴露 request 外的私有前置条件）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredPortableAssetActionPlan {
    pub public: PortableAssetActionPlanDto,
    pub request: PreviewPortableAssetActionRequest,
    pub owner_fingerprint: String,
    /// 参与 target 的 CLI 指纹（version|executable|configRoot）
    pub target_fingerprints: Vec<String>,
}
