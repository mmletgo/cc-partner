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
mod tray;
mod transfer;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::atomic::AtomicU16;
use std::sync::{Arc, Mutex, RwLock};

use crate::commands::{
    cc_history as cc_history_cmd, claude_md as claude_md_cmd, config as config_cmd,
    devices as device_cmd, health as health_cmd,
    permissions as permissions_cmd, prompts as prompt_cmd, screenshot as screenshot_cmd,
    sync as sync_cmd, transfer as transfer_cmd, updater as updater_cmd,
};
use crate::net::{discovery, http_server, peer_client::PeerClient};
use crate::state::AppState;
use crate::storage::{ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, TransferRepo};
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
const CC_INDEXES: &str = "CREATE INDEX IF NOT EXISTS idx_ch_proj ON claude_history(project_path, occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_ch_dev ON claude_history(device_id)";

/// user 级 CLAUDE.md 单例表（全表仅一行，id 恒为 "claude_md"）。
const CLAUDE_MD_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS claude_md (
    id TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    device_id TEXT NOT NULL,
    vector_clock TEXT NOT NULL
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
    // 健康提醒：活动采样表 + 喝水记录表（在 CLAUDE_MD_SCHEMA 之后执行）
    sqlx::query(HEALTH_SCHEMA).execute(&pool).await?;
    sqlx::query(WATER_SCHEMA).execute(&pool).await?;
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
                // 健康提醒：仓库（共享 pool）+ 运行时（状态机/贪睡/暂停）+ daemon 取消令牌占位
                let health_repo = Arc::new(crate::storage::health_repo::HealthRepo::new(pool.clone()));
                let health = Arc::new(crate::health::HealthRuntime::new());
                let health_cancel =
                    Arc::new(Mutex::new(None::<tokio_util::sync::CancellationToken>));
                let state = AppState {
                    config: Arc::new(RwLock::new(config)),
                    db: pool,
                    prompt_repo,
                    transfer_repo,
                    claude_md_repo,
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
                    cc_collector_cancel: Arc::new(Mutex::new(None)),
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
                let want_autostart = state
                    .config
                    .read()
                    .expect("config 读锁中毒")
                    .health
                    .enabled;
                let autostart = app.autolaunch();
                if want_autostart {
                    if let Err(e) = autostart.enable() {
                        tracing::warn!("开机自启 enable 失败: {e}");
                    }
                } else if let Err(e) = autostart.disable() {
                    tracing::warn!("开机自启 disable 失败: {e}");
                }
                tracing::info!("开机自启: {}", if want_autostart { "已启用" } else { "已禁用" });
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
                hotkey::register_screenshot_hotkey(app.handle(), &hotkey, hotkey::screenshot_handler);
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
            config_cmd::update_config,
            config_cmd::get_version,
            config_cmd::choose_dir,
            device_cmd::list_devices,
            device_cmd::get_local_device,
            sync_cmd::trigger_sync,
            claude_md_cmd::get_claude_md,
            claude_md_cmd::update_claude_md,
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
            // M10 健康提醒（状态/开关/暂停/贪睡/跳过/配置/统计）
            health_cmd::get_health_config,
            health_cmd::get_health_status,
            health_cmd::toggle_health_enabled,
            health_cmd::toggle_health_paused,
            health_cmd::snooze_reminder,
            health_cmd::skip_reminder,
            health_cmd::update_health_config,
            health_cmd::get_activity_stats,
            health_cmd::get_activity_detail,
            health_cmd::record_water,
            health_cmd::close_health_overlay,
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
                // 停止健康监测 daemon（采样线程 + 处理 task）
                if let Some(t) = state.health_cancel.lock().unwrap().take() {
                    t.cancel();
                    tracing::info!("健康监测 daemon 已停止");
                }
                tracing::info!("应用已退出，mDNS 已注销");
            }
        });
}
