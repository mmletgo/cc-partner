//! commands/orchestrator 目录模块。
//!
//! Business Logic（为什么需要这个模块）:
//!     保持 orchestrator_cmd:: 注册名不变。
//!
//! Code Logic（这个模块做什么）:
//!     子模块 + 显式 re-export：命令入口（含 tauri 隐藏宏）、pub DTO 与 pub(crate) helper。

mod actions;
mod common;
mod evidence;
mod experiments;
mod remote;
mod runtime;
mod tasks;

#[cfg(test)]
mod tests;

// Tauri command 入口（含 generate_handler 依赖的 __cmd__/__tauri_command_name_ 宏）
pub use actions::{
    __cmd__abort_orchestrator_task, __cmd__complete_orchestrator_agent_run,
    __cmd__dispatch_orchestrator_once, __cmd__queue_orchestrator_task,
    __cmd__retry_orchestrator_task, __tauri_command_name_abort_orchestrator_task,
    __tauri_command_name_complete_orchestrator_agent_run,
    __tauri_command_name_dispatch_orchestrator_once, __tauri_command_name_queue_orchestrator_task,
    __tauri_command_name_retry_orchestrator_task, abort_orchestrator_task,
    complete_orchestrator_agent_run, dispatch_orchestrator_once, queue_orchestrator_task,
    retry_orchestrator_task,
};
pub use evidence::{
    __cmd__get_orchestrator_config_for_project, __cmd__get_orchestrator_project_config,
    __cmd__list_orchestrator_task_evidence, __cmd__list_orchestrator_task_evidence_for_project,
    __tauri_command_name_get_orchestrator_config_for_project,
    __tauri_command_name_get_orchestrator_project_config,
    __tauri_command_name_list_orchestrator_task_evidence,
    __tauri_command_name_list_orchestrator_task_evidence_for_project,
    get_orchestrator_config_for_project, get_orchestrator_project_config,
    list_orchestrator_task_evidence, list_orchestrator_task_evidence_for_project,
};
pub use experiments::{
    __cmd__approve_orchestrator_experiment_winner, __cmd__cancel_orchestrator_experiment,
    __cmd__create_orchestrator_experiment, __cmd__get_orchestrator_experiment,
    __cmd__list_orchestrator_experiments, __cmd__prepare_experiment_downgrade,
    __tauri_command_name_approve_orchestrator_experiment_winner,
    __tauri_command_name_cancel_orchestrator_experiment,
    __tauri_command_name_create_orchestrator_experiment,
    __tauri_command_name_get_orchestrator_experiment,
    __tauri_command_name_list_orchestrator_experiments,
    __tauri_command_name_prepare_experiment_downgrade, approve_orchestrator_experiment_winner,
    approve_orchestrator_experiment_winner_for_state, cancel_orchestrator_experiment,
    cancel_orchestrator_experiment_for_state, create_local_orchestrator_experiment,
    create_orchestrator_experiment, create_orchestrator_experiment_for_state,
    get_orchestrator_experiment, get_orchestrator_experiment_for_state,
    list_orchestrator_experiments, list_orchestrator_experiments_for_state,
    prepare_experiment_downgrade,
};
pub use remote::{
    __cmd__discard_orchestrator_remote_outbox, __cmd__retry_orchestrator_remote_outbox,
    __tauri_command_name_discard_orchestrator_remote_outbox,
    __tauri_command_name_retry_orchestrator_remote_outbox, discard_orchestrator_remote_outbox,
    retry_orchestrator_remote_outbox,
};
pub use runtime::{
    __cmd__abort_orchestrator_task_view, __cmd__get_operational_notification_snapshot,
    __cmd__get_orchestrator_runtime_snapshot, __cmd__queue_orchestrator_task_view,
    __cmd__retry_orchestrator_task_view, __tauri_command_name_abort_orchestrator_task_view,
    __tauri_command_name_get_operational_notification_snapshot,
    __tauri_command_name_get_orchestrator_runtime_snapshot,
    __tauri_command_name_queue_orchestrator_task_view,
    __tauri_command_name_retry_orchestrator_task_view, abort_orchestrator_task_view,
    get_operational_notification_snapshot, get_orchestrator_runtime_snapshot,
    queue_orchestrator_task_view, retry_orchestrator_task_view,
};
pub use tasks::{
    __cmd__cancel_orchestrator_task_view, __cmd__create_orchestrator_task,
    __cmd__create_orchestrator_task_view, __cmd__deliver_reviewed_orchestrator_task_view,
    __cmd__get_workflow_document, __cmd__list_orchestrator_task_views,
    __cmd__list_orchestrator_tasks, __cmd__move_orchestrator_task_workflow_state,
    __cmd__refresh_orchestrator_project, __cmd__request_orchestrator_task_rework_view,
    __cmd__save_workflow_document, __cmd__start_orchestrator_task_view,
    __cmd__validate_workflow_document, __tauri_command_name_cancel_orchestrator_task_view,
    __tauri_command_name_create_orchestrator_task,
    __tauri_command_name_create_orchestrator_task_view,
    __tauri_command_name_deliver_reviewed_orchestrator_task_view,
    __tauri_command_name_get_workflow_document, __tauri_command_name_list_orchestrator_task_views,
    __tauri_command_name_list_orchestrator_tasks,
    __tauri_command_name_move_orchestrator_task_workflow_state,
    __tauri_command_name_refresh_orchestrator_project,
    __tauri_command_name_request_orchestrator_task_rework_view,
    __tauri_command_name_save_workflow_document, __tauri_command_name_start_orchestrator_task_view,
    __tauri_command_name_validate_workflow_document, cancel_orchestrator_task_view,
    create_orchestrator_task, create_orchestrator_task_view,
    deliver_reviewed_orchestrator_task_view, get_workflow_document, list_orchestrator_task_views,
    list_orchestrator_tasks, move_orchestrator_task_workflow_state, refresh_orchestrator_project,
    request_orchestrator_task_rework_view, save_workflow_document, start_orchestrator_task_view,
    validate_workflow_document,
};

// 外部 crate 内模块（routes / completion / remote_client / tests）需要的 helper
pub(crate) use actions::{
    complete_orchestrator_agent_run_for_attempt, discard_orchestrator_remote_outbox_for_repos,
    retry_orchestrator_remote_outbox_for_repos,
};
pub(crate) use common::{
    build_orchestrator_task_row, dispatch_orchestrator_best_effort,
    ensure_reviewed_delivery_allowed, get_orchestrator_runtime_snapshot_for_project,
    run_delivery_for_task,
};
pub use common::{
    CreateOrchestratorTaskRequest, OrchestratorRuntimeSnapshotDto, OrchestratorTaskViewDto,
};
pub(crate) use runtime::create_orchestrator_task_view_for_http_with_request_id;
pub(crate) use tasks::{
    deliver_reviewed_orchestrator_task_view_for_state, get_local_owner_workflow_document,
    get_orchestrator_runtime_snapshot_for_state_with_request_id, get_workflow_document_for_state,
    list_orchestrator_task_views_for_state, list_orchestrator_task_views_for_state_with_request_id,
    save_local_owner_workflow_document, save_workflow_document_for_state,
    validate_local_owner_workflow_document, validate_workflow_document_for_state,
};
