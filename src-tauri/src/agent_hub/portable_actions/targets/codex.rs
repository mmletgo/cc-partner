//! portable_actions/targets/codex — Codex 本机动作（manifest 允许时）
//!
//! Business Logic（为什么需要这个模块）:
//!     Codex mutation 仅在 support/manifest 证据允许时执行；否则 fail-closed 且零 spawn。
//!
//! Code Logic（这个模块做什么）:
//!     未获写认证时返回 Blocked 且不调用 ProcessRunner。

use super::{TargetActionContext, TargetActionExecutor, TargetActionRawOutcome};
use crate::agent_hub::portable_actions::models::{
    PortableAssetActionChangeDto, PortableAssetActionPlanDto,
};
use crate::agent_hub::portable_inventory::PortableInventoryItemDto;
use crate::error::AppError;

/// Codex target executor（默认 fail-closed）。
pub struct CodexTargetExecutor;

impl TargetActionExecutor for CodexTargetExecutor {
    fn execute_change(
        &self,
        _ctx: &TargetActionContext,
        _plan: &PortableAssetActionPlanDto,
        change: &PortableAssetActionChangeDto,
        _pre_item: Option<&PortableInventoryItemDto>,
    ) -> Result<TargetActionRawOutcome, AppError> {
        if !change.blocking_reasons.is_empty() {
            return Ok(TargetActionRawOutcome::Blocked {
                code: change
                    .blocking_reasons
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "PORTABLE_ASSET_ACTION_BLOCKED".into()),
                message: "plan change blocked".into(),
            });
        }
        // Codex 本机 mutation 尚未获得 L3 写认证：fail-closed，零 spawn。
        Ok(TargetActionRawOutcome::Blocked {
            code: "PORTABLE_ASSET_ACTION_TARGET_WRITE_NOT_CERTIFIED".into(),
            message: "codex portable mutation blocked until manifest evidence allows".into(),
        })
    }
}
