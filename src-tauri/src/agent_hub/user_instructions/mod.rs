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
mod plan;

pub use inventory::{
    inspect_user_instruction_workspace, inspect_user_instruction_workspace_with_env,
    UserInstructionAction, UserInstructionActivationSupport, UserInstructionCanonicalDto,
    UserInstructionCapabilityDto, UserInstructionCapabilityLevel, UserInstructionCliDto,
    UserInstructionHealthState, UserInstructionManagementMode, UserInstructionOwnership,
    UserInstructionProjectionDto, UserInstructionProjectionState, UserInstructionSetupState,
    UserInstructionSourceDto, UserInstructionSourceRole, UserInstructionTargetDto,
    UserInstructionWorkspaceDto,
};
pub use plan::{
    apply_user_instruction_plan, preview_user_instruction_setup, preview_user_instruction_update,
    ApplyUserInstructionPlanRequest, ApplyUserInstructionPlanResultDto,
    PreviewUserInstructionRequest, UserInstructionPlanChangeDto, UserInstructionPlanDto,
    UserInstructionPlanOperation, UserInstructionTargetApplyResultDto,
    UserInstructionTargetApplyState, UserInstructionTargetSelectionDto,
};
