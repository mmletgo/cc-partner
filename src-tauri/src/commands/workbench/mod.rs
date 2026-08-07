//! commands/workbench 目录模块。
//!
//! Business Logic（为什么需要这个模块）:
//!     保持 workbench_cmd:: 注册名不变。
//!
//! Code Logic（这个模块做什么）:
//!     子模块 + 显式 pub / pub(crate) use 再导出 tauri 命令（含 generate_handler 隐藏符号）与 crate 内 helper。

mod agent_ledger;
mod agent_runtime;
mod browser;
mod browser_verification;
mod common;
mod files;
mod fleet;
mod git;
mod layout;
mod projects;
mod sessions;

#[cfg(test)]
mod tests;

// ---- tauri 命令（pub，含 __cmd__/__tauri_command_name_ 供 generate_handler）----

pub use agent_ledger::{
    __cmd__clear_agent_ledger, __cmd__list_agent_ledger, __cmd__summarize_agent_ledger,
    __tauri_command_name_clear_agent_ledger, __tauri_command_name_list_agent_ledger,
    __tauri_command_name_summarize_agent_ledger, clear_agent_ledger, list_agent_ledger,
    summarize_agent_ledger,
};

pub use agent_runtime::{
    __cmd__get_agent_runtime_snapshot, __tauri_command_name_get_agent_runtime_snapshot,
    get_agent_runtime_snapshot,
};

pub use fleet::{
    __cmd__get_workbench_lan_fleet, __tauri_command_name_get_workbench_lan_fleet,
    get_workbench_lan_fleet, get_workbench_lan_fleet_for_state,
};

pub(crate) use agent_ledger::{
    clear_agent_ledger_for_state, list_agent_ledger_for_state, summarize_agent_ledger_for_state,
    ListAgentLedgerReq, SummarizeAgentLedgerReq,
};

pub use browser::{
    __cmd__create_workbench_browser_preview, __cmd__discover_workbench_browser_targets,
    __tauri_command_name_create_workbench_browser_preview,
    __tauri_command_name_discover_workbench_browser_targets, create_workbench_browser_preview,
    discover_workbench_browser_targets,
};

pub use browser_verification::{
    __cmd__cancel_workbench_browser_verification, __cmd__get_workbench_browser_verification,
    __cmd__get_workbench_browser_verification_artifact,
    __cmd__start_workbench_browser_verification,
    __tauri_command_name_cancel_workbench_browser_verification,
    __tauri_command_name_get_workbench_browser_verification,
    __tauri_command_name_get_workbench_browser_verification_artifact,
    __tauri_command_name_start_workbench_browser_verification,
    cancel_workbench_browser_verification, get_workbench_browser_verification,
    get_workbench_browser_verification_artifact, start_workbench_browser_verification,
    BrowserVerificationArtifactDto,
};

pub use files::{
    __cmd__format_workbench_structured_content, __cmd__open_workbench_file,
    __cmd__preview_workbench_html_asset, __cmd__preview_workbench_sqlite,
    __cmd__save_workbench_text_file, __tauri_command_name_format_workbench_structured_content,
    __tauri_command_name_open_workbench_file, __tauri_command_name_preview_workbench_html_asset,
    __tauri_command_name_preview_workbench_sqlite, __tauri_command_name_save_workbench_text_file,
    format_workbench_structured_content, open_workbench_file, preview_workbench_html_asset,
    preview_workbench_sqlite, save_workbench_text_file,
};

pub use git::{
    __cmd__commit_workbench_worktree, __cmd__create_workbench_worktree,
    __cmd__get_workbench_mutation_operation, __cmd__list_workbench_git_commits,
    __cmd__list_workbench_worktrees, __cmd__merge_workbench_worktree,
    __cmd__push_workbench_worktree, __cmd__remove_workbench_worktree,
    __cmd__repair_worktree_hook_failure, __tauri_command_name_commit_workbench_worktree,
    __tauri_command_name_create_workbench_worktree,
    __tauri_command_name_get_workbench_mutation_operation,
    __tauri_command_name_list_workbench_git_commits, __tauri_command_name_list_workbench_worktrees,
    __tauri_command_name_merge_workbench_worktree, __tauri_command_name_push_workbench_worktree,
    __tauri_command_name_remove_workbench_worktree,
    __tauri_command_name_repair_worktree_hook_failure, commit_workbench_worktree,
    create_workbench_worktree, get_workbench_mutation_operation, list_workbench_git_commits,
    list_workbench_worktrees, merge_workbench_worktree, push_workbench_worktree,
    remove_workbench_worktree, repair_worktree_hook_failure,
};

pub use projects::{
    __cmd__add_workbench_project, __cmd__get_workbench_launch_summary,
    __cmd__get_workbench_remote_path_info, __cmd__list_workbench_projects,
    __cmd__list_workbench_remote_dir, __cmd__list_workbench_remote_roots,
    __cmd__open_workbench_remote_project, __cmd__remove_workbench_project,
    __cmd__reorder_workbench_projects, __cmd__touch_workbench_project,
    __tauri_command_name_add_workbench_project, __tauri_command_name_get_workbench_launch_summary,
    __tauri_command_name_get_workbench_remote_path_info,
    __tauri_command_name_list_workbench_projects, __tauri_command_name_list_workbench_remote_dir,
    __tauri_command_name_list_workbench_remote_roots,
    __tauri_command_name_open_workbench_remote_project,
    __tauri_command_name_remove_workbench_project, __tauri_command_name_reorder_workbench_projects,
    __tauri_command_name_touch_workbench_project, add_workbench_project,
    get_workbench_launch_summary, get_workbench_remote_path_info, list_workbench_projects,
    list_workbench_remote_dir, list_workbench_remote_roots, open_workbench_remote_project,
    remove_workbench_project, reorder_workbench_projects, touch_workbench_project,
};
// control_workbench 经 crate::commands::workbench:: 路径访问 pub(crate) helper
pub(crate) use projects::reorder_workbench_projects_for_state;

pub use sessions::{
    __cmd__close_workbench_pane, __cmd__close_workbench_session, __cmd__create_workbench_dir,
    __cmd__create_workbench_file, __cmd__create_workbench_session, __cmd__delete_workbench_path,
    __cmd__enqueue_workbench_terminal_input, __cmd__focus_workbench_session,
    __cmd__get_claude_session_preview, __cmd__get_focused_workbench_session,
    __cmd__get_workbench_path_info, __cmd__list_workbench_dir, __cmd__list_workbench_sessions,
    __cmd__rename_workbench_path, __cmd__rename_workbench_session, __cmd__replay_workbench_session,
    __cmd__resize_workbench_session, __cmd__resume_claude_session, __cmd__search_claude_sessions,
    __cmd__select_workbench_pane_at, __cmd__split_workbench_pane, __cmd__switch_workbench_pane,
    __cmd__write_workbench_session_input, __cmd__zoom_workbench_pane,
    __tauri_command_name_close_workbench_pane, __tauri_command_name_close_workbench_session,
    __tauri_command_name_create_workbench_dir, __tauri_command_name_create_workbench_file,
    __tauri_command_name_create_workbench_session, __tauri_command_name_delete_workbench_path,
    __tauri_command_name_enqueue_workbench_terminal_input,
    __tauri_command_name_focus_workbench_session, __tauri_command_name_get_claude_session_preview,
    __tauri_command_name_get_focused_workbench_session,
    __tauri_command_name_get_workbench_path_info, __tauri_command_name_list_workbench_dir,
    __tauri_command_name_list_workbench_sessions, __tauri_command_name_rename_workbench_path,
    __tauri_command_name_rename_workbench_session, __tauri_command_name_replay_workbench_session,
    __tauri_command_name_resize_workbench_session, __tauri_command_name_resume_claude_session,
    __tauri_command_name_search_claude_sessions, __tauri_command_name_select_workbench_pane_at,
    __tauri_command_name_split_workbench_pane, __tauri_command_name_switch_workbench_pane,
    __tauri_command_name_write_workbench_session_input, __tauri_command_name_zoom_workbench_pane,
    close_workbench_pane, close_workbench_session, create_workbench_dir, create_workbench_file,
    create_workbench_session, delete_workbench_path, enqueue_workbench_terminal_input,
    focus_workbench_session, get_claude_session_preview, get_focused_workbench_session,
    get_workbench_path_info, list_workbench_dir, list_workbench_sessions, rename_workbench_path,
    rename_workbench_session, replay_workbench_session, resize_workbench_session,
    resume_claude_session, search_claude_sessions, select_workbench_pane_at, split_workbench_pane,
    switch_workbench_pane, write_workbench_session_input, zoom_workbench_pane,
};

pub use projects::add_local_workbench_project_from_path;

pub use layout::{
    __cmd__apply_workspace_restore_cmd, __cmd__delete_named_workspace_layout,
    __cmd__get_workspace_layout, __cmd__list_named_workspace_layouts,
    __cmd__preflight_workspace_restore_cmd, __cmd__save_workspace_layout,
    __tauri_command_name_apply_workspace_restore_cmd,
    __tauri_command_name_delete_named_workspace_layout, __tauri_command_name_get_workspace_layout,
    __tauri_command_name_list_named_workspace_layouts,
    __tauri_command_name_preflight_workspace_restore_cmd,
    __tauri_command_name_save_workspace_layout, apply_workspace_restore_cmd,
    delete_named_workspace_layout, get_workspace_layout, list_named_workspace_layouts,
    preflight_workspace_restore_cmd, save_workspace_layout,
};

// ---- crate 内 helper / DTO（pub(crate)）----

pub(crate) use browser::create_workbench_browser_preview_for_state;
pub(crate) use browser_verification::{
    cancel_browser_verification_for_state, get_browser_verification_artifact_for_state,
    get_browser_verification_for_state, start_browser_verification_for_state,
    StartBrowserVerificationReq,
};

pub(crate) use layout::{
    apply_workspace_restore_for_state, delete_named_workspace_layout_for_state,
    get_workspace_layout_for_state, list_named_workspace_layouts_for_state,
    owner_local_preflight_for_state, owner_local_safe_attach_for_state,
    preflight_workspace_restore_for_state, save_workspace_layout_for_state,
};

// control_workbench 与其它 crate 内 owner 路径需要 common 中的远程映射/设备 helper。
pub(crate) use common::{
    build_remote_project_shortcut_row, device_base_url, device_name_from_state,
    ensure_main_worktree, ensure_remote_event_bridge_for_project_mapping,
    ensure_remote_project_context, get_project, local_project_id_for_remote_inner_project,
    map_remote_session_dtos_with_project, now_iso, remote_inner_session_id,
    remote_inner_worktree_id, remove_local_workbench_project_with_barrier, WorkbenchMergeResultDto,
};

pub(crate) use files::{
    list_workbench_sessions_for_state, local_list_workbench_sessions,
    local_preview_workbench_html_asset, local_preview_workbench_sqlite,
    local_save_workbench_text_file, save_workbench_text_file_for_state,
};

pub(crate) use git::{
    commit_workbench_worktree_for_state, create_workbench_worktree_for_state,
    get_workbench_mutation_operation_for_state, list_workbench_git_commits_for_state,
    local_commit_workbench_worktree, local_commit_workbench_worktree_with_ledger,
    local_create_workbench_worktree, local_list_workbench_git_commits,
    local_merge_workbench_worktree, local_merge_workbench_worktree_with_ledger,
    local_open_workbench_file, local_push_workbench_worktree,
    local_push_workbench_worktree_with_ledger, local_remove_workbench_worktree,
    local_remove_workbench_worktree_with_ledger, merge_workbench_worktree_for_state,
    open_workbench_file_for_state, push_workbench_worktree_for_state,
    remove_workbench_worktree_for_state, repair_worktree_hook_failure_for_state,
};

pub(crate) use projects::{
    discover_workbench_browser_targets_for_state, list_workbench_worktrees_for_state,
    local_get_workbench_worktree, local_list_workbench_worktrees,
};

pub(crate) use sessions::{
    close_workbench_pane_for_state, close_workbench_session_for_state,
    create_workbench_session_for_state, deactivate_workbench_terminal_stream_for_state,
    focus_workbench_session_for_state, get_claude_session_preview_for_state,
    get_focused_workbench_session_for_state, get_workbench_path_info_for_state,
    list_workbench_dir_for_state, local_close_workbench_pane, local_close_workbench_session,
    local_create_workbench_dir, local_create_workbench_file, local_create_workbench_session,
    local_create_workbench_session_with_preallocated_ids, local_delete_workbench_path,
    local_focus_workbench_session, local_get_workbench_path_info, local_list_workbench_dir,
    local_rename_workbench_path, local_rename_workbench_session, local_resize_workbench_session,
    local_select_workbench_pane_at, local_split_workbench_pane, local_switch_workbench_pane,
    local_write_workbench_session_input, local_zoom_workbench_pane,
    replay_workbench_session_for_state, resize_workbench_session_for_state,
    resume_claude_session_for_state, search_claude_sessions_for_state,
    select_workbench_pane_at_for_state, split_workbench_pane_for_state,
    switch_workbench_pane_for_state, write_workbench_session_input_for_state,
    zoom_workbench_pane_for_state,
};
