//! state.rs — 应用共享状态
//!
//! Business Logic（为什么需要这个模块）:
//!     Tauri 命令通过 `State<'_, AppState>` 注入共享依赖，axum HTTP server 也通过
//!     `with_state` 共享同一份状态。AppState 聚合配置、数据库、Prompt 仓库、设备 ID、
//!     已发现设备列表、实际 HTTP 监听端口、mDNS 守护句柄与 peer client，
//!     供本地 IPC 命令与 P2P 通信两端访问。
//!
//! Code Logic（这个模块做什么）:
//!     用 `Arc` 内部可变（config 用 RwLock 因可写；device_id 只读故 String 足够），
//!     整体 Clone 廉价（Arc 引用计数），满足 Tauri manage/State 与 axum State 的要求。
//!     devices 用 RwLock<HashMap>（发现写入 / 命令读取并发）；
//!     actual_http_port 用 AtomicU16（启动后高频只读，无锁更高效）；
//!     discovery 句柄用 Mutex<Option<...>>（仅启动/关闭时写）。

use crate::config::AppConfig;
use crate::models::device::Device;
use crate::net::peer_client::PeerClient;
use crate::storage::{ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, TransferRepo};
use crate::transfer::registry::TransferRegistry;
use mdns_sd::ServiceDaemon;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::atomic::AtomicU16;
use std::sync::{Arc, Mutex, RwLock};
use tauri::async_runtime::JoinHandle;
use tauri::AppHandle;
use tauri_plugin_updater::Update;

/// 应用全局共享状态。Clone 仅增加 Arc 引用计数。
#[derive(Clone)]
pub struct AppState {
    /// 配置（前端可写 deviceName/receiveDir/screenshotHotkey，故 RwLock）
    pub config: Arc<RwLock<AppConfig>>,
    /// SQLite 连接池（M3+ axum server 共享此 pool；M1 仅 prompt_repo 通过独立 clone 使用）
    #[allow(dead_code)]
    pub db: SqlitePool,
    /// Prompt 仓库
    pub prompt_repo: Arc<PromptRepo>,
    /// 传输历史仓库（M5）
    pub transfer_repo: Arc<TransferRepo>,
    /// CLAUDE.md 单例仓库（user 级 CLAUDE.md 同步）
    pub claude_md_repo: Arc<ClaudeMdRepo>,
    /// 本机设备 ID（从 config 取出，高频只读访问，单独缓存一份 String）
    pub device_id: Arc<String>,
    /// 已发现的对端设备表 {device_id: Device}（mDNS 发现写入，list_devices 读取）
    pub devices: Arc<RwLock<HashMap<String, Device>>>,
    /// axum HTTP server 实际监听端口（动态分配，启动后回填；0 表示尚未启动）
    pub actual_http_port: Arc<AtomicU16>,
    /// mDNS 守护句柄（启动后持有，应用关闭时 shutdown）。None 表示未启用发现
    pub discovery: Arc<Mutex<Option<ServiceDaemon>>>,
    /// 对端 HTTP 客户端（调对端 /api/health、sync、transfer）
    #[allow(dead_code)]
    pub peer_client: Arc<PeerClient>,
    /// 活跃传输任务登记表（M5）：含每任务 CancellationToken，供发送/接收两端与 cancel 命令共享
    pub transfers: Arc<TransferRegistry>,
    /// Tauri AppHandle（M5）：axum transfer handler 需 emit 接收进度/完成事件给前端
    #[allow(dead_code)]
    pub app_handle: AppHandle,
    /// M8 更新下载状态机（status/progress/error/filePath/url/filename/size），对齐前端 UpdateDownloadStatus，
    /// 前端 get_download_status 轮询读取
    pub update_status: Arc<RwLock<crate::commands::updater::UpdateDownloadStatus>>,
    /// M8 check_update 命中新版本后缓存的 Update 对象（download/install 时取出 clone 操作），
    /// 避免跨命令重复请求 endpoint。Update 实现 Clone，owned 无生命周期参数，可安全长期持有
    pub update_pending: Arc<Mutex<Option<Update>>>,
    /// M8 download 完成后缓存的安装包字节（install 时取出喂给 update.install）。
    /// tauri-plugin-updater 的 install 接受 &[u8]，下载结果需跨命令传递
    pub update_bytes: Arc<Mutex<Option<Vec<u8>>>>,
    /// M8 正在进行的下载任务句柄（cancel_download 时 abort 强制中断 reqwest 流）
    pub update_download_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// M8 当前下载任务的取消令牌（cancel_download 时 cancel()，spawn 体内 is_cancelled 判定为 Cancelled）
    pub update_cancel_token: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    /// Claude Code 历史仓库（claude_history / claude_history_scan_state 表访问）
    pub cc_history_repo: Arc<ClaudeHistoryRepo>,
    /// CC 历史采集器的取消令牌（应用退出时 cancel 优雅停止后台扫描任务）
    pub cc_collector_cancel: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    /// 云端同步（GitHub 私有仓库）后台 scheduler 的取消令牌（应用退出时 cancel 优雅停止）
    pub cloud_sync_cancel: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
}

impl AppState {
    /// 读取本机设备名（从 config RwLock 取，供 mDNS 注册与命令层复用）。
    pub fn device_name(&self) -> String {
        self.config
            .read()
            .expect("config 读锁中毒")
            .device_name
            .clone()
    }
}
