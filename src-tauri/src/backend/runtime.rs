//! backend/runtime.rs — GUI 与 headless 后端共享的运行时装配。
//!
//! Business Logic（为什么需要这个模块）:
//!     GUI 和独立后端进程需要复用同一套数据库、AppState、P2P 服务与后台任务装配逻辑，
//!     避免未来 CLI/headless 与桌面入口各自维护一套运行时。
//!
//! Code Logic（这个模块做什么）:
//!     提供数据库初始化、AppState 构造、HTTP/mDNS 服务启动、后台任务启动与退出清理函数。

use crate::backend::control;
use crate::backend::runtime_metrics::RuntimeMetrics;
use crate::backend::ui::BackendUi;
use crate::config::AppConfig;
use crate::config_runtime::ConfigRuntime;
use crate::config_store::FsConfigStore;
use crate::error::AppError;
use crate::net::{discovery, http_server, peer_client::PeerClient};
use crate::orchestrator::repo::OrchestratorRepo;
use crate::state::AppState;
use crate::storage::{
    ClaudeHistoryRepo, ClaudeMdRepo, DatabaseMaintenanceGate, PromptRepo, ScratchpadRepo,
    SshTargetRepo, TransferRepo, WorkbenchBrowserRepo, WorkbenchProjectRepo, WorkbenchSessionRepo,
    WorkbenchWorktreeRepo,
};
use crate::transfer::registry::TransferRegistry;
use serde::Deserialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// sidecar 健康检查超时秒数。
const SIDECAR_HEALTH_TIMEOUT_SECS: u64 = 2;

/// sidecar `/api/health` 响应中启动判定需要的字段。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 复用独立后端前必须确认控制文件指向的端口确实是当前设备的健康后端，而不是 stale 文件或其它服务。
///
/// Code Logic（这个结构做什么）:
///     反序列化 health JSON 的 ok、device_id 和 http_port 字段，其它字段由 serde 忽略。
#[derive(Debug, Deserialize)]
struct SidecarHealthResponse {
    ok: bool,
    device_id: String,
    http_port: u16,
}

/// 后端运行模式。
///
/// Business Logic（为什么需要这个枚举）:
///     GUI 与独立 headless 后端共享运行时，但两种入口启动的后台任务不同，必须显式区分。
///
/// Code Logic（这个枚举做什么）:
///     `Headless` 表示独立后端负责 CC/cloud/orchestrator 等后台任务；`Gui` 表示桌面壳只保留 UI 专属能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendRuntimeMode {
    Gui,
    Headless,
}

/// 后端运行时启动选项。
///
/// Business Logic（为什么需要这个结构）:
///     启动方需要把运行模式、mDNS 是否宣告本机以及是否浏览局域网设备作为一个稳定契约传给共享 runtime。
///
/// Code Logic（这个结构做什么）:
///     保存 mode/advertise/browse 三个布尔或枚举开关，供后续 CLI 与 GUI lifecycle 复用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendRuntimeOptions {
    pub mode: BackendRuntimeMode,
    pub advertise: bool,
    pub browse: bool,
}

/// 已启动的后端运行时句柄。
///
/// Business Logic（为什么需要这个结构）:
///     CLI/headless 后续入口需要把构造出的 AppState、实际服务端口和运行模式作为一个整体返回和管理。
///
/// Code Logic（这个结构做什么）:
///     聚合 `AppState`、实际端口和 `BackendRuntimeMode`；当前任务先定义契约，后续 CLI 入口可直接使用。
#[derive(Clone)]
pub struct BackendRuntime {
    pub state: AppState,
    pub port: u16,
    pub mode: BackendRuntimeMode,
}

/// 建表 SQL（对照 migrations/0001_init.sql，全 CREATE TABLE IF NOT EXISTS）。
///
/// Business Logic（为什么需要这个常量）:
///     旧库可能没有 `_sqlx_migrations` 表，运行时必须用幂等 schema 确保用户数据可直接升级。
///
/// Code Logic（这个常量做什么）:
///     定义 prompts 表结构，供 `init_db` 手动执行。
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

/// transfer_history 建表（文档 + 新库基线）。N5 recovery 列由 CREATE 声明；
/// 旧库升级走 `TransferRepo::ensure_schema` 的幂等 ALTER（禁止 sqlx::migrate!）。
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
    completed_at TEXT,
    phase TEXT,
    failure_stage TEXT,
    failure_code TEXT,
    failure_retryable INTEGER,
    failure_message TEXT,
    attempt INTEGER NOT NULL DEFAULT 1,
    logical_transfer_id TEXT,
    attempt_id TEXT,
    protocol_transfer_id TEXT,
    client_operation_id TEXT,
    operation_payload_hash TEXT
)";

/// Claude Code 历史 prompt 表（采集入库 + 跨设备同步）。
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

/// 健康提醒 - 喝水打卡表（自增 id 主键，ts 仅为普通列，支持同秒多次 +1 杯）。
const WATER_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS water_records (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL
)";

/// rest_records 表 schema:记录久坐提醒触发与完成的休息事件,用于习惯统计。
const REST_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS rest_records (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    kind TEXT NOT NULL,
    duration_seconds INTEGER NOT NULL DEFAULT 0
)";

/// GitHub Trending 首页缓存表（榜单 + Claude CLI 中英文解说）。
const GITHUB_TRENDING_CACHE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS github_trending_cache (
    key TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    ai_status TEXT NOT NULL,
    ai_error TEXT
)";

/// 工作台本机项目表（最近项目列表持久化）。
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

/// Workbench 浏览器预览目标表（项目/worktree 最近一次目标 URL）。
const WORKBENCH_BROWSER_TARGET_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS workbench_browser_targets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL,
    worktree_id TEXT,
    worktree_key TEXT GENERATED ALWAYS AS (IFNULL(worktree_id, '')) STORED,
    target_url TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    UNIQUE(project_id, worktree_key)
)";

/// Workbench 浏览器预览目标项目查询索引。
const WORKBENCH_BROWSER_TARGET_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_workbench_browser_targets_project
    ON workbench_browser_targets(project_id, updated_at DESC)";

/// 初始化数据库连接池：开启 WAL，手动建表，返回 SqlitePool。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 与 headless 后端必须共享完全一致的数据 schema 初始化路径，保证旧用户库无损升级。
///
/// Code Logic（这个函数做什么）:
///     用 `SqliteConnectOptions` 开启 create_if_missing、WAL，并**显式** `busy_timeout=5s`
///     （不依赖 sqlx 默认值，避免升级时无声漂移）；`max_connections(1)` 保持单连接语义；
///     再按固定顺序逐条执行幂等建表 SQL 与各 repo 的 schema 迁移 helper。
pub(crate) async fn init_db(db_path: &str) -> Result<sqlx::SqlitePool, AppError> {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path))?
        .create_if_missing(true)
        .pragma("journal_mode", "WAL")
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;

    sqlx::query(PROMPTS_SCHEMA).execute(&pool).await?;
    sqlx::query(TRANSFER_SCHEMA).execute(&pool).await?;
    // N5 transfer recovery：幂等补列 + client_operation_id 部分唯一索引
    crate::storage::TransferRepo::ensure_schema(&pool).await?;
    sqlx::query(CC_HISTORY_SCHEMA).execute(&pool).await?;
    sqlx::query(CC_SCAN_STATE_SCHEMA).execute(&pool).await?;
    for stmt in CC_INDEXES.split(';') {
        let s = stmt.trim();
        if s.is_empty() {
            continue;
        }
        sqlx::query(s).execute(&pool).await?;
    }
    sqlx::query(CLAUDE_MD_SCHEMA).execute(&pool).await?;
    sqlx::query(SSH_TARGET_SCHEMA).execute(&pool).await?;
    sqlx::query(SCRATCHPAD_SCHEMA).execute(&pool).await?;
    ScratchpadRepo::new(pool.clone()).ensure_schema().await?;
    // N2 sync push-batch 幂等 ledger（UNIQUE claimed_device_id+domain+client_request_id）
    crate::storage::SyncRequestLedgerRepo::ensure_schema(&pool).await?;
    // N2 conflict/history + delete epoch watermark/floor 表
    crate::storage::ContentVersionRepo::ensure_schema(&pool).await?;
    crate::storage::SyncWatermarkRepo::ensure_schema(&pool).await?;
    crate::storage::SyncDeleteSequenceRepo::ensure_schema(&pool).await?;
    crate::storage::DeletionFloorRepo::ensure_schema(&pool).await?;
    crate::storage::ensure_domain_delete_epoch_columns(&pool).await?;
    // N2 recovery_jobs 状态机（导出/恢复）
    crate::storage::RecoveryJobRepo::ensure_schema(&pool).await?;
    sqlx::query(HEALTH_SCHEMA).execute(&pool).await?;

    let needs_recreate: bool = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('water_records') WHERE name = 'id'",
    )
    .fetch_one(&pool)
    .await
    .unwrap_or(0)
        == 0;
    if needs_recreate {
        sqlx::query("DROP TABLE IF EXISTS water_records")
            .execute(&pool)
            .await
            .ok();
    }

    sqlx::query(WATER_SCHEMA).execute(&pool).await?;
    sqlx::query(REST_SCHEMA).execute(&pool).await?;
    sqlx::query(GITHUB_TRENDING_CACHE_SCHEMA)
        .execute(&pool)
        .await?;
    sqlx::query(WORKBENCH_PROJECT_SCHEMA).execute(&pool).await?;
    sqlx::query(WORKBENCH_WORKTREE_SCHEMA)
        .execute(&pool)
        .await?;
    sqlx::query(WORKBENCH_SESSION_SCHEMA).execute(&pool).await?;
    sqlx::query(WORKBENCH_BROWSER_TARGET_SCHEMA)
        .execute(&pool)
        .await?;
    sqlx::query(WORKBENCH_BROWSER_TARGET_INDEX)
        .execute(&pool)
        .await?;
    sqlx::query(crate::workbench::operation_ledger::WORKBENCH_MUTATION_OPERATIONS_SCHEMA)
        .execute(&pool)
        .await?;
    WorkbenchWorktreeRepo::new(pool.clone())
        .ensure_schema()
        .await?;
    WorkbenchSessionRepo::new(pool.clone())
        .ensure_schema()
        .await?;
    OrchestratorRepo::init_schema(&pool).await?;
    Ok(pool)
}

/// 构造共享 `AppState`（默认 HeadlessOwner）。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 与 headless sidecar 共享初始化路径，但运行时角色不同：
///     sidecar 是唯一 `HeadlessOwner`，GUI 是 `GuiClient` 只能代理。
///
/// Code Logic（这个函数做什么）:
///     委托 `build_app_state_with_role`，默认 `HeadlessOwner`（CLI/serve 与多数测试）。
pub async fn build_app_state(ui: Arc<dyn BackendUi>) -> Result<AppState, AppError> {
    build_app_state_with_role(ui, crate::backend::authority::RuntimeRole::HeadlessOwner).await
}

/// 按运行时角色构造 `AppState`。
///
/// Business Logic（为什么需要这个函数）:
///     GUI setup 必须显式注入 `GuiClient`，禁止 GUI 进程本地 attach/bridge/mutation。
///
/// Code Logic（这个函数做什么）:
///     load config → init_db → 组装 AppState，写入 `runtime_role`。
pub async fn build_app_state_with_role(
    ui: Arc<dyn BackendUi>,
    runtime_role: crate::backend::authority::RuntimeRole,
) -> Result<AppState, AppError> {
    let loaded = AppConfig::load()?;
    let store = Arc::new(FsConfigStore::default_path()?);
    // sidecar/GUI 共享构造入口：生成一次 owner 实例 id，供 control 文件与 ConfigRuntime CAS 共用。
    let owner_instance_id = uuid::Uuid::new_v4().to_string();
    let event_bus = Arc::new(crate::backend::event_bus::RuntimeEventBus::new(
        owner_instance_id.clone(),
    ));
    let config_runtime = Arc::new(ConfigRuntime::with_owner(loaded, store, owner_instance_id));
    let config = config_runtime.shared_value();
    let device_id = config
        .read()
        .map_err(|_| AppError::generic("配置读锁中毒"))?
        .device_id
        .clone();
    let db_path = config
        .read()
        .map_err(|_| AppError::generic("配置读锁中毒"))?
        .db_path
        .clone();
    let pool = init_db(&db_path).await?;
    // 全局写屏障：所有生产 SQLite writer 共享；restore 独占。
    let maintenance_gate = Arc::new(DatabaseMaintenanceGate::new());

    // 生产 writer 一律 with_gate，共享 restore exclusive 屏障。
    let prompt_repo = Arc::new(PromptRepo::with_gate(
        pool.clone(),
        maintenance_gate.clone(),
    ));
    let transfer_repo = Arc::new(TransferRepo::with_gate(
        pool.clone(),
        maintenance_gate.clone(),
    ));
    let cc_history_repo = Arc::new(ClaudeHistoryRepo::with_gate(
        pool.clone(),
        maintenance_gate.clone(),
    ));
    let claude_md_repo = Arc::new(ClaudeMdRepo::with_gate(
        pool.clone(),
        maintenance_gate.clone(),
    ));
    let ssh_target_repo = Arc::new(SshTargetRepo::with_gate(
        pool.clone(),
        maintenance_gate.clone(),
    ));
    let scratchpad_repo = Arc::new(ScratchpadRepo::with_gate(
        pool.clone(),
        maintenance_gate.clone(),
    ));
    let workbench_project_repo = Arc::new(WorkbenchProjectRepo::with_gate(
        pool.clone(),
        maintenance_gate.clone(),
    ));
    let workbench_session_repo = Arc::new(WorkbenchSessionRepo::with_gate(
        pool.clone(),
        maintenance_gate.clone(),
    ));
    let workbench_worktree_repo = Arc::new(WorkbenchWorktreeRepo::with_gate(
        pool.clone(),
        maintenance_gate.clone(),
    ));
    let workbench_browser_repo = Arc::new(WorkbenchBrowserRepo::with_gate(
        pool.clone(),
        maintenance_gate.clone(),
    ));
    let workbench_browser_previews =
        Arc::new(crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new());
    let orchestrator_repo = Arc::new(OrchestratorRepo::with_gate(
        pool.clone(),
        maintenance_gate.clone(),
    ));
    let workbench_sessions = Arc::new(crate::workbench::sessions::WorkbenchSessionRegistry::new());
    let (workbench_remote_events, _) = tokio::sync::broadcast::channel(1024);
    let workbench_remote_event_bridges =
        Arc::new(crate::workbench::remote_events::RemoteEventBridgeRegistry::new());
    let workbench_dependency =
        Arc::new(crate::workbench::dependencies::WorkbenchDependencyInstallRuntime::new());
    let health_repo = Arc::new(crate::storage::health_repo::HealthRepo::with_gate(
        pool.clone(),
        maintenance_gate.clone(),
    ));
    let health = Arc::new(crate::health::HealthRuntime::new());

    Ok(AppState {
        config,
        config_runtime,
        db: pool,
        maintenance_gate,
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
        ui,
        update_runtime: Arc::new(crate::updater::UpdateRuntime::new()),
        cc_history_repo,
        workbench_project_repo,
        workbench_session_repo,
        workbench_worktree_repo,
        workbench_browser_repo,
        workbench_browser_previews,
        workbench_sessions,
        workbench_remote_events,
        workbench_remote_event_bridges,
        workbench_dependency,
        cc_collector_cancel: Arc::new(Mutex::new(None)),
        cloud_sync_runtime: Arc::new(crate::cloud_sync::CloudSyncRuntime::new()),
        cloud_sync_cancel: Arc::new(Mutex::new(None)),
        health,
        health_repo,
        health_cancel: Arc::new(Mutex::new(None)),
        orchestrator_repo,
        orchestrator_scheduler_telemetry:
            crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry::new(),
        orchestrator_cancel: Arc::new(Mutex::new(None)),
        orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
        workbench_claude_session_indexes: Arc::new(RwLock::new(std::collections::HashMap::new())),
        workbench_claude_session_watchers: Arc::new(Mutex::new(std::collections::HashMap::new())),
        workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        workbench_claude_session_index_dispose_epochs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        runtime_metrics: Arc::new(RuntimeMetrics::new()),
        runtime_role,
        event_bus,
    })
}

/// 启动 HTTP/mDNS 后端服务组。
///
/// Business Logic（为什么需要这个函数）:
///     headless 后端需要监听 HTTP 并宣告自己；GUI 连接 sidecar 时只需要浏览局域网设备并复用 sidecar 端口。
///
/// Code Logic（这个函数做什么）:
///     `advertise=true` 时启动 axum HTTP server 并用实际端口传给 discovery；
///     `advertise=false,browse=true` 时先验证 sidecar 控制文件与 health，再写入 `actual_http_port` 并只启动 mDNS browse。
pub async fn start_backend_services(
    state: &AppState,
    advertise: bool,
    browse: bool,
) -> Result<u16, AppError> {
    if !advertise && !browse {
        return Ok(state.actual_http_port.load(Ordering::SeqCst));
    }

    let port = if advertise {
        http_server::start_http_server(state.clone()).await?
    } else {
        verified_sidecar_port_for_browse_only(state)
            .await
            .ok_or_else(|| {
                AppError::generic("未验证到运行中的独立后端，无法进入 browse-only 模式")
            })?
    };

    discovery::start_discovery(state, port, advertise, browse)
        .await
        .map_err(AppError::generic)?;

    Ok(port)
}

/// 启动 GUI 入口需要的后端服务并返回后台任务模式。
///
/// Business Logic（为什么需要这个函数）:
///     桌面 GUI 必须复用已验证的独立 sidecar，只启动 browse-only mDNS 来发现局域网设备，避免重复 advertise 自己。
///
/// Code Logic（这个函数做什么）:
///     调用 `start_backend_services(advertise=false,browse=true)` 验证控制文件/health，写入 sidecar 端口并启动 browse。
pub async fn start_gui_backend_services(state: &AppState) -> Result<BackendRuntimeMode, AppError> {
    start_backend_services(state, false, true).await?;
    Ok(BackendRuntimeMode::Gui)
}

/// 启动运行模式对应的后台任务。
///
/// Business Logic（为什么需要这个函数）:
///     独立 headless 后端应负责 CC 历史采集、云端同步、Orchestrator 调度和远端 outbox；
///     GUI sidecar 模式不能重复启动这些后台任务。
///
/// Code Logic（这个函数做什么）:
///     `Headless` 模式按任务类型启动并保存取消令牌；`Gui` 模式不启动 headless 后台任务。
pub fn start_background_tasks(state: &AppState, mode: BackendRuntimeMode) {
    match mode {
        BackendRuntimeMode::Headless => {
            // 启动时诚实回收崩溃遗留的 recovery Applying 任务，禁止伪装成功。
            {
                let reclaim_state = state.clone();
                tauri::async_runtime::spawn(async move {
                    let service = crate::backup::restore::BackupRestoreService::new(reclaim_state);
                    match service.reclaim_on_startup().await {
                        Ok(n) if n > 0 => {
                            tracing::warn!("已回收 {n} 个卡住的 recovery Applying 任务为 Failed");
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!("回收卡住 recovery jobs 失败: {e}");
                        }
                    }
                });
            }
            // N5：恢复 insert-before-spawn 的 transfer Queued claim 行。
            {
                let transfer_state = state.clone();
                tauri::async_runtime::spawn(async move {
                    match crate::transfer::sender::recover_pending_claimed_operations(
                        &transfer_state,
                    )
                    .await
                    {
                        Ok(n) if n > 0 => {
                            tracing::info!(
                                "已恢复 {n} 个 transfer insert-before-spawn Queued 任务"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!("恢复 pending transfer claim 失败: {e}");
                        }
                    }
                });
            }
            // 周期性 tombstone → deletion floor GC（与同步结束后的 best-effort 互补）
            {
                let gc_state = state.clone();
                tauri::async_runtime::spawn(async move {
                    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    // 启动后稍等再跑一次，避免与 recovery reclaim 抢写
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    loop {
                        ticker.tick().await;
                        match crate::sync::engine::run_tombstone_gc_best_effort(&gc_state).await {
                            Ok(n) if n > 0 => {
                                tracing::info!("后台 tombstone GC 压缩了 {n} 条");
                            }
                            Ok(_) => {}
                            Err(e) => tracing::warn!("后台 tombstone GC 失败: {e}"),
                        }
                    }
                });
            }
            start_cancelled_task_once(&state.cc_collector_cancel, "CC 历史采集器", || {
                crate::cc::collector::start(state.clone())
            });
            start_cancelled_task_once(&state.cloud_sync_cancel, "云端同步 scheduler", || {
                crate::cloud_sync::scheduler::start(state.clone())
            });
            start_cancelled_task_once(&state.orchestrator_cancel, "Orchestrator scheduler", || {
                crate::orchestrator::scheduler::start_orchestrator_scheduler(state.clone())
            });
            start_cancelled_task_once(
                &state.orchestrator_outbox_cancel,
                "Orchestrator remote outbox dispatcher",
                || {
                    crate::orchestrator::outbox::start_orchestrator_remote_outbox_dispatcher(
                        state.clone(),
                    )
                },
            );
        }
        BackendRuntimeMode::Gui => {
            tracing::info!("GUI 模式跳过 headless 后台任务启动");
        }
    }
}

/// 关闭共享后端运行时。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 与 headless 退出时都需要一致地注销 mDNS、停止后台任务并断开 Workbench 运行期会话。
///
/// Code Logic（这个函数做什么）:
///     调用 discovery shutdown，逐个 take 并 cancel 可选后台任务令牌，最后调用 `workbench_sessions.shutdown_all()`。
pub fn shutdown_backend_runtime(state: &AppState) {
    discovery::stop_discovery(state);
    cancel_runtime_token(&state.cc_collector_cancel, "CC 历史采集器");
    cancel_runtime_token(&state.cloud_sync_cancel, "云端同步 scheduler");
    cancel_runtime_token(&state.orchestrator_cancel, "Orchestrator scheduler");
    cancel_runtime_token(
        &state.orchestrator_outbox_cancel,
        "Orchestrator remote outbox dispatcher",
    );
    cancel_runtime_token(&state.health_cancel, "健康监测 daemon");

    let cleaned = state.workbench_sessions.shutdown_all();
    if cleaned > 0 {
        tracing::info!("工作台会话已清理: {cleaned}");
    }

    // Claude session 索引 watcher：cancel + abort debounce/scan 句柄，再 drop watcher/index。
    crate::workbench::claude_sessions::shutdown_all_claude_session_indexes(state);

    // 同步路径：cancel + abort 全部 remote event bridge，避免关机后 ghost reconnect。
    // 测试路径可 await `RemoteEventBridgeRegistry::shutdown_all` 等待任务自然退出。
    state.workbench_remote_event_bridges.force_shutdown();
}

/// 验证 browse-only 模式应复用的 sidecar 端口。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 只有确认独立后端控制文件、设备身份、端口与 HTTP health 都匹配时，才能跳过自身 HTTP/mDNS 广播和后台任务。
///
/// Code Logic（这个函数做什么）:
///     读取控制文件；基础校验通过后 GET `127.0.0.1:{port}/api/health` 并校验响应；失败时记录 warn 并尽量清理控制文件。
pub async fn verified_sidecar_port_for_browse_only(state: &AppState) -> Option<u16> {
    let control = match control::read_control_file() {
        Ok(Some(control)) => control,
        Ok(None) => {
            tracing::info!("未发现独立后端控制文件，GUI 无法进入 browse-only 模式");
            return None;
        }
        Err(e) => {
            tracing::warn!("读取独立后端控制文件失败，GUI 无法进入 browse-only 模式: {e}");
            remove_stale_control_files();
            return None;
        }
    };

    let health = match fetch_sidecar_health(control.port).await {
        Ok(health) => health,
        Err(e) => {
            tracing::warn!("独立后端健康检查失败，GUI 无法进入 browse-only 模式: {e}");
            remove_stale_control_files();
            return None;
        }
    };

    match validate_sidecar_health(&control, state.device_id.as_str(), &health) {
        Ok(port) => {
            state.actual_http_port.store(port, Ordering::SeqCst);
            tracing::info!("已验证独立后端 sidecar，GUI 进入 browse-only 模式，端口 {port}");
            Some(port)
        }
        Err(e) => {
            tracing::warn!("独立后端控制文件与健康响应不匹配，GUI 无法进入 browse-only 模式: {e}");
            remove_stale_control_files();
            None
        }
    }
}

/// 拉取 sidecar `/api/health` 响应。
///
/// Business Logic（为什么需要这个函数）:
///     控制文件可能因崩溃或升级残留，必须通过真实 HTTP 请求确认独立后端仍在运行。
///
/// Code Logic（这个函数做什么）:
///     用短超时 reqwest Client 请求 localhost health；非 2xx、网络错误或 JSON 解析失败都转为 AppError。
async fn fetch_sidecar_health(port: u16) -> Result<SidecarHealthResponse, AppError> {
    if port == 0 {
        return Err(AppError::generic("独立后端控制文件端口无效"));
    }

    let url = format!("http://127.0.0.1:{port}/api/health");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(SIDECAR_HEALTH_TIMEOUT_SECS))
        .build()
        .map_err(|e| AppError::generic(format!("构造 sidecar health client 失败: {e}")))?;
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::generic(format!("请求 sidecar health 失败 ({url}): {e}")))?;
    if !response.status().is_success() {
        return Err(AppError::generic(format!(
            "sidecar health 返回 HTTP {}",
            response.status()
        )));
    }
    response
        .json::<SidecarHealthResponse>()
        .await
        .map_err(|e| AppError::generic(format!("解析 sidecar health 响应失败: {e}")))
}

/// 校验 sidecar 控制文件和 health 响应是否匹配当前 GUI 设备。
///
/// Business Logic（为什么需要这个函数）:
///     stale 控制文件、端口复用或不同设备残留都不能让 GUI 误入 browse-only，否则会停止本进程后台任务。
///
/// Code Logic（这个函数做什么）:
///     校验 control.device_id、control.port、health.ok、health.device_id 和 health.http_port，成功返回可复用端口。
fn validate_sidecar_health(
    control: &control::BackendControlFile,
    expected_device_id: &str,
    health: &SidecarHealthResponse,
) -> Result<u16, AppError> {
    if control.device_id != expected_device_id {
        return Err(AppError::generic(format!(
            "独立后端 device_id 不匹配: expected {}, got {}",
            expected_device_id, control.device_id
        )));
    }

    if control.port == 0 {
        return Err(AppError::generic("独立后端控制文件端口无效"));
    }

    if !health.ok {
        return Err(AppError::generic("独立后端 health.ok=false"));
    }

    if health.device_id != expected_device_id {
        return Err(AppError::generic(format!(
            "独立后端 health device_id 不匹配: expected {}, got {}",
            expected_device_id, health.device_id
        )));
    }

    if health.http_port != control.port {
        return Err(AppError::generic(format!(
            "独立后端 health 端口不匹配: control {}, health {}",
            control.port, health.http_port
        )));
    }

    Ok(control.port)
}

/// 尽力清理 stale sidecar 控制文件。
///
/// Business Logic（为什么需要这个函数）:
///     一旦发现控制文件无效，保留它会让下一次 GUI 启动重复误判，需要主动清理残留。
///
/// Code Logic（这个函数做什么）:
///     调用 control::remove_control_files；失败只记录 warn，不阻断 GUI fallback 到 in-process 后端。
fn remove_stale_control_files() {
    if let Err(e) = control::remove_control_files() {
        tracing::warn!("清理独立后端控制文件失败: {e}");
    }
}

/// 只在对应取消令牌槽为空时启动后台任务。
///
/// Business Logic（为什么需要这个函数）:
///     后台 scheduler/collector 重复启动会产生重复扫描、重复调度或重复远端投递，必须集中防重。
///
/// Code Logic（这个函数做什么）:
///     持锁检查 `Option<CancellationToken>`；已存在则跳过，空槽则调用 start closure 并保存返回令牌。
fn start_cancelled_task_once<F>(slot: &Mutex<Option<CancellationToken>>, label: &str, start: F)
where
    F: FnOnce() -> CancellationToken,
{
    let mut guard = slot.lock().expect("后台任务取消令牌锁中毒");
    if guard.is_some() {
        tracing::warn!("{label} 已启动，跳过重复启动");
        return;
    }
    *guard = Some(start());
    tracing::info!("{label} 已启动");
}

/// 取消并移除一个运行时后台任务令牌。
///
/// Business Logic（为什么需要这个函数）:
///     退出时各后台任务都应只 cancel 一次并清空槽位，避免重复退出钩子产生误导日志。
///
/// Code Logic（这个函数做什么）:
///     从 `Option<CancellationToken>` 中 take；存在则调用 cancel 并记录日志，不存在则静默跳过。
fn cancel_runtime_token(slot: &Mutex<Option<CancellationToken>>, label: &str) {
    if let Some(token) = slot.lock().expect("后台任务取消令牌锁中毒").take() {
        token.cancel();
        tracing::info!("{label} 已停止");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 runtime sidecar 校验测试所需的控制文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     多个测试都需要一份匹配当前设备的独立后端控制文件，集中构造避免样板噪音。
    ///
    /// Code Logic（这个函数做什么）:
    ///     填充 pid、port、device_id 等 BackendControlFile 字段，返回可直接传入校验 helper 的值。
    fn control_file_for_test(device_id: &str, port: u16) -> control::BackendControlFile {
        control::BackendControlFile {
            pid: 1234,
            port,
            device_id: device_id.to_string(),
            device_name: "测试后端".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            control_token: "test-token".to_string(),
            control_schema_version: crate::backend::authority::CONTROL_SCHEMA_VERSION,
            owner_instance_id: Some("owner-test".to_string()),
        }
    }

    /// 验证 sidecar 健康响应端口必须匹配控制文件端口。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     stale 控制文件可能指向旧端口；GUI 不能仅凭控制文件存在就进入 browse-only。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 device_id 匹配但 health.http_port 不同的响应，断言校验 helper 拒绝该 sidecar。
    #[test]
    fn sidecar_health_validation_rejects_port_mismatch() {
        let control = control_file_for_test("device-a", 62116);
        let health = SidecarHealthResponse {
            ok: true,
            device_id: "device-a".to_string(),
            http_port: 62117,
        };

        let result = validate_sidecar_health(&control, "device-a", &health);

        assert!(result.is_err());
    }

    /// 验证 sidecar 控制文件和健康响应完全匹配时返回可复用端口。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI 只有确认独立后端健康且身份一致时，才应复用 sidecar 端口并跳过自身后台任务。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 device_id/port 全匹配的控制文件与 health 响应，断言校验 helper 返回控制文件端口。
    #[test]
    fn sidecar_health_validation_accepts_matching_control_and_health() {
        let control = control_file_for_test("device-a", 62116);
        let health = SidecarHealthResponse {
            ok: true,
            device_id: "device-a".to_string(),
            http_port: 62116,
        };

        let port = validate_sidecar_health(&control, "device-a", &health)
            .expect("matching sidecar health should be accepted");

        assert_eq!(port, 62116);
    }
}
