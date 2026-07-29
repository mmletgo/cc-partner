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
//!     `config` 与 `config_runtime.value` 共享同一 `Arc<RwLock<AppConfig>>`；生产 writer 走
//!     `config_runtime` 事务路径，读路径可继续用廉价 `config` 读锁。
//!     devices 用 RwLock<HashMap>（发现写入 / 命令读取并发）；
//!     actual_http_port 用 AtomicU16（启动后高频只读，无锁更高效）；
//!     discovery 句柄用 Mutex<Option<...>>（仅启动/关闭时写）。

use crate::backend::authority::RuntimeRole;
use crate::backend::control_client::BackendControlClientRuntime;
use crate::backend::event_bus::RuntimeEventBus;
use crate::backend::runtime_metrics::RuntimeMetrics;
use crate::backend::ui::{serialize_event_payload, BackendAsset, BackendUi};
use crate::cloud_sync::CloudSyncRuntime;
use crate::config::AppConfig;
use crate::config_runtime::ConfigRuntime;
use crate::models::device::Device;
use crate::net::peer_client::PeerClient;
use crate::orchestrator::repo::OrchestratorRepo;
use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
use crate::storage::{
    AgentHubRepo, AgentLedgerRepo, ClaudeHistoryRepo, ClaudeMdRepo, DatabaseMaintenanceGate,
    PromptRepo, ScratchpadRepo, TransferRepo, WorkbenchAgentSessionRepo, WorkbenchBrowserRepo,
    WorkbenchProjectRepo, WorkbenchSessionRepo, WorkbenchWorkspaceLayoutRepo,
    WorkbenchWorktreeRepo,
};
use crate::transfer::registry::TransferRegistry;
use crate::updater::UpdateRuntime;
use crate::workbench::agent_ledger::AgentLedgerService;
use mdns_sd::ServiceDaemon;
use serde::Serialize;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::atomic::AtomicU16;
use std::sync::{Arc, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

/// 应用全局共享状态。Clone 仅增加 Arc 引用计数。
#[derive(Clone)]
pub struct AppState {
    /// 配置读路径（与 `config_runtime.value` 共享同一 Arc，禁止旁路 save）
    pub config: Arc<RwLock<AppConfig>>,
    /// 配置串行事务运行时（生产 writer 唯一入口；持有 owner_instance_id + generation CAS）
    pub config_runtime: Arc<ConfigRuntime>,
    /// SQLite 连接池（M3+ axum server 共享此 pool；M1 仅 prompt_repo 通过独立 clone 使用）
    #[allow(dead_code)]
    pub db: SqlitePool,
    /// 全局 DB maintenance 写屏障（restore exclusive / ordinary shared；生产写事务唯一入口）
    pub maintenance_gate: Arc<DatabaseMaintenanceGate>,
    /// Prompt 仓库
    pub prompt_repo: Arc<PromptRepo>,
    /// 传输历史仓库（M5）
    pub transfer_repo: Arc<TransferRepo>,
    /// CLAUDE.md 单例仓库（user 级 CLAUDE.md 同步）
    pub claude_md_repo: Arc<ClaudeMdRepo>,
    /// 速记本单例仓库（scratchpad 表访问，自动保存 + 局域网/GitHub 同步）
    pub scratchpad_repo: Arc<ScratchpadRepo>,
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
    /// 后端 UI adapter（GUI 使用 Tauri，headless 使用 filesystem/no-op）
    pub ui: Arc<dyn BackendUi>,
    /// M8 自动更新 generation 状态机（单锁聚合 status/pending/bytes/task/token）
    pub update_runtime: Arc<UpdateRuntime>,
    /// Claude Code 历史仓库（claude_history / claude_history_scan_state 表访问）
    pub cc_history_repo: Arc<ClaudeHistoryRepo>,
    /// SSH 目标仓库（ssh_targets 表访问，跨设备同步）
    pub ssh_target_repo: Arc<crate::storage::SshTargetRepo>,
    /// 工作台项目仓库（workbench_projects 表访问，本机最近项目持久化）
    #[allow(dead_code)]
    pub workbench_project_repo: Arc<WorkbenchProjectRepo>,
    /// 工作台终端会话元数据仓库（workbench_sessions 表访问，重启恢复终端 tab）
    #[allow(dead_code)]
    pub workbench_session_repo: Arc<WorkbenchSessionRepo>,
    /// Agent session 运行时仓库（workbench_agent_sessions；provider-neutral lifecycle）
    #[allow(dead_code)]
    pub workbench_agent_session_repo: Arc<WorkbenchAgentSessionRepo>,
    /// Agent Metadata Ledger 仓库（metadata-only 历史；agent_session_id 唯一）
    pub agent_ledger_repo: Arc<AgentLedgerRepo>,
    /// Agent Metadata Ledger 写入服务（失败隔离 + usage 缓存 + 有界重试 metric）
    pub agent_ledger_service: Arc<AgentLedgerService>,
    /// Multi-CLI Agent Hub canonical 仓库（scope/asset/revision/binding/conflict）
    #[allow(dead_code)]
    pub agent_hub_repo: Arc<AgentHubRepo>,
    /// 工作台 Git worktree 元数据仓库（workbench_worktrees 表访问，重启恢复工作区列表）
    #[allow(dead_code)]
    pub workbench_worktree_repo: Arc<WorkbenchWorktreeRepo>,
    /// Workbench 浏览器预览目标仓库（workbench_browser_targets 表访问，保存项目/worktree 最近目标）
    pub workbench_browser_repo: Arc<WorkbenchBrowserRepo>,
    /// Workbench 工作现场 layout 仓库（自动 slot + 命名 snapshot，revision CAS）
    pub workbench_workspace_layout_repo: Arc<WorkbenchWorkspaceLayoutRepo>,
    /// Workbench 浏览器预览会话注册表（previewId 到本机 target 或远端 relay 的短期映射）
    pub workbench_browser_previews:
        Arc<crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry>,
    /// 浏览器自动验证服务（owner 侧 ephemeral managed Chromium；测试可用 FakeEngine）
    pub browser_verification:
        Arc<crate::workbench::browser_verification::BrowserVerificationService>,
    /// 工作台 PTY 会话注册表（运行期 PTY/tmux attach 句柄，元数据由 workbench_session_repo 持久化）
    #[allow(dead_code)]
    pub workbench_sessions: Arc<crate::workbench::sessions::WorkbenchSessionRegistry>,
    /// Workbench 远端事件有序总线（owner+sequence + ring catch-up + Gap；供 `/api/workbench/events`）
    pub workbench_remote_events: Arc<crate::workbench::remote_events::WorkbenchRemoteEventBus>,
    /// Workbench 远端事件桥接登记表（本机订阅其他设备 `/api/workbench/events`，按设备去重）
    pub workbench_remote_event_bridges:
        Arc<crate::workbench::remote_events::RemoteEventBridgeRegistry>,
    /// 工作台 tmux 依赖安装/检测状态机（供 check/install/status/cancel 四个命令共享）
    pub workbench_dependency:
        Arc<crate::workbench::dependencies::WorkbenchDependencyInstallRuntime>,
    /// CC 历史采集器的取消令牌（应用退出时 cancel 优雅停止后台扫描任务）
    pub cc_collector_cancel: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    /// 云端同步（GitHub 私有仓库）工作区单飞门闸（手动/scheduler/CLAUDE.md push 共享）
    pub cloud_sync_runtime: Arc<CloudSyncRuntime>,
    /// 云端同步（GitHub 私有仓库）后台 scheduler 的取消令牌（应用退出时 cancel 优雅停止）
    pub cloud_sync_cancel: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    /// 健康提醒运行时共享状态（状态机 + 贪睡/暂停标记，daemon task 与命令层共享同一份）
    pub health: Arc<crate::health::HealthRuntime>,
    /// 健康提醒数据库仓库（activity_records / water_records 读写，统计活跃/闲置分钟数）
    pub health_repo: Arc<crate::storage::health_repo::HealthRepo>,
    /// 健康监测 daemon 的取消令牌（应用退出时 cancel 优雅停止采样/处理任务）
    pub health_cancel: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    /// Orchestrator 任务编排仓储（任务队列、事件和证据持久化）
    #[allow(dead_code)]
    pub orchestrator_repo: Arc<OrchestratorRepo>,
    /// Orchestrator scheduler 最近 tick / dispatch 结果（内存可观测状态，供 runtime snapshot 状态条展示）
    pub orchestrator_scheduler_telemetry: OrchestratorSchedulerTelemetry,
    /// Orchestrator 后台 scheduler 的取消令牌（应用退出时 cancel，停止自动领取任务）
    pub orchestrator_cancel: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    /// Orchestrator 远端 outbox dispatcher 的取消令牌（应用退出时 cancel，停止 pending 远端任务投递）
    pub orchestrator_outbox_cancel: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    /// Agent Metadata Ledger 保留清理后台任务取消令牌
    pub agent_ledger_cancel: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    /// Workbench Claude session 搜索的内存索引，key = worktree_path canonical string。
    /// 首次搜索某 worktree 时 lazy 初始化并启动文件监听。
    pub workbench_claude_session_indexes: Arc<
        RwLock<
            HashMap<String, Arc<RwLock<crate::workbench::claude_sessions::WorktreeSessionIndex>>>,
        >,
    >,
    /// 每个 worktree 的 Claude session 文件监听运行时，key 同 workbench_claude_session_indexes。
    ///
    /// Business Logic（为什么需要这个字段）:
    ///     每个 worktree 的 watcher 必须与取消令牌、debounce/scan 句柄同生命周期；项目移除与
    ///     进程 shutdown 要能 cancel/abort 全部后台任务再 drop watcher，避免幽灵重扫。
    ///
    /// Code Logic（这个字段做什么）:
    ///     `Mutex<HashMap<String, ClaudeSessionWatcherRuntime>>`；监听失败时该 key 不存在
    ///     （降级为每次搜索重扫）。
    pub workbench_claude_session_watchers:
        Arc<Mutex<HashMap<String, crate::workbench::claude_sessions::ClaudeSessionWatcherRuntime>>>,
    /// Claude session 索引 singleflight 表：key = worktree canonical，value = watch Receiver。
    ///
    /// Business Logic（为什么需要这个字段）:
    ///     同一 worktree 并发搜索时只允许一次阻塞扫描，其余调用共享结果，避免重复 CPU/IO。
    ///
    /// Code Logic（这个字段做什么）:
    ///     tokio Mutex 保护 HashMap；leader 插入 Receiver(None)，完成后 send Some 并 remove。
    pub workbench_claude_session_index_inflight: Arc<
        tokio::sync::Mutex<
            HashMap<String, crate::workbench::claude_sessions::ClaudeSessionIndexInflightRx>,
        >,
    >,
    /// Claude session 索引 per-key dispose 世代，防止 dispose 后 concurrent ensure 写回幽灵索引。
    ///
    /// Business Logic（为什么需要这个字段）:
    ///     用户移除项目时可能正有搜索在 singleflight 扫描中；扫描结束后若仍 insert/spawn watcher，
    ///     会留下已删除项目的幽灵索引与 notify 运行时。世代计数让 finish 路径识别“扫描期间已被 dispose”。
    ///
    /// Code Logic（这个字段做什么）:
    ///     `Mutex<HashMap<String, u64>>`；dispose 时 key 的 epoch+1；ensure 在 leader/回退路径
    ///     捕获 start_epoch，finish_scan_and_insert 在 insert/watcher 前比对，世代变化则 no-op。
    pub workbench_claude_session_index_dispose_epochs: Arc<Mutex<HashMap<String, u64>>>,
    /// 进程内有界本地运行时指标（耗时/计数/EWMA）；不上传、不记录正文/路径/凭据。
    pub runtime_metrics: Arc<RuntimeMetrics>,
    /// 运行时角色：sidecar=`HeadlessOwner`（唯一 Workbench/runtime owner），GUI=`GuiClient`（仅代理）。
    pub runtime_role: RuntimeRole,
    /// sidecar 有界事件总线（owner 发布；GUI 经 control relay afterSequence 消费）。
    pub event_bus: Arc<RuntimeEventBus>,
    /// GUI control client 运行时缓存（复用 loopback client；mutation 失败仅失效、不自动重放）。
    /// HeadlessOwner 可持有但业务路径不使用，避免双构造路径。
    pub backend_control_client_runtime: Arc<BackendControlClientRuntime>,
    /// GUI owner event relay 取消令牌（setup 时写入，shutdown 时 cancel，避免幽灵 stream task）。
    pub gui_event_relay_cancel: Arc<Mutex<Option<CancellationToken>>>,
}

impl AppState {
    /// 读取本机设备名。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     mDNS 注册、健康检查和控制文件都需要展示当前配置中的设备名。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 config RwLock 读取 device_name 并 clone 返回。
    pub fn device_name(&self) -> String {
        self.config
            .read()
            .expect("config 读锁中毒")
            .device_name
            .clone()
    }

    /// 发送后端 UI 事件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     运行时业务层需要广播终端输出、传输状态等事件，但 GUI/headless 两种模式的处理方式不同。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先把任意可序列化 payload 转为 JSON Value；HeadlessOwner 同时写入 `event_bus`
    ///     （供 GUI control relay），再委托 `BackendUi::emit`；序列化失败仅 warn。
    #[allow(dead_code)]
    pub fn emit_event<T>(&self, event: &str, payload: T)
    where
        T: Serialize,
    {
        match serialize_event_payload(payload) {
            Ok(value) => {
                if self.runtime_role == RuntimeRole::HeadlessOwner {
                    let _ = self.event_bus.publish(event, value.clone());
                }
                self.ui.emit(event, value);
            }
            Err(error) => tracing::warn!("序列化事件 {event} 失败: {error}"),
        }
    }

    /// 读取移动端静态资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     HTTP `/mobile` fallback 需要按运行模式从 GUI 嵌入资源或 headless dist 目录读取移动端页面资源。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将已规范化的 asset key 委托给当前 `BackendUi` adapter，并返回统一 `BackendAsset`。
    pub fn mobile_asset(&self, asset_key: &str) -> Option<BackendAsset> {
        self.ui.asset(asset_key)
    }
}
