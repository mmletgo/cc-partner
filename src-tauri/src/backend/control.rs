use crate::config::config_dir;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const CONTROL_FILE_NAME: &str = "backend-control.json";
const PID_FILE_NAME: &str = "backend.pid";
const START_LOCK_FILE_NAME: &str = "backend-start.lock";
const SERVE_LOCK_FILE_NAME: &str = "backend-serve.lock";
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(200);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
static SHUTDOWN_NOTIFIER: OnceLock<Mutex<Option<watch::Sender<bool>>>> = OnceLock::new();

/// `/api/health` 响应中后端状态检查需要的字段。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 与 CLI 判断 sidecar 是否可用时不能只看 pid，必须确认控制文件端口返回的是当前设备的后端。
///
/// Code Logic（这个结构做什么）:
///     反序列化 health JSON 的 ok、device_id、http_port 字段，其它字段由 serde 忽略。
#[derive(Debug, Deserialize)]
struct BackendHealthResponse {
    ok: bool,
    device_id: String,
    http_port: u16,
}

/// stop control route 响应中必须校验的字段。
///
/// Business Logic（为什么需要这个结构）:
///     HTTP 2xx 只代表本地 route 响应成功；调用方还需要确认后端确实触发了 shutdown notifier。
///
/// Code Logic（这个结构做什么）:
///     反序列化 `{ok:boolean}`，其它字段忽略；`ok=false` 表示 stop 请求未真正生效。
#[derive(Debug, Deserialize)]
struct StopRouteResponse {
    ok: bool,
}

/// 独立后端进程写入磁盘的控制文件内容。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 和后续 `cc-partner-backend` CLI 需要用同一份控制文件识别后端进程、HTTP 端口和设备身份，
///     从而支持 start/stop/status 的跨进程协作；并携带 versioned owner 描述符供 GUI 判断权威性。
///
/// Code Logic（这个结构做什么）:
///     以 camelCase JSON 保存 pid、port、设备信息、启动时间、控制令牌、
///     `control_schema_version` 与可选 `owner_instance_id`；读写 helper 直接序列化/反序列化该结构。
///     schema/owner 字段带 serde default，legacy JSON 可先反序列化再由 authority 分类为 stale。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackendControlFile {
    pub pid: u32,
    pub port: u16,
    pub device_id: String,
    pub device_name: String,
    pub started_at: String,
    pub control_token: String,
    /// 控制文件 schema 版本；缺失时 serde default 为 0，分类为需重启。
    #[serde(default)]
    pub control_schema_version: u32,
    /// 本 sidecar 进程 owner 实例 id；legacy 文件为 None，不可作权威 owner。
    #[serde(default)]
    pub owner_instance_id: Option<String>,
}

impl BackendControlFile {
    /// 构造测试用控制文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单元测试只关心状态分类所需的最小 pid/port/device_id，不应为无关字段重复造样板数据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用传入 pid、port、device_id 填充关键字段，并给 device_name、started_at、control_token 提供稳定占位值；
    ///     schema/owner 默认为 legacy（0/None），需要权威描述符的测试再显式覆盖。
    #[cfg(test)]
    pub(crate) fn for_test(pid: u32, port: u16, device_id: &str) -> Self {
        Self {
            pid,
            port,
            device_id: device_id.to_string(),
            device_name: "test-device".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            control_token: "test-token".to_string(),
            control_schema_version: 0,
            owner_instance_id: None,
        }
    }
}

/// 后端状态分类。
///
/// Business Logic（为什么需要这个枚举）:
///     CLI status 和 GUI 生命周期管理需要把“没有控制文件、进程残留、健康检查失败、真实运行中”
///     这些情形压缩为用户可理解的固定状态。
///
/// Code Logic（这个枚举做什么）:
///     使用 camelCase 序列化输出 Running/Stopped/Stale/Error 四种状态，供后续命令层直接返回。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BackendStatusKind {
    Running,
    Stopped,
    Stale,
    Error,
}

/// 后端 status 查询结果。
///
/// Business Logic（为什么需要这个结构）:
///     用户执行 status 时不仅要看到最终状态，还需要在可用时看到控制文件细节，并在异常时看到错误原因。
///
/// Code Logic（这个结构做什么）:
///     包装状态枚举、可选控制文件和可选错误字符串，保持 camelCase JSON 输出契约。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendStatus {
    pub kind: BackendStatusKind,
    pub control: Option<BackendControlFile>,
    pub error: Option<String>,
}

/// 返回后端控制 JSON 文件路径。
///
/// Business Logic（为什么需要这个函数）:
///     GUI、CLI 与独立后端必须在同一用户配置目录下定位同一份控制文件，避免多处硬编码路径。
///     CLI/smoke 通过 `CC_PARTNER_DATA_DIR` 隔离时，控制文件也必须落在同一数据根。
///
/// Code Logic（这个函数做什么）:
///     基于 `config_dir()`（内部委托 `data_dir()`，支持 `CC_PARTNER_DATA_DIR` override）
///     派生 `backend-control.json` 的绝对路径；路径解析失败返回 Validation/IO 错误，不 panic。
pub fn control_file_path() -> Result<PathBuf, AppError> {
    Ok(config_dir()?.join(CONTROL_FILE_NAME))
}

/// 返回后端 pid 文件路径。
///
/// Business Logic（为什么需要这个函数）:
///     stop/status 等命令需要一个轻量 pid 文件与控制 JSON 并存，兼容只需读取进程号的后续逻辑。
///
/// Code Logic（这个函数做什么）:
///     基于 `config_dir()`（内部委托 `data_dir()`）派生 `backend.pid` 的绝对路径；
///     路径解析失败返回错误，不 panic。
pub fn pid_file_path() -> Result<PathBuf, AppError> {
    Ok(config_dir()?.join(PID_FILE_NAME))
}

/// 读取后端控制文件。
///
/// Business Logic（为什么需要这个函数）:
///     status 和 stop 需要先读取当前后端的进程、端口和控制令牌；文件不存在表示后端未启动。
///
/// Code Logic（这个函数做什么）:
///     若控制文件不存在返回 `Ok(None)`；存在则按 UTF-8 读取并反序列化为 `BackendControlFile`。
pub fn read_control_file() -> Result<Option<BackendControlFile>, AppError> {
    let path = control_file_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&content)?))
}

/// 写入后端控制文件和 pid 文件。
///
/// Business Logic（为什么需要这个函数）:
///     独立后端启动成功后需要发布自己的运行信息，GUI/CLI 才能发现并管理这个后端实例。
///
/// Code Logic（这个函数做什么）:
///     确保配置目录存在，将控制结构写成 pretty JSON UTF-8，并把 pid 单独写入 `backend.pid`。
pub fn write_control_file(control: &BackendControlFile) -> Result<(), AppError> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join(CONTROL_FILE_NAME),
        serde_json::to_string_pretty(control)?,
    )?;
    fs::write(dir.join(PID_FILE_NAME), control.pid.to_string())?;
    Ok(())
}

/// 删除后端控制文件和 pid 文件。
///
/// Business Logic（为什么需要这个函数）:
///     后端停止或发现 stale 状态后需要清理控制文件，避免下一次 status/start 误判旧实例仍可管理。
///
/// Code Logic（这个函数做什么）:
///     分别尝试删除控制 JSON 与 pid 文件；文件不存在视为清理成功，其它 IO 错误向上返回。
pub fn remove_control_files() -> Result<(), AppError> {
    for path in [control_file_path()?, pid_file_path()?] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

/// 返回 data_dir 作用域下的 start 互斥锁文件路径。
///
/// Business Logic（为什么需要这个函数）:
///     并发 `start` 必须按隔离数据根互斥，避免两个进程同时观察到 stopped 后双开 serve。
///
/// Code Logic（这个函数做什么）:
///     基于 `config_dir()` 派生 `backend-start.lock` 绝对路径。
pub fn start_lock_path() -> Result<PathBuf, AppError> {
    Ok(config_dir()?.join(START_LOCK_FILE_NAME))
}

/// 返回 data_dir 作用域下的 serve 单实例锁文件路径。
///
/// Business Logic（为什么需要这个函数）:
///     直接执行 `serve` 或异常重试时必须跨进程互斥，避免多个 writer 同时轮转 backend.log。
///
/// Code Logic（这个函数做什么）:
///     基于 `config_dir()` 派生 `backend-serve.lock` 绝对路径。
pub fn serve_lock_path() -> Result<PathBuf, AppError> {
    Ok(config_dir()?.join(SERVE_LOCK_FILE_NAME))
}

/// data_dir 作用域的跨进程 start 锁守卫。
///
/// Business Logic（为什么需要这个结构）:
///     `start` 的 check-then-spawn 不是原子的；必须用磁盘锁串行化同一 data_dir 上的启动声明。
///
/// Code Logic（这个结构做什么）:
///     持有已 `try_lock`/`lock` 成功的 OS 文件锁句柄；进程退出或 Drop 时内核自动释放，
///     不依赖删除路径，避免 create_new+pid 文件的 ABA / 空文件误回收竞态。
pub struct StartLockGuard {
    path: PathBuf,
    file: fs::File,
    ownership_token: String,
}

impl Drop for StartLockGuard {
    /// 释放 start 锁。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     无论 start 成功、超时还是 panic，都必须释放 data_dir 级启动互斥，避免永久卡死后续 start。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当锁文件仍包含本守卫写入的 ownership token 时才 best-effort 清理诊断内容并 unlock；
    ///     文件删除不是互斥前提——OS 文件锁在 fd 关闭时自动释放。
    fn drop(&mut self) {
        if lock_file_owned_by(&self.path, &self.ownership_token) {
            // 诊断字段可清，但不要 unlink：其它进程可能已 open 同一路径等待 lock。
            let _ = self.file.set_len(0);
        }
        // 全限定调用 fs4，避免解析到 std::fs::File::unlock（MSRV 1.89+）。
        if let Err(err) = fs4::FileExt::unlock(&self.file) {
            // 路径可能含 home/用户名，走脱敏 helper；不记录锁文件内容
            crate::backend::logging::OperationLog::new(
                "control",
                "start_lock_release",
                crate::backend::logging::OperationResult::Error,
            )
            .level(tracing::Level::WARN)
            .error_code("internal")
            .message(format!(
                "释放 backend start 锁失败 path={:?}: {err}",
                self.path
            ))
            .emit();
        }
    }
}

/// 在有界时间内获取 data_dir 作用域的跨进程 start 锁。
///
/// Business Logic（为什么需要这个函数）:
///     两个并发 `start` 都可能读到 stopped 并各自 spawn serve，随后竞争 control 文件留下孤儿进程。
///     需要按隔离根原子声明“我正在启动”。
///
/// Code Logic（这个函数做什么）:
///     打开（或创建）`backend-start.lock` 后用 OS 级 exclusive lock（`fs4::FileExt::try_lock`，
///     全限定调用以避开 std 1.89+ 同名方法与 crate MSRV 1.77 冲突）抢占；
///     成功后写入 ownership token + pid 作诊断。进程崩溃时内核自动释放锁，无需 pid 回收，
///     也消除了“空/半写锁文件被误删”导致双持有的 ABA。轮询 try_lock 直到 deadline。
pub async fn acquire_start_lock(timeout: Duration) -> Result<StartLockGuard, AppError> {
    let path = start_lock_path()?;
    acquire_named_lock(path, "start", timeout).await
}

/// 在有界时间内获取 data_dir 作用域的跨进程 serve 单实例锁。
///
/// Business Logic（为什么需要这个函数）:
///     serve 进程是 backend.log 的唯一写入方，必须在打开日志前抢到覆盖整个生命周期的实例锁，
///     否则并发 serve 会破坏轮转精确性与 control 文件一致性。
///
/// Code Logic（这个函数做什么）:
///     打开/创建 `backend-serve.lock` 后用 OS exclusive lock 抢占；成功后写入 ownership token + pid；
///     守卫持有直到 serve 退出（Drop 时内核释放）。
pub async fn acquire_serve_lock(timeout: Duration) -> Result<StartLockGuard, AppError> {
    let path = serve_lock_path()?;
    acquire_named_lock(path, "serve", timeout).await
}

/// 在有界时间内获取 data_dir 作用域的命名跨进程锁。
///
/// Business Logic（为什么需要这个函数）:
///     start 与 serve 需要同一套 OS 文件锁语义，避免重复实现导致 ABA/空文件误回收。
///
/// Code Logic（这个函数做什么）:
///     确保目录存在后循环 `try_lock`；成功写入 ownership payload 并返回守卫；超时返回 conflict。
async fn acquire_named_lock(
    path: PathBuf,
    kind: &str,
    timeout: Duration,
) -> Result<StartLockGuard, AppError> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    let deadline = Instant::now() + timeout;

    loop {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        // 全限定调用 fs4，避免 rustc 优先解析到 std::fs::File::try_lock（MSRV 1.89+）。
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {
                let ownership_token = uuid::Uuid::new_v4().to_string();
                write_start_lock_payload(&mut file, &ownership_token)?;
                return Ok(StartLockGuard {
                    path,
                    file,
                    ownership_token,
                });
            }
            Err(fs4::TryLockError::WouldBlock) => {
                drop(file);
                if Instant::now() >= deadline {
                    return Err(AppError::conflict(format!(
                        "获取 backend {kind} 锁超时（{timeout:?}）: {kind} 锁被占用: {}",
                        path.display()
                    )));
                }
                tokio::time::sleep(STATUS_POLL_INTERVAL).await;
            }
            Err(fs4::TryLockError::Error(err)) => return Err(err.into()),
        }
    }
}

/// 把 ownership token 与持有者 pid 写入 start 锁文件（仅诊断，不参与互斥）。
///
/// Business Logic（为什么需要这个函数）:
///     OS 文件锁保证互斥；锁文件内容仅用于排障时识别谁持有锁，避免再走 pid 回收路径。
///
/// Code Logic（这个函数做什么）:
///     truncate 后写入 `token=`/`pid=` 两行并 flush。
fn write_start_lock_payload(file: &mut fs::File, ownership_token: &str) -> Result<(), AppError> {
    file.set_len(0)?;
    let payload = format!("token={ownership_token}\npid={}\n", std::process::id());
    file.write_all(payload.as_bytes())?;
    file.flush()?;
    Ok(())
}

/// 判断 start 锁文件是否仍由给定 ownership token 持有。
///
/// Business Logic（为什么需要这个函数）:
///     Drop 时不应误清他人写入的诊断内容；仅 token 匹配才允许 truncate。
///
/// Code Logic（这个函数做什么）:
///     读锁文件，解析 `token=` 行并与期望 token 比较；读失败视为不匹配。
fn lock_file_owned_by(path: &std::path::Path, ownership_token: &str) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    content.lines().any(|line| {
        line.strip_prefix("token=")
            .map(|raw| raw.trim() == ownership_token)
            .unwrap_or(false)
    })
}

/// 返回进程内后端关闭通知槽。
///
/// Business Logic（为什么需要这个函数）:
///     headless `serve` 进程需要让 HTTP control route 唤醒自身退出，但 route 层不能依赖 CLI 模块。
///
/// Code Logic（这个函数做什么）:
///     用 `OnceLock` 延迟初始化一个 `Mutex<Option<watch::Sender<bool>>>`，供 serve 安装、route 触发、退出清理。
fn shutdown_notifier_slot() -> &'static Mutex<Option<watch::Sender<bool>>> {
    SHUTDOWN_NOTIFIER.get_or_init(|| Mutex::new(None))
}

/// 安装 headless 后端关闭通知器。
///
/// Business Logic（为什么需要这个函数）:
///     `cc-partner-backend serve` 启动后必须把自己的 shutdown receiver 暴露给本进程 HTTP route，
///     这样 `stop` 命令才能通过本地 control API 请求它优雅关闭。
///
/// Code Logic（这个函数做什么）:
///     将 watch sender 写入全局通知槽；后续同进程 route 调 `request_backend_shutdown` 会发送 true。
pub fn install_shutdown_notifier(sender: watch::Sender<bool>) {
    let mut guard = shutdown_notifier_slot().lock().expect("后端关闭通知锁中毒");
    *guard = Some(sender);
}

/// 清理 headless 后端关闭通知器。
///
/// Business Logic（为什么需要这个函数）:
///     serve 退出后不应保留旧 sender，否则测试或后续同进程重启会向已经失效的控制通道发送信号。
///
/// Code Logic（这个函数做什么）:
///     将全局通知槽重置为 None。
pub fn clear_shutdown_notifier() {
    let mut guard = shutdown_notifier_slot().lock().expect("后端关闭通知锁中毒");
    *guard = None;
}

/// 请求 headless 后端优雅关闭。
///
/// Business Logic（为什么需要这个函数）:
///     本地 stop route 通过 token 校验后需要通知 serve 主循环退出，进而执行 runtime shutdown 和控制文件清理。
///
/// Code Logic（这个函数做什么）:
///     若已安装 watch sender，则发送 true 并返回是否发送成功；未安装 sender 时返回 false。
pub fn request_backend_shutdown() -> bool {
    let guard = shutdown_notifier_slot().lock().expect("后端关闭通知锁中毒");
    guard
        .as_ref()
        .map(|sender| sender.send(true).is_ok())
        .unwrap_or(false)
}

/// 根据控制文件、进程存活、健康检查和错误信息生成后端状态。
///
/// Business Logic（为什么需要这个函数）:
///     CLI/GUI 需要统一的状态判定口径：没有控制文件是停止，有错误优先报错，只有进程和健康检查都通过才是运行中。
///
/// Code Logic（这个函数做什么）:
///     按 `error -> Error`、`None -> Stopped`、`process_alive && health_ok -> Running`，
///     其它组合归为 `Stale`，并保留原控制文件与错误详情。
pub fn classify_status(
    control: Option<BackendControlFile>,
    process_alive: bool,
    health_ok: bool,
    error: Option<String>,
) -> BackendStatus {
    // Stale 仅表示「控制文件残留但 pid 已死」。pid 存活而 health 瞬时失败仍为 Running，
    // 禁止误删 control 导致 serve lock 孤儿（Codex: 瞬时 health 切断控制面）。
    let kind = if error.is_some() {
        BackendStatusKind::Error
    } else if control.is_none() {
        BackendStatusKind::Stopped
    } else if process_alive {
        let _ = health_ok; // health 失败时仍 Running；调用方可再 probe health
        BackendStatusKind::Running
    } else {
        BackendStatusKind::Stale
    };

    BackendStatus {
        kind,
        control,
        error,
    }
}

/// 计算当前独立后端状态。
///
/// Business Logic（为什么需要这个函数）:
///     GUI Tauri command、GUI setup 和 CLI 都需要同一套状态判断，避免对 stale/running 的认知分叉。
///
/// Code Logic（这个函数做什么）:
///     读取控制文件；存在控制文件时检查 pid 存活与 HTTP health，再委托 `classify_status` 生成统一 DTO。
pub async fn current_status() -> BackendStatus {
    let control = match read_control_file() {
        Ok(control) => control,
        Err(error) => return classify_status(None, false, false, Some(error.to_string())),
    };

    let (process_alive, health_ok) = match control.as_ref() {
        Some(control) => {
            let process_alive = process_is_alive(control.pid);
            let health_ok = health_ok(control).await;
            (process_alive, health_ok)
        }
        None => (false, false),
    };

    classify_status(control, process_alive, health_ok, None)
}

/// 请求运行中的独立后端停止。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 的“前后端都关闭”和 CLI stop 都需要通过 control token 请求 serve 进程优雅退出，而不是直接 kill。
///
/// Code Logic（这个函数做什么）:
///     POST `controlToken` 到本机 `/api/backend/control/stop`；非 2xx、JSON 解析失败或 `ok=false` 都返回业务错误。
pub async fn request_stop_route(control: &BackendControlFile) -> Result<(), AppError> {
    let client = reqwest::Client::builder()
        .timeout(HEALTH_TIMEOUT)
        .build()
        .map_err(|error| AppError::generic(format!("构造 stop client 失败: {error}")))?;
    let url = format!("http://127.0.0.1:{}/api/backend/control/stop", control.port);
    let response = client
        .post(&url)
        .json(&serde_json::json!({ "controlToken": control.control_token }))
        .send()
        .await
        .map_err(|error| AppError::generic(format!("请求 stop route 失败: {error}")))?;

    if !response.status().is_success() {
        return Err(AppError::generic(format!(
            "stop route 返回 HTTP {}",
            response.status()
        )));
    }

    let stop_response = response
        .json::<StopRouteResponse>()
        .await
        .map_err(|error| AppError::generic(format!("解析 stop route 响应失败: {error}")))?;
    if !stop_response.ok {
        return Err(AppError::generic(
            "stop route 返回 ok=false，后端未触发 shutdown",
        ));
    }

    Ok(())
}

/// 等待独立后端停止。
///
/// Business Logic（为什么需要这个函数）:
///     stop route 返回只表示关闭请求已送达；GUI/CLI 应等待 health 失败或 pid 退出后再清理控制文件。
///
/// Code Logic（这个函数做什么）:
///     在固定超时时间内轮询 pid 和 health；任一证明服务不可用即返回，超时返回错误避免误删仍运行的控制文件。
pub async fn wait_until_stopped(control: &BackendControlFile) -> Result<(), AppError> {
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !process_is_alive(control.pid) || !health_ok(control).await {
            return Ok(());
        }
        tokio::time::sleep(STATUS_POLL_INTERVAL).await;
    }
    Err(AppError::generic(format!(
        "等待后端停止超时: pid={}, port={}",
        control.pid, control.port
    )))
}

/// 通过控制文件停止独立后端并返回最终状态。
///
/// Business Logic（为什么需要这个函数）:
///     GUI full-close 路径需要在退出前尽力停止后台 sidecar；stale/stopped 状态应幂等处理，不能误报失败。
///
/// Code Logic（这个函数做什么）:
///     读取当前状态；stale 直接清理控制文件；running 走 stop route + wait + 清理；其它状态原样返回。
pub async fn stop_backend_process() -> Result<BackendStatus, AppError> {
    let status = current_status().await;
    let Some(control) = status.control.clone() else {
        return Ok(status);
    };

    if status.kind == BackendStatusKind::Stale {
        remove_control_files()?;
        return Ok(current_status().await);
    }

    if status.kind != BackendStatusKind::Running {
        return Ok(status);
    }

    request_stop_route(&control).await?;
    wait_until_stopped(&control).await?;
    remove_control_files()?;
    Ok(current_status().await)
}

/// 检查控制文件对应的 health 是否可用。
///
/// Business Logic（为什么需要这个函数）:
///     pid 存活不代表该端口仍是当前 cc-partner 后端；health 响应要与控制文件设备和端口匹配。
///
/// Code Logic（这个函数做什么）:
///     GET `/api/health`，成功解析后校验 ok、device_id、http_port；任何失败都返回 false。
async fn health_ok(control: &BackendControlFile) -> bool {
    if control.port == 0 {
        return false;
    }
    let url = format!("http://127.0.0.1:{}/api/health", control.port);
    let client = match reqwest::Client::builder().timeout(HEALTH_TIMEOUT).build() {
        Ok(client) => client,
        Err(_) => return false,
    };
    let response = match client.get(url).send().await {
        Ok(response) if response.status().is_success() => response,
        _ => return false,
    };
    let health = match response.json::<BackendHealthResponse>().await {
        Ok(health) => health,
        Err(_) => return false,
    };
    health.ok && health.device_id == control.device_id && health.http_port == control.port
}

/// 检查 pid 是否仍存活。
///
/// Business Logic（为什么需要这个函数）:
///     stale 控制文件的常见形态是 pid 已退出；status/start/stop 都需要先识别这种残留。
///     doctor 在区分“可恢复 stale”与“端口被占/进程存活但不可达”时也需要同一口径。
///
/// Code Logic（这个函数做什么）:
///     委托平台相关实现查询进程存在性；pid 为 0 直接视为无效。
pub(crate) fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    platform_process_is_alive(pid)
}

/// Unix 平台进程存活检查。
///
/// Business Logic（为什么需要这个函数）:
///     macOS/Linux headless 后端需要用本机工具判断 pid 文件是否仍指向活进程。
///
/// Code Logic（这个函数做什么）:
///     执行 `kill -0 <pid>`，成功表示进程存在且当前用户可探测。
#[cfg(unix)]
fn platform_process_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Windows 平台进程存活检查。
///
/// Business Logic（为什么需要这个函数）:
///     Windows 用户同样需要 status/start/stop 正确识别 stale pid 文件。
///
/// Code Logic（这个函数做什么）:
///     使用系统 `tasklist` 过滤 PID，并在输出中查找目标 pid。
#[cfg(windows)]
fn platform_process_is_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .stdin(Stdio::null())
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
        })
        .unwrap_or(false)
}

/// 兜底平台进程存活检查。
///
/// Business Logic（为什么需要这个函数）:
///     若未来支持其它平台，状态判断不应因缺少平台 API 而无法编译。
///
/// Code Logic（这个函数做什么）:
///     暂时返回 false，让控制文件被归类为 stale。
#[cfg(not(any(unix, windows)))]
fn platform_process_is_alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::authority::classify_control_descriptor;

    /// 验证控制文件 round-trip 保留 owner 描述符 camelCase 字段。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI 与 sidecar 通过控制文件交换 schema/owner id；序列化契约必须稳定为 camelCase。
    ///
    /// Code Logic（这个测试做什么）:
    ///     设置 control_schema_version 与 owner_instance_id 后 to_value，断言 camelCase 键值。
    #[test]
    fn control_file_round_trips_owner_descriptor() {
        let mut file = BackendControlFile::for_test(1, 62116, "device-a");
        file.control_schema_version = 2;
        file.owner_instance_id = Some("owner-a".to_string());
        let value = serde_json::to_value(&file).unwrap();
        assert_eq!(value["controlSchemaVersion"], 2);
        assert_eq!(value["ownerInstanceId"], "owner-a");
    }

    /// 验证 legacy 控制文件可反序列化但被分类为 needs_restart，不可作权威。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     旧 sidecar 写出的控制文件缺 schema/owner；GUI 必须提示重启，不能伪装实时成功。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化无 schema/owner 的 legacy JSON，断言 classify_control_descriptor.needs_restart()。
    #[test]
    fn legacy_control_file_is_stale_not_authoritative() {
        let legacy = serde_json::json!({
            "pid": 1,
            "port": 62116,
            "controlToken": "x",
            "deviceId": "device-a",
            "deviceName": "Desk A",
            "startedAt": "2026-07-14T00:00:00Z"
        });
        let parsed: BackendControlFile = serde_json::from_value(legacy).unwrap();
        assert!(classify_control_descriptor(&parsed).needs_restart());
    }

    /// 验证缺少控制文件时状态为停止。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     用户查询后端状态时，未启动过独立后端应看到 Stopped，而不是错误或残留状态。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入空控制文件、进程未存活、健康检查失败且无错误，断言分类结果为 Stopped 且不携带控制文件。
    #[test]
    fn classify_status_reports_stopped_without_control_file() {
        let status = classify_status(None, false, false, None);
        assert_eq!(status.kind, BackendStatusKind::Stopped);
        assert!(status.control.is_none());
    }

    /// 验证错误信息优先于缺失控制文件。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     status 查询过程中如果读取控制文件或健康检查发生错误，用户需要先看到 Error，避免真实故障被误报为未启动。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入空控制文件和错误字符串，断言 `classify_status` 返回 Error，并保留错误详情。
    #[test]
    fn classify_status_reports_error_when_error_exists_without_control_file() {
        let status = classify_status(None, false, false, Some("read failed".to_string()));
        assert_eq!(status.kind, BackendStatusKind::Error);
        assert_eq!(status.error.as_deref(), Some("read failed"));
    }

    /// 验证控制文件存在但 pid 不存活时状态为 stale。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     后端异常退出后可能留下控制文件，用户查询状态时应看到 Stale 以便后续清理残留。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造测试控制文件并传入进程未存活、健康检查失败且无错误，断言分类结果为 Stale。
    #[test]
    fn classify_status_reports_stale_when_pid_dead() {
        let control = BackendControlFile::for_test(1234, 62116, "device-a");
        let status = classify_status(Some(control), false, false, None);
        assert_eq!(status.kind, BackendStatusKind::Stale);
    }

    /// 验证 pid 存活时即使 health 失败仍为 running（瞬时 health 不得拆控制面）。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     单次 health 超时不得把仍存活的 owner 标 Stale 并删除控制文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     process_alive=true、health_ok=false → Running；process_alive=false → Stale。
    #[test]
    fn classify_status_reports_running_when_pid_alive_even_if_health_fails() {
        let control = BackendControlFile::for_test(1234, 62116, "device-a");
        let status = classify_status(Some(control.clone()), true, false, None);
        assert_eq!(status.kind, BackendStatusKind::Running);
        assert_eq!(status.control.unwrap().pid, 1234);

        let status_ok = classify_status(Some(control.clone()), true, true, None);
        assert_eq!(status_ok.kind, BackendStatusKind::Running);
    }
}
