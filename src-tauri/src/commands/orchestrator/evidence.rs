//! evidence/config
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
use super::remote::*;
use super::runtime::*;
use super::tasks::*;

/// 按项目读取 remote-aware Orchestrator evidence。
///
/// Business Logic（为什么需要这个函数）:
///     remote task 详情需要展示 owning device 上真实写入的 evidence；local 任务继续读本机 SQLite。
///
/// Code Logic（这个函数做什么）:
///     local 项目走 repo.list_evidence；remote 项目打开远端项目后调用 RemoteOrchestratorClient::get_evidence。
#[tauri::command]
pub async fn list_orchestrator_task_evidence_for_project(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<Vec<OrchestratorEvidenceDto>, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        return state.orchestrator_repo.list_evidence(&task_id).await;
    }
    let context = open_remote_project_for_shortcut(state.inner(), &project, None).await?;
    let remote_task_id = remote_inner_task_id_for_shortcut(&project, &task_id)?;
    let evidence = RemoteOrchestratorClient::new()
        .get_evidence(&context.base_url, &remote_task_id)
        .await?;
    Ok(map_remote_evidence_for_shortcut(evidence, &project))
}

/// 按项目读取 remote-aware Orchestrator 自动化配置。
///
/// Business Logic（为什么需要这个函数）:
///     remote 项目展示的自动化策略应来自远端设备 Settings，而不是本机 shortcut 的配置。
///
/// Code Logic（这个函数做什么）:
///     local 项目返回本机 AppConfig.orchestrator DTO；remote 项目打开远端后调用远端 config endpoint。
#[tauri::command]
pub async fn get_orchestrator_config_for_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<OrchestratorAutomationConfigDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        let config = state
            .config
            .read()
            .expect("config 读锁中毒")
            .orchestrator
            .clone();
        return Ok(config.into());
    }
    let context = open_remote_project_for_shortcut(state.inner(), &project, None).await?;
    RemoteOrchestratorClient::new()
        .get_config(&context.base_url)
        .await
}

/// 查询 legacy Orchestrator 项目配置。
///
/// Business Logic（为什么需要这个函数）:
///     历史版本写入过项目级自动化配置，后端仍保留兼容/调试读取能力。
///     用户可见配置入口已经收敛到 Settings 自动化 tab，scheduler、验证和 delivery 统一读取 AppConfig.orchestrator。
///
/// Code Logic（这个函数做什么）:
///     委托仓储 get_or_create_project_config，并返回 camelCase DTO。
#[tauri::command]
pub async fn get_orchestrator_project_config(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<OrchestratorProjectConfigDto, AppError> {
    state
        .orchestrator_repo
        .get_or_create_project_config(&project_id)
        .await
}

/// 查询 Orchestrator 任务证据列表。
///
/// Business Logic（为什么需要这个函数）:
///     任务详情右侧 evidence 卡需要读取当前任务的验证输出与交付证据。
///
/// Code Logic（这个函数做什么）:
///     透传 task_id 给仓储 list_evidence，并返回 camelCase DTO 列表。
#[tauri::command]
pub async fn list_orchestrator_task_evidence(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<OrchestratorEvidenceDto>, AppError> {
    state.orchestrator_repo.list_evidence(&task_id).await
}
