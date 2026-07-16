//! Orchestrator 命令单测。
#![allow(dead_code)]
#![allow(unused_imports)]

use super::common::*;
use super::*;
use crate::config::OrchestratorAutomationConfig;
use crate::error::AppError;
use crate::orchestrator::agent_adapter::types::RunnerAttemptPolicy;
use crate::orchestrator::models::{
    OrchestratorAttemptPhase, OrchestratorAttemptStatus, OrchestratorCreateAction,
    OrchestratorEvidenceDto, OrchestratorProjectConfigDto, OrchestratorRunState,
    OrchestratorTaskAttemptRow, OrchestratorTaskDto, OrchestratorTaskRow, OrchestratorTaskStatus,
    OrchestratorWorkflowState, SplitTaskState, EVIDENCE_KIND_REPAIR_PROMPT,
    EVIDENCE_KIND_VERIFICATION_REVIEW,
};
use crate::orchestrator::outbox::{OrchestratorRemoteOutboxRow, RemoteOutboxStatus};
use crate::orchestrator::repo::OrchestratorRepo;
use crate::orchestrator::scheduler::{
    OrchestratorSchedulerTelemetry, OrchestratorSchedulerTelemetrySnapshot,
};
use crate::orchestrator::verifier::VerifierReview;
use crate::state::AppState;
use crate::workbench::models::WorkbenchProjectRow;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::fs;
use std::path::Path;
use std::str::FromStr;
use uuid::Uuid;

/// Business Logic（为什么需要这个函数）:
///     命令层测试需要 Orchestrator 仓储验证真实 SQLite 条件更新语义，避免只测纯函数。
///
/// Code Logic（这个函数做什么）:
///     创建单连接内存 SQLite、初始化 Orchestrator schema，并返回可供命令 helper 使用的 repo。
async fn setup_orchestrator_repo() -> OrchestratorRepo {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .expect("sqlite options")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("sqlite pool");
    OrchestratorRepo::init_schema(&pool)
        .await
        .expect("orchestrator schema");
    OrchestratorRepo::new(pool)
}

/// Business Logic（为什么需要这个函数）:
///     命令层状态转换测试需要稳定构造 Verifying/Aborted 任务行。
///
/// Code Logic（这个函数做什么）:
///     返回字段完整的测试任务 Row，调用方传入 id 和 status，其它字段使用固定值。
fn command_task_row(id: &str, status: OrchestratorTaskStatus) -> OrchestratorTaskRow {
    OrchestratorTaskRow {
        id: id.to_string(),
        project_id: "project-1".to_string(),
        title: format!("Task {id}"),
        goal: "goal".to_string(),
        acceptance_criteria: "criteria".to_string(),
        status,
        priority: 0,
        branch_name: None,
        worktree_id: Some("worktree-1".to_string()),
        session_id: None,
        prepare_claim_token: None,
        blocked_reason: None,
        attempt: 0,
        created_at: "2026-07-05T00:00:00Z".to_string(),
        updated_at: "2026-07-05T00:00:00Z".to_string(),
        started_at: None,
        finished_at: None,
        ..OrchestratorTaskRow::default_for_status(status)
    }
}

/// Business Logic（为什么需要这个函数）:
///     remote-aware helper 测试需要一个本机 remote shortcut，模拟前端当前打开的远端项目。
///
/// Code Logic（这个函数做什么）:
///     构造完整 WorkbenchProjectRow，id 是本机 shortcut projectId，device_id 是远端设备。
fn remote_shortcut_row() -> WorkbenchProjectRow {
    WorkbenchProjectRow {
        id: "shortcut-project-1".to_string(),
        name: "Remote Project".to_string(),
        kind: "remote".to_string(),
        device_id: "device-a".to_string(),
        device_name: "Mac mini".to_string(),
        path: "/Users/hans/remote-project".to_string(),
        last_opened_at: "2026-07-05T00:00:00Z".to_string(),
        created_at: "2026-07-05T00:00:00Z".to_string(),
        updated_at: "2026-07-05T00:00:00Z".to_string(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     新增本机-only 拖拽命令和 runtime snapshot 都需要 Workbench 本机项目上下文，测试应复用稳定项目行。
///
/// Code Logic（这个函数做什么）:
///     构造 kind=local 的 WorkbenchProjectRow，path 由调用方传入以便 workflow resolver 读取临时目录。
fn local_project_row(path: String) -> WorkbenchProjectRow {
    WorkbenchProjectRow {
        id: "project-1".to_string(),
        name: "Local Project".to_string(),
        kind: "local".to_string(),
        device_id: "device-local".to_string(),
        device_name: "MacBook".to_string(),
        path,
        last_opened_at: "2026-07-05T00:00:00Z".to_string(),
        created_at: "2026-07-05T00:00:00Z".to_string(),
        updated_at: "2026-07-05T00:00:00Z".to_string(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     workflow snapshot 测试需要一个真实项目目录，避免依赖用户机器上的固定路径。
///
/// Code Logic（这个函数做什么）:
///     在系统临时目录下创建唯一文件夹并返回路径字符串，调用方负责按需写 WORKFLOW.md。
fn temp_project_dir(name: &str) -> String {
    let path = std::env::temp_dir().join(format!("{name}-{}", Uuid::new_v4()));
    fs::create_dir_all(&path).expect("create temp project dir");
    path.to_string_lossy().to_string()
}

/// Business Logic（为什么需要这个函数）:
///     多个 snapshot 测试只关心 repo/workflow，不需要调度器真实 tick 时，应提供一致的空观测输入。
///
/// Code Logic（这个函数做什么）:
///     构造新的 OrchestratorSchedulerTelemetry 并读取其空 snapshot，供 runtime snapshot helper 使用。
fn empty_scheduler_snapshot() -> OrchestratorSchedulerTelemetrySnapshot {
    OrchestratorSchedulerTelemetry::new().snapshot()
}

/// Business Logic（为什么需要这个函数）:
///     Orchestrator 任务创建命令必须拒绝没有项目归属的任务，避免调度器后续无法定位工作台项目。
///
/// Code Logic（这个函数做什么）:
///     构造空 project_id 请求并断言 row builder 返回错误。
#[test]
fn build_task_row_rejects_blank_project_id() {
    let request = CreateOrchestratorTaskRequest {
        project_id: "  ".to_string(),
        title: "实现任务".to_string(),
        goal: "完成目标".to_string(),
        acceptance_criteria: "测试通过".to_string(),
        priority: None,
        create_action: OrchestratorCreateAction::Backlog,
        source: None,
        external_id: None,
        external_identifier: None,
        external_url: None,
        external_state: None,
        external_labels: None,
    };

    let result = build_orchestrator_task_row(request);

    assert!(result.is_err());
}

/// Business Logic（为什么需要这个函数）:
///     Orchestrator 任务必须有可展示标题和明确目标，否则任务列表与调度执行都缺少必要语义。
///
/// Code Logic（这个函数做什么）:
///     分别构造空标题和空目标请求，并断言 row builder 拒绝。
#[test]
fn build_task_row_rejects_blank_title_and_goal() {
    let blank_title = CreateOrchestratorTaskRequest {
        project_id: "project-1".to_string(),
        title: " ".to_string(),
        goal: "完成目标".to_string(),
        acceptance_criteria: "测试通过".to_string(),
        priority: None,
        create_action: OrchestratorCreateAction::Backlog,
        source: None,
        external_id: None,
        external_identifier: None,
        external_url: None,
        external_state: None,
        external_labels: None,
    };
    let blank_goal = CreateOrchestratorTaskRequest {
        project_id: "project-1".to_string(),
        title: "实现任务".to_string(),
        goal: " ".to_string(),
        acceptance_criteria: "测试通过".to_string(),
        priority: None,
        create_action: OrchestratorCreateAction::Backlog,
        source: None,
        external_id: None,
        external_identifier: None,
        external_url: None,
        external_state: None,
        external_labels: None,
    };

    assert!(build_orchestrator_task_row(blank_title).is_err());
    assert!(build_orchestrator_task_row(blank_goal).is_err());
}

/// Business Logic（为什么需要这个函数）:
///     新建任务进入草稿态，用户输入的文本需要清理首尾空白，未显式设置优先级时按普通任务处理。
///
/// Code Logic（这个函数做什么）:
///     构造带空白和空 priority 的请求，断言生成 Row 的初始状态、trim 和默认字段。
#[test]
fn build_task_row_trims_fields_and_sets_defaults() {
    let request = CreateOrchestratorTaskRequest {
        project_id: "  project-1  ".to_string(),
        title: "  实现 API  ".to_string(),
        goal: "  暴露任务命令  ".to_string(),
        acceptance_criteria: "  测试通过  ".to_string(),
        priority: None,
        create_action: OrchestratorCreateAction::Backlog,
        source: None,
        external_id: None,
        external_identifier: None,
        external_url: None,
        external_state: None,
        external_labels: None,
    };

    let row = build_orchestrator_task_row(request).expect("row");

    assert_eq!(row.project_id, "project-1");
    assert_eq!(row.title, "实现 API");
    assert_eq!(row.goal, "暴露任务命令");
    assert_eq!(row.acceptance_criteria, "测试通过");
    assert_eq!(row.status, OrchestratorTaskStatus::Draft);
    assert_eq!(row.priority, 0);
    assert_eq!(row.attempt, 0);
    assert!(row.branch_name.is_none());
    assert!(row.worktree_id.is_none());
    assert!(row.session_id.is_none());
    assert!(row.blocked_reason.is_none());
    assert!(row.started_at.is_none());
    assert!(row.finished_at.is_none());
    assert!(!row.id.trim().is_empty());
    assert!(!row.created_at.trim().is_empty());
    assert_eq!(row.created_at, row.updated_at);
}

/// Business Logic（为什么需要这个函数）:
///     手动 dispatch 命令需要给前端和测试返回稳定的 dispatched 数量，便于确认本次触发领取了多少任务。
///
/// Code Logic（这个函数做什么）:
///     调用响应构造 helper，并断言 JSON 中的 dispatched 字段保留原始数量。
#[test]
fn dispatch_response_contains_dispatched_count() {
    let value = build_dispatch_once_response(2);

    assert_eq!(value["dispatched"], serde_json::json!(2));
}

/// Business Logic（为什么需要这个函数）:
///     本机拖拽命令需要校验项目归属后返回 Local task view，供前端直接刷新看板卡片。
///
/// Code Logic（这个函数做什么）:
///     创建本机 Backlog 任务，调用命令层 helper 移到 Todo，断言返回 Local DTO 且 workflow_state 更新。
#[tokio::test]
async fn move_workflow_state_for_local_project_returns_local_view() {
    let repo = setup_orchestrator_repo().await;
    let task = command_task_row("task-local-move", OrchestratorTaskStatus::Draft);
    repo.create_task(&task).await.unwrap();
    let project = local_project_row(temp_project_dir("orch-local-move"));

    let view = move_orchestrator_task_workflow_state_for_project(
        &repo,
        &project,
        MoveOrchestratorTaskWorkflowStateRequest {
            project_id: "project-1".to_string(),
            task_id: task.id.clone(),
            target_state: OrchestratorWorkflowState::Todo,
        },
    )
    .await
    .unwrap();

    match view {
        OrchestratorTaskViewDto::Local { task } => {
            assert_eq!(task.id, "task-local-move");
            assert_eq!(task.workflow_state, OrchestratorWorkflowState::Todo);
        }
        other => panic!("expected local view, got {other:?}"),
    }
}

/// Business Logic（为什么需要这个函数）:
///     本轮拖拽动作只支持本机项目，remote shortcut 必须被明确拒绝，避免本机误改 mirror 或远端权威状态。
///
/// Code Logic（这个函数做什么）:
///     用 remote WorkbenchProjectRow 调用命令层 helper，断言返回中文 remote 拒绝错误。
#[tokio::test]
async fn move_workflow_state_rejects_remote_project() {
    let repo = setup_orchestrator_repo().await;
    let project = remote_shortcut_row();

    let error = move_orchestrator_task_workflow_state_for_project(
        &repo,
        &project,
        MoveOrchestratorTaskWorkflowStateRequest {
            project_id: project.id.clone(),
            task_id: "remote:device-a:task-1".to_string(),
            target_state: OrchestratorWorkflowState::Todo,
        },
    )
    .await
    .expect_err("remote drag must be rejected");

    assert!(error.to_string().contains("远端项目暂不支持拖拽移动"));
}

/// Business Logic（为什么需要这个函数）:
///     拖拽命令必须防止前端请求 projectId 与当前项目上下文不一致时误改任务。
///
/// Code Logic（这个函数做什么）:
///     用本机项目上下文和不匹配 request.projectId 调用 helper，断言返回项目不一致错误。
#[tokio::test]
async fn move_workflow_state_rejects_project_id_mismatch() {
    let repo = setup_orchestrator_repo().await;
    let project = local_project_row(temp_project_dir("orch-project-mismatch"));

    let error = move_orchestrator_task_workflow_state_for_project(
        &repo,
        &project,
        MoveOrchestratorTaskWorkflowStateRequest {
            project_id: "other-project".to_string(),
            task_id: "task-1".to_string(),
            target_state: OrchestratorWorkflowState::Todo,
        },
    )
    .await
    .expect_err("project mismatch must be rejected");

    assert!(error.to_string().contains("请求项目与当前项目不一致"));
}

/// Business Logic（为什么需要这个函数）:
///     拖拽命令必须确认任务属于当前 Workbench 项目，避免跨项目卡片被误移动。
///
/// Code Logic（这个函数做什么）:
///     创建 project-2 的任务，在 project-1 上下文中请求移动，断言返回任务归属错误且状态不变。
#[tokio::test]
async fn move_workflow_state_rejects_task_from_another_project() {
    let repo = setup_orchestrator_repo().await;
    let task = OrchestratorTaskRow {
        project_id: "project-2".to_string(),
        ..command_task_row("task-other-project", OrchestratorTaskStatus::Draft)
    };
    repo.create_task(&task).await.unwrap();
    let project = local_project_row(temp_project_dir("orch-task-project-mismatch"));

    let error = move_orchestrator_task_workflow_state_for_project(
        &repo,
        &project,
        MoveOrchestratorTaskWorkflowStateRequest {
            project_id: project.id.clone(),
            task_id: task.id.clone(),
            target_state: OrchestratorWorkflowState::Todo,
        },
    )
    .await
    .expect_err("foreign task must be rejected");
    let persisted = repo.get_task(&task.id).await.unwrap();

    assert!(error.to_string().contains("任务不属于当前项目"));
    assert_eq!(persisted.workflow_state, OrchestratorWorkflowState::Backlog);
}

/// Business Logic（为什么需要这个函数）:
///     runtime snapshot 需要向 UI 暴露当前项目使用内置 workflow 还是项目覆盖，方便用户诊断自动化行为。
///
/// Code Logic（这个函数做什么）:
///     使用没有 WORKFLOW.md 的临时项目目录构建 snapshot，断言 source 为 builtInDefault 且 workflow_valid=true。
#[tokio::test]
async fn runtime_snapshot_reports_built_in_workflow_source() {
    let repo = setup_orchestrator_repo().await;
    let project = local_project_row(temp_project_dir("orch-snapshot-valid"));
    let config = OrchestratorAutomationConfig {
        enabled: true,
        max_concurrent_tasks: 2,
        ..OrchestratorAutomationConfig::default()
    };

    let scheduler_snapshot = empty_scheduler_snapshot();
    let snapshot = get_orchestrator_runtime_snapshot_for_project(
        &repo,
        &config,
        &project,
        &scheduler_snapshot,
    )
    .await
    .unwrap();

    assert_eq!(snapshot.project_id, "project-1");
    assert_eq!(snapshot.project_kind, "local");
    assert_eq!(snapshot.remote_status, "local");
    assert!(snapshot.scheduler_enabled);
    assert_eq!(snapshot.workflow_source, "builtInDefault");
    assert!(snapshot.workflow_valid);
    assert_eq!(snapshot.max_concurrent_tasks, 2);
    assert_eq!(snapshot.slots_used, 0);
    assert_eq!(snapshot.slots_available, 2);
    assert!(snapshot.latest_tick_at.is_none());
}

/// Business Logic（为什么需要这个测试）:
///     Workbench 状态条需要同时解释当前活跃任务、待重试任务、最近事件和最近调度 tick，用户才能判断自动化是否卡住。
///
/// Code Logic（这个测试做什么）:
///     插入 running 与 blocked/rework 任务并追加事件，写入 scheduler telemetry 后构建 snapshot，
///     断言任务摘要、recentEvents、latestError、projectKind/remoteStatus 和 latestTickAt 都可见。
#[tokio::test]
async fn runtime_snapshot_reports_running_summaries_events_and_latest_tick() {
    let repo = setup_orchestrator_repo().await;
    let project = local_project_row(temp_project_dir("orch-snapshot-runtime"));
    let mut running = command_task_row("task-running", OrchestratorTaskStatus::Running);
    running.title = "实现状态条".to_string();
    running.attempt_phase = Some(OrchestratorAttemptPhase::Streaming);
    running.session_id = Some("session-running".to_string());
    running.worktree_id = Some("worktree-running".to_string());
    running.last_runtime_message = Some("正在运行测试".to_string());
    running.last_activity_at = Some("2026-07-06T01:02:03Z".to_string());
    let mut retrying = command_task_row("task-retry", OrchestratorTaskStatus::Blocked);
    retrying.title = "修复验证失败".to_string();
    retrying.workflow_state = OrchestratorWorkflowState::Rework;
    retrying.run_state = OrchestratorRunState::Blocked;
    retrying.blocked_reason = Some("验证器要求修复".to_string());
    retrying.updated_at = "2026-07-06T01:03:00Z".to_string();
    repo.create_task(&running).await.unwrap();
    repo.create_task(&retrying).await.unwrap();
    repo.add_event(&running.id, "runner", "Runner 已启动", None)
        .await
        .unwrap();
    repo.add_event(&retrying.id, "blocked", "验证器要求修复", None)
        .await
        .unwrap();
    let telemetry = OrchestratorSchedulerTelemetry::new();
    telemetry.record_dispatch_result("2026-07-06T01:04:05Z".to_string(), 1, None);

    let telemetry_snapshot = telemetry.snapshot();
    let snapshot = get_orchestrator_runtime_snapshot_for_project(
        &repo,
        &OrchestratorAutomationConfig::default(),
        &project,
        &telemetry_snapshot,
    )
    .await
    .unwrap();

    assert_eq!(snapshot.project_kind, "local");
    assert_eq!(snapshot.remote_status, "local");
    assert_eq!(
        snapshot.latest_tick_at.as_deref(),
        Some("2026-07-06T01:04:05Z")
    );
    assert_eq!(snapshot.running_tasks.len(), 1);
    assert_eq!(snapshot.running_tasks[0].task_id, "task-running");
    assert_eq!(snapshot.running_tasks[0].title, "实现状态条");
    assert_eq!(
        snapshot.running_tasks[0].attempt_phase,
        Some(OrchestratorAttemptPhase::Streaming)
    );
    assert_eq!(
        snapshot.running_tasks[0].last_runtime_message.as_deref(),
        Some("正在运行测试")
    );
    assert_eq!(snapshot.retrying_tasks.len(), 1);
    assert_eq!(snapshot.retrying_tasks[0].task_id, "task-retry");
    assert_eq!(snapshot.latest_error.as_deref(), Some("验证器要求修复"));
    assert_eq!(snapshot.recent_events.len(), 2);
    assert!(snapshot
        .recent_events
        .iter()
        .any(|event| event.task_id == "task-retry" && event.kind == "blocked"));
    assert!(snapshot
        .recent_events
        .iter()
        .any(|event| event.task_id == "task-running" && event.kind == "runner"));
}

/// Business Logic（为什么需要这个函数）:
///     项目 WORKFLOW.md 解析失败时，状态条必须展示无效 workflow，而不是假装内置默认可用。
///
/// Code Logic（这个函数做什么）:
///     写入非法 YAML front matter，构建 runtime snapshot 并断言 invalidProjectOverride 与错误信息存在。
#[tokio::test]
async fn runtime_snapshot_reports_invalid_project_workflow() {
    let repo = setup_orchestrator_repo().await;
    let project_dir = temp_project_dir("orch-snapshot-invalid");
    fs::write(
        std::path::Path::new(&project_dir).join("WORKFLOW.md"),
        "---\nrunner: [invalid\n---\n",
    )
    .expect("write workflow");
    let project = local_project_row(project_dir);

    let scheduler_snapshot = empty_scheduler_snapshot();
    let snapshot = get_orchestrator_runtime_snapshot_for_project(
        &repo,
        &OrchestratorAutomationConfig::default(),
        &project,
        &scheduler_snapshot,
    )
    .await
    .unwrap();

    assert_eq!(snapshot.workflow_source, "invalidProjectOverride");
    assert!(!snapshot.workflow_valid);
    assert_eq!(snapshot.max_concurrent_tasks, 1);
    assert!(snapshot
        .workflow_error
        .as_deref()
        .unwrap_or_default()
        .contains("WORKFLOW.md"));
}

/// Business Logic（为什么需要这个函数）:
///     远端 live 映射测试需要一个含任务/事件/telemetry 的 owner 快照，验证本机只改身份字段。
///
/// Code Logic（这个函数做什么）:
///     构造带唯一 generatedAt/tick/slots/workflow 与 running/retrying/events 的 owner DTO，
///     使用远端裸 task/worktree/session id 以便断言映射结果。
fn owner_runtime_snapshot_fixture() -> OrchestratorRuntimeSnapshotDto {
    OrchestratorRuntimeSnapshotDto {
        project_id: "owner-local-project-1".to_string(),
        project_kind: "local".to_string(),
        remote_status: "local".to_string(),
        generated_at: "2026-07-12T09:08:07.006Z".to_string(),
        latest_tick_at: Some("2026-07-12T09:07:00Z".to_string()),
        last_dispatch_at: Some("2026-07-12T09:06:30Z".to_string()),
        last_dispatched_count: 4,
        scheduler_enabled: true,
        workflow_source: "projectOverride".to_string(),
        workflow_valid: true,
        workflow_error: None,
        max_concurrent_tasks: 5,
        slots_used: 2,
        slots_available: 3,
        latest_error: Some("owner-only-latest-error".to_string()),
        running_tasks: vec![OrchestratorRuntimeTaskSummaryDto {
            task_id: "task-owner-running".to_string(),
            title: "远端运行任务".to_string(),
            workflow_state: OrchestratorWorkflowState::InProgress,
            run_state: OrchestratorRunState::Running,
            attempt_phase: Some(OrchestratorAttemptPhase::Streaming),
            session_id: Some("session-owner-1".to_string()),
            worktree_id: Some("worktree-owner-1".to_string()),
            last_runtime_message: Some("owner streaming".to_string()),
            last_activity_at: Some("2026-07-12T09:05:00Z".to_string()),
        }],
        retrying_tasks: vec![OrchestratorRuntimeTaskSummaryDto {
            task_id: "task-owner-retry".to_string(),
            title: "远端重试任务".to_string(),
            workflow_state: OrchestratorWorkflowState::Rework,
            run_state: OrchestratorRunState::Blocked,
            attempt_phase: None,
            session_id: None,
            worktree_id: Some("worktree-owner-2".to_string()),
            last_runtime_message: Some("owner blocked".to_string()),
            last_activity_at: Some("2026-07-12T09:04:00Z".to_string()),
        }],
        recent_events: vec![OrchestratorRuntimeEventDto {
            id: "event-owner-1".to_string(),
            task_id: "task-owner-running".to_string(),
            task_title: "远端运行任务".to_string(),
            kind: "runner".to_string(),
            message: "owner event".to_string(),
            created_at: "2026-07-12T09:03:00Z".to_string(),
        }],
    }
}

/// Business Logic（为什么需要这个测试）:
///     live 成功路径必须把 owning device 快照映射到本机 remote shortcut 表面，
///     并保留 owner telemetry；任何本机 scheduler/config 值都不得混入。
///
/// Code Logic（这个测试做什么）:
///     用 owner fixture 调用 map_remote_runtime_snapshot_for_shortcut，
///     断言 project/task/worktree/session id 映射，以及 generatedAt/tick/slots/events 原样保留。
#[test]
fn remote_runtime_snapshot_maps_live_owner_fields_and_ids() {
    let shortcut = remote_shortcut_row();
    let owner = owner_runtime_snapshot_fixture();

    let mapped = map_remote_runtime_snapshot_for_shortcut(owner, &shortcut);

    assert_eq!(mapped.project_id, "shortcut-project-1");
    assert_eq!(mapped.project_kind, "remote");
    assert_eq!(mapped.remote_status, "live");
    assert_eq!(mapped.generated_at, "2026-07-12T09:08:07.006Z");
    assert_eq!(
        mapped.latest_tick_at.as_deref(),
        Some("2026-07-12T09:07:00Z")
    );
    assert_eq!(
        mapped.last_dispatch_at.as_deref(),
        Some("2026-07-12T09:06:30Z")
    );
    assert_eq!(mapped.last_dispatched_count, 4);
    assert!(mapped.scheduler_enabled);
    assert_eq!(mapped.workflow_source, "projectOverride");
    assert!(mapped.workflow_valid);
    assert!(mapped.workflow_error.is_none());
    assert_eq!(mapped.max_concurrent_tasks, 5);
    assert_eq!(mapped.slots_used, 2);
    assert_eq!(mapped.slots_available, 3);
    assert_eq!(
        mapped.latest_error.as_deref(),
        Some("owner-only-latest-error")
    );
    assert_eq!(mapped.running_tasks.len(), 1);
    assert_eq!(
        mapped.running_tasks[0].task_id,
        "remote:device-a:task-owner-running"
    );
    assert_eq!(
        mapped.running_tasks[0].session_id.as_deref(),
        Some("remote:device-a:session-owner-1")
    );
    assert_eq!(
        mapped.running_tasks[0].worktree_id.as_deref(),
        Some("remote:device-a:worktree-owner-1")
    );
    assert_eq!(
        mapped.running_tasks[0].last_runtime_message.as_deref(),
        Some("owner streaming")
    );
    assert_eq!(mapped.retrying_tasks.len(), 1);
    assert_eq!(
        mapped.retrying_tasks[0].task_id,
        "remote:device-a:task-owner-retry"
    );
    assert_eq!(
        mapped.retrying_tasks[0].worktree_id.as_deref(),
        Some("remote:device-a:worktree-owner-2")
    );
    assert_eq!(mapped.recent_events.len(), 1);
    assert_eq!(
        mapped.recent_events[0].task_id,
        "remote:device-a:task-owner-running"
    );
    assert_eq!(mapped.recent_events[0].message, "owner event");
    // 回归：映射结果中不得出现本机 local project id / local telemetry 文案。
    assert_ne!(mapped.project_id, "project-1");
    assert_ne!(mapped.generated_at, "local-telemetry-should-not-appear");
    assert!(!mapped
        .latest_error
        .as_deref()
        .unwrap_or_default()
        .contains("local"));
}

/// Business Logic（为什么需要这个测试 / T7 owner 端到端透传）:
///     route→client 解析后的 owner DTO 再经 command 层 shortcut 映射时，
///     generatedAt/tick/slots/events 必须与 owner 种子逐字相等，仅 ID/表面字段被改写。
///
/// Code Logic（这个测试做什么）:
///     用唯一 owner 指纹构造 DTO，JSON round-trip 模拟 remote client 反序列化，
///     再 map_remote_runtime_snapshot_for_shortcut，断言 owner 运行时字段精确相等，
///     仅 projectId/kind/status 与 entity id 发生表面映射。
#[test]
fn remote_runtime_snapshot_preserves_owner_fields_after_client_json_and_shortcut_mapping() {
    let shortcut = remote_shortcut_row();
    let owner = owner_runtime_snapshot_fixture();
    let owner_generated_at = owner.generated_at.clone();
    let owner_tick = owner.latest_tick_at.clone();
    let owner_dispatch = owner.last_dispatch_at.clone();
    let owner_dispatched_count = owner.last_dispatched_count;
    let owner_slots_used = owner.slots_used;
    let owner_slots_available = owner.slots_available;
    let owner_max = owner.max_concurrent_tasks;
    let owner_latest_error = owner.latest_error.clone();
    let owner_event_message = owner.recent_events[0].message.clone();
    let owner_running_message = owner.running_tasks[0].last_runtime_message.clone();
    let owner_workflow_source = owner.workflow_source.clone();

    // 模拟 remote_client 成功路径：owner JSON → Deserialize → command 映射。
    let wire = serde_json::to_value(&owner).expect("owner DTO serialize");
    let decoded: OrchestratorRuntimeSnapshotDto =
        serde_json::from_value(wire).expect("owner DTO deserialize after client parse");
    let mapped = map_remote_runtime_snapshot_for_shortcut(decoded, &shortcut);

    // 表面/身份映射。
    assert_eq!(mapped.project_id, shortcut.id);
    assert_eq!(mapped.project_kind, "remote");
    assert_eq!(mapped.remote_status, "live");
    assert_eq!(
        mapped.running_tasks[0].task_id,
        "remote:device-a:task-owner-running"
    );
    assert_eq!(
        mapped.running_tasks[0].session_id.as_deref(),
        Some("remote:device-a:session-owner-1")
    );
    assert_eq!(
        mapped.running_tasks[0].worktree_id.as_deref(),
        Some("remote:device-a:worktree-owner-1")
    );
    assert_eq!(
        mapped.retrying_tasks[0].task_id,
        "remote:device-a:task-owner-retry"
    );
    assert_eq!(
        mapped.recent_events[0].task_id,
        "remote:device-a:task-owner-running"
    );

    // owner 运行时字段逐字保留（禁止本机 telemetry 替代）。
    assert_eq!(mapped.generated_at, owner_generated_at);
    assert_eq!(mapped.latest_tick_at, owner_tick);
    assert_eq!(mapped.last_dispatch_at, owner_dispatch);
    assert_eq!(mapped.last_dispatched_count, owner_dispatched_count);
    assert_eq!(mapped.slots_used, owner_slots_used);
    assert_eq!(mapped.slots_available, owner_slots_available);
    assert_eq!(mapped.max_concurrent_tasks, owner_max);
    assert_eq!(mapped.latest_error, owner_latest_error);
    assert_eq!(mapped.workflow_source, owner_workflow_source);
    assert_eq!(
        mapped.running_tasks[0].last_runtime_message,
        owner_running_message
    );
    assert_eq!(mapped.recent_events[0].message, owner_event_message);
    assert_ne!(
        mapped.generated_at, "local-telemetry-should-not-appear",
        "不得用本机 telemetry 补 owner generatedAt"
    );
}

/// Business Logic（为什么需要这个测试）:
///     open-project preflight 设备缺失/传输中断必须回落 offline 空快照，且不得把 owner URL 写进 DTO。
///
/// Code Logic（这个测试做什么）:
///     用类型化 Unavailable（设备缺失、send 失败、body-read 中断带 URL）调用
///     remote_runtime_snapshot_from_open_error，断言 remoteStatus=offline 且脱敏。
#[test]
fn remote_runtime_snapshot_open_preflight_maps_device_missing_to_offline_without_url() {
    let project = remote_shortcut_row();
    let offline =
        remote_runtime_snapshot_from_open_error(&project, AppError::unavailable("远端设备不在线"));
    assert_eq!(offline.remote_status, "offline");
    assert_eq!(offline.project_id, "shortcut-project-1");
    assert!(offline.running_tasks.is_empty());
    assert!(offline.recent_events.is_empty());
    let offline_err = offline.latest_error.unwrap_or_default();
    assert!(offline_err.contains("离线"));
    assert!(!offline_err.contains("http://"));
    assert!(!offline_err.contains("https://"));

    // send 失败（旧前缀形态）
    let network = remote_runtime_snapshot_from_open_error(
        &project,
        AppError::unavailable(
            "远端 Workbench 请求失败: error sending request for url (http://192.168.9.9:62116/api/workbench/projects/open)",
        ),
    );
    assert_eq!(network.remote_status, "offline");
    let network_err = network.latest_error.unwrap_or_default();
    assert!(!network_err.contains("http://"));
    assert!(!network_err.contains("192.168.9.9"));
    assert!(!network_err.contains("62116"));

    // body-read 中断（peer_call_error_to_app_error 括号 URL 形态）仍按类型判 offline
    let body_read = remote_runtime_snapshot_from_open_error(
        &project,
        AppError::unavailable(
            "远端 Workbench 请求失败 (http://192.168.9.9:62116/api/workbench/projects/open): connection reset",
        ),
    );
    assert_eq!(body_read.remote_status, "offline");
    let body_err = body_read.latest_error.unwrap_or_default();
    assert!(!body_err.contains("http://"));
    assert!(!body_err.contains("192.168.9.9"));
}

/// Business Logic（为什么需要这个测试）:
///     open-project 业务/协议失败必须回落 unavailable，且响应不得泄漏 owner base URL；
///     即使文案含“连接/离线”等误导词也不能靠文案误判 offline。
///
/// Code Logic（这个测试做什么）:
///     用 Validation/Internal 类型错误（含 URL 与误导中文）映射，断言 unavailable 且脱敏。
#[test]
fn remote_runtime_snapshot_open_preflight_maps_business_failure_to_unavailable_without_url() {
    let project = remote_shortcut_row();
    let with_url = remote_runtime_snapshot_from_open_error(
        &project,
        AppError::generic("打开远端项目失败: http://10.0.0.8:62116/api/workbench/projects/open"),
    );
    assert_eq!(with_url.remote_status, "unavailable");
    let msg = with_url.latest_error.unwrap_or_default();
    assert!(!msg.contains("http://"));
    assert!(!msg.contains("10.0.0.8"));
    assert!(!msg.contains("62116"));

    let plain =
        remote_runtime_snapshot_from_open_error(&project, AppError::generic("路径不能为空"));
    assert_eq!(plain.remote_status, "unavailable");
    assert_eq!(plain.latest_error.as_deref(), Some("路径不能为空"));
    assert!(plain.running_tasks.is_empty());
    assert_eq!(plain.max_concurrent_tasks, 0);

    // 误导文案 + Validation 分类：必须 unavailable，不能被“离线/连接”关键词带偏。
    let misleading = remote_runtime_snapshot_from_open_error(
        &project,
        AppError::validation("路径无效：远端连接配置离线占位"),
    );
    assert_eq!(misleading.remote_status, "unavailable");
    assert_eq!(
        misleading.latest_error.as_deref(),
        Some("路径无效：远端连接配置离线占位")
    );
}

/// Business Logic（为什么需要这个测试）:
///     在线 owner 返回的结构化 503/504 业务信封（AppError::Remote）不得被误判为传输离线，
///     否则会错误展示陈旧 offline 缓存；契约要求映射为 unavailable。
///
/// Code Logic（这个测试做什么）:
///     用 code=unavailable/timeout 的 Remote 信封调用 preflight 映射，断言 remoteStatus=unavailable。
#[test]
fn remote_runtime_snapshot_open_preflight_maps_remote_envelope_to_unavailable() {
    let project = remote_shortcut_row();
    let remote_unavailable = remote_runtime_snapshot_from_open_error(
        &project,
        AppError::remote(
            "owner open 暂不可用",
            crate::error::RemoteErrorMeta {
                code: "unavailable".to_string(),
                status: 503,
                retryable: true,
                request_id: "req-503".to_string(),
                details: serde_json::json!({}),
            },
        ),
    );
    assert_eq!(remote_unavailable.remote_status, "unavailable");
    assert!(remote_unavailable.running_tasks.is_empty());
    assert!(remote_unavailable.recent_events.is_empty());

    let remote_timeout = remote_runtime_snapshot_from_open_error(
        &project,
        AppError::remote(
            "owner open 超时",
            crate::error::RemoteErrorMeta {
                code: "timeout".to_string(),
                status: 504,
                retryable: true,
                request_id: "req-504".to_string(),
                details: serde_json::json!({}),
            },
        ),
    );
    assert_eq!(remote_timeout.remote_status, "unavailable");
    assert_eq!(
        remote_timeout.latest_error.as_deref(),
        Some("owner open 超时")
    );
}

/// Business Logic（为什么需要这个测试）:
///     capability 缺失时状态条必须展示 unsupported，而不是 offline/unavailable 或本机数据。
///
/// Code Logic（这个测试做什么）:
///     用 PeerCallError::Unsupported 调用 remote_runtime_snapshot_from_peer_error，
///     断言 remoteStatus=unsupported 且为空快照。
#[test]
fn remote_runtime_snapshot_maps_unsupported_peer_error() {
    use crate::net::peer_error::PeerCallError;
    let project = remote_shortcut_row();
    let error = PeerCallError::Unsupported {
        url: "http://peer.local".to_string(),
        capability: "orchestrator.runtime-snapshot.v1",
    };

    let snapshot = remote_runtime_snapshot_from_peer_error(&project, error);

    assert_eq!(snapshot.project_id, "shortcut-project-1");
    assert_eq!(snapshot.project_kind, "remote");
    assert_eq!(snapshot.remote_status, "unsupported");
    assert!(!snapshot.scheduler_enabled);
    assert_eq!(snapshot.max_concurrent_tasks, 0);
    assert_eq!(snapshot.slots_used, 0);
    assert_eq!(snapshot.slots_available, 0);
    assert!(snapshot.running_tasks.is_empty());
    assert!(snapshot.retrying_tasks.is_empty());
    assert!(snapshot.recent_events.is_empty());
    assert!(snapshot.latest_tick_at.is_none());
    assert!(snapshot.last_dispatch_at.is_none());
    assert_eq!(snapshot.last_dispatched_count, 0);
    assert!(snapshot
        .latest_error
        .as_deref()
        .unwrap_or_default()
        .contains("暂不支持运行时快照"));
}

/// Business Logic（为什么需要这个测试）:
///     对端离线时状态条必须展示 offline，不能误报 unsupported 或把本机 scheduler 当远端。
///
/// Code Logic（这个测试做什么）:
///     通过真实不可达地址构造 PeerCallError::Network，断言 remoteStatus=offline 空快照。
#[tokio::test]
async fn remote_runtime_snapshot_maps_network_peer_error_to_offline() {
    use crate::net::peer_error::PeerCallError;
    let project = remote_shortcut_row();
    // 通过真实失败的 reqwest 请求构造 Network 变体（reqwest::Error 无法手工 new）。
    let network_err = reqwest::Client::new()
        .get("http://127.0.0.1:1/")
        .timeout(std::time::Duration::from_millis(50))
        .send()
        .await
        .expect_err("unreachable local port should fail");
    let error = PeerCallError::Network {
        url: "http://127.0.0.1:1".to_string(),
        source: network_err,
    };

    let snapshot = remote_runtime_snapshot_from_peer_error(&project, error);

    assert_eq!(snapshot.remote_status, "offline");
    assert_eq!(snapshot.project_kind, "remote");
    assert!(!snapshot.scheduler_enabled);
    assert!(snapshot.running_tasks.is_empty());
    assert!(snapshot.retrying_tasks.is_empty());
    assert!(snapshot.recent_events.is_empty());
    assert_eq!(snapshot.max_concurrent_tasks, 0);
    assert!(snapshot
        .latest_error
        .as_deref()
        .unwrap_or_default()
        .contains("离线"));
}

/// Business Logic（为什么需要这个测试）:
///     协议违例或对端业务错误应展示 unavailable，不能把 404/文案误判为 capability 缺失。
///
/// Code Logic（这个测试做什么）:
///     分别构造 InvalidResponse 与 Remote 变体，断言两者都映射到 remoteStatus=unavailable。
#[test]
fn remote_runtime_snapshot_maps_invalid_and_remote_peer_errors_to_unavailable() {
    use crate::net::peer_error::PeerCallError;
    let project = remote_shortcut_row();

    let invalid = remote_runtime_snapshot_from_peer_error(
        &project,
        PeerCallError::InvalidResponse {
            url: "http://peer.local".to_string(),
            reason: "not json".to_string(),
        },
    );
    let remote = remote_runtime_snapshot_from_peer_error(
        &project,
        PeerCallError::Remote {
            url: "http://peer.local".to_string(),
            status: 503,
            code: "unavailable".to_string(),
            message: "owner busy".to_string(),
            request_id: "req-1".to_string(),
            retryable: true,
            legacy: false,
            details: serde_json::json!({}),
        },
    );

    for snapshot in [invalid, remote] {
        assert_eq!(snapshot.remote_status, "unavailable");
        assert_eq!(snapshot.project_kind, "remote");
        assert!(!snapshot.scheduler_enabled);
        assert!(snapshot.running_tasks.is_empty());
        assert!(snapshot.retrying_tasks.is_empty());
        assert!(snapshot.recent_events.is_empty());
        assert_eq!(snapshot.slots_used, 0);
        assert_eq!(snapshot.workflow_source, "remoteUnavailable");
        assert!(!snapshot.workflow_valid);
        assert!(snapshot
            .latest_error
            .as_deref()
            .unwrap_or_default()
            .contains("暂时不可用"));
    }
}

/// Business Logic（为什么需要这个测试）:
///     远端空态不得读取本机 scheduler/config/workflow；即使本机有 telemetry，
///     empty helper 也必须返回清零槽位与空任务列表。
///
/// Code Logic（这个测试做什么）:
///     直接调用 remote_runtime_snapshot_empty 的四种状态，断言均不包含本机 runtime 字段。
#[test]
fn remote_runtime_snapshot_empty_states_ignore_local_runtime() {
    let project = remote_shortcut_row();
    for status in ["unsupported", "offline", "unavailable"] {
        let snapshot = remote_runtime_snapshot_empty(&project, status, "msg");
        assert_eq!(snapshot.remote_status, status);
        assert_eq!(snapshot.project_id, project.id);
        assert_eq!(snapshot.project_kind, "remote");
        assert!(!snapshot.scheduler_enabled);
        assert_eq!(snapshot.max_concurrent_tasks, 0);
        assert_eq!(snapshot.slots_used, 0);
        assert_eq!(snapshot.slots_available, 0);
        assert_eq!(snapshot.last_dispatched_count, 0);
        assert!(snapshot.latest_tick_at.is_none());
        assert!(snapshot.last_dispatch_at.is_none());
        assert!(snapshot.running_tasks.is_empty());
        assert!(snapshot.retrying_tasks.is_empty());
        assert!(snapshot.recent_events.is_empty());
        assert_eq!(snapshot.workflow_source, "remoteUnavailable");
        assert!(!snapshot.workflow_valid);
    }
}

/// Business Logic（为什么需要这个测试）:
///     runtime snapshot DTO 是本机命令与未来 owning-device P2P 路由共享的稳定契约，
///     任何字段顺序、camelCase 形状或字段集变化都会破坏两端一致性。golden test 锁住完整本地 DTO。
///
/// Code Logic（这个测试做什么）:
///     构造一个 running + 一个 rework/blocked 任务并追加事件，写入完整 scheduler telemetry
///     （含 lastDispatchAt、lastDispatchedCount 与 latestError）后调用共享 builder，
///     断言全部 DTO 字段、任务摘要字段、事件字段、generatedAt 形状、lastDispatchAt 与 lastDispatchedCount。
#[tokio::test]
async fn runtime_snapshot_locks_full_local_dto_for_local_and_remote_callers() {
    let repo = setup_orchestrator_repo().await;
    let project_dir = temp_project_dir("orch-snapshot-golden");
    let project = local_project_row(project_dir);
    let mut running = command_task_row("task-running-golden", OrchestratorTaskStatus::Running);
    running.title = "实现运行时快照".to_string();
    running.attempt_phase = Some(OrchestratorAttemptPhase::Streaming);
    running.session_id = Some("session-golden".to_string());
    running.worktree_id = Some("worktree-golden".to_string());
    running.last_runtime_message = Some("正在执行测试".to_string());
    running.last_activity_at = Some("2026-07-12T01:02:03Z".to_string());
    let mut retrying = command_task_row("task-retrying-golden", OrchestratorTaskStatus::Blocked);
    retrying.title = "修复验证失败".to_string();
    retrying.workflow_state = OrchestratorWorkflowState::Rework;
    retrying.run_state = OrchestratorRunState::Blocked;
    retrying.blocked_reason = Some("验证器要求修复".to_string());
    retrying.updated_at = "2026-07-12T01:03:00Z".to_string();
    repo.create_task(&running).await.unwrap();
    repo.create_task(&retrying).await.unwrap();
    repo.add_event(&running.id, "runner", "Runner 已启动", None)
        .await
        .unwrap();
    repo.add_event(&retrying.id, "blocked", "验证器要求修复", None)
        .await
        .unwrap();
    let config = OrchestratorAutomationConfig {
        enabled: true,
        max_concurrent_tasks: 3,
        ..OrchestratorAutomationConfig::default()
    };
    let telemetry = OrchestratorSchedulerTelemetry::new();
    telemetry.record_dispatch_result(
        "2026-07-12T01:04:05Z".to_string(),
        2,
        Some("  ".to_string()),
    );

    let snapshot = get_orchestrator_runtime_snapshot_for_project(
        &repo,
        &config,
        &project,
        &telemetry.snapshot(),
    )
    .await
    .unwrap();

    // 顶层 DTO 契约：projectId/kind、remoteStatus=local、generatedAt RFC3339、tick/slot/调度字段。
    assert_eq!(snapshot.project_id, "project-1");
    assert_eq!(snapshot.project_kind, "local");
    assert_eq!(snapshot.remote_status, "local");
    assert!(snapshot.scheduler_enabled);
    // generatedAt 由 chrono to_rfc3339() 生成，形如 2026-07-12T01:02:03+00:00，
    // 锁定其结构而非具体值：包含日期分隔符 'T' 与 UTC offset（Z 或 +00:00）。
    assert!(snapshot.generated_at.contains('T'));
    assert!(
        snapshot.generated_at.ends_with('Z') || snapshot.generated_at.ends_with("+00:00"),
        "generatedAt 应是 UTC RFC3339 时间，实际: {}",
        snapshot.generated_at
    );
    assert!(snapshot.generated_at.len() > 15);
    assert_eq!(
        snapshot.latest_tick_at.as_deref(),
        Some("2026-07-12T01:04:05Z")
    );
    assert_eq!(
        snapshot.last_dispatch_at.as_deref(),
        Some("2026-07-12T01:04:05Z")
    );
    assert_eq!(snapshot.last_dispatched_count, 2);
    // 空 telemetry 错误会被归一为 None，此时 latestError 由 repo 的最近 blocked_reason 回填。
    assert_eq!(snapshot.latest_error.as_deref(), Some("验证器要求修复"));
    assert_eq!(snapshot.workflow_source, "builtInDefault");
    assert!(snapshot.workflow_valid);
    assert!(snapshot.workflow_error.is_none());
    assert_eq!(snapshot.max_concurrent_tasks, 3);
    assert_eq!(snapshot.slots_used, 1); // 仅 running 计入 active 槽位
    assert_eq!(snapshot.slots_available, 2);

    // running 摘要契约：包含 runner runtime 字段，供 P2P 远端 UI 与本机状态条共用。
    assert_eq!(snapshot.running_tasks.len(), 1);
    let running_summary = &snapshot.running_tasks[0];
    assert_eq!(running_summary.task_id, "task-running-golden");
    assert_eq!(running_summary.title, "实现运行时快照");
    assert_eq!(
        running_summary.workflow_state,
        OrchestratorWorkflowState::InProgress
    );
    assert_eq!(running_summary.run_state, OrchestratorRunState::Running);
    assert_eq!(
        running_summary.attempt_phase,
        Some(OrchestratorAttemptPhase::Streaming)
    );
    assert_eq!(
        running_summary.session_id.as_deref(),
        Some("session-golden")
    );
    assert_eq!(
        running_summary.worktree_id.as_deref(),
        Some("worktree-golden")
    );
    assert_eq!(
        running_summary.last_runtime_message.as_deref(),
        Some("正在执行测试")
    );
    assert_eq!(
        running_summary.last_activity_at.as_deref(),
        Some("2026-07-12T01:02:03Z")
    );

    // retrying 摘要契约：rework/blocked 任务进入 retrying 列表，本机命令与 P2P 路由共用。
    assert_eq!(snapshot.retrying_tasks.len(), 1);
    let retrying_summary = &snapshot.retrying_tasks[0];
    assert_eq!(retrying_summary.task_id, "task-retrying-golden");
    assert_eq!(retrying_summary.title, "修复验证失败");
    assert_eq!(
        retrying_summary.workflow_state,
        OrchestratorWorkflowState::Rework
    );
    assert_eq!(retrying_summary.run_state, OrchestratorRunState::Blocked);

    // recent_events 契约：camelCase DTO 字段稳定。
    assert_eq!(snapshot.recent_events.len(), 2);
    let runner_event = snapshot
        .recent_events
        .iter()
        .find(|event| event.kind == "runner")
        .expect("runner event present");
    assert_eq!(runner_event.task_id, "task-running-golden");
    assert_eq!(runner_event.task_title, "实现运行时快照");
    assert_eq!(runner_event.message, "Runner 已启动");
    assert!(runner_event.created_at.contains('T'));
    assert!(!runner_event.id.is_empty());

    // 序列化形状：camelCase key 锁死，未来 P2P 客户端反序列化与前端契约保持一致。
    let value = serde_json::to_value(&snapshot).expect("serialize snapshot");
    assert_eq!(value["projectId"], "project-1");
    assert_eq!(value["projectKind"], "local");
    assert_eq!(value["remoteStatus"], "local");
    assert_eq!(value["schedulerEnabled"], true);
    assert_eq!(value["latestTickAt"], "2026-07-12T01:04:05Z");
    assert_eq!(value["lastDispatchAt"], "2026-07-12T01:04:05Z");
    assert_eq!(value["lastDispatchedCount"], 2);
    assert_eq!(value["maxConcurrentTasks"], 3);
    assert_eq!(value["slotsUsed"], 1);
    assert_eq!(value["slotsAvailable"], 2);
    assert_eq!(value["runningTasks"][0]["taskId"], "task-running-golden");
    assert_eq!(value["runningTasks"][0]["attemptPhase"], "streaming");
    // recentEvents 顺序依赖数据库 created_at/id，断言存在性而非位置以避免抖动。
    let recent_events = value["recentEvents"]
        .as_array()
        .expect("recentEvents is array");
    assert_eq!(recent_events.len(), 2);
    assert!(recent_events
        .iter()
        .any(|event| { event["taskId"] == "task-running-golden" && event["kind"] == "runner" }));
    assert!(recent_events
        .iter()
        .any(|event| { event["taskId"] == "task-retrying-golden" && event["kind"] == "blocked" }));
    // 单条事件 camelCase 字段形状稳定（取 runner 事件作为代表）。
    let runner_event_value = recent_events
        .iter()
        .find(|event| event["kind"] == "runner")
        .expect("runner event in serialized output");
    assert_eq!(runner_event_value["taskTitle"], "实现运行时快照");
    assert_eq!(runner_event_value["message"], "Runner 已启动");
    assert!(!runner_event_value["createdAt"].as_str().unwrap().is_empty());
    assert!(!runner_event_value["id"].as_str().unwrap().is_empty());
}

/// Business Logic（为什么需要这个测试）:
///     scheduler 最近一次 dispatch 失败时，latestError 必须回填到 snapshot，让用户能在状态条看到调度异常。
///
/// Code Logic（这个测试做什么）:
///     用空项目目录调用 builder，scheduler telemetry 写入错误，断言 latestError 来自 telemetry 且槽位为空。
#[tokio::test]
async fn runtime_snapshot_surfaces_scheduler_latest_error() {
    let repo = setup_orchestrator_repo().await;
    let project = local_project_row(temp_project_dir("orch-snapshot-scheduler-error"));
    let telemetry = OrchestratorSchedulerTelemetry::new();
    telemetry.record_dispatch_result(
        "2026-07-12T02:00:00Z".to_string(),
        0,
        Some("runner 启动失败".to_string()),
    );

    let snapshot = get_orchestrator_runtime_snapshot_for_project(
        &repo,
        &OrchestratorAutomationConfig::default(),
        &project,
        &telemetry.snapshot(),
    )
    .await
    .unwrap();

    assert_eq!(snapshot.latest_error.as_deref(), Some("runner 启动失败"));
    assert_eq!(snapshot.last_dispatched_count, 0);
    assert_eq!(snapshot.slots_used, 0);
    assert_eq!(snapshot.max_concurrent_tasks, 1);
    assert_eq!(snapshot.slots_available, 1);
    assert!(snapshot.running_tasks.is_empty());
    assert!(snapshot.retrying_tasks.is_empty());
    assert!(snapshot.recent_events.is_empty());
}

/// Business Logic（为什么需要这个测试）:
///     repo 查询槽位失败时（例如数据库不可用），builder 必须把错误透传，而不是用 0 槽位掩盖仓储异常。
///     本机命令与未来 P2P 路由调用同一 builder，错误语义必须一致。
///
/// Code Logic（这个测试做什么）:
///     用未初始化 schema 的内存 SQLite 构造 repo，调用 builder，断言返回仓储错误而非空快照。
#[tokio::test]
async fn runtime_snapshot_propagates_repo_failure() {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .expect("sqlite options")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("sqlite pool");
    // 故意不调用 OrchestratorRepo::init_schema，模拟表缺失导致的仓储错误。
    let repo = OrchestratorRepo::new(pool);
    let project = local_project_row(temp_project_dir("orch-snapshot-repo-failure"));

    let result = get_orchestrator_runtime_snapshot_for_project(
        &repo,
        &OrchestratorAutomationConfig::default(),
        &project,
        &empty_scheduler_snapshot(),
    )
    .await;

    assert!(result.is_err(), "repo 查询失败必须透传，不能用空快照掩盖");
    let error = result.expect_err("snapshot must error");
    assert!(
        error.to_string().to_lowercase().contains("no such table")
            || error.to_string().contains("orchestrator"),
        "错误信息应指向 orchestrator 表缺失，实际: {error}"
    );
}

/// Business Logic（为什么需要这个测试）:
///     未来 owning-device P2P 路由（T2）会绕过 tauri 命令直接调用共享 builder，
///     因此 builder 必须只接受已校验的本地 WorkbenchProject，不能内部分支远端 shortcut。
///     本测试通过直接调用 builder 确认其行为稳定，T2 接入时无需改动 builder 逻辑。
///
/// Code Logic（这个测试做什么）:
///     用本地项目行直接调用 builder（模拟 P2P 路由的调用路径），断言返回 local 快照，
///     证明命令入口与未来 P2P 路由使用的是同一构造逻辑。
#[tokio::test]
async fn runtime_snapshot_builder_is_reusable_by_p2p_route() {
    let repo = setup_orchestrator_repo().await;
    let project = local_project_row(temp_project_dir("orch-snapshot-p2p-route"));
    let config = OrchestratorAutomationConfig {
        enabled: true,
        max_concurrent_tasks: 2,
        ..OrchestratorAutomationConfig::default()
    };

    // 模拟 P2P 路由：仅持有 repo + 已校验的本地 project + scheduler snapshot，
    // 不经过 tauri command 层，确认同一 builder 直接可用。
    let snapshot = get_orchestrator_runtime_snapshot_for_project(
        &repo,
        &config,
        &project,
        &empty_scheduler_snapshot(),
    )
    .await
    .unwrap();

    assert_eq!(snapshot.project_id, "project-1");
    assert_eq!(snapshot.project_kind, "local");
    assert_eq!(snapshot.remote_status, "local");
    assert!(snapshot.scheduler_enabled);
    assert_eq!(snapshot.slots_available, 2);
}

/// Business Logic（为什么需要这个测试）:
///     未来 owning-device P2P 路由会把本机 builder 产出的 OrchestratorRuntimeSnapshotDto
///     序列化为 JSON 返回给请求端，请求端必须能用同一类型反序列化。本测试锁死 DTO 图的
///     round-trip（Serialize -> Deserialize）能力，证明 Deserialize 派生覆盖了所有嵌套类型。
///
/// Code Logic（这个测试做什么）:
///     用本地 builder 构造真实 snapshot，序列化为 JSON Value 后再反序列化回
///     OrchestratorRuntimeSnapshotDto，断言关键字段与原 DTO 一致，证明远端客户端可解析。
#[tokio::test]
async fn runtime_snapshot_dto_round_trips_through_json_for_remote_client() {
    let repo = setup_orchestrator_repo().await;
    let mut running = command_task_row("task-roundtrip", OrchestratorTaskStatus::Running);
    running.attempt_phase = Some(OrchestratorAttemptPhase::Streaming);
    running.session_id = Some("session-roundtrip".to_string());
    repo.create_task(&running).await.unwrap();
    repo.add_event(&running.id, "runner", "Runner roundtrip", None)
        .await
        .unwrap();
    let project = local_project_row(temp_project_dir("orch-snapshot-roundtrip"));
    let config = OrchestratorAutomationConfig {
        enabled: true,
        max_concurrent_tasks: 2,
        ..OrchestratorAutomationConfig::default()
    };
    let telemetry = OrchestratorSchedulerTelemetry::new();
    telemetry.record_dispatch_result("2026-07-12T03:00:00Z".to_string(), 1, None);

    let snapshot = get_orchestrator_runtime_snapshot_for_project(
        &repo,
        &config,
        &project,
        &telemetry.snapshot(),
    )
    .await
    .unwrap();

    let json = serde_json::to_value(&snapshot).expect("serialize snapshot");
    let parsed: OrchestratorRuntimeSnapshotDto =
        serde_json::from_value(json).expect("deserialize snapshot for remote client");

    assert_eq!(parsed.project_id, "project-1");
    assert_eq!(parsed.project_kind, "local");
    assert_eq!(parsed.remote_status, "local");
    assert!(parsed.scheduler_enabled);
    assert_eq!(
        parsed.latest_tick_at.as_deref(),
        Some("2026-07-12T03:00:00Z")
    );
    assert_eq!(parsed.last_dispatched_count, 1);
    assert_eq!(parsed.slots_used, 1);
    assert_eq!(parsed.slots_available, 1);
    assert_eq!(parsed.running_tasks.len(), 1);
    assert_eq!(parsed.running_tasks[0].task_id, "task-roundtrip");
    assert_eq!(
        parsed.running_tasks[0].attempt_phase,
        Some(OrchestratorAttemptPhase::Streaming)
    );
    assert_eq!(
        parsed.running_tasks[0].session_id.as_deref(),
        Some("session-roundtrip")
    );
    assert_eq!(parsed.recent_events.len(), 1);
    assert_eq!(parsed.recent_events[0].task_id, "task-roundtrip");
    assert_eq!(parsed.recent_events[0].kind, "runner");
}

/// Business Logic（为什么需要这个测试）:
///     在线远端创建如果响应超时后落入 pending outbox，必须复用同一次请求生成的 clientRequestId 作为幂等键。
///
/// Code Logic（这个测试做什么）:
///     从本机 create request 投影远端请求，断言生成了非空 client_request_id，且 clone 后保持同一个 key。
#[test]
fn remote_create_request_from_local_generates_stable_client_request_id() {
    let request = CreateOrchestratorTaskRequest {
        project_id: "shortcut-project-1".to_string(),
        title: "远端任务".to_string(),
        goal: "完成目标".to_string(),
        acceptance_criteria: "验收".to_string(),
        priority: Some(3),
        create_action: OrchestratorCreateAction::Start,
        source: None,
        external_id: None,
        external_identifier: None,
        external_url: None,
        external_state: None,
        external_labels: None,
    };

    let remote_request = remote_create_request_from_local(&request);
    let cloned_for_outbox = remote_request.clone();

    assert!(remote_request
        .client_request_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty()));
    assert_eq!(
        remote_request.client_request_id,
        cloned_for_outbox.client_request_id
    );
}

/// Business Logic（为什么需要这个测试）:
///     远端任务返回给本机前端时必须使用 Workbench remote id 通道，否则后续按钮会把裸远端 id 当成本机 id。
///
/// Code Logic（这个测试做什么）:
///     构造含 worktree/session 的远端任务 DTO，映射后断言 task/project/worktree/session id 均符合本机 remote 规则。
#[test]
fn remote_task_mapping_wraps_ids_for_local_shortcut() {
    let shortcut = remote_shortcut_row();
    let mut task = OrchestratorTaskDto::from(command_task_row(
        "remote-task-1",
        OrchestratorTaskStatus::Running,
    ));
    task.project_id = "remote-project-1".to_string();
    task.worktree_id = Some("remote-worktree-1".to_string());
    task.session_id = Some("remote-session-1".to_string());

    let mapped = map_remote_task_for_shortcut(task, &shortcut);

    assert_eq!(mapped.id, "remote:device-a:remote-task-1");
    assert_eq!(mapped.project_id, "shortcut-project-1");
    assert_eq!(
        mapped.worktree_id.as_deref(),
        Some("remote:device-a:remote-worktree-1")
    );
    assert_eq!(
        mapped.session_id.as_deref(),
        Some("remote:device-a:remote-session-1")
    );
}

/// Business Logic（为什么需要这个测试）:
///     前端 remote-aware API 直接消费 OrchestratorTaskViewDto，设备字段必须保持 camelCase。
///
/// Code Logic（这个测试做什么）:
///     序列化 Remote 视图并断言 origin、deviceId、deviceName 字段符合 Tauri invoke 契约。
#[test]
fn remote_task_view_serializes_fields_as_camel_case() {
    let shortcut = remote_shortcut_row();
    let task = OrchestratorTaskDto::from(command_task_row(
        "remote-task-1",
        OrchestratorTaskStatus::Running,
    ));
    let view = remote_task_view(task, &shortcut);

    let value = serde_json::to_value(view).expect("serialize task view");

    assert_eq!(value["origin"], "remote");
    assert_eq!(value["deviceId"], "device-a");
    assert_eq!(value["deviceName"], "Mac mini");
    assert!(value.get("device_id").is_none());
    assert!(value.get("device_name").is_none());
}

/// Business Logic（为什么需要这个测试）:
///     远端 queue/retry/abort/evidence 入参可能是本机 UI 的 remote id，发送前必须剥离为 owning device 裸 id。
///
/// Code Logic（这个测试做什么）:
///     分别传入匹配设备 remote id、裸 id 和错误设备 remote id，断言只接受前两类。
#[test]
fn remote_task_id_helper_strips_matching_remote_id_and_rejects_wrong_device() {
    let shortcut = remote_shortcut_row();

    let stripped = remote_inner_task_id_for_shortcut(&shortcut, "remote:device-a:task-1").unwrap();
    let raw = remote_inner_task_id_for_shortcut(&shortcut, "task-2").unwrap();
    let error = remote_inner_task_id_for_shortcut(&shortcut, "remote:device-b:task-3")
        .expect_err("wrong device must fail");

    assert_eq!(stripped, "task-1");
    assert_eq!(raw, "task-2");
    assert!(error.to_string().contains("远端任务不属于当前设备"));
}

/// Business Logic（为什么需要这个函数）:
///     项目 WORKFLOW.md 声明验证命令时，completion pipeline 必须优先执行项目命令而不是全局默认。
///
/// Code Logic（这个函数做什么）:
///     写入 validation.commands，构造带全局 fallback 的配置，断言 helper 返回项目命令。
#[test]
fn agent_completion_validation_commands_prefer_project_workflow() {
    let project_dir = temp_project_dir("validation-project");
    fs::write(
        Path::new(&project_dir).join("WORKFLOW.md"),
        "---\nvalidation:\n  commands:\n    - cargo test workflow_runtime --lib\n---\nBody",
    )
    .expect("write WORKFLOW.md");
    let config = OrchestratorAutomationConfig {
        verification_commands: vec!["cargo test orchestrator::delivery --lib".to_string()],
        ..OrchestratorAutomationConfig::default()
    };

    let commands = validation_commands_for_agent_completion(Path::new(&project_dir), &config)
        .expect("workflow validation commands");

    assert_eq!(
        commands,
        vec!["cargo test workflow_runtime --lib".to_string()]
    );
}

/// Business Logic（为什么需要这个函数）:
///     缺少 WORKFLOW.md 或项目 workflow 未声明验证命令时，Settings 里的 verificationCommands 仍是全局默认。
///
/// Code Logic（这个函数做什么）:
///     使用无 WORKFLOW.md 的临时目录，断言 helper 回落到全局配置命令。
#[test]
fn agent_completion_validation_commands_fallback_to_global_config_when_workflow_omits_them() {
    let project_dir = temp_project_dir("validation-default");
    let config = OrchestratorAutomationConfig {
        verification_commands: vec!["cargo test orchestrator::delivery --lib".to_string()],
        ..OrchestratorAutomationConfig::default()
    };

    let commands = validation_commands_for_agent_completion(Path::new(&project_dir), &config)
        .expect("fallback validation commands");

    assert_eq!(
        commands,
        vec!["cargo test orchestrator::delivery --lib".to_string()]
    );
}

/// Business Logic（为什么需要这个函数）:
///     WORKFLOW.md 无效时 completion 不应静默落回 Settings，否则用户会误以为项目策略已生效。
///
/// Code Logic（这个函数做什么）:
///     写入非法 workflow front matter，断言 helper 返回错误，供 completion pipeline 写 evidence 并 Blocked。
#[test]
fn agent_completion_validation_commands_error_on_invalid_project_workflow() {
    let project_dir = temp_project_dir("validation-invalid");
    fs::write(
        Path::new(&project_dir).join("WORKFLOW.md"),
        "---\n[\n---\nBody",
    )
    .expect("write invalid WORKFLOW.md");
    let config = OrchestratorAutomationConfig {
        verification_commands: vec!["cargo test orchestrator::delivery --lib".to_string()],
        ..OrchestratorAutomationConfig::default()
    };

    let error = validation_commands_for_agent_completion(Path::new(&project_dir), &config)
        .expect_err("invalid workflow must block validation");

    assert!(error.to_string().contains("WORKFLOW.md"));
}

/// Business Logic（为什么需要这个函数）:
///     验证通过后进入 Delivering 必须用 expected-status，不能覆盖验证期间用户点击 Abort 的结果。
///
/// Code Logic（这个函数做什么）:
///     创建 Aborted 任务并调用命令层交付转换 helper，断言 transitioned=false 且数据库仍为 Aborted。
#[tokio::test]
async fn delivery_transition_skips_when_task_is_no_longer_verifying() {
    let repo = setup_orchestrator_repo().await;
    let task = command_task_row("task-aborted", OrchestratorTaskStatus::Aborted);
    repo.create_task(&task).await.expect("insert task");

    let transition = transition_verified_task_to_delivering(&repo, &task.id)
        .await
        .expect("transition result");
    let persisted = repo.get_task(&task.id).await.expect("persisted task");

    assert!(!transition.transitioned);
    assert_eq!(transition.task.status, OrchestratorTaskStatus::Aborted);
    assert_eq!(persisted.status, OrchestratorTaskStatus::Aborted);
}

/// Business Logic（为什么需要这个函数）:
///     verifier 通过但 full-auto delivery 关闭时，任务应进入人工复核泳道，而不是继续自动交付或阻塞。
///
/// Code Logic（这个函数做什么）:
///     创建 Verifying 任务并调用 HumanReview 条件转换 helper，断言 legacy status 与 split state 精确落库。
#[tokio::test]
async fn human_review_transition_sets_split_state_when_delivery_disabled() {
    let repo = setup_orchestrator_repo().await;
    let task = command_task_row("task-human-review", OrchestratorTaskStatus::Verifying);
    repo.create_task(&task).await.expect("insert task");

    let transition = transition_verified_task_to_human_review(&repo, &task.id)
        .await
        .expect("transition result");
    let persisted = repo.get_task(&task.id).await.expect("persisted task");

    assert!(transition.transitioned);
    assert_eq!(persisted.status, OrchestratorTaskStatus::Done);
    assert_eq!(
        persisted.workflow_state,
        OrchestratorWorkflowState::HumanReview
    );
    assert_eq!(persisted.run_state, OrchestratorRunState::Idle);
    assert_eq!(
        persisted.attempt_phase,
        Some(OrchestratorAttemptPhase::Succeeded)
    );
}

/// Business Logic（为什么需要这个函数）:
///     verifier 未通过时，任务必须明确进入 Rework 并标记本轮 failed，同时保持 Preparing 以便立即启动修复 Runner。
///
/// Code Logic（这个函数做什么）:
///     创建 Verifying 任务并调用 repair 条件转换 helper，断言 workflow/run/attempt phase 与修复准备语义一致。
#[tokio::test]
async fn repair_transition_sets_rework_preparing_failed_phase() {
    let repo = setup_orchestrator_repo().await;
    let task = command_task_row("task-rework", OrchestratorTaskStatus::Verifying);
    repo.create_task(&task).await.expect("insert task");

    let transition = transition_failed_verification_task_to_preparing(&repo, &task.id)
        .await
        .expect("transition result");
    let persisted = repo.get_task(&task.id).await.expect("persisted task");

    assert!(transition.transitioned);
    assert_eq!(persisted.status, OrchestratorTaskStatus::Preparing);
    assert_eq!(persisted.workflow_state, OrchestratorWorkflowState::Rework);
    assert_eq!(persisted.run_state, OrchestratorRunState::Preparing);
    assert_eq!(
        persisted.attempt_phase,
        Some(OrchestratorAttemptPhase::Failed)
    );
    let token = persisted
        .prepare_claim_token
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .expect("repair transition must issue claim token");
    assert!(!token.is_empty());
    assert_eq!(transition.task.prepare_claim_token.as_deref(), Some(token));
}

/// Business Logic（为什么需要这个函数）:
///     只有全局自动化和四个自动交付开关全部打开时才应进入 Delivering，默认配置应转人工复核。
///
/// Code Logic（这个函数做什么）:
///     构造默认关闭、全部开启和单项关闭配置，断言 helper 对所有开关执行 AND 语义。
#[test]
fn auto_delivery_enabled_requires_all_flags() {
    assert!(!auto_delivery_enabled(
        &OrchestratorAutomationConfig::default()
    ));

    let enabled = OrchestratorAutomationConfig {
        enabled: true,
        ..OrchestratorAutomationConfig::default()
    };
    assert!(auto_delivery_enabled(&enabled));

    let disabled = OrchestratorAutomationConfig {
        enabled: true,
        auto_push_main: false,
        ..OrchestratorAutomationConfig::default()
    };
    assert!(!auto_delivery_enabled(&disabled));
}

/// Business Logic（为什么需要这个函数）:
///     显式 deliverReviewedTask 也必须受 Settings full-auto delivery 总闸控制，避免用户复核页按钮绕过交付策略。
///
/// Code Logic（这个函数做什么）:
///     使用默认关闭、全部开启和单项关闭三组配置调用 gate helper，断言关闭时返回可读 Settings 错误。
#[test]
fn reviewed_delivery_gate_requires_full_auto_settings() {
    let default_config = OrchestratorAutomationConfig::default();
    let default_error = ensure_reviewed_delivery_allowed(&default_config)
        .expect_err("default settings should block explicit delivery");
    assert!(default_error.to_string().contains("Settings"));

    let enabled = OrchestratorAutomationConfig {
        enabled: true,
        ..OrchestratorAutomationConfig::default()
    };
    assert!(ensure_reviewed_delivery_allowed(&enabled).is_ok());

    let partial = OrchestratorAutomationConfig {
        enabled: true,
        auto_merge_to_main: false,
        ..OrchestratorAutomationConfig::default()
    };
    let partial_error = ensure_reviewed_delivery_allowed(&partial)
        .expect_err("partial delivery settings should block explicit delivery");
    assert!(partial_error.to_string().contains("Settings"));
}

/// Business Logic（为什么需要这个函数）:
///     手动完成按钮与 sentinel 共用 completion helper，Running->Verifying 成功后必须把当前 active attempt 标为 completed。
///
/// Code Logic（这个函数做什么）:
///     创建 Running 任务和 running attempt，模拟 helper 的状态转移后调用 active attempt 完成 helper，
///     断言 session 反查不再返回 running attempt。
#[tokio::test]
async fn completion_helper_marks_active_running_attempt_completed_after_verifying() {
    let repo = setup_orchestrator_repo().await;
    let mut task = command_task_row("task-running", OrchestratorTaskStatus::Running);
    task.attempt = 1;
    task.worktree_id = Some("worktree-1".to_string());
    task.session_id = Some("session-1".to_string());
    repo.create_task(&task).await.expect("insert task");
    repo.add_attempt(
        &task.id,
        1,
        "worktree-1",
        "session-1",
        "prompt",
        &RunnerAttemptPolicy::claude_default(),
        None,
        OrchestratorAttemptStatus::Running,
    )
    .await
    .expect("insert attempt");

    let verifying = repo
        .transition_task_status(
            &task.id,
            OrchestratorTaskStatus::Running,
            OrchestratorTaskStatus::Verifying,
            None,
        )
        .await
        .expect("transition to verifying");
    let completed = mark_active_running_attempt_completed(&repo, &verifying)
        .await
        .expect("complete active attempt");
    let running = repo
        .get_running_attempt_by_session("session-1")
        .await
        .expect("query running attempt");

    assert_eq!(completed.status, "completed");
    assert!(completed.completed_at.is_some());
    assert!(running.is_none());
}

/// Business Logic（为什么需要这个函数）:
///     Agent 完成按钮只允许 Running 任务进入验证，避免用户把草稿或已结束任务误推进状态机。
///
/// Code Logic（这个函数做什么）:
///     构造 Running 与 Draft 任务，断言完成校验 helper 只接受 Running。
#[test]
fn complete_agent_run_guard_accepts_only_running_tasks() {
    let running = OrchestratorTaskRow {
        id: "task-running".to_string(),
        project_id: "project-1".to_string(),
        title: "运行中".to_string(),
        goal: "goal".to_string(),
        acceptance_criteria: "criteria".to_string(),
        status: OrchestratorTaskStatus::Running,
        priority: 0,
        branch_name: None,
        worktree_id: Some("worktree-1".to_string()),
        session_id: None,
        prepare_claim_token: None,
        blocked_reason: None,
        attempt: 0,
        created_at: "2026-07-05T00:00:00Z".to_string(),
        updated_at: "2026-07-05T00:00:00Z".to_string(),
        started_at: None,
        finished_at: None,
        ..OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Running)
    };
    let draft = OrchestratorTaskRow {
        status: OrchestratorTaskStatus::Draft,
        ..OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Draft)
    };

    assert!(ensure_task_can_complete_agent_run(&running).is_ok());
    let error = ensure_task_can_complete_agent_run(&draft).expect_err("draft must fail");
    assert!(error.to_string().contains("运行中"));
}

/// Business Logic（为什么需要这个函数）:
///     缺少 worktree 或找不到 worktree 这类验证前置失败，也必须作为 failed verification evidence 展示给用户。
///
/// Code Logic（这个函数做什么）:
///     调用验证失败 evidence helper，断言 kind/title/summary/content 精确匹配 add_evidence 入参契约。
#[test]
fn verification_failure_evidence_uses_failed_verification_contract() {
    let evidence = verification_failure_evidence("任务缺少 worktree，无法运行验证命令。");

    assert_eq!(evidence.kind, "verificationOutput");
    assert_eq!(evidence.title, "验证命令");
    assert_eq!(evidence.summary, "failed");
    assert_eq!(evidence.content, "任务缺少 worktree，无法运行验证命令。");
}

/// Business Logic（为什么需要这个函数）:
///     任务进入 Verifying 后，读取 worktree 失败这类可预期错误必须统一转成 failed evidence 和 Blocked 原因。
///
/// Code Logic（这个函数做什么）:
///     调用验证失败 outcome helper，断言上下文和底层错误会进入 reason 与 evidence content。
#[test]
fn verification_failure_outcome_includes_context_error_and_failed_evidence() {
    let error = AppError::generic("数据库读取失败: locked");

    let outcome = verification_failure_outcome("读取任务 worktree 失败", &error);

    assert!(outcome.reason.contains("读取任务 worktree 失败"));
    assert!(outcome.reason.contains("locked"));
    assert_eq!(outcome.evidence.kind, "verificationOutput");
    assert_eq!(outcome.evidence.summary, "failed");
    assert_eq!(outcome.evidence.content, outcome.reason);
}

/// Business Logic（为什么需要这个函数）:
///     verifier 审查结果会直接驱动 delivery 或 repair，evidence summary 必须稳定反映 passed 布尔值。
///
/// Code Logic（这个函数做什么）:
///     分别构造 passed/failed VerifierReview，断言 evidence kind 和 summary 对齐 Phase8 契约。
#[test]
fn verification_review_evidence_uses_pass_fail_summary() {
    let passed = VerifierReview {
        passed: true,
        reason: "满足验收".to_string(),
        repair_prompt: None,
        risk_notes: Vec::new(),
    };
    let failed = VerifierReview {
        passed: false,
        reason: "测试失败".to_string(),
        repair_prompt: Some("修复测试失败".to_string()),
        risk_notes: vec!["需要复跑 cargo test".to_string()],
    };

    let passed_evidence = verification_review_evidence(&passed);
    let failed_evidence = verification_review_evidence(&failed);

    assert_eq!(passed_evidence.kind, EVIDENCE_KIND_VERIFICATION_REVIEW);
    assert_eq!(passed_evidence.summary, "passed");
    assert_eq!(failed_evidence.summary, "failed");
    assert!(failed_evidence.content.contains("修复测试失败"));
}

/// Business Logic（为什么需要这个函数）:
///     verifier 失败时 repairPrompt evidence 是下一轮自动修复的审计入口，kind/summary 不能与验证输出混淆。
///
/// Code Logic（这个函数做什么）:
///     调用 repair prompt evidence helper，断言 kind、summary 和 content 精确匹配。
#[test]
fn repair_prompt_evidence_uses_repair_prompt_contract() {
    let evidence = repair_prompt_evidence("只修复 orchestrator completion 流程");

    assert_eq!(evidence.kind, EVIDENCE_KIND_REPAIR_PROMPT);
    assert_eq!(evidence.title, "修复指令");
    assert_eq!(evidence.summary, "failed");
    assert_eq!(evidence.content, "只修复 orchestrator completion 流程");
}

/// Business Logic（为什么需要这个函数）:
///     verifier 判定失败后，如果用户已经 Abort，系统不能把任务回退到 Preparing 并启动新 runner。
///
/// Code Logic（这个函数做什么）:
///     创建 Aborted 任务并调用 repair 转换 helper，断言 transitioned=false 且数据库仍为 Aborted。
#[tokio::test]
async fn repair_transition_skips_when_task_is_no_longer_verifying() {
    let repo = setup_orchestrator_repo().await;
    let task = command_task_row("task-aborted-repair", OrchestratorTaskStatus::Aborted);
    repo.create_task(&task).await.expect("insert task");

    let transition = transition_failed_verification_task_to_preparing(&repo, &task.id)
        .await
        .expect("transition result");
    let persisted = repo.get_task(&task.id).await.expect("persisted task");

    assert!(!transition.transitioned);
    assert_eq!(transition.task.status, OrchestratorTaskStatus::Aborted);
    assert_eq!(persisted.status, OrchestratorTaskStatus::Aborted);
}

/// Business Logic（为什么需要这个函数）:
///     Blocked 任务的重试按钮只能把阻塞任务重新排队，不应允许 Running/Done 等状态被回退。
///
/// Code Logic（这个函数做什么）:
///     调用重试状态 helper，断言 Blocked 返回 Queued，Running 返回业务错误。
#[test]
fn retry_status_guard_accepts_only_blocked_tasks() {
    assert_eq!(
        retry_orchestrator_task_target_status(OrchestratorTaskStatus::Blocked).unwrap(),
        OrchestratorTaskStatus::Queued
    );

    let error = retry_orchestrator_task_target_status(OrchestratorTaskStatus::Running)
        .expect_err("running retry must fail");
    assert!(error.to_string().contains("阻塞"));
}

/// Business Logic（为什么需要这个函数）:
///     终止按钮只是把任务转为 Aborted，不删除 worktree 或 session。
///
/// Code Logic（这个函数做什么）:
///     调用终止状态 helper，断言任意输入状态都会得到 Aborted。
#[test]
fn abort_status_helper_always_targets_aborted() {
    assert_eq!(
        abort_orchestrator_task_target_status(OrchestratorTaskStatus::Running),
        OrchestratorTaskStatus::Aborted
    );
    assert_eq!(
        abort_orchestrator_task_target_status(OrchestratorTaskStatus::Queued),
        OrchestratorTaskStatus::Aborted
    );
}

/// Business Logic（为什么需要这个函数）:
///     outbox Retry/Discard 命令测试需要隔离 SQLite 与 Workbench 项目表，验证归属与状态机。
///
/// Code Logic（这个函数做什么）:
///     创建单连接内存库、初始化 Orchestrator schema 与最小 workbench_projects 表，返回两个仓储。
async fn setup_outbox_action_repos() -> (OrchestratorRepo, crate::storage::WorkbenchProjectRepo) {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .expect("sqlite options")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("sqlite pool");
    OrchestratorRepo::init_schema(&pool)
        .await
        .expect("orchestrator schema");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS workbench_projects (\
         id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, device_id TEXT NOT NULL, \
         device_name TEXT NOT NULL, path TEXT NOT NULL, last_opened_at TEXT NOT NULL, \
         created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .expect("workbench projects schema");
    (
        OrchestratorRepo::new(pool.clone()),
        crate::storage::WorkbenchProjectRepo::new(pool),
    )
}

/// Business Logic（为什么需要这个函数）:
///     outbox 动作测试需要稳定插入 remote shortcut 与 failed outbox。
///
/// Code Logic（这个函数做什么）:
///     upsert remote project，插入 pending outbox 后标 failed，返回 outbox id。
async fn insert_failed_outbox_for_shortcut(
    orchestrator_repo: &OrchestratorRepo,
    workbench_project_repo: &crate::storage::WorkbenchProjectRepo,
    project_id: &str,
    request_json: &str,
) -> String {
    let project = remote_shortcut_row();
    let mut row = project.clone();
    row.id = project_id.to_string();
    workbench_project_repo
        .upsert(&row)
        .await
        .expect("upsert project");
    let item = orchestrator_repo
        .insert_remote_outbox_pending(
            &project.device_id,
            &project.device_name,
            &project.path,
            None,
            request_json,
        )
        .await
        .expect("insert outbox");
    let claimed = orchestrator_repo
        .claim_remote_outbox_item_as_sending(&item.id)
        .await
        .expect("claim")
        .expect("claimed");
    orchestrator_repo
        .mark_remote_outbox_failed(&claimed.id, "协议错误：远端拒绝")
        .await
        .expect("mark failed");
    claimed.id
}

/// Business Logic（为什么需要这个测试）:
///     Retry 只能作用在当前 remote shortcut 拥有的 failed outbox，且必须保留 payload 并清空 last_error。
///
/// Code Logic（这个测试做什么）:
///     构造 failed outbox，调用 retry helper，断言 DTO camelCase 字段、status=pending、request_json 不变。
#[tokio::test]
async fn retry_remote_outbox_action_requires_project_ownership_and_failed_status() {
    let (orchestrator_repo, workbench_project_repo) = setup_outbox_action_repos().await;
    let request_json = r#"{"projectId":"remote-local","title":"t","goal":"g","acceptanceCriteria":"a","priority":0,"createAction":"backlog","clientRequestId":"req-1"}"#;
    let outbox_id = insert_failed_outbox_for_shortcut(
        &orchestrator_repo,
        &workbench_project_repo,
        "shortcut-project-1",
        request_json,
    )
    .await;

    let dto = retry_orchestrator_remote_outbox_for_repos(
        &orchestrator_repo,
        &workbench_project_repo,
        "shortcut-project-1",
        &outbox_id,
    )
    .await
    .expect("retry failed outbox");

    assert_eq!(dto.id, outbox_id);
    assert_eq!(dto.status.as_str(), "pending");
    assert_eq!(dto.request_json, request_json);
    assert!(dto.last_error.is_none());
    assert_eq!(dto.device_id, "device-a");
    assert_eq!(dto.remote_project_path, "/Users/hans/remote-project");

    let json = serde_json::to_value(&dto).expect("serialize dto");
    assert!(json.get("deviceId").is_some());
    assert!(json.get("remoteProjectPath").is_some());
    assert!(json.get("requestJson").is_some());
    assert!(json.get("lastError").is_some());
    assert!(json.get("device_id").is_none());

    let missing = retry_orchestrator_remote_outbox_for_repos(
        &orchestrator_repo,
        &workbench_project_repo,
        "shortcut-project-1",
        "missing-outbox",
    )
    .await
    .expect_err("missing outbox");
    assert!(missing.to_string().contains("不存在"));

    let wrong_project = remote_shortcut_row();
    let mut other = wrong_project;
    other.id = "other-shortcut".to_string();
    other.path = "/other/path".to_string();
    workbench_project_repo
        .upsert(&other)
        .await
        .expect("other project");
    let wrong = retry_orchestrator_remote_outbox_for_repos(
        &orchestrator_repo,
        &workbench_project_repo,
        "other-shortcut",
        &outbox_id,
    )
    .await
    .expect_err("wrong project ownership");
    assert!(wrong.to_string().contains("不属于当前项目"));

    // pending 后再次 retry 必须拒绝，证明 failed-only。
    let again = retry_orchestrator_remote_outbox_for_repos(
        &orchestrator_repo,
        &workbench_project_repo,
        "shortcut-project-1",
        &outbox_id,
    )
    .await
    .expect_err("non-failed retry");
    assert!(again.to_string().contains("失败"));
}

/// Business Logic（为什么需要这个测试）:
///     Discard 只能作用在当前 shortcut 的 failed outbox，并保留 last_error 审计。
///
/// Code Logic（这个测试做什么）:
///     构造 failed outbox，调用 discard helper，断言 status=discarded 且 last_error 保留；local 项目被拒绝。
#[tokio::test]
async fn discard_remote_outbox_action_is_local_only_and_failed_only() {
    let (orchestrator_repo, workbench_project_repo) = setup_outbox_action_repos().await;
    let request_json = r#"{"projectId":"remote-local","title":"t","goal":"g","acceptanceCriteria":"a","priority":0,"createAction":"backlog","clientRequestId":"req-2"}"#;
    let outbox_id = insert_failed_outbox_for_shortcut(
        &orchestrator_repo,
        &workbench_project_repo,
        "shortcut-project-1",
        request_json,
    )
    .await;

    let local = local_project_row("/tmp/local-project".to_string());
    workbench_project_repo
        .upsert(&local)
        .await
        .expect("local project");
    let local_err = discard_orchestrator_remote_outbox_for_repos(
        &orchestrator_repo,
        &workbench_project_repo,
        "project-1",
        &outbox_id,
    )
    .await
    .expect_err("local project must not operate remote outbox");
    assert!(local_err.to_string().contains("远端项目快捷方式"));

    let dto = discard_orchestrator_remote_outbox_for_repos(
        &orchestrator_repo,
        &workbench_project_repo,
        "shortcut-project-1",
        &outbox_id,
    )
    .await
    .expect("discard failed outbox");
    assert_eq!(dto.status.as_str(), "discarded");
    assert_eq!(dto.request_json, request_json);
    assert_eq!(dto.last_error.as_deref(), Some("协议错误：远端拒绝"));

    let again = discard_orchestrator_remote_outbox_for_repos(
        &orchestrator_repo,
        &workbench_project_repo,
        "shortcut-project-1",
        &outbox_id,
    )
    .await
    .expect_err("discarded cannot discard again");
    assert!(again.to_string().contains("失败"));
}

/// Business Logic（为什么需要这个函数）:
///     deliver digest 测试需要可复现的临时 Git worktree 夹具。
///
/// Code Logic（这个函数做什么）:
///     初始化仓库并提交 base README，返回 TempDir。
fn review_digest_git_fixture() -> tempfile::TempDir {
    use std::process::Command;
    let dir = tempfile::tempdir().expect("tempdir");
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init"]);
    git(&["config", "user.name", "Test"]);
    git(&["config", "user.email", "test@example.com"]);
    fs::write(dir.path().join("README.md"), "base\n").expect("readme");
    git(&["add", "README.md"]);
    git(&["commit", "-m", "init"]);
    dir
}

/// Business Logic（为什么需要这个测试）:
///     generate_handler! 必须注册三个 workflow document 命令，否则桌面向导 invoke 失败。
///
/// Code Logic（这个测试做什么）:
///     断言命令名常量与 lib.rs 字面量包含 get/validate/save_workflow_document。
#[test]
fn workflow_document_commands_command_registration() {
    assert_eq!(
        crate::commands::orchestrator::__tauri_command_name_get_workflow_document!(),
        "get_workflow_document"
    );
    assert_eq!(
        crate::commands::orchestrator::__tauri_command_name_validate_workflow_document!(),
        "validate_workflow_document"
    );
    assert_eq!(
        crate::commands::orchestrator::__tauri_command_name_save_workflow_document!(),
        "save_workflow_document"
    );
    let lib_src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    for name in [
        "get_workflow_document",
        "validate_workflow_document",
        "save_workflow_document",
    ] {
        assert!(
            lib_src.contains(name),
            "lib.rs generate_handler 必须包含 {name}"
        );
    }
}

/// Business Logic（为什么需要这个测试）:
///     本地 owner helper 必须能从真实项目路径 load/validate/save，且 save 不隐式 dispatch。
///
/// Code Logic（这个测试做什么）:
///     用临时目录构造 local project row，跑 get/validate/save CAS 一轮。
#[test]
fn local_owner_workflow_document_helpers_round_trip_without_dispatch() {
    use crate::orchestrator::workflow::{
        default_workflow_template, WorkflowDocumentStatus, WORKFLOW_DOCUMENT_CHANGED_CODE,
    };
    use crate::workbench::models::WorkbenchProjectRow;

    let dir = tempfile::tempdir().expect("tempdir");
    let project = WorkbenchProjectRow {
        id: "proj-wf".to_string(),
        name: "wf".to_string(),
        kind: "local".to_string(),
        device_id: "dev".to_string(),
        device_name: "dev".to_string(),
        path: dir.path().to_string_lossy().to_string(),
        last_opened_at: "t".to_string(),
        created_at: "t".to_string(),
        updated_at: "t".to_string(),
    };

    let missing = get_local_owner_workflow_document(&project).expect("get missing");
    assert_eq!(missing.status, WorkflowDocumentStatus::Missing);

    let template = default_workflow_template();
    let validated = validate_local_owner_workflow_document(&project, &template).expect("validate");
    assert_eq!(validated.status, WorkflowDocumentStatus::Valid);

    let saved = save_local_owner_workflow_document(&project, "", &template).expect("create save");
    assert_eq!(saved.status, WorkflowDocumentStatus::Valid);
    let hash = saved.content_hash.clone().expect("hash");

    let conflict =
        save_local_owner_workflow_document(&project, "bad", &template).expect_err("hash conflict");
    assert_eq!(conflict.code(), WORKFLOW_DOCUMENT_CHANGED_CODE);

    let updated = template.replace("300000", "120000");
    let saved2 =
        save_local_owner_workflow_document(&project, &hash, &updated).expect("update save");
    assert_eq!(saved2.status, WorkflowDocumentStatus::Valid);
    assert_ne!(saved2.content_hash.as_deref(), Some(hash.as_str()));
}
