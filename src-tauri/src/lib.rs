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

mod cc;
mod claude_cli;
mod claude_code_assets;
mod cloud_sync;
mod commands;
mod config;
mod error;
mod health;
mod hotkey;
mod models;
mod net;
mod permissions;
mod screenshot;
mod state;
mod storage;
mod sync;
mod transfer;
mod tray;
mod workbench;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::atomic::AtomicU16;
use std::sync::{Arc, Mutex, RwLock};

use crate::commands::{
    cc_history as cc_history_cmd, claude_code_assets as claude_code_assets_cmd,
    claude_md as claude_md_cmd, cloud_sync as cloud_sync_cmd, config as config_cmd,
    devices as device_cmd, github_trending as github_trending_cmd, health as health_cmd,
    permissions as permissions_cmd, prompt_optimizer as prompt_optimizer_cmd,
    prompts as prompt_cmd, scratchpad as scratchpad_cmd, screenshot as screenshot_cmd,
    ssh_target as ssh_target_cmd, sync as sync_cmd, transfer as transfer_cmd,
    updater as updater_cmd, workbench as workbench_cmd,
    workbench_dependencies as workbench_dependency_cmd,
};
use crate::net::{discovery, http_server, peer_client::PeerClient};
use crate::state::AppState;
use crate::storage::{
    ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, ScratchpadRepo, SshTargetRepo, TransferRepo,
    WorkbenchProjectRepo, WorkbenchSessionRepo, WorkbenchWorktreeRepo,
};
use crate::transfer::registry::TransferRegistry;
use tauri::Manager;

/// 健康检查命令：验证前端 invoke 与 Rust 后端的 IPC 通路是否打通（M0 脚手架验证用，保留）。
#[tauri::command]
fn ping() -> &'static str {
    "pong"
}

/// 建表 SQL（对照 migrations/0001_init.sql，全 CREATE TABLE IF NOT EXISTS）。
///
/// Business Logic: 不用 sqlx::migrate!（它对"表已存在但无 _sqlx_migrations 表"的旧库有坑），
///     手动逐条执行 CREATE TABLE IF NOT EXISTS，对旧库是无操作，保用户数据。
const PROMPTS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS prompts (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    tags TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    device_id TEXT NOT NULL,
    vector_clock TEXT NOT NULL,
    deleted INTEGER DEFAULT 0
)";

const TRANSFER_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS transfer_history (
    id TEXT PRIMARY KEY,
    filename TEXT NOT NULL,
    file_path TEXT NOT NULL,
    size INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    direction TEXT NOT NULL,
    peer_device_id TEXT NOT NULL,
    status TEXT NOT NULL,
    transferred_bytes INTEGER DEFAULT 0,
    created_at TEXT NOT NULL,
    completed_at TEXT
)";

/// Claude Code 历史 prompt 表（采集入库 + 跨设备同步）。
///
/// Business Logic: 存储从 ~/.claude/projects jsonl 采集的用户输入 prompt，按 project_path 归类。
///     vector_clock 采集恒 {device_id:1}，仅 delete_cc_prompt 软删除时递增；deleted 软删除传播。
const CC_HISTORY_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS claude_history (
    id TEXT PRIMARY KEY,
    project_path TEXT NOT NULL,
    project_name TEXT NOT NULL,
    session_id TEXT NOT NULL,
    content TEXT NOT NULL,
    git_branch TEXT,
    cc_version TEXT,
    occurred_at TEXT NOT NULL,
    device_id TEXT NOT NULL,
    vector_clock TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted INTEGER DEFAULT 0
)";

/// CC 历史采集扫描状态表（增量去重：记录每个 jsonl 文件的 mtime/size，未变则跳过）。
const CC_SCAN_STATE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS claude_history_scan_state (
    file_path TEXT PRIMARY KEY,
    mtime_sec INTEGER NOT NULL,
    size INTEGER NOT NULL,
    scanned_at TEXT NOT NULL
)";

/// CC 历史表索引（项目路径+时间倒序查询、设备_id 查询加速）。
const CC_INDEXES: &str =
    "CREATE INDEX IF NOT EXISTS idx_ch_proj ON claude_history(project_path, occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_ch_dev ON claude_history(device_id)";

/// user 级 CLAUDE.md 单例表（全表仅一行，id 恒为 "claude_md"）。
const CLAUDE_MD_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS claude_md (
    id TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    device_id TEXT NOT NULL,
    vector_clock TEXT NOT NULL
)";

/// SSH 连接目标表（每 host 一行：用户名/端口/向量时钟，跨设备同步）。
const SSH_TARGET_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ssh_targets (
    host TEXT PRIMARY KEY,
    port INTEGER NOT NULL DEFAULT 22,
    username TEXT NOT NULL,
    label TEXT,
    device_id TEXT NOT NULL,
    vector_clock TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted INTEGER DEFAULT 0
)";

/// 速记本页面表（旧默认页 id 恒为 "scratchpad"，新页用 UUID）。
const SCRATCHPAD_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS scratchpad (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '速记本',
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    device_id TEXT NOT NULL,
    vector_clock TEXT NOT NULL,
    deleted INTEGER DEFAULT 0
)";

/// 健康提醒 - 每分钟活动采样表（分钟级 unix 时间戳为主键，同分钟重采覆盖）。
const HEALTH_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS activity_records (
    ts INTEGER PRIMARY KEY,
    is_active INTEGER NOT NULL,
    process_name TEXT,
    window_title TEXT
)";

/// 健康提醒 - 喝水打卡表（以时间戳为主键，INSERT OR REPLACE 幂等）。
const WATER_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS water_records (
    ts INTEGER PRIMARY KEY
)";

/// GitHub Trending 首页缓存表（榜单 + Claude CLI 中英文解说）。
///
/// Business Logic: 首页每天只抓取一次 GitHub Trending Weekly，并把 Claude CLI 生成结果持久化，
///     避免重复网络请求和重复 AI 消耗。payload 为完整前端响应主体（不含 fromCache/stale）。
const GITHUB_TRENDING_CACHE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS github_trending_cache (
    key TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    ai_status TEXT NOT NULL,
    ai_error TEXT
)";

/// 工作台本机项目表（最近项目列表持久化）。
///
/// Business Logic（为什么需要这个常量）:
///     用户添加到工作台的本机项目需要在应用重启后保留，并按最近打开时间排序。
///
/// Code Logic（这个常量做什么）:
///     定义 workbench_projects 表结构；项目内容仍在磁盘目录中，本表只保存项目元数据。
const WORKBENCH_PROJECT_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS workbench_projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    device_id TEXT NOT NULL,
    device_name TEXT NOT NULL,
    path TEXT NOT NULL,
    last_opened_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
)";

/// 工作台 Git worktree 表（项目下多个工作区的持久化元数据）。
///
/// Business Logic（为什么需要这个常量）:
///     用户在 Workbench 中创建的 worktree 需要在应用重启后继续展示，并和终端 window 关联。
///
/// Code Logic（这个常量做什么）:
///     定义 workbench_worktrees 表结构；Git 状态不落库，命令层动态读取。
const WORKBENCH_WORKTREE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS workbench_worktrees (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    branch TEXT,
    base_branch TEXT,
    path TEXT NOT NULL,
    is_main INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
)";

/// 工作台终端会话表（终端 tab 元数据持久化，PTY/tmux attach 运行期重建）。
///
/// Business Logic（为什么需要这个常量）:
///     用户希望重启 cc-partner 后之前打开的终端仍出现在工作台，并在可重连后端可用时继续原上下文。
///
/// Code Logic（这个常量做什么）:
///     定义 workbench_sessions 表结构；backend/backend_id 保存 tmux 等重连后端信息，关闭 tab 时删除记录。
const WORKBENCH_SESSION_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS workbench_sessions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    worktree_id TEXT,
    name TEXT NOT NULL,
    command TEXT NOT NULL,
    cwd TEXT,
    status TEXT NOT NULL,
    cols INTEGER NOT NULL,
    rows INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    exited_at TEXT,
    exit_code INTEGER,
    backend TEXT NOT NULL,
    backend_id TEXT,
    backend_window_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
)";
/// 初始化数据库连接池：开启 WAL，手动建表，返回 SqlitePool。
///
/// Business Logic: 单连接语义与 Python aiosqlite 一致（max_connections(1)）。
/// Code Logic: 用 ConnectOptions 开启 create_if_missing 与 WAL pragma；逐条执行建表 SQL。
async fn init_db(db_path: &str) -> Result<sqlx::SqlitePool, error::AppError> {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path))?
        .create_if_missing(true)
        // 开启 WAL 模式（对照 Python PRAGMA journal_mode=WAL）
        .pragma("journal_mode", "WAL");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    // 手动建表（不用 migrate! 宏，规避旧库无 _sqlx_migrations 表的坑）
    sqlx::query(PROMPTS_SCHEMA).execute(&pool).await?;
    sqlx::query(TRANSFER_SCHEMA).execute(&pool).await?;
    // Claude Code 历史表 + 扫描状态表（在 TRANSFER_SCHEMA 之后执行）
    sqlx::query(CC_HISTORY_SCHEMA).execute(&pool).await?;
    sqlx::query(CC_SCAN_STATE_SCHEMA).execute(&pool).await?;
    // CC 索引：CC_INDEXES 含多条语句，sqlx 默认不开启多语句，按 ';' 拆分逐条执行
    for stmt in CC_INDEXES.split(';') {
        let s = stmt.trim();
        if s.is_empty() {
            continue;
        }
        sqlx::query(s).execute(&pool).await?;
    }
    // user 级 CLAUDE.md 单例表
    sqlx::query(CLAUDE_MD_SCHEMA).execute(&pool).await?;
    // SSH 连接目标表（host 主键 + 端口 + 用户名 + 向量时钟，跨设备同步）
    sqlx::query(SSH_TARGET_SCHEMA).execute(&pool).await?;
    // 速记本页面表（旧库缺 title 时补列，保证旧单例内容迁移为“速记本”页）
    sqlx::query(SCRATCHPAD_SCHEMA).execute(&pool).await?;
    ScratchpadRepo::new(pool.clone()).ensure_schema().await?;
    // 健康提醒：活动采样表 + 喝水记录表（在 CLAUDE_MD_SCHEMA 之后执行）
    sqlx::query(HEALTH_SCHEMA).execute(&pool).await?;
    sqlx::query(WATER_SCHEMA).execute(&pool).await?;
    // GitHub Trending 首页缓存（榜单 + Claude CLI 解说），独立于同步数据。
    sqlx::query(GITHUB_TRENDING_CACHE_SCHEMA)
        .execute(&pool)
        .await?;
    // 工作台最近项目列表 + 终端会话元数据（PTY 句柄运行期重建）
    sqlx::query(WORKBENCH_PROJECT_SCHEMA).execute(&pool).await?;
    sqlx::query(WORKBENCH_WORKTREE_SCHEMA)
        .execute(&pool)
        .await?;
    sqlx::query(WORKBENCH_SESSION_SCHEMA).execute(&pool).await?;
    WorkbenchWorktreeRepo::new(pool.clone())
        .ensure_schema()
        .await?;
    WorkbenchSessionRepo::new(pool.clone())
        .ensure_schema()
        .await?;
    Ok(pool)
}

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
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // 日志统一由 run() 开头的 tracing_subscriber 接管（tracing 宏 + 经 tracing-log 桥接 log）。
            // 不再注册 tauri-plugin-log：它也会设置全局 log logger，与 tracing_subscriber 冲突，
            // 触发 "attempted to set a logger after the logging system was already initialized" panic。
            // （此 bug 从 M4 引入 tracing init 后潜伏，因 M4-M6 仅 cargo build/test 未跑 dev，直到 M7 后首次 dev 才暴露）

            // 在 tauri 异步运行时上完成资源初始化（load config + db + 建表 + axum + mDNS）
            let app_handle = app.handle().clone();
            let state = tauri::async_runtime::block_on(async {
                // 1) 加载配置（不存在则生成默认并持久化）
                let config = config::AppConfig::load()?;
                // 2) 初始化数据库
                let pool = init_db(&config.db_path).await?;
                // 3) 构造仓库与共享状态（先构造完整 AppState，再启动 axum/mDNS 共享同一份 Arc）
                let device_id = config.device_id.clone();
                let prompt_repo = Arc::new(PromptRepo::new(pool.clone()));
                let transfer_repo = Arc::new(TransferRepo::new(pool.clone()));
                let cc_history_repo = Arc::new(ClaudeHistoryRepo::new(pool.clone()));
                let claude_md_repo = Arc::new(ClaudeMdRepo::new(pool.clone()));
                let ssh_target_repo = Arc::new(SshTargetRepo::new(pool.clone()));
                let scratchpad_repo = Arc::new(ScratchpadRepo::new(pool.clone()));
                let workbench_project_repo = Arc::new(WorkbenchProjectRepo::new(pool.clone()));
                let workbench_session_repo = Arc::new(WorkbenchSessionRepo::new(pool.clone()));
                let workbench_worktree_repo = Arc::new(WorkbenchWorktreeRepo::new(pool.clone()));
                let workbench_sessions =
                    Arc::new(crate::workbench::sessions::WorkbenchSessionRegistry::new());
                let workbench_dependency = Arc::new(
                    crate::workbench::dependencies::WorkbenchDependencyInstallRuntime::new(),
                );
                // 健康提醒：仓库（共享 pool）+ 运行时（状态机/贪睡/暂停）+ daemon 取消令牌占位
                let health_repo =
                    Arc::new(crate::storage::health_repo::HealthRepo::new(pool.clone()));
                let health = Arc::new(crate::health::HealthRuntime::new());
                let health_cancel =
                    Arc::new(Mutex::new(None::<tokio_util::sync::CancellationToken>));
                let state = AppState {
                    config: Arc::new(RwLock::new(config)),
                    db: pool,
                    prompt_repo,
                    transfer_repo,
                    claude_md_repo,
                    scratchpad_repo,
                    ssh_target_repo,
                    device_id: Arc::new(device_id),
                    devices: Arc::new(RwLock::new(std::collections::HashMap::new())),
                    actual_http_port: Arc::new(AtomicU16::new(0)),
                    discovery: Arc::new(Mutex::new(None)),
                    peer_client: Arc::new(PeerClient::new()),
                    transfers: Arc::new(TransferRegistry::new()),
                    app_handle: app_handle.clone(),
                    // M8 更新器状态：下载状态机 + 缓存的 Update + 下载字节 + 任务句柄 + 取消令牌
                    update_status: Arc::new(RwLock::new(
                        crate::commands::updater::UpdateDownloadStatus::default(),
                    )),
                    update_pending: Arc::new(Mutex::new(None)),
                    update_bytes: Arc::new(Mutex::new(None)),
                    update_download_task: Arc::new(Mutex::new(None)),
                    update_cancel_token: Arc::new(Mutex::new(None)),
                    // Claude Code 历史：仓库 + 采集器取消令牌（start 在 manage 之后调用）
                    cc_history_repo,
                    workbench_project_repo,
                    workbench_session_repo,
                    workbench_worktree_repo,
                    workbench_sessions,
                    workbench_dependency,
                    cc_collector_cancel: Arc::new(Mutex::new(None)),
                    // 云端同步：后台 scheduler 取消令牌（start 在 manage 之后调用）
                    cloud_sync_cancel: Arc::new(Mutex::new(None)),
                    // 健康提醒：运行时 + 仓库 + daemon 取消令牌（start 在 manage 之后调用）
                    health,
                    health_repo,
                    health_cancel,
                };

                // 4) 启动 axum HTTP server（绑定动态端口，回填 actual_http_port）
                //    失败不阻断应用启动（P2P 不可用但本地功能仍可用），仅记录日志。
                match http_server::start_http_server(state.clone()).await {
                    Ok(port) => {
                        // 5) 用实际端口启动 mDNS 发现（端口必须与 axum 一致，对端才能连）
                        if let Err(e) = discovery::start_discovery(&state, port).await {
                            tracing::error!("mDNS 发现启动失败（P2P 发现不可用）: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::error!("axum HTTP server 启动失败（P2P 不可用）: {e}");
                    }
                }

                Ok::<AppState, error::AppError>(state)
            })?;

            // 注入共享状态供命令层使用（axum/mDNS 已持有同一份 Arc 的 Clone）
            app.manage(state);

            // 启动 Claude Code 历史采集器（立即扫一次 + 每 5 分钟增量扫描），
            // 取消令牌存入 AppState 供应用退出时优雅停止。
            {
                let state: tauri::State<'_, AppState> = app.state();
                let cancel = crate::cc::collector::start(state.inner().clone());
                *state.cc_collector_cancel.lock().unwrap() = Some(cancel);
            }

            // 启动云端同步后台 scheduler（无条件启动；内部每 tick 按 config 的 enabled/auto/
            // interval 决定是否真同步）。取消令牌存入 AppState 供应用退出时优雅停止。
            {
                let state: tauri::State<'_, AppState> = app.state();
                let cancel = crate::cloud_sync::scheduler::start(state.inner().clone());
                *state.cloud_sync_cancel.lock().unwrap() = Some(cancel);
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
            prompt_cmd::list_prompts,
            prompt_cmd::get_prompt,
            prompt_cmd::create_prompt,
            prompt_cmd::update_prompt,
            prompt_cmd::delete_prompt,
            prompt_cmd::list_tags,
            config_cmd::get_config,
            config_cmd::get_default_config,
            config_cmd::update_config,
            config_cmd::get_version,
            config_cmd::choose_dir,
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
            transfer_cmd::list_transfers,
            transfer_cmd::send_transfer,
            transfer_cmd::cancel_transfer,
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
            // GitHub Trending 首页（榜单缓存 + Claude CLI 双语解说）
            github_trending_cmd::list_github_trending_repos,
            github_trending_cmd::get_github_trending_config,
            github_trending_cmd::get_default_github_trending_config,
            github_trending_cmd::update_github_trending_config,
            github_trending_cmd::test_claude_cli,
            // Prompt 优化（复用 Claude CLI pure/headless helper，不保存历史）
            prompt_optimizer_cmd::optimize_prompt,
            prompt_optimizer_cmd::stream_optimize_prompt_to_workbench_session,
            // M10 健康提醒（14 命令：配置/状态/开关/暂停/贪睡/跳过/配置回写/统计/活动明细/喝水/跳过喝水/延迟喝水/全屏遮罩/恢复默认）
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
            // 工作台（本机项目 + Claude Code PTY 终端 + 项目文件树）
            workbench_cmd::list_workbench_projects,
            workbench_cmd::add_workbench_project,
            workbench_cmd::remove_workbench_project,
            workbench_cmd::touch_workbench_project,
            workbench_cmd::list_workbench_worktrees,
            workbench_cmd::create_workbench_worktree,
            workbench_cmd::commit_workbench_worktree,
            workbench_cmd::push_workbench_worktree,
            workbench_cmd::merge_workbench_worktree,
            workbench_cmd::remove_workbench_worktree,
            workbench_cmd::list_workbench_git_commits,
            workbench_cmd::list_workbench_sessions,
            workbench_cmd::create_workbench_session,
            workbench_cmd::write_workbench_session_input,
            workbench_cmd::resize_workbench_session,
            workbench_cmd::focus_workbench_session,
            workbench_cmd::get_focused_workbench_session,
            workbench_cmd::split_workbench_pane,
            workbench_cmd::close_workbench_pane,
            workbench_cmd::close_workbench_session,
            workbench_cmd::rename_workbench_session,
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
        ])
        .build(tauri::generate_context!())
        .map_err(|e| {
            // build 失败通常是资源/配置问题，打印后退出（保留 expect 语义但带上下文）
            eprintln!("Tauri 应用构建失败: {e}");
            e
        })
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // M7：窗口关闭（应用退出）时优雅注销 mDNS，对照 Python app 关闭清理顺序。
            // 用 RunEvent::Exit 兜底，确保无论退出路径都触发 stop_discovery。
            if let tauri::RunEvent::Exit = event {
                let state: tauri::State<'_, AppState> = app_handle.state();
                discovery::stop_discovery(&state);
                // 停止 Claude Code 历史采集器后台任务
                if let Some(t) = state.cc_collector_cancel.lock().unwrap().take() {
                    t.cancel();
                    tracing::info!("CC 历史采集器已停止");
                }
                // 停止云端同步后台 scheduler
                if let Some(t) = state.cloud_sync_cancel.lock().unwrap().take() {
                    t.cancel();
                    tracing::info!("云端同步 scheduler 已停止");
                }
                // 停止健康监测 daemon（采样线程 + 处理 task）
                if let Some(t) = state.health_cancel.lock().unwrap().take() {
                    t.cancel();
                    tracing::info!("健康监测 daemon 已停止");
                }
                // 停止工作台中仍运行的 PTY attach；tmux 后端会保留 session 供下次启动重连。
                let cleaned = state.workbench_sessions.shutdown_all();
                if cleaned > 0 {
                    tracing::info!("工作台会话已清理: {cleaned}");
                }
                tracing::info!("应用已退出，mDNS 已注销");
            }
        });
}
