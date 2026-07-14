//! remote outbox
//!
//! Business Logic（为什么需要这个模块）:
//!     拆分 monofile 本领域命令。
//!
//! Code Logic（这个模块做什么）:
//!     命令与 pub(crate) helpers。

#![allow(dead_code)]
#![allow(unused_imports)]

use crate::config::OrchestratorAutomationConfig;
use crate::error::AppError;
use crate::orchestrator::config::OrchestratorAutomationConfigDto;
use crate::orchestrator::models::{
    OrchestratorAttemptPhase, OrchestratorCreateAction, OrchestratorEvidenceDto,
    OrchestratorProjectConfigDto, OrchestratorRunState, OrchestratorTaskAttemptRow,
    OrchestratorTaskDto, OrchestratorTaskRow, OrchestratorTaskStatus, OrchestratorWorkflowState,
    SplitTaskState, EVIDENCE_KIND_DELIVERY, EVIDENCE_KIND_REPAIR_PROMPT,
    EVIDENCE_KIND_VERIFICATION_OUTPUT, EVIDENCE_KIND_VERIFICATION_REVIEW,
};
use crate::orchestrator::outbox::{
    create_pending_remote_task, is_remote_network_error, mirror_payload_from_task,
    open_remote_project_for_shortcut, sync_remote_task_mirror_for_project,
    OrchestratorRemoteOutboxDto, RemoteMirrorTask,
};
use crate::orchestrator::prompt::RepairPromptContext;
use crate::orchestrator::remote_client::RemoteOrchestratorClient;
use crate::orchestrator::remote_protocol::RemoteCreateOrchestratorTaskReq;
use crate::orchestrator::repo::{OrchestratorRecentEventRow, OrchestratorRepo};
use crate::orchestrator::runner::prepare_repair_runner;
use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetrySnapshot;
use crate::orchestrator::verifier::{self, VerifierReview};
use crate::orchestrator::workflow::{resolve_project_workflow, WorkflowSource};
use crate::state::AppState;
use crate::workbench::models::WorkbenchProjectRow;
use crate::workbench::remote_ids::{parse_remote_entity_id, remote_entity_id};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::State;
use uuid::Uuid;

use super::actions::*;
use super::common::*;
use super::evidence::*;
use super::runtime::*;
use super::tasks::*;

/// 重试失败的远端 outbox。
///
/// Business Logic（为什么需要这个函数）:
///     用户在原 Automation UI 对 failed outbox 点 Retry 时，应保留原 payload/clientRequestId 并回到 pending。
///
/// Code Logic（这个函数做什么）:
///     Tauri command 解包 State 与字符串参数，委托 state helper。
#[tauri::command]
pub async fn retry_orchestrator_remote_outbox(
    state: State<'_, AppState>,
    project_id: String,
    outbox_id: String,
) -> Result<OrchestratorRemoteOutboxDto, AppError> {
    retry_orchestrator_remote_outbox_for_state(state.inner(), &project_id, &outbox_id).await
}

/// 放弃失败的远端 outbox。
///
/// Business Logic（为什么需要这个函数）:
///     用户确认放弃后，failed outbox 进入 discarded 审计终态，不再参与 dispatcher/active 列表。
///
/// Code Logic（这个函数做什么）:
///     Tauri command 解包 State 与字符串参数，委托 state helper。
#[tauri::command]
pub async fn discard_orchestrator_remote_outbox(
    state: State<'_, AppState>,
    project_id: String,
    outbox_id: String,
) -> Result<OrchestratorRemoteOutboxDto, AppError> {
    discard_orchestrator_remote_outbox_for_state(state.inner(), &project_id, &outbox_id).await
}
