// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! lib.rs — Tauri 应用入口：装配共享状态并注册全部 invoke 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     应用启动时需完成一次性的资源初始化（加载配置、连接数据库、建表），
//!     并把共享状态注入命令层。M1 聚焦配置+模型+存储，后续里程碑在此追加网络/同步等装配。
//!
//! Code Logic（这个模块做什么）:
//!     setup 闭包内：load config → 建 SqlitePool（WAL + 手动建表）→ 构造 AppState → manage。
//!     所有命令在 invoke_handler 注册。保留 M0 的 ping。

mod attention;
pub mod backend;
pub mod backup;
mod cc;
mod gui_bootstrap;
mod gui_startup;
// 集成 smoke（tests/lan_trust_boundary_smoke.rs）经 app_lib 调用固定 LAN 边界矩阵。
pub use net::lan_trust_boundary_harness;
// S5 Task6: mixed-version CC history sync harness for integration tests.
pub use cc::mixed_version_harness;
// S5 scale_safety 产品路径 rollback / bulk 测试需要直连 repo 与 row 类型。
pub use cc::models::ClaudeHistoryRow;
pub use storage::ClaudeHistoryRepo;
// S6 quality_faults L2：scratchpad 事务 inject rollback 需直连 repo 与 row 类型。
pub use models::scratchpad::ScratchpadRow;
pub use storage::ScratchpadRepo;
// S6 quality_faults：peer 响应丢失幂等收敛 + 稳定 code 分类需直连 PeerClient。
pub use net::peer_client::{PeerClient, TransferCompletePolicy};
pub use net::peer_error::PeerCallError;
// N5 transfer recovery smoke：claim / capability / operation 对账黑盒验证。
pub use models::transfer::{
    canonical_recovery_payload_hash, canonical_send_payload_hash, LocalTransferOpenTarget,
    TransferDirection, TransferFailure, TransferFailureStage, TransferOpenAction,
    TransferOperationStatus, TransferPhase, TransferRecoveryKind, TransferStatus, TransferTask,
};
pub use net::protocol::{server_protocol_info, CAPABILITY_TRANSFER_RESUME_V1, PROTOCOL_VERSION_V1};
pub use storage::transfer_repo::SenderClaimOutcome;
pub use storage::TransferRepo;
// T3：integration smoke 可直接调用发送端 operation 查询与 lost-ACK 对账。
pub use transfer::sender::{
    get_transfer_operation, operation_status_from_task, reconcile_lost_final_ack,
};
mod claude_cli;
mod claude_code_assets;
pub mod cloud_sync;
mod commands;
pub mod config;
pub mod config_runtime;
pub mod config_store;
pub mod error;
pub mod health;
mod hotkey;
mod mobile;
mod models;
mod net;
pub mod orchestrator;
mod permissions;
mod screenshot;
mod state;
mod storage;
mod sync;
mod transfer;
mod tray;
pub mod updater;
mod workbench;
/// A5：集成 smoke / 外部 crate 测试需要直连浏览器验证服务与 FakeEngine。
pub use workbench::browser_verification;

use std::sync::Arc;

use crate::backend::runtime::{
    build_app_state_with_role, shutdown_backend_runtime, start_background_tasks,
    start_gui_backend_services, BackendRuntimeMode,
};
use crate::backend::ui::{BackendUi, TauriBackendUi};
use crate::commands::{
    attention as attention_cmd, backend as backend_cmd, backup as backup_cmd,
    cc_history as cc_history_cmd, claude_code_assets as claude_code_assets_cmd,
    claude_md as claude_md_cmd, cloud_sync as cloud_sync_cmd, config as config_cmd,
    devices as device_cmd, github_trending as github_trending_cmd,
    gui_bootstrap as gui_bootstrap_cmd, health as health_cmd,
    lan_firewall_dependency as lan_firewall_dependency_cmd, mobile as mobile_cmd,
    orchestrator as orchestrator_cmd, orchestrator_adapters as orchestrator_adapters_cmd, orchestrator_config as orchestrator_config_cmd,
    permissions as permissions_cmd, prompt_optimizer as prompt_optimizer_cmd,
    prompts as prompt_cmd, scratchpad as scratchpad_cmd, screenshot as screenshot_cmd,
    ssh_target as ssh_target_cmd, sync as sync_cmd, transfer as transfer_cmd,
    updater as updater_cmd, workbench as workbench_cmd,
    workbench_dependencies as workbench_dependency_cmd,
};
use crate::gui_startup::{GuiStartupCoordinator, ProductionBackendLifecycle, SetupOutcome};
use crate::state::AppState;
use tauri::Manager;

/// 健康检查命令：验证前端 invoke 与 Rust 后端的 IPC 通路是否打通（M0 脚手架验证用，保留）。
#[tauri::command]
fn ping() -> &'static str {
    "pong"
}

/// 启动 Tauri 桌面应用。
///
/// Business Logic（为什么需要这个函数）:
///     用户打开桌面端时需要初始化 GUI、共享后端状态、P2P/移动端服务、后台任务、健康监测、托盘和快捷键。
///
/// Code Logic（这个函数做什么）:
///     配置 tracing 与 Tauri plugins；setup 中构造 AppState，按 sidecar 验证结果启动 GUI 后端服务并选择后台任务模式；
///     注册全部 invoke 命令，并在 RunEvent::Exit 时统一关闭共享后端运行时。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 初始化 tracing 日志（输出到 stderr），让 tracing::info!/error! 在 axum/mDNS/sync 等模块生效。
    // 优先读 RUST_LOG 环境变量，缺省回退到 "info,mdns_sd=off"。必须在 setup 闭包外、Builder 构造前调用。
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            // mdns_sd=off 过滤库噪音：mdns-sd 0.11 收到针对本机 hostname 的 A/AAAA 查询时，会对每个
            // 接口视图查地址；纯 IPv6 link-local 视图（fe80::）上无 IPv4，库会打 error
            // "Cannot find valid addrs for TYPE_A response"——属日志噪音（A 记录实际走 IPv4 视图正常响应，
            // 不影响 P2P 发现）。mDNS 关键错误已在 discovery.rs 用项目自有 tracing 宏记录，故安全关闭。
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,mdns_sd=off")),
        )
        .try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        // M8 自动更新：updater 负责 check/download/install（签名校验 + 三平台替换），
        // process 提供 restart 能力（rust 侧用 app.request_restart()）
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // M10 健康提醒：notification 供前端弹出久坐提醒（后端仅 emit 事件，通知文案/弹窗走前端），
        // autostart 提供开机自启能力（macOS 用 LaunchAgent；第二参 args 为 None 表示无额外启动参数）。
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // 日志统一由 run() 开头的 tracing_subscriber 接管（tracing 宏 + 经 tracing-log 桥接 log）。
            // 不再注册 tauri-plugin-log：它也会设置全局 log logger，与 tracing_subscriber 冲突，
            // 触发 "attempted to set a logger after the logging system was already initialized" panic。
            // （此 bug 从 M4 引入 tracing init 后潜伏，因 M4-M6 仅 cargo build/test 未跑 dev，直到 M7 后首次 dev 才暴露）

            // 在 tauri 异步运行时上完成共享运行时初始化（load config + db + 建表）。
            // LAN disclosure 未确认前跳过 ensure sidecar 与 start_gui_backend_services。
            let app_handle = app.handle().clone();
            let state = tauri::async_runtime::block_on(async {
                let ui: Arc<dyn BackendUi> = Arc::new(TauriBackendUi::new(app_handle.clone()));
                // GUI 进程仅作 GuiClient：Workbench/runtime mutation 一律代理到 sidecar owner。
                build_app_state_with_role(ui, crate::backend::authority::RuntimeRole::GuiClient)
                    .await
            })?;

            // 注入共享状态供命令层使用
            app.manage(state);

            // 装配启动协调器（ensure/start 闭包在 manage 之后捕获 AppHandle）
            let handle_for_ensure = app.handle().clone();
            #[allow(clippy::type_complexity)]
            let ensure: Arc<
                dyn Fn() -> std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = Result<
                                        crate::backend::control::BackendStatus,
                                        error::AppError,
                                    >,
                                > + Send,
                        >,
                    > + Send
                    + Sync,
            > = Arc::new(move || {
                let handle = handle_for_ensure.clone();
                Box::pin(async move { backend_cmd::ensure_backend_process_for_gui(&handle).await })
            });
            let handle_for_start = app.handle().clone();
            #[allow(clippy::type_complexity)]
            let start: Arc<
                dyn Fn() -> std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<u16, error::AppError>> + Send>,
                    > + Send
                    + Sync,
            > = Arc::new(move || {
                let handle = handle_for_start.clone();
                Box::pin(async move {
                    let state: tauri::State<'_, AppState> = handle.state();
                    start_gui_backend_services(state.inner()).await?;
                    Ok(state
                        .actual_http_port
                        .load(std::sync::atomic::Ordering::SeqCst))
                })
            });
            #[allow(clippy::type_complexity)]
            let probe: Arc<
                dyn Fn() -> std::pin::Pin<
                        Box<
                            dyn std::future::Future<Output = crate::backend::control::BackendStatus>
                                + Send,
                        >,
                    > + Send
                    + Sync,
            > = Arc::new(|| Box::pin(async { crate::backend::control::current_status().await }));
            let lifecycle = ProductionBackendLifecycle::new(ensure, start, probe);
            let coordinator = GuiStartupCoordinator::new(lifecycle);

            let setup_outcome =
                tauri::async_runtime::block_on(coordinator.setup_if_acknowledged())?;
            let lan_acked = matches!(setup_outcome, SetupOutcome::Started(_));
            match &setup_outcome {
                SetupOutcome::Started(result) => {
                    tracing::info!(
                        "GUI 已连接独立后端 sidecar: port={}, reused={}",
                        result.actual_http_port,
                        result.reused_existing
                    );
                }
                SetupOutcome::SkippedUnacknowledged => {
                    tracing::info!(
                        "LAN 风险披露尚未确认：跳过 ensure sidecar 与 GUI backend services"
                    );
                }
            }
            app.manage(coordinator);

            // GUI 已连接独立 sidecar，仅 Headless 模式会启动后端后台任务。
            if lan_acked {
                let state: tauri::State<'_, AppState> = app.state();
                start_background_tasks(state.inner(), BackendRuntimeMode::Gui);
            }

            // N1 Task5：GUI 订阅 sidecar 事件总线（afterSequence + Gap resync）。
            // 未确认 disclosure 时 sidecar 可能尚未 ensure，relay 会等待 control/health 就绪。
            {
                let state: tauri::State<'_, AppState> = app.state();
                let ui = Arc::clone(&state.ui);
                let cancel = tokio_util::sync::CancellationToken::new();
                let cancel_for_task = cancel.clone();
                let _ = cancel;
                tauri::async_runtime::spawn(async move {
                    crate::backend::ui::run_gui_owner_event_relay(ui, cancel_for_task).await;
                });
            }

            // 启动健康监测 daemon（采样线程 + 处理 task），取消令牌存入 AppState 供应用退出时优雅停止。
            // start_health_daemon 内部用 tauri::async_runtime::spawn，同步段调用安全（无需当前线程 reactor）。
            {
                let state: tauri::State<'_, AppState> = app.state();
                let cancel = crate::health::start_health_daemon(
                    app.handle().clone(),
                    Arc::new(state.inner().clone()),
                );
                *state.health_cancel.lock().unwrap() = Some(cancel);
            }

            // M10 健康提醒：按 config.health.enabled 同步开机自启（enabled→注册 LaunchAgent，disabled→移除）。
            // 简单实现：每次启动按 enabled 强同步。tauri_plugin_autostart 用 macOS LaunchAgent，
            // enable/disable 内部幂等（重复调用安全）。失败仅记录不阻断启动。
            {
                use tauri_plugin_autostart::ManagerExt;
                let state: tauri::State<'_, AppState> = app.state();
                let want_autostart = state.config.read().expect("config 读锁中毒").health.enabled;
                let autostart = app.autolaunch();
                if want_autostart {
                    if let Err(e) = autostart.enable() {
                        tracing::warn!("开机自启 enable 失败: {e}");
                    }
                } else if let Err(e) = autostart.disable() {
                    tracing::warn!("开机自启 disable 失败: {e}");
                }
                tracing::info!(
                    "开机自启: {}",
                    if want_autostart {
                        "已启用"
                    } else {
                        "已禁用"
                    }
                );
            }

            // M7：创建系统托盘（图标 + 菜单 + 双击显窗），失败仅记录不阻断启动
            if let Err(e) = tray::build_tray(app.handle()) {
                tracing::error!("系统托盘创建失败: {e}");
            }

            // M7：注册截图全局快捷键（从 config 读 pynput 格式，转换后绑定到 plugin handler）
            {
                let state: tauri::State<'_, AppState> = app.state();
                let hotkey = state
                    .config
                    .read()
                    .expect("config 读锁中毒")
                    .screenshot_hotkey
                    .clone();
                hotkey::register_screenshot_hotkey(
                    app.handle(),
                    &hotkey,
                    hotkey::screenshot_handler,
                );
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ping,
            backend_cmd::get_backend_status,
            backend_cmd::start_backend_process,
            backend_cmd::stop_backend_process,
            backend_cmd::exit_gui,
            backend_cmd::get_runtime_diagnostics,
            backend_cmd::open_backend_log_dir,
            gui_bootstrap_cmd::get_lan_disclosure_status,
            gui_bootstrap_cmd::acknowledge_lan_disclosure_and_start_backend,
            prompt_cmd::list_prompts,
            prompt_cmd::get_prompt,
            prompt_cmd::create_prompt,
            prompt_cmd::update_prompt,
            prompt_cmd::delete_prompt,
            prompt_cmd::list_tags,
            prompt_cmd::list_prompt_versions,
            prompt_cmd::restore_prompt_version,
            config_cmd::get_config,
            config_cmd::get_default_config,
            config_cmd::update_config,
            config_cmd::get_version,
            config_cmd::choose_dir,
            mobile_cmd::get_mobile_access_info,
            attention_cmd::list_attention_items,
            attention_cmd::list_attention_items_v2,
            device_cmd::list_devices,
            device_cmd::get_local_device,
            sync_cmd::trigger_sync,
            claude_md_cmd::get_claude_md,
            claude_md_cmd::update_claude_md,
            claude_md_cmd::push_claude_md,
            scratchpad_cmd::list_scratchpad_pages,
            scratchpad_cmd::get_scratchpad_page,
            scratchpad_cmd::create_scratchpad_page,
            scratchpad_cmd::update_scratchpad_page_content,
            scratchpad_cmd::rename_scratchpad_page,
            scratchpad_cmd::delete_scratchpad_page,
            scratchpad_cmd::sync_scratchpad,
            scratchpad_cmd::list_scratchpad_versions,
            scratchpad_cmd::restore_scratchpad_version,
            transfer_cmd::list_transfers,
            transfer_cmd::send_transfer,
            transfer_cmd::cancel_transfer,
            transfer_cmd::retry_transfer,
            transfer_cmd::resume_transfer,
            transfer_cmd::get_transfer_operation,
            transfer_cmd::prepare_transfer_open,
            screenshot_cmd::start_region_capture,
            screenshot_cmd::get_region_snapshot,
            screenshot_cmd::save_clipboard_image,
            screenshot_cmd::cancel_region_capture,
            permissions_cmd::check_permissions,
            permissions_cmd::request_permission,
            // M8 自动更新（5 命令，返回类型对齐前端 types.ts）
            updater_cmd::check_update,
            updater_cmd::download_update,
            updater_cmd::get_download_status,
            updater_cmd::cancel_download,
            updater_cmd::install_update,
            // Claude Code 历史（5 命令：项目列表 / 项目内 prompt 列表 / 详情 / 手动刷新 / 删除）
            cc_history_cmd::list_cc_projects,
            cc_history_cmd::list_cc_prompts,
            cc_history_cmd::get_cc_prompt,
            cc_history_cmd::refresh_cc_history,
            cc_history_cmd::delete_cc_prompt,
            // Claude Code assets（本机管理 + 局域网选择性拉取）
            claude_code_assets_cmd::list_claude_code_assets,
            claude_code_assets_cmd::set_claude_code_asset_enabled,
            claude_code_assets_cmd::install_claude_code_asset,
            claude_code_assets_cmd::uninstall_claude_code_asset,
            claude_code_assets_cmd::list_remote_claude_code_assets,
            claude_code_assets_cmd::pull_claude_code_assets,
            // SSH 目标（4 命令：列表 / 新增更新 / 删除 / 本机 OS 检测）
            ssh_target_cmd::list_ssh_targets,
            ssh_target_cmd::upsert_ssh_target,
            ssh_target_cmd::delete_ssh_target,
            ssh_target_cmd::get_os_info,
            // 云端同步（GitHub 私有仓库）：配置读写 / 手动触发 / 测试连通
            cloud_sync_cmd::get_cloud_sync_config,
            cloud_sync_cmd::get_default_cloud_sync_config,
            cloud_sync_cmd::update_cloud_sync_config,
            cloud_sync_cmd::trigger_cloud_sync_cmd,
            cloud_sync_cmd::test_cloud_sync,
            // 可验证导出/恢复（N2）：create/inspect/restore/list jobs/list backups/rollback
            backup_cmd::create_backup,
            backup_cmd::inspect_backup,
            backup_cmd::restore_backup,
            backup_cmd::list_recovery_jobs,
            backup_cmd::list_pre_restore_backups,
            backup_cmd::rollback_recovery_job,
            // GitHub Trending 首页（榜单缓存 + Claude CLI 双语解说）
            github_trending_cmd::list_github_trending_repos,
            github_trending_cmd::get_github_trending_config,
            github_trending_cmd::get_default_github_trending_config,
            github_trending_cmd::update_github_trending_config,
            github_trending_cmd::test_claude_cli,
            // Prompt 优化（复用 Claude CLI pure/headless helper，不保存历史）
            prompt_optimizer_cmd::optimize_prompt,
            prompt_optimizer_cmd::complete_orchestrator_task_prompt,
            prompt_optimizer_cmd::stream_optimize_prompt_to_workbench_session,
            // Orchestrator 任务 API（任务列表 / 创建草稿任务 / 入队 / evidence / legacy 配置兼容读取）
            orchestrator_adapters_cmd::list_orchestrator_agent_adapters,
            orchestrator_adapters_cmd::prepare_orchestrator_agent_downgrade,
            orchestrator_cmd::list_orchestrator_tasks,
            orchestrator_cmd::create_orchestrator_task,
            orchestrator_cmd::queue_orchestrator_task,
            orchestrator_cmd::list_orchestrator_task_views,
            orchestrator_cmd::create_orchestrator_task_view,
            orchestrator_cmd::move_orchestrator_task_workflow_state,
            orchestrator_cmd::get_orchestrator_runtime_snapshot,
            orchestrator_cmd::get_operational_notification_snapshot,
            orchestrator_cmd::start_orchestrator_task_view,
            orchestrator_cmd::request_orchestrator_task_rework_view,
            orchestrator_cmd::deliver_reviewed_orchestrator_task_view,
            orchestrator_cmd::cancel_orchestrator_task_view,
            orchestrator_cmd::refresh_orchestrator_project,
            orchestrator_cmd::queue_orchestrator_task_view,
            orchestrator_cmd::retry_orchestrator_task_view,
            orchestrator_cmd::retry_orchestrator_remote_outbox,
            orchestrator_cmd::discard_orchestrator_remote_outbox,
            orchestrator_cmd::abort_orchestrator_task_view,
            orchestrator_cmd::list_orchestrator_task_evidence_for_project,
            orchestrator_cmd::get_orchestrator_review_diff,
            orchestrator_cmd::get_workflow_document,
            orchestrator_cmd::validate_workflow_document,
            orchestrator_cmd::save_workflow_document,
            orchestrator_cmd::get_orchestrator_config_for_project,
            orchestrator_cmd::get_orchestrator_project_config,
            orchestrator_cmd::list_orchestrator_task_evidence,
            orchestrator_cmd::complete_orchestrator_agent_run,
            orchestrator_cmd::retry_orchestrator_task,
            orchestrator_cmd::abort_orchestrator_task,
            orchestrator_cmd::dispatch_orchestrator_once,
            // A4 Automated Candidate Experiments
            orchestrator_cmd::create_orchestrator_experiment,
            orchestrator_cmd::list_orchestrator_experiments,
            orchestrator_cmd::get_orchestrator_experiment,
            orchestrator_cmd::approve_orchestrator_experiment_winner,
            orchestrator_cmd::cancel_orchestrator_experiment,
            orchestrator_cmd::prepare_experiment_downgrade,
            // Orchestrator 全局自动化配置（设备级 AppConfig，不写 legacy 项目配置表）
            orchestrator_config_cmd::get_orchestrator_config,
            orchestrator_config_cmd::get_default_orchestrator_config,
            orchestrator_config_cmd::update_orchestrator_config,
            // M10 健康提醒（18 命令：配置/状态/开关/暂停/贪睡/跳过/配置回写/统计/活动明细/喝水/跳过喝水/延迟喝水/全屏遮罩/恢复默认 + 习惯统计4）
            health_cmd::get_health_config,
            health_cmd::get_default_health_config,
            health_cmd::get_health_status,
            health_cmd::toggle_health_enabled,
            health_cmd::toggle_health_paused,
            health_cmd::snooze_reminder,
            health_cmd::skip_reminder,
            health_cmd::update_health_config,
            health_cmd::get_activity_stats,
            health_cmd::get_activity_detail,
            health_cmd::record_water,
            health_cmd::skip_water_reminder,
            health_cmd::snooze_water_reminder,
            health_cmd::close_health_overlay,
            health_cmd::add_water_manual,
            health_cmd::delete_water_record,
            health_cmd::record_rest_completed,
            health_cmd::get_habit_stats,
            // 工作台（本机项目 + Claude Code PTY 终端 + 项目文件树）
            workbench_cmd::list_workbench_projects,
            workbench_cmd::get_workbench_launch_summary,
            workbench_cmd::add_workbench_project,
            workbench_cmd::list_workbench_remote_roots,
            workbench_cmd::list_workbench_remote_dir,
            workbench_cmd::get_workbench_remote_path_info,
            workbench_cmd::open_workbench_remote_project,
            workbench_cmd::remove_workbench_project,
            workbench_cmd::touch_workbench_project,
            workbench_cmd::discover_workbench_browser_targets,
            workbench_cmd::create_workbench_browser_preview,
            workbench_cmd::start_workbench_browser_verification,
            workbench_cmd::get_workbench_browser_verification,
            workbench_cmd::cancel_workbench_browser_verification,
            workbench_cmd::get_workbench_browser_verification_artifact,
            workbench_cmd::get_workspace_layout,
            workbench_cmd::save_workspace_layout,
            workbench_cmd::list_named_workspace_layouts,
            workbench_cmd::delete_named_workspace_layout,
            workbench_cmd::preflight_workspace_restore_cmd,
            workbench_cmd::apply_workspace_restore_cmd,
            workbench_cmd::list_workbench_worktrees,
            workbench_cmd::create_workbench_worktree,
            workbench_cmd::commit_workbench_worktree,
            workbench_cmd::push_workbench_worktree,
            workbench_cmd::merge_workbench_worktree,
            workbench_cmd::remove_workbench_worktree,
            workbench_cmd::get_workbench_mutation_operation,
            workbench_cmd::list_workbench_git_commits,
            workbench_cmd::get_agent_runtime_snapshot,
            workbench_cmd::get_workbench_lan_fleet,
            workbench_cmd::list_workbench_sessions,
            workbench_cmd::create_workbench_session,
            workbench_cmd::write_workbench_session_input,
            workbench_cmd::resize_workbench_session,
            workbench_cmd::focus_workbench_session,
            workbench_cmd::get_focused_workbench_session,
            workbench_cmd::split_workbench_pane,
            workbench_cmd::switch_workbench_pane,
            workbench_cmd::zoom_workbench_pane,
            workbench_cmd::close_workbench_pane,
            workbench_cmd::close_workbench_session,
            workbench_cmd::rename_workbench_session,
            workbench_cmd::search_claude_sessions,
            workbench_cmd::get_claude_session_preview,
            workbench_cmd::resume_claude_session,
            workbench_cmd::list_workbench_dir,
            workbench_cmd::get_workbench_path_info,
            workbench_cmd::open_workbench_file,
            workbench_cmd::save_workbench_text_file,
            workbench_cmd::format_workbench_structured_content,
            workbench_cmd::preview_workbench_sqlite,
            workbench_cmd::preview_workbench_html_asset,
            workbench_cmd::create_workbench_file,
            workbench_cmd::create_workbench_dir,
            workbench_cmd::rename_workbench_path,
            workbench_cmd::delete_workbench_path,
            // 工作台运行时依赖（tmux 检测 / 安装 / 状态 / 取消）
            workbench_dependency_cmd::check_workbench_dependency,
            workbench_dependency_cmd::install_workbench_dependency,
            workbench_dependency_cmd::get_workbench_dependency_install_status,
            workbench_dependency_cmd::cancel_workbench_dependency_install,
            // 局域网互联防火墙依赖（只读检测监听/IP/端口开放状态，返回平台化放行方法，不自动改防火墙）
            lan_firewall_dependency_cmd::check_lan_firewall_dependency,
        ])
        .build(tauri::generate_context!())
        .map_err(|e| {
            // build 失败通常是资源/配置问题，打印后退出（保留 expect 语义但带上下文）
            eprintln!("Tauri 应用构建失败: {e}");
            e
        })
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // 用 RunEvent::Exit 兜底，确保无论退出路径都走共享后端运行时清理。
            if let tauri::RunEvent::Exit = event {
                let state: tauri::State<'_, AppState> = app_handle.state();
                shutdown_backend_runtime(&state);
                tracing::info!("应用已退出，共享后端运行时已清理");
            }
        });
}
