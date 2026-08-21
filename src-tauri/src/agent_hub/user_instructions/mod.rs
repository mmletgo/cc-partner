//! agent_hub/user_instructions — 用户级指令管理 V2。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户需要先看清三个 CLI 实际读取的用户级文件，再通过可验证的 preview/apply
//!     管理 canonical 和本机投影，不应直接操作 presence/enabled 布尔组合。
//!
//! Code Logic（这个模块做什么）:
//!     inventory 只读枚举 source chain/ownership/capability；plan 生成有界 diff 和短期 token，
//!     apply 重新验证 revision/hash/ownership/support 后才允许进入投影。

mod inventory;
mod native_files;
mod plan;
mod slot_history;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 分析拆解请求（camelCase IPC / P2P）。
///
/// Business Logic: 独有页把 owning device 上的原始文件拆成三槽草稿。
/// Code Logic: 与 Tauri invoke / control / P2P body 同形；Serialize 供远端 POST。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeInstructionOriginalRequest {
    pub original_markdown: String,
    pub agent: String,
}

/// 分析拆解结果：公共 / 当前 agent 适配 / 当前 agent 独有。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeInstructionOriginalResult {
    pub common: String,
    pub adapted: String,
    pub exclusive: String,
}

/// 适配到其他 agent 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptInstructionToOtherAgentsRequest {
    pub source_agent: String,
    pub adapted_markdown: String,
}

/// 适配到其他 agent 结果：destination → rewritten body。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptInstructionToOtherAgentsResult {
    pub variants: BTreeMap<String, String>,
}

/// AI 辅助改写当前提示词槽请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviseInstructionSlotRequest {
    pub lane: String,
    pub agent: String,
    pub direction: String,
    pub common_markdown: Option<String>,
    pub exclusive_markdown: Option<String>,
    pub adapted_variants: Option<BTreeMap<String, String>>,
}

/// AI 辅助改写结果：按 lane 只填对应字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviseInstructionSlotResult {
    pub common: Option<String>,
    pub exclusive: Option<String>,
    pub variants: Option<BTreeMap<String, String>>,
}

pub use inventory::{
    inspect_user_instruction_workspace, inspect_user_instruction_workspace_with_env,
    list_user_instruction_slot_versions, restore_user_instruction_slot_version,
    save_user_instruction_blocks, ListUserInstructionSlotVersionsRequest,
    RestoreUserInstructionSlotRequest, SaveUserInstructionBlocksRequest, UserInstructionAction,
    UserInstructionActivationSupport, UserInstructionCanonicalDto, UserInstructionCapabilityDto,
    UserInstructionCapabilityLevel, UserInstructionCliDto, UserInstructionHealthState,
    UserInstructionManagementMode, UserInstructionOwnership, UserInstructionProjectionDto,
    UserInstructionProjectionState, UserInstructionSetupState, UserInstructionSourceDto,
    UserInstructionSourceRole, UserInstructionTargetDto, UserInstructionWorkspaceDto,
};
pub use native_files::{
    read_user_native_instruction_file, write_user_native_instruction_file,
    ReadUserNativeInstructionFileRequest, UserNativeInstructionFileDto,
    WriteUserNativeInstructionFileRequest,
};
pub use plan::{
    apply_user_instruction_plan, preview_user_instruction_setup, preview_user_instruction_update,
    ApplyUserInstructionPlanRequest, ApplyUserInstructionPlanResultDto,
    PreviewUserInstructionRequest, UserInstructionPlanChangeDto, UserInstructionPlanDto,
    UserInstructionPlanOperation, UserInstructionTargetApplyResultDto,
    UserInstructionTargetApplyState, UserInstructionTargetSelectionDto,
};
pub(crate) use plan::{read_text_bounded, render_bounded_diff};
pub use slot_history::{
    all_slot_keys, extract_slot_text, replace_slot_text, snapshot_dirty_slot_versions,
    InstructionSlotKey, SlotSnapshot,
};
