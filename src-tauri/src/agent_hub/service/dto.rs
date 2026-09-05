//! agent_hub/service/dto — Agent Hub DTO / Request 定义
//!
//! Business Logic（为什么需要这个模块）:
//!     Tauri command / loopback control / P2P route 需要稳定的 camelCase wire 结构；
//!     DTO 集中定义避免各调用方自定义键名漂移，且不携带任何业务方法。
//!
//! Code Logic（这个模块做什么）:
//!     定义 service 对外暴露的全部 DTO / Request struct 与枚举（serde camelCase），
//!     供 service 门面与外部模块（commands / backend / routes）复用。

use crate::agent_hub::models::{AgentTarget, DesiredPresence};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// CLI probe DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     首屏展示可执行文件、版本与支持级别。
///
/// Code Logic（这个结构体做什么）:
///     camelCase probe 字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubProbeDto {
    pub target: AgentTarget,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub support: String,
    pub config_root: Option<String>,
}

/// Agent Hub 运行时状态。
///
/// Business Logic（为什么需要这个结构体）:
///     UI 首屏与升级门闸依赖 enabled / writeCompatible / 冲突计数。
///
/// Code Logic（这个结构体做什么）:
///     camelCase status 聚合。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubStatusDto {
    pub enabled: bool,
    pub background_enabled: bool,
    pub agent_hub_api_version: u32,
    pub owner_instance_id: Option<String>,
    pub write_compatible: bool,
    pub probes: Vec<AgentHubProbeDto>,
    pub conflict_count: u32,
    pub blocked_materialization_count: u32,
}

/// 资产 target 单元格。
///
/// Business Logic（为什么需要这个结构体）:
///     列表按 Claude/Codex/OpenCode 三列展示 desired 与 materialization；
///     Task 8 矩阵需要 supported/verified/sourceOnly 输入，禁止仅凭 Synced 推断 full。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 单元格（无 binding id）+ 聚合输入字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubTargetCellDto {
    pub target: AgentTarget,
    pub desired_presence: DesiredPresence,
    pub desired_enabled: bool,
    pub materialization_status: Option<String>,
    pub last_error: Option<String>,
    /// 是否在 requested 集合（有 binding 行）
    pub requested: bool,
    /// 目标当前是否 supported（probe 失败/unsupported → false）
    pub supported: bool,
    /// 是否仅 sourceOnly（无可投影 materialization）
    pub source_only: bool,
    /// 是否 verified（package activation/list 通过；指令 Synced 即 verified）
    pub verified: bool,
}

/// 资产在某一 target 上的绑定摘要（含 id）。
///
/// Business Logic（为什么需要这个结构体）:
///     set_target_binding 等路径可能需要 binding/materialization id。
///
/// Code Logic（这个结构体做什么）:
///     在 TargetCell 基础上附加 bindingId/materializationId。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubTargetBindingDto {
    pub target: AgentTarget,
    pub desired_presence: DesiredPresence,
    pub desired_enabled: bool,
    pub materialization_status: Option<String>,
    pub last_error: Option<String>,
    pub binding_id: Option<String>,
    pub materialization_id: Option<String>,
}

/// 资产列表摘要。
///
/// Business Logic（为什么需要这个结构体）:
///     Hub 列表展示 logical asset 与 target 单元格；聚合态供矩阵 UI 直接消费。
///
/// Code Logic（这个结构体做什么）:
///     camelCase summary + hasConflict + aggregateStatus。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubAssetSummaryDto {
    pub asset_id: String,
    pub scope_id: String,
    pub kind: String,
    pub display_name: String,
    pub logical_key: String,
    pub origin_namespace: String,
    pub policy: String,
    pub current_revision_id: Option<String>,
    pub targets: Vec<AgentHubTargetCellDto>,
    pub has_conflict: bool,
    /// 派生聚合状态：`full|partial|sourceOnly|activationRequired|externalCollision|detached|blocked`
    pub aggregate_status: String,
}

/// 指令块 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     InstructionBlocksDrawer 编辑 shared/adapted/targetOnly 与变体。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；commonMarkdown 为字符串，variants 为 map。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstructionBlockDto {
    pub id: String,
    pub mode: String,
    pub common_markdown: String,
    pub variants: Option<BTreeMap<String, String>>,
    pub heading_path: Option<Vec<String>>,
    pub source_target: Option<AgentTarget>,
    pub needs_adaptation: bool,
}

/// 任务文档中的指令块别名。
pub type AgentHubInstructionBlockDto = InstructionBlockDto;

/// 冲突摘要。
///
/// Business Logic（为什么需要这个结构体）:
///     详情页冲突列表与 resolve 后刷新。
///
/// Code Logic（这个结构体做什么）:
///     最小 camelCase 字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubConflictDto {
    pub id: String,
    pub target: Option<AgentTarget>,
    pub detail_json: Option<String>,
    pub created_at: String,
}

/// 资产详情。
///
/// Business Logic（为什么需要这个结构体）:
///     选中资产后展示矩阵、正文、块与冲突；聚合态与 summary 同源。
///
/// Code Logic（这个结构体做什么）:
///     扁平 summary 字段 + blocks/content/conflicts。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubAssetDetailDto {
    pub asset_id: String,
    pub scope_id: String,
    pub kind: String,
    pub display_name: String,
    pub logical_key: String,
    pub origin_namespace: String,
    pub policy: String,
    pub current_revision_id: Option<String>,
    pub targets: Vec<AgentHubTargetCellDto>,
    pub has_conflict: bool,
    /// 派生聚合状态（与 summary 同源）
    pub aggregate_status: String,
    pub blocks: Vec<InstructionBlockDto>,
    pub content_markdown: Option<String>,
    pub conflicts: Vec<AgentHubConflictDto>,
}

/// 列表过滤请求。
///
/// Business Logic（为什么需要这个结构体）:
///     list_assets control/IPC payload 承载可选 scope/kind。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 可选字段，default 空。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ListAssetsRequest {
    pub scope_id: Option<String>,
    pub kind: Option<String>,
}

/// 更新整份指令请求。
///
/// Business Logic（为什么需要这个结构体）:
///     保存编辑器正文并可选 CAS expected revision。
///
/// Code Logic（这个结构体做什么）:
///     camelCase assetId/contentMarkdown/expectedRevisionId。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInstructionRequest {
    pub asset_id: String,
    pub content_markdown: String,
    pub expected_revision_id: Option<String>,
}

/// 更新单个块请求。
///
/// Business Logic（为什么需要这个结构体）:
///     块抽屉可改 mode/common/variants。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 可选字段 + expected revision。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInstructionBlockRequest {
    pub asset_id: String,
    pub block_id: String,
    pub mode: Option<String>,
    pub common_markdown: Option<String>,
    pub variants: Option<BTreeMap<String, String>>,
    pub expected_revision_id: Option<String>,
}

/// 配对变体请求。
///
/// Business Logic（为什么需要这个结构体）:
///     将多个块合并为 adapted。
///
/// Code Logic（这个结构体做什么）:
///     blockIds + 可选 commonMarkdown。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairInstructionVariantsRequest {
    pub asset_id: String,
    pub block_ids: Vec<String>,
    pub common_markdown: Option<String>,
    pub expected_revision_id: Option<String>,
}

/// 解决冲突请求。
///
/// Business Logic（为什么需要这个结构体）:
///     keepHub/keepExternal/manual + 可选手工正文。
///
/// Code Logic（这个结构体做什么）:
///     camelCase resolution 字符串。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveConflictRequest {
    pub asset_id: String,
    pub conflict_id: String,
    pub resolution: String,
    pub content_markdown: Option<String>,
}

/// 设置 binding 请求。
///
/// Business Logic（为什么需要这个结构体）:
///     用户按 target 列开关 presence/enabled。
///
/// Code Logic（这个结构体做什么）:
///     camelCase target enum + presence + enabled。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTargetBindingRequest {
    pub asset_id: String,
    pub target: AgentTarget,
    pub desired_presence: DesiredPresence,
    pub desired_enabled: bool,
}

/// 设置 target presence 请求。
///
/// Business Logic: desiredPresence 是 target-local；Absent 只卸该 target。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTargetPresenceRequest {
    pub asset_id: String,
    pub target: AgentTarget,
    pub desired_presence: DesiredPresence,
}

/// 设置 target enabled 请求。
///
/// Business Logic: desiredEnabled 是 target-local；不改其它 CLI。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTargetEnabledRequest {
    pub asset_id: String,
    pub target: AgentTarget,
    pub desired_enabled: bool,
}

/// 恢复 detached target 请求。
///
/// Business Logic: 外部整文件删除后用户显式恢复，调度投影。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreDetachedTargetRequest {
    pub asset_id: String,
    pub target: AgentTarget,
}

/// 从所有 target 删除资产请求。
///
/// Business Logic: 唯一生成 canonical tombstone 的入口。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAssetEverywhereRequest {
    pub asset_id: String,
}

/// 冲突解决策略枚举（自由函数 API 使用）。
///
/// Business Logic（为什么需要这个枚举）:
///     稳定 wire token keepHub/keepExternal/manual。
///
/// Code Logic（这个枚举做什么）:
///     camelCase serde。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentHubConflictResolution {
    KeepHub,
    KeepExternal,
    Manual,
}
