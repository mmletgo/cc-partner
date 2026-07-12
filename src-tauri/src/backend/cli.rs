//! backend/cli.rs — 独立 headless 后端 CLI。
//!
//! Business Logic（为什么需要这个模块）:
//!     远端设备或 GUI sidecar 模式需要无需桌面窗口即可启动 cc-partner 后端，并能通过
//!     `start|serve|stop|status` 管理本机后台进程。
//!
//! Code Logic（这个模块做什么）:
//!     提供 `run_from_env()` 入口；`serve` 装配共享 backend runtime 并等待 ctrl-c/control route；
//!     `start` detach 当前可执行文件的 serve 子进程；`status` 输出机器可读 JSON；`stop` 调本地控制 route。

use crate::backend::control::{self, BackendControlFile, BackendStatus, BackendStatusKind};
use crate::backend::runtime::{
    build_app_state, shutdown_backend_runtime, start_backend_services, start_background_tasks,
    BackendRuntimeMode,
};
use crate::backend::ui::{BackendUi, HeadlessBackendUi};
use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use serde::Serialize;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use uuid::Uuid;

const START_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(200);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

/// CLI status 中暴露的控制信息。
///
/// Business Logic（为什么需要这个结构）:
///     status 是机器可读入口，但不能泄露 stop control token；用户只需要 pid 和 HTTP port。
///
/// Code Logic（这个结构做什么）:
///     从 `BackendControlFile` 投影出 pid/port，并用 camelCase JSON 输出。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliControlStatus {
    pid: u32,
    port: u16,
}

/// CLI status 输出结构。
///
/// Business Logic（为什么需要这个结构）:
///     `start|status|stop` 都应输出稳定 JSON，便于 GUI 或脚本读取后端生命周期状态。
///
/// Code Logic（这个结构做什么）:
///     将内部 `BackendStatus` 转成不含敏感令牌的 JSON 视图。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliStatusOutput {
    kind: BackendStatusKind,
    control: Option<CliControlStatus>,
    error: Option<String>,
}

/// `/api/health` 响应中 status 检查需要的字段。
///
/// Business Logic（为什么需要这个结构）:
///     status 不能只看 pid 文件；端口可能被其它服务复用，必须校验 health 响应确实是 cc-partner 后端。
///
/// Code Logic（这个结构做什么）:
///     反序列化 health JSON 的 ok、device_id、http_port 字段，其它字段忽略。
#[derive(Debug, serde::Deserialize)]
struct BackendHealthResponse {
    ok: bool,
    device_id: String,
    http_port: u16,
}

/// stop control route 响应中 CLI 必须校验的字段。
///
/// Business Logic（为什么需要这个结构）:
///     HTTP 2xx 只代表本地 route 响应成功；CLI stop 还必须确认后端确实触发了 shutdown notifier。
///
/// Code Logic（这个结构做什么）:
///     反序列化 `{ok:boolean}`，其它字段忽略；`ok=false` 会被 stop 命令视为失败。
#[derive(Debug, serde::Deserialize)]
struct StopRouteResponse {
    ok: bool,
}

/// 从当前进程环境运行后端 CLI。
///
/// Business Logic（为什么需要这个函数）:
///     `src/bin/cc-partner-backend.rs` 需要一个稳定入口，把 CLI 执行结果转成进程退出码。
///
/// Code Logic（这个函数做什么）:
///     收集 `std::env::args()` 后委托命令分发；异步命令内部自建 Tokio runtime。
pub fn run_from_env() -> i32 {
    dispatch(std::env::args())
}

/// 分发 CLI 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     start/serve/stop/status 是独立用户入口，需要在同一套解析逻辑中保持用法和退出码一致。
///
/// Code Logic（这个函数做什么）:
///     解析第二个参数作为子命令；未知或缺失命令打印用法并返回非零；异步命令通过 Tokio runtime 执行。
fn dispatch<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    let command = args.get(1).map(String::as_str);
    let result = match command {
        Some("serve") => run_async(serve()),
        Some("start") => run_async(start()),
        Some("stop") => run_async(stop()),
        Some("status") => run_async(print_status()),
        _ => {
            eprintln!("用法: cc-partner-backend <start|serve|stop|status>");
            return 2;
        }
    };

    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

/// 测试用 CLI 命令分发入口。
///
/// Business Logic（为什么需要这个函数）:
///     单元测试需要验证命令解析错误路径，而不应读取真实环境参数或启动后端服务。
///
/// Code Logic（这个函数做什么）:
///     直接复用 `dispatch`，仅在测试编译时暴露给测试模块。
#[cfg(test)]
fn dispatch_for_test<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    dispatch(args)
}

/// 在独立 Tokio runtime 中运行异步 CLI 命令。
///
/// Business Logic（为什么需要这个函数）:
///     CLI bin 是同步 `main`，但 serve/status/stop/start 都需要 async HTTP、信号或 runtime 初始化能力。
///
/// Code Logic（这个函数做什么）:
///     创建 multi-thread Tokio runtime，enable_all 后 block_on 传入 future 并返回其结果。
fn run_async<F>(future: F) -> Result<(), AppError>
where
    F: std::future::Future<Output = Result<(), AppError>>,
{
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(future)
}

/// 运行 `serve` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     远端设备需要一个长期运行的 headless 后端，负责 HTTP/mDNS、后台任务和 Workbench 服务。
///
/// Code Logic（这个函数做什么）:
///     初始化 tracing 与 headless AppState；启动 HTTP/mDNS；先安装 shutdown notifier，再写控制文件并启动后台任务；
///     等待 ctrl-c 或 control route；
///     退出时 shutdown runtime、移除控制文件并清理 shutdown notifier。
async fn serve() -> Result<(), AppError> {
    init_tracing();
    let ui: Arc<dyn BackendUi> = Arc::new(HeadlessBackendUi::new(headless_dist_dir()));
    let state = build_app_state(ui).await?;
    let port = start_backend_services(&state, true, true).await?;
    let control = build_control_file(&state, port);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    control::install_shutdown_notifier(shutdown_tx);
    if let Err(error) = control::write_control_file(&control) {
        control::clear_shutdown_notifier();
        return Err(error);
    }
    start_background_tasks(&state, BackendRuntimeMode::Headless);
    crate::backend::logging::OperationLog::new(
        "control",
        "serve_start",
        crate::backend::logging::OperationResult::Ok,
    )
    .message(format!("cc-partner headless backend 已启动，监听端口 {port}"))
    .emit();

    wait_for_shutdown(shutdown_rx).await;
    control::clear_shutdown_notifier();
    shutdown_backend_runtime(&state);
    control::remove_control_files()?;
    crate::backend::logging::OperationLog::new(
        "control",
        "serve_stop",
        crate::backend::logging::OperationResult::Ok,
    )
    .message("cc-partner headless backend 已停止")
    .emit();
    Ok(())
}

/// 运行 `start` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要用短生命周期命令启动后台后端，并在命令返回后继续使用该服务。
///
/// Code Logic（这个函数做什么）:
///     先获取 data_dir 作用域跨进程 start 锁，再读状态：running 直接返回；stale 清控制文件；
///     否则 spawn 当前 exe 的 `serve` 子进程并轮询 status 最多 10 秒。锁在函数返回时释放。
async fn start() -> Result<(), AppError> {
    // 跨进程互斥：防止两个 start 同时看到 stopped 后各自 spawn serve。
    let _start_lock = control::acquire_start_lock(START_TIMEOUT).await?;

    let initial = current_status().await;
    match initial.kind {
        BackendStatusKind::Running => {
            println!("{}", render_status_json(&initial)?);
            return Ok(());
        }
        BackendStatusKind::Stale => {
            control::remove_control_files()?;
        }
        BackendStatusKind::Error => {
            return Err(AppError::generic(
                initial
                    .error
                    .unwrap_or_else(|| "后端状态读取失败".to_string()),
            ));
        }
        BackendStatusKind::Stopped => {}
    }

    // start detach 后 serve 子进程独立读取环境变量；必须显式继承 CC_PARTNER_DATA_DIR，
    // 保证 control/config/db/log 与父进程落在同一隔离根，避免写回用户真实 home。
    let current_exe = std::env::current_exe()?;
    let mut command = Command::new(current_exe);
    command
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    inherit_data_dir_env(&mut command);
    configure_detached_child(&mut command);
    let mut child = command.spawn()?;

    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Err(AppError::generic(format!(
                "serve 子进程过早退出，状态: {status}"
            )));
        }

        let status = current_status().await;
        if status.kind == BackendStatusKind::Running {
            println!("{}", render_status_json(&status)?);
            return Ok(());
        }
        tokio::time::sleep(STATUS_POLL_INTERVAL).await;
    }

    Err(AppError::generic("等待后端启动超时"))
}

/// 运行 `status` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     用户和脚本需要无需解析日志即可知道 headless 后端当前是否可用。
///
/// Code Logic（这个函数做什么）:
///     读取控制文件、pid 和 health 状态，渲染为不含控制令牌的 JSON 并输出到 stdout。
async fn print_status() -> Result<(), AppError> {
    let status = current_status().await;
    println!("{}", render_status_json(&status)?);
    Ok(())
}

/// 运行 `stop` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要通过短生命周期命令优雅停止 serve 进程，并清掉异常残留的控制文件。
///
/// Code Logic（这个函数做什么）:
///     对 running 状态调用本地 control stop route；随后轮询到 health 失败或 pid 退出；stale/stopped 直接清理并打印最终状态。
async fn stop() -> Result<(), AppError> {
    let status = current_status().await;
    let Some(control) = status.control.clone() else {
        println!("{}", render_status_json(&status)?);
        return Ok(());
    };

    if status.kind == BackendStatusKind::Stale {
        control::remove_control_files()?;
        println!("{}", render_status_json(&current_status().await)?);
        return Ok(());
    }

    if status.kind != BackendStatusKind::Running {
        println!("{}", render_status_json(&status)?);
        return Ok(());
    }

    request_stop_route(&control).await?;
    wait_until_stopped(&control).await?;
    control::remove_control_files()?;
    println!("{}", render_status_json(&current_status().await)?);
    Ok(())
}

/// 计算当前后端状态。
///
/// Business Logic（为什么需要这个函数）:
///     start/stop/status 都需要同一套状态判断，避免多个 CLI 命令对 stale/running 的认知不一致。
///
/// Code Logic（这个函数做什么）:
///     读取控制文件；若读取失败返回 Error；存在控制文件时检查 pid 存活与 HTTP health，再委托 `classify_status`。
async fn current_status() -> BackendStatus {
    let control = match control::read_control_file() {
        Ok(control) => control,
        Err(error) => return control::classify_status(None, false, false, Some(error.to_string())),
    };

    let (process_alive, health_ok) = match control.as_ref() {
        Some(control) => {
            let process_alive = process_is_alive(control.pid);
            let health_ok = health_ok(control).await;
            (process_alive, health_ok)
        }
        None => (false, false),
    };

    control::classify_status(control, process_alive, health_ok, None)
}

/// 渲染 CLI status JSON。
///
/// Business Logic（为什么需要这个函数）:
///     status 输出必须稳定、可解析且不泄露 control token，供用户和 GUI lifecycle 管理复用。
///
/// Code Logic（这个函数做什么）:
///     将内部状态映射成 CLI 视图后用 serde_json 序列化为单行 JSON。
fn render_status_json(status: &BackendStatus) -> Result<String, AppError> {
    let output = CliStatusOutput {
        kind: status.kind,
        control: status.control.as_ref().map(|control| CliControlStatus {
            pid: control.pid,
            port: control.port,
        }),
        error: status.error.clone(),
    };
    Ok(serde_json::to_string(&output)?)
}

/// 构造 serve 进程控制文件内容。
///
/// Business Logic（为什么需要这个函数）:
///     start/stop/status 需要通过磁盘控制文件跨进程共享 pid、端口、设备身份和 stop 令牌。
///
/// Code Logic（这个函数做什么）:
///     从 AppState 读取设备信息，填入当前进程 pid、实际 HTTP 端口、UTC 启动时间和随机 UUID token。
fn build_control_file(state: &AppState, port: u16) -> BackendControlFile {
    BackendControlFile {
        pid: std::process::id(),
        port,
        device_id: state.device_id.as_ref().clone(),
        device_name: state.device_name(),
        started_at: Utc::now().to_rfc3339(),
        control_token: Uuid::new_v4().to_string(),
    }
}

/// 等待 serve 关闭信号。
///
/// Business Logic（为什么需要这个函数）:
///     headless 后端应同时支持终端 Ctrl-C 和 `cc-partner-backend stop` 的 HTTP control 请求。
///
/// Code Logic（这个函数做什么）:
///     在 Tokio select 中等待 ctrl_c 或 watch receiver 变为 true，任一触发即返回。
async fn wait_for_shutdown(mut shutdown_rx: watch::Receiver<bool>) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = async {
            loop {
                if *shutdown_rx.borrow() {
                    break;
                }
                if shutdown_rx.changed().await.is_err() {
                    break;
                }
            }
        } => {}
    }
}

/// 请求本地 stop control route。
///
/// Business Logic（为什么需要这个函数）:
///     stop 命令不能直接 kill 进程；它应使用控制令牌请求 serve 进程自行执行 shutdown 清理。
///
/// Code Logic（这个函数做什么）:
///     POST `controlToken` 到 `127.0.0.1:<port>/api/backend/control/stop`；非成功状态转业务错误；
///     成功状态继续解析 `{ok}`，只有 `ok=true` 才视为 shutdown 请求已被接收。
async fn request_stop_route(control: &BackendControlFile) -> Result<(), AppError> {
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

/// 等待后端停止。
///
/// Business Logic（为什么需要这个函数）:
///     stop route 返回只表示关闭请求已送达；CLI 应等到 health 失败或 pid 退出，避免用户误以为仍在运行。
///
/// Code Logic（这个函数做什么）:
///     在 5 秒内轮询 pid 和 health；任一证明服务不可用即返回；超时返回错误，避免误删仍在运行的控制文件。
async fn wait_until_stopped(control: &BackendControlFile) -> Result<(), AppError> {
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
///
/// Code Logic（这个函数做什么）:
///     委托平台相关实现查询进程存在性；pid 为 0 直接视为无效。
fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    platform_process_is_alive(pid)
}

/// 让 start 拉起的 serve 子进程继承数据目录隔离环境变量。
///
/// Business Logic（为什么需要这个函数）:
///     smoke/CI 用 `CC_PARTNER_DATA_DIR` 隔离后端状态；`start` detach 后子进程若丢失该变量，
///     会把 control/db 写回真实 `~/.cc-partner`，破坏隔离并污染用户数据。
///
/// Code Logic（这个函数做什么）:
///     若当前进程设置了 `CC_PARTNER_DATA_DIR`，显式写入 child Command 的 env；
///     未设置则不改动（子进程默认继承父环境，保持生产行为）。
fn inherit_data_dir_env(command: &mut Command) {
    if let Some(value) = std::env::var_os("CC_PARTNER_DATA_DIR") {
        command.env("CC_PARTNER_DATA_DIR", value);
    }
}

/// 配置 Unix serve 子进程脱离父会话。
///
/// Business Logic（为什么需要这个函数）:
///     `start` 是短生命周期命令，父进程退出后 `serve` 必须继续运行，不能被终端 hangup 一并关闭。
///
/// Code Logic（这个函数做什么）:
///     在 child exec 前调用 `setsid()` 创建新 session；失败时让 spawn 返回对应 IO 错误。
#[cfg(unix)]
fn configure_detached_child(command: &mut Command) {
    apply_unix_detached_pre_exec(command);
}

/// Unix detached lifecycle 的生产 pre_exec seam（setsid 新会话/进程组）。
///
/// Business Logic（为什么需要这个函数）:
///     smoke 必须验证真实生产路径的进程组语义，而不是在测试里复制 setpgid/setsid 实现。
///
/// Code Logic（这个函数做什么）:
///     给 Command 安装 pre_exec：子进程 exec 前 `setsid()`；失败返回 last_os_error。
#[cfg(unix)]
pub fn apply_unix_detached_pre_exec(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Windows backend lifecycle 使用的 creation flags。
///
/// Business Logic（为什么需要这个函数）:
///     smoke/单元测试必须锁定生产路径真正使用的 DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP，
///     禁止在测试里复制字面量导致生产改坏仍绿。
///
/// Code Logic（这个函数做什么）:
///     返回 `DETACHED_PROCESS (0x8) | CREATE_NEW_PROCESS_GROUP (0x200)`。
#[cfg(windows)]
pub fn windows_detached_creation_flags() -> u32 {
    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
}

/// 配置 Windows serve 子进程脱离父控制台。
///
/// Business Logic（为什么需要这个函数）:
///     Windows 下 `start` 返回后 headless 后端也应独立存活，供后续 status/stop 管理。
///
/// Code Logic（这个函数做什么）:
///     设置 DETACHED_PROCESS 与 CREATE_NEW_PROCESS_GROUP creation flags（经可测 seam 计算）。
#[cfg(windows)]
fn configure_detached_child(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(windows_detached_creation_flags());
}

/// 配置其它平台 serve 子进程脱离父进程。
///
/// Business Logic（为什么需要这个函数）:
///     保留跨平台编译兜底，即使暂不支持特定平台也不阻断 CLI 构建。
///
/// Code Logic（这个函数做什么）:
///     当前无平台 API 可用时不修改 Command。
#[cfg(not(any(unix, windows)))]
fn configure_detached_child(_command: &mut Command) {}

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
///     若未来支持其它平台，CLI 不应因缺少平台 API 而无法编译。
///
/// Code Logic（这个函数做什么）:
///     暂时返回 false，让控制文件被归类为 stale。
#[cfg(not(any(unix, windows)))]
fn platform_process_is_alive(_pid: u32) -> bool {
    false
}

/// 返回 headless 移动端静态资源目录。
///
/// Business Logic（为什么需要这个函数）:
///     独立后端没有 Tauri asset resolver，但仍需尽量从本地 web/dist 服务 `/mobile`。
///
/// Code Logic（这个函数做什么）:
///     优先使用 `CC_PARTNER_WEB_DIST`；否则回退到源码树 `../web/dist`。
fn headless_dist_dir() -> PathBuf {
    std::env::var_os("CC_PARTNER_WEB_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../web/dist"))
}

/// 初始化 CLI tracing。
///
/// Business Logic（为什么需要这个函数）:
///     serve 运行时需要把 HTTP/mDNS/后台任务错误输出到 stderr，便于用户排查 headless 服务问题。
///
/// Code Logic（这个函数做什么）:
///     按 GUI 入口相同的 env-filter 规则初始化 tracing_subscriber；重复初始化错误被忽略。
fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,mdns_sd=off")),
        )
        .try_init();
}

#[cfg(test)]
mod tests {
    use crate::backend::control::{BackendControlFile, BackendStatus, BackendStatusKind};
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;
    use tokio::net::TcpListener;

    /// 验证 status 输出符合 CLI JSON 契约。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     用户和 GUI sidecar 管理逻辑都会把 `cc-partner-backend status` 当作机器可读入口，
    ///     输出必须是稳定 JSON，而不是人类日志文本。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 stopped 状态，调用 CLI JSON 渲染 helper，断言 kind/error/control 字段与 brief 中的契约一致。
    #[test]
    fn render_status_outputs_stable_json() {
        let status = BackendStatus {
            kind: BackendStatusKind::Stopped,
            control: None,
            error: None,
        };

        let rendered = super::render_status_json(&status).expect("status JSON should render");
        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("status output should be JSON");

        assert_eq!(parsed["kind"], "stopped");
        assert!(parsed["control"].is_null());
        assert!(parsed["error"].is_null());
    }

    /// 验证 start 子进程会显式继承 `CC_PARTNER_DATA_DIR`。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     detach 后 serve 必须与父进程共用同一隔离数据根，否则 control 文件会写回用户 home。
    ///
    /// Code Logic（这个测试做什么）:
    ///     设置环境变量后构造 Command，调用 inherit helper，断言 env 中含有该键值；
    ///     Drop 守卫保证 panic 后仍恢复环境。
    #[test]
    fn start_inherits_data_dir_env_for_detached_serve() {
        use std::ffi::OsString;
        use std::sync::{Mutex, MutexGuard, OnceLock};

        struct EnvGuard {
            _lock: MutexGuard<'static, ()>,
            previous: Option<OsString>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.previous {
                    Some(value) => std::env::set_var("CC_PARTNER_DATA_DIR", value),
                    None => std::env::remove_var("CC_PARTNER_DATA_DIR"),
                }
            }
        }

        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("data dir inherit 测试锁中毒");
        let previous = std::env::var_os("CC_PARTNER_DATA_DIR");
        std::env::set_var("CC_PARTNER_DATA_DIR", "/tmp/cc-partner-isolated");
        let _guard = EnvGuard {
            _lock: lock,
            previous,
        };

        let mut command = std::process::Command::new("true");
        super::inherit_data_dir_env(&mut command);
        // Command 不公开 env 查询 API；通过 debug 字符串粗检 env 注入（平台无关）。
        let debug = format!("{command:?}");
        assert!(
            debug.contains("CC_PARTNER_DATA_DIR") && debug.contains("cc-partner-isolated"),
            "start 子进程应继承数据目录 override，实际 Command: {debug}"
        );
    }

    /// 验证未知命令返回非零并提示用法。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     独立后端 CLI 是用户入口，输错命令时应明确失败，避免误以为服务已启动或停止。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入不存在的子命令，断言命令分发返回失败退出码。
    #[test]
    fn dispatch_rejects_unknown_command() {
        let exit_code = super::dispatch_for_test(["cc-partner-backend", "unknown"]);

        assert_ne!(exit_code, 0);
    }

    /// 构造 CLI stop 测试用控制文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     stop helper 只需要 pid、port、device_id 和 token；测试应避免重复拼接无关字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用传入端口生成匹配本机 HTTP 测试服务的 `BackendControlFile`。
    fn stop_control_for_test(port: u16) -> BackendControlFile {
        BackendControlFile {
            pid: std::process::id(),
            port,
            device_id: "device-a".to_string(),
            device_name: "测试设备".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            control_token: "expected-token".to_string(),
        }
    }

    /// 验证 stop route 返回 200 但 ok=false 时 CLI 仍失败。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     HTTP 200 只代表 route 收到请求；`ok=false` 表示 serve 进程没有真正收到 shutdown 通知，不能继续清理控制文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动本地 axum 测试 route 返回 `{ok:false}`，断言 `request_stop_route` 把它转换成业务错误。
    #[tokio::test]
    async fn request_stop_route_rejects_false_ok_response() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("应能绑定本地测试端口");
        let port = listener.local_addr().expect("应能读取本地端口").port();
        let app = Router::new().route(
            "/api/backend/control/stop",
            axum::routing::post(|| async { Json(json!({ "ok": false })) }),
        );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let error = super::request_stop_route(&stop_control_for_test(port))
            .await
            .expect_err("ok=false 应让 CLI stop 失败");

        server.abort();
        assert!(error.to_string().contains("ok=false"));
    }

    /// 验证等待停止超时时返回错误。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     stop 命令只有确认 pid 退出或 health 失败后才能清理控制文件；超时仍成功会掩盖后台进程继续运行。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动持续返回匹配 health 的本地服务，并使用当前测试进程 pid，让 `wait_until_stopped` 超时后必须返回 Err。
    #[tokio::test]
    async fn wait_until_stopped_errors_when_backend_stays_alive() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("应能绑定本地测试端口");
        let port = listener.local_addr().expect("应能读取本地端口").port();
        let app = Router::new().route(
            "/api/health",
            get(move || async move {
                Json(json!({
                    "ok": true,
                    "device_id": "device-a",
                    "device_name": "测试设备",
                    "http_port": port,
                    "ts": "2026-01-01T00:00:00Z"
                }))
            }),
        );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let error = super::wait_until_stopped(&stop_control_for_test(port))
            .await
            .expect_err("后端持续存活时等待停止应超时报错");

        server.abort();
        assert!(error.to_string().contains("等待后端停止超时"));
    }
}
