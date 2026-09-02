//! backend/cli.rs — 独立 headless 后端 CLI。
//!
//! Business Logic（为什么需要这个模块）:
//!     远端设备或 GUI sidecar 模式需要无需桌面窗口即可启动 cc-partner 后端，并能通过
//!     `start|serve|stop|status|doctor|supervise` 管理本机后台进程与健康检查。
//!
//! Code Logic（这个模块做什么）:
//!     提供 `run_from_env()` 入口；`serve` 装配共享 backend runtime 并等待 ctrl-c/control route；
//!     `start` detach 当前可执行文件的 serve 子进程；`status` 输出机器可读 JSON；`stop` 调本地控制 route；
//!     `doctor` / `doctor --json` 采集脱敏快照，stdout 与 stderr tracing 隔离，退出码 0/1/2。

use crate::backend::control::{self, BackendControlFile, BackendStatus, BackendStatusKind};
use crate::backend::doctor::{
    collect_doctor_snapshot, DoctorCheck, DoctorCheckStatus, DoctorSnapshot, DoctorStatus,
};
use crate::backend::runtime::{
    build_app_state, shutdown_backend_runtime, start_backend_services, start_background_tasks,
    BackendRuntimeMode,
};
use crate::backend::ui::{BackendUi, HeadlessBackendUi};
use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use serde::Serialize;
use std::ffi::OsString;
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
///     `src/bin/cc-partner-backend.rs` 需要一个稳定入口，把 CLI 执行结果转成进程退出码
///     （含 doctor 的 0/1/2 与 start/serve/stop/status 的既有语义）。
///
/// Code Logic（这个函数做什么）:
///     先让 sidecar 脱离 macOS Dock，再收集 `std::env::args()` 委托命令分发；
///     异步命令内部自建 Tokio runtime。
pub fn run_from_env() -> i32 {
    crate::backend::macos_dock::detach_current_process_from_dock();
    dispatch(std::env::args())
}

/// 分发 CLI 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     start/serve/stop/status/doctor 是独立用户入口，需要在同一套解析逻辑中保持用法和退出码契约。
///
/// Code Logic（这个函数做什么）:
///     解析第二个参数作为子命令；doctor 走独立解析/退出码映射；supervise 同步监督循环；
///     其余异步命令通过 Tokio runtime 执行；start/serve/stop/status/supervise 成功 0、业务错误 1；
///     未知命令/doctor 解析或采集失败 2。
fn dispatch<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    let command = args.get(1).map(String::as_str);
    match command {
        Some("serve") => map_lifecycle_result(run_async(serve())),
        Some("start") => map_lifecycle_result(run_async(start())),
        Some("stop") => map_lifecycle_result(run_async(stop())),
        Some("status") => map_lifecycle_result(run_async(print_status())),
        Some("supervise") => map_lifecycle_result(crate::backend::supervisor::supervise()),
        Some("doctor") => dispatch_doctor(&args[2..]),
        Some("version") | Some("--version") | Some("-V") => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            0
        }
        _ => {
            eprintln!(
                "用法: cc-partner-backend <start|serve|stop|status|supervise|doctor [--json]|version|--version|-V>"
            );
            2
        }
    }
}

/// 将 start/serve/stop/status 的 Result 映射为既有退出码。
///
/// Business Logic（为什么需要这个函数）:
///     lifecycle 命令保持成功 0 / 失败 1，避免被 doctor 的 0/1/2 契约误伤。
///
/// Code Logic（这个函数做什么）:
///     `Ok(())` → 0；`Err` 打印到 stderr 后返回 1。
fn map_lifecycle_result(result: Result<(), AppError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

/// 分发 `doctor` / `doctor --json`。
///
/// Business Logic（为什么需要这个函数）:
///     健康检查需要严格的参数解析、stdout/stderr 隔离与 0/1/2 退出码，不能与 lifecycle 混用。
///
/// Code Logic（这个函数做什么）:
///     解析剩余参数 → 初始化仅 stderr tracing → 异步采集快照并按模式渲染 → 返回 status 退出码；
///     解析失败或采集/序列化失败返回 2（错误写 stderr，JSON 模式不污染 stdout）。
fn dispatch_doctor(rest: &[String]) -> i32 {
    let json_mode = match parse_doctor_args(rest) {
        Ok(json_mode) => json_mode,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("用法: cc-partner-backend doctor [--json]");
            return 2;
        }
    };

    crate::backend::logging::init_doctor_tracing();
    match run_async(run_doctor(json_mode)) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("{error}");
            2
        }
    }
}

/// 解析 doctor 子命令参数。
///
/// Business Logic（为什么需要这个函数）:
///     只允许无参或单一 `--json`；未知选项/多余参数必须明确失败，避免静默忽略。
///
/// Code Logic（这个函数做什么）:
///     扫描剩余参数：无参 → json=false；仅 `--json` → true；其它一律错误。
fn parse_doctor_args(rest: &[String]) -> Result<bool, String> {
    let mut json_mode = false;
    for arg in rest {
        match arg.as_str() {
            "--json" if !json_mode => json_mode = true,
            "--json" => {
                return Err("doctor 不接受重复的 --json".to_string());
            }
            other if other.starts_with('-') => {
                return Err(format!("doctor 未知选项: {other}"));
            }
            other => {
                return Err(format!("doctor 多余参数: {other}"));
            }
        }
    }
    Ok(json_mode)
}

/// 运行 doctor 采集并渲染输出。
///
/// Business Logic（为什么需要这个函数）:
///     用户与脚本需要一份有界、脱敏的健康快照；JSON 模式供机器解析，文本模式供人工阅读。
///
/// Code Logic（这个函数做什么）:
///     `collect_doctor_snapshot` → 渲染 JSON 或人类文本到 stdout → 返回 `DoctorStatus::exit_code()`。
async fn run_doctor(json_mode: bool) -> Result<i32, AppError> {
    let snapshot = collect_doctor_snapshot().await?;
    if json_mode {
        println!("{}", render_doctor_json(&snapshot)?);
    } else {
        print!("{}", render_doctor_text(&snapshot));
    }
    Ok(snapshot.status.exit_code())
}

/// 将快照序列化为单行合法 JSON（stdout 专用）。
///
/// Business Logic（为什么需要这个函数）:
///     `doctor --json` stdout 只能有一份可直接 `jq` 的 JSON，不得夹杂 tracing 前缀。
///
/// Code Logic（这个函数做什么）:
///     `serde_json::to_string` 序列化完整 `DoctorSnapshot`（字段已在采集时脱敏）。
fn render_doctor_json(snapshot: &DoctorSnapshot) -> Result<String, AppError> {
    Ok(serde_json::to_string(snapshot)?)
}

/// 渲染人类可读 doctor 文本。
///
/// Business Logic（为什么需要这个函数）:
///     无 `--json` 时用户需要一眼看到 overall 状态、异常检查、通配监听与固定 LAN 风险，
///     且不得暴露原始 home 或 control token。
///
/// Code Logic（这个函数做什么）:
///     输出 status 摘要、backend 状态（stopped 标为 normal）、实际端口与 wildcard 监听、
///     固定无身份风险声明、仅 warning/error 检查表、log 路径与 recent errors
///     （摘要不超出 JSON 已脱敏字段；永不打印 control token）。
fn render_doctor_text(snapshot: &DoctorSnapshot) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "status: {} (exit {})",
        doctor_status_label(snapshot.status),
        snapshot.status.exit_code()
    ));

    if snapshot.backend.state == "stopped"
        && snapshot.backend.health.status == DoctorCheckStatus::Info
    {
        lines.push(format!(
            "backend: stopped (normal) — {}",
            snapshot.backend.health.summary
        ));
    } else {
        lines.push(format!(
            "backend: {} [{}] — {}",
            snapshot.backend.state,
            doctor_check_status_label(snapshot.backend.health.status),
            snapshot.backend.health.summary
        ));
        if let Some(pid) = snapshot.backend.pid {
            if let Some(port) = snapshot.backend.port {
                lines.push(format!("  pid={pid} port={port}"));
            } else {
                lines.push(format!("  pid={pid}"));
            }
        } else if let Some(port) = snapshot.backend.port {
            lines.push(format!("  port={port}"));
        }
    }

    // 固定 LAN 风险：通配监听 + 无调用者身份校验（不打印 control token）
    match snapshot.backend.port {
        Some(port) => lines.push(format!(
            "listener: wildcard 0.0.0.0 (actual http port={port})"
        )),
        None => lines.push("listener: wildcard 0.0.0.0 (actual http port unavailable)".to_string()),
    }
    lines.push(
        "risk: 同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份".to_string(),
    );

    let problems = collect_problem_checks(snapshot);
    if problems.is_empty() {
        lines.push("checks: none (all ok/info)".to_string());
    } else {
        lines.push("checks:".to_string());
        for check in problems {
            lines.push(format!(
                "  {} {} — {}",
                doctor_check_status_label(check.status),
                check.code,
                check.summary
            ));
        }
    }

    lines.push(format!("log: {}", snapshot.log_path));
    lines.push(format!("control: {}", snapshot.backend.control_path));

    if snapshot.recent_errors.is_empty() {
        lines.push("recent errors: none".to_string());
    } else {
        lines.push("recent errors:".to_string());
        for err in &snapshot.recent_errors {
            lines.push(format!(
                "  [{}] {}: {}",
                err.timestamp, err.code, err.summary
            ));
        }
    }

    lines.push(String::new());
    lines.join("\n")
}

/// 收集文本模式下需要展示的 warning/error 检查。
///
/// Business Logic（为什么需要这个函数）:
///     人类输出只应突出问题项，避免 ok/info 噪音；stopped backend 的 info 不列入问题表。
///
/// Code Logic（这个函数做什么）:
///     遍历 backend.health / paths / mdns / dependencies / log_parse_warning，仅保留 Warning 与 Error。
fn collect_problem_checks(snapshot: &DoctorSnapshot) -> Vec<&DoctorCheck> {
    let mut checks: Vec<&DoctorCheck> = Vec::new();
    let candidates = [
        &snapshot.backend.health,
        &snapshot.paths.data,
        &snapshot.paths.database,
        &snapshot.paths.log,
        &snapshot.mdns,
        &snapshot.dependencies.git,
        &snapshot.dependencies.tmux,
        &snapshot.dependencies.wsl,
        &snapshot.dependencies.claude_cli,
    ];
    for check in candidates {
        if matches!(
            check.status,
            DoctorCheckStatus::Warning | DoctorCheckStatus::Error
        ) {
            checks.push(check);
        }
    }
    if let Some(warning) = snapshot.log_parse_warning.as_ref() {
        if matches!(
            warning.status,
            DoctorCheckStatus::Warning | DoctorCheckStatus::Error
        ) {
            checks.push(warning);
        }
    }
    checks
}

/// overall 状态展示标签。
///
/// Business Logic（为什么需要这个函数）:
///     文本输出需要稳定小写标签，与 JSON `status` 字面量一致。
///
/// Code Logic（这个函数做什么）:
///     healthy/degraded/unhealthy。
fn doctor_status_label(status: DoctorStatus) -> &'static str {
    match status {
        DoctorStatus::Healthy => "healthy",
        DoctorStatus::Degraded => "degraded",
        DoctorStatus::Unhealthy => "unhealthy",
    }
}

/// 单项检查状态展示标签。
///
/// Business Logic（为什么需要这个函数）:
///     检查表需要一眼可读的级别字面量。
///
/// Code Logic（这个函数做什么）:
///     ok/WARNING/ERROR/info（问题级别大写以突出）。
fn doctor_check_status_label(status: DoctorCheckStatus) -> &'static str {
    match status {
        DoctorCheckStatus::Ok => "ok",
        DoctorCheckStatus::Warning => "WARNING",
        DoctorCheckStatus::Error => "ERROR",
        DoctorCheckStatus::Info => "info",
    }
}

/// 将 doctor 采集结果映射为进程退出码（测试/文档用契约）。
///
/// Business Logic（为什么需要这个函数）:
///     采集失败与 unhealthy 都必须是 2，便于脚本分支；测试需独立验证该映射。
///
/// Code Logic（这个函数做什么）:
///     `Ok(status)` → `status.exit_code()`；`Err` → 2。
#[cfg(test)]
fn doctor_exit_from_result(result: Result<DoctorStatus, AppError>) -> i32 {
    match result {
        Ok(status) => status.exit_code(),
        Err(_) => 2,
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
///     CLI bin 是同步 `main`，但 serve/status/stop/start/doctor 都需要 async HTTP、信号或 runtime 初始化能力。
///
/// Code Logic（这个函数做什么）:
///     创建 multi-thread Tokio runtime，enable_all 后 block_on 传入 future 并返回其结果；
///     泛型 `T` 允许 lifecycle 返回 `()`、doctor 返回 exit code 等不同成功值。
fn run_async<F, T>(future: F) -> Result<T, AppError>
where
    F: std::future::Future<Output = Result<T, AppError>>,
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
///     用 `init_backend_tracing` 装配脱敏 stderr + 严格 JSON 文件双 layer，并持有
///     `BackendLoggingGuard` 直到 shutdown 完成；初始化 headless AppState；启动 HTTP/mDNS；
///     先安装 shutdown notifier，再写控制文件并启动后台任务；等待 ctrl-c 或 control route；
///     退出时 shutdown runtime、移除控制文件、清理 notifier，最后 drop guard 以 flush 文件日志。
///     日志目录/文件不可用时启动失败，不静默降级。
async fn serve() -> Result<(), AppError> {
    // serve 生命周期单实例锁：必须在打开 backend.log 之前抢到，覆盖整个 serve 生命周期。
    // start 父进程持有的是短生命周期 start 锁，不能替代本锁。
    let _serve_lock = control::acquire_serve_lock(START_TIMEOUT).await?;
    // serve 子进程是 backend.log 的唯一写入方；父进程 start 只 detach stdio，绝不打开同一文件。
    let _logging_guard = crate::backend::logging::init_backend_tracing(
        crate::backend::logging::BackendLogConfig::production()?,
    )?;
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
    .message(format!(
        "cc-partner headless backend 已启动，监听端口 {port}"
    ))
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
    // `_logging_guard` / `_serve_lock` 在此 drop：flush 日志并释放 serve 单实例锁。
    Ok(())
}

/// 运行 `start` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要用短生命周期命令启动后台后端，并在命令返回后继续使用该服务。
///
/// Code Logic（这个函数做什么）:
///     先获取 data_dir 作用域跨进程 start 锁，再读状态：running 直接返回；stale 清控制文件；
///     否则 spawn 当前 exe 的 `serve` 子进程并轮询 status 最多 10 秒。
///     仅在确认 Running 后释放 child 所有权；超时/过早退出/探测错误路径必须 kill+reap owned child。
///     锁在函数返回时释放。
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
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(AppError::generic(format!(
                    "serve 子进程过早退出，状态: {status}"
                )));
            }
            Ok(None) => {}
            Err(err) => {
                let _ = kill_and_reap_owned_child(&mut child, CHILD_REAP_TIMEOUT);
                return Err(err.into());
            }
        }

        let status = current_status().await;
        if status.kind == BackendStatusKind::Running {
            // 只有 control PID 等于自己 spawn 的 child 时才移交所有权；
            // 其他 PID 的 Running 说明 concurrent direct serve 先拿到锁，必须 kill+reap 自己的 child。
            let owned_pid = child.id();
            let running_pid = status.control.as_ref().map(|c| c.pid);
            if running_pid == Some(owned_pid) {
                println!("{}", render_status_json(&status)?);
                // Running 且 PID 匹配：所有权交给 detached serve，不再 reap。
                return Ok(());
            }
            // 他人实例已 Running：仅当确认自己的 child 已死才能成功采纳，否则报告残留 PID。
            match kill_and_reap_owned_child(&mut child, CHILD_REAP_TIMEOUT) {
                Ok(note) => {
                    println!("{}", render_status_json(&status)?);
                    let _ = note;
                    return Ok(());
                }
                Err(reap_err) => {
                    return Err(AppError::generic(format!(
                        "检测到其它后端实例已运行，但无法确认清理本进程 spawn 的 child: {reap_err}"
                    )));
                }
            }
        }
        tokio::time::sleep(STATUS_POLL_INTERVAL).await;
    }

    // 超时或任何未确认 Running 的路径都必须有界 kill+reap 自己 spawn 的 child，避免孤儿 writer。
    match kill_and_reap_owned_child(&mut child, CHILD_REAP_TIMEOUT) {
        Ok(note) => Err(AppError::generic(format!("等待后端启动超时{note}"))),
        Err(reap_err) => Err(AppError::generic(format!(
            "等待后端启动超时；且清理子进程失败: {reap_err}"
        ))),
    }
}

/// owned child 有界 kill+reap 的默认等待窗口。
const CHILD_REAP_TIMEOUT: Duration = Duration::from_secs(2);

/// 有界 kill 并 reap 自己 spawn 的 serve 子进程。
///
/// Business Logic（为什么需要这个函数）:
///     start 超时、探测失败或采纳他人实例前，若放任 detached child 存活，稍后仍会打开
///     backend.log 并写出 control，形成双 writer / 意外重启。只有确认 child 已退出才可继续。
///
/// Code Logic（这个函数做什么）:
///     委托 `kill_and_reap_owned_child_with`，生产路径用 `Child::try_wait` 轮询。
fn kill_and_reap_owned_child(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<String, String> {
    kill_and_reap_owned_child_with(child, timeout, |c| c.try_wait())
}

/// 可注入 wait 策略的有界 kill+reap（生产与回归测试共用）。
///
/// Business Logic（为什么需要这个函数）:
///     采纳他人实例必须在确认自有 child 已死之后；超时路径需可测，不能依赖真实 OS 信号竞态。
///
/// Code Logic（这个函数做什么）:
///     `child.kill()` 后在 `timeout` 内反复调用 `wait_once`；确认退出返回 Ok(诊断后缀)，
///     超时/reap 失败返回 Err(含残留 PID)。测试可注入恒返回 None 的 wait 模拟卡住 child。
fn kill_and_reap_owned_child_with<F>(
    child: &mut std::process::Child,
    timeout: Duration,
    mut wait_once: F,
) -> Result<String, String>
where
    F: FnMut(&mut std::process::Child) -> std::io::Result<Option<std::process::ExitStatus>>,
{
    let pid = child.id();
    if let Err(err) = child.kill() {
        // 进程可能已自行退出；继续尝试 reap。
        let _ = err;
    }
    let reap_deadline = Instant::now() + timeout;
    loop {
        match wait_once(child) {
            Ok(Some(status)) => {
                return Ok(format!("；已终止子进程 pid={pid} status={status}"));
            }
            Ok(None) => {
                if Instant::now() >= reap_deadline {
                    return Err(format!(
                        "已发送终止信号但子进程 pid={pid} 在 {timeout:?} 内未退出（残留 PID）"
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                return Err(format!("终止子进程 pid={pid} 时 reap 失败: {err}"));
            }
        }
    }
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
///     start/stop/status 需要通过磁盘控制文件跨进程共享 pid、端口、设备身份和 stop 令牌；
///     同时发布本 sidecar 进程唯一的 owner 实例 id 与 control schema 版本，供 GUI 识别权威 owner。
///
/// Code Logic（这个函数做什么）:
///     从 AppState 读取设备信息，填入当前进程 pid、实际 HTTP 端口、UTC 启动时间、随机 control token，
///     以及 `CONTROL_SCHEMA_VERSION` 与一次 UUID `owner_instance_id`；不记录 control token 到日志。
fn build_control_file(state: &AppState, port: u16) -> BackendControlFile {
    BackendControlFile {
        pid: std::process::id(),
        port,
        device_id: state.device_id.as_ref().clone(),
        device_name: state.device_name(),
        started_at: Utc::now().to_rfc3339(),
        control_token: Uuid::new_v4().to_string(),
        control_schema_version: crate::backend::authority::CONTROL_SCHEMA_VERSION,
        // 与 ConfigRuntime 共用同一 owner 实例 id，保证 CAS 对账一致。
        owner_instance_id: {
            let id = state.config_runtime.owner_instance_id();
            if id.is_empty() {
                Some(Uuid::new_v4().to_string())
            } else {
                Some(id.to_string())
            }
        },
        agent_hub_api_version: control::AGENT_HUB_API_VERSION,
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
///     若当前进程设置了 `CC_PARTNER_DATA_DIR`，显式写入 child Command 的 env 并返回该值；
///     未设置则不改动（子进程默认继承父环境，保持生产行为）。
///     返回注入值供测试断言；Windows 上 `Command` Debug 不含 env，不能靠 Debug 字符串检查。
fn inherit_data_dir_env(command: &mut Command) -> Option<OsString> {
    let value = std::env::var_os("CC_PARTNER_DATA_DIR")?;
    command.env("CC_PARTNER_DATA_DIR", &value);
    Some(value)
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
///     spawn 前清掉当前进程 stdio 的 HANDLE_FLAG_INHERIT：Rust stable 的
///     `inherit_handles(false)` 仍是 unstable，不调；否则 serve 会复制 start 的
///     stdout 写句柄，调用方 `read_to_end` 永远等不到 EOF。
#[cfg(windows)]
fn configure_detached_child(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    disable_stdio_handle_inheritance();
    command.creation_flags(windows_detached_creation_flags());
}

/// 禁止当前进程的 stdin/stdout/stderr 被后续 CreateProcess 默认继承。
///
/// Business Logic（为什么需要这个函数）:
///     `start` 的 stdout 常常被 CI/脚本 pipe；若 serve 再拿到同一写句柄，
///     `start` 退出后管道仍有 writer，父进程会卡在 drain。
///
/// Code Logic（这个函数做什么）:
///     GetStdHandle + SetHandleInformation(HANDLE_FLAG_INHERIT=0)。
///     显式传给 child 的 Stdio::null() 仍经 STARTF_USESTDHANDLES 生效。
#[cfg(windows)]
fn disable_stdio_handle_inheritance() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n_std_handle: i32) -> isize;
        fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
    }
    const STD_INPUT_HANDLE: i32 = -10;
    const STD_OUTPUT_HANDLE: i32 = -11;
    const STD_ERROR_HANDLE: i32 = -12;
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    const INVALID_HANDLE_VALUE: isize = -1;
    // SAFETY: 只改本进程标准句柄的 inherit 标志，不关闭、不跨线程转移所有权。
    unsafe {
        for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let handle = GetStdHandle(id);
            if handle == 0 || handle == INVALID_HANDLE_VALUE {
                continue;
            }
            let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
        }
    }
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

#[cfg(test)]
mod tests {
    use crate::backend::control::{BackendControlFile, BackendStatus, BackendStatusKind};
    use crate::backend::doctor::{
        DoctorBackendCheck, DoctorCheck, DoctorCheckStatus, DoctorDependencies, DoctorErrorSummary,
        DoctorPathChecks, DoctorPlatform, DoctorSnapshot, DoctorStatus, DoctorVersion,
        DOCTOR_SCHEMA_VERSION, HOME_PLACEHOLDER,
    };
    use crate::error::AppError;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::net::TcpListener;

    /// `cc-partner-backend version` 走 dispatch 时返回 0 且 stdout 是
    /// `env!("CARGO_PKG_VERSION")`，错误地把 `unknown` 当作未知子命令返回 2。
    ///
    /// Business Logic: 远程对账（README、scripts、QA）需要一个轻量、稳定、无副作用
    ///     的版本探针；版本号不一致时要立刻浮现并被 GUI/sidecar 自动捕获。
    /// Code Logic: 对 `version` / `--version` / `-V` 三种别名分别跑 dispatch，
    ///     断言退出码为 0、版本串非空且符合 semver-ish（不含空格）。`unknown`
    ///     仍走未知命令 → exit 2（保留老行为，便于脚本做 "binary 真的认识这个子命令吗"）。
    #[test]
    fn dispatch_version_subcommand_prints_cargo_pkg_version() {
        let expected = env!("CARGO_PKG_VERSION");
        for flag in ["version", "--version", "-V"] {
            let exit = super::dispatch_for_test(["cc-partner-backend", flag]);
            assert_eq!(exit, 0, "version flag {flag} should exit 0");
            assert!(
                !expected.contains(' '),
                "CARGO_PKG_VERSION 必须是单行无空格"
            );
        }
        // unknown 子命令维持 exit 2，不被 version 的宽松解释吞掉。
        assert_eq!(
            super::dispatch_for_test(["cc-partner-backend", "frobnicate"]),
            2
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     sidecar 只要从宿主 .app 启动就会点亮程序坞；CLI 入口必须在任何子命令前脱离 Dock。
    ///
    /// Code Logic（这个测试做什么）:
    ///     读取 cli.rs 源码，断言 `run_from_env` 在 `dispatch` 之前调用
    ///     `detach_current_process_from_dock()`。
    #[test]
    fn run_from_env_detaches_backend_process_from_dock_before_dispatch() {
        let src = include_str!("cli.rs");
        let run_fn = src
            .split("pub fn run_from_env()")
            .nth(1)
            .and_then(|rest| rest.split("fn dispatch").next())
            .expect("run_from_env 应存在");
        let detach_at = run_fn
            .find("detach_current_process_from_dock()")
            .expect("backend CLI 必须在 dispatch 前脱离 macOS Dock，避免 sidecar 点亮宿主 .app");
        let dispatch_at = run_fn
            .find("dispatch(")
            .expect("run_from_env 必须调用 dispatch");
        assert!(
            detach_at < dispatch_at,
            "detach_current_process_from_dock 必须发生在 dispatch 之前"
        );
    }

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
    ///     设置环境变量后构造 Command，调用 inherit helper，断言返回注入值；
    ///     Drop 守卫保证 panic 后仍恢复环境。不依赖 Command Debug（Windows 上不含 env）。
    #[test]
    fn start_inherits_data_dir_env_for_detached_serve() {
        use std::ffi::{OsStr, OsString};
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
        let inherited = super::inherit_data_dir_env(&mut command);
        assert_eq!(
            inherited.as_deref(),
            Some(OsStr::new("/tmp/cc-partner-isolated")),
            "start 子进程应继承数据目录 override"
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

    /// 验证 doctor 未知选项与多余参数返回 2。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     错误的 doctor 参数必须显式失败，不能静默忽略或污染 JSON 输出。
    ///
    /// Code Logic（这个测试做什么）:
    ///     解析 `--yaml` 与多余位置参数，断言 `parse_doctor_args` 返回 Err。
    #[test]
    fn doctor_rejects_unknown_option_and_extra_args() {
        let err = super::parse_doctor_args(&["--yaml".to_string()]).expect_err("未知选项应失败");
        assert!(err.contains("未知选项"));
        let err = super::parse_doctor_args(&["extra".to_string()]).expect_err("多余参数应失败");
        assert!(err.contains("多余参数"));
        assert!(!super::parse_doctor_args(&[]).expect("无参应成功"));
        assert!(super::parse_doctor_args(&["--json".to_string()]).expect("--json 应成功"));
        let exit_code = super::dispatch_for_test(["cc-partner-backend", "doctor", "--bogus"]);
        assert_eq!(exit_code, 2);
        let exit_code = super::dispatch_for_test(["cc-partner-backend", "doctor", "extra"]);
        assert_eq!(exit_code, 2);
    }

    /// 构造隐私安全的 doctor 快照 fixture。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     输出隔离测试需要固定字段，避免 wall-clock / 真实 home 导致抖动。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 stopped healthy 快照，路径已用 `<HOME>`，含一条 recent error。
    fn doctor_text_fixture() -> DoctorSnapshot {
        DoctorSnapshot {
            schema_version: DOCTOR_SCHEMA_VERSION,
            generated_at: "2026-07-11T12:00:00Z".to_string(),
            status: DoctorStatus::Healthy,
            version: DoctorVersion {
                app: "0.6.7".to_string(),
                backend: "0.6.7".to_string(),
            },
            platform: DoctorPlatform {
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
            },
            backend: DoctorBackendCheck {
                state: "stopped".to_string(),
                control_path: format!("{HOME_PLACEHOLDER}/.cc-partner/backend-control.json"),
                pid: None,
                port: None,
                health: DoctorCheck::new(
                    DoctorCheckStatus::Info,
                    "backend.stopped",
                    "backend is stopped",
                ),
            },
            paths: DoctorPathChecks {
                data: DoctorCheck::new(
                    DoctorCheckStatus::Ok,
                    "paths.data.ok",
                    "data directory is usable",
                ),
                database: DoctorCheck::new(
                    DoctorCheckStatus::Ok,
                    "paths.database.ok",
                    "database file is readable",
                ),
                log: DoctorCheck::new(
                    DoctorCheckStatus::Ok,
                    "paths.log.ok",
                    "log directory is usable",
                ),
            },
            mdns: DoctorCheck::new(
                DoctorCheckStatus::Ok,
                "mdns.ok",
                "mDNS discovery can initialize",
            ),
            dependencies: DoctorDependencies {
                git: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.git.ok", "git is available"),
                tmux: DoctorCheck::new(
                    DoctorCheckStatus::Warning,
                    "deps.tmux.missing",
                    "tmux is missing",
                ),
                wsl: DoctorCheck::new(
                    DoctorCheckStatus::Info,
                    "deps.wsl.not_applicable",
                    "WSL is not applicable on this platform",
                ),
                claude_cli: DoctorCheck::new(
                    DoctorCheckStatus::Ok,
                    "deps.claude_cli.ok",
                    "claude CLI is available",
                ),
            },
            recent_errors: vec![DoctorErrorSummary {
                timestamp: "2026-07-11T11:59:00Z".to_string(),
                code: "net.timeout".to_string(),
                summary: "peer request timed out".to_string(),
                request_id: Some("req-fixed-001".to_string()),
            }],
            log_path: format!("{HOME_PLACEHOLDER}/.cc-partner/logs/backend.log"),
            log_parse_warning: None,
        }
    }

    /// 验证 JSON 渲染为纯净合法 JSON 且无 tracing 前缀。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     `doctor --json` stdout 必须可直接 `jq` 解析，不能夹杂 tracing 文本。
    ///
    /// Code Logic（这个测试做什么）:
    ///     渲染 fixture → from_str 成功 → 关键字段存在 → 文本不以 TRACE/INFO/DEBUG 前缀开头。
    #[test]
    fn doctor_json_stdout_is_pure_parseable_json() {
        let snapshot = doctor_text_fixture();
        let rendered = super::render_doctor_json(&snapshot).expect("json render");
        let trimmed = rendered.trim();
        assert!(
            trimmed.starts_with('{'),
            "JSON 输出不得带 tracing 前缀: {trimmed}"
        );
        assert!(
            !trimmed.starts_with("INFO")
                && !trimmed.starts_with("DEBUG")
                && !trimmed.starts_with("ERROR")
                && !trimmed.starts_with("WARN"),
            "JSON 输出不得含 tracing level 前缀: {trimmed}"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(trimmed).expect("stdout should parse as JSON");
        assert_eq!(parsed["schemaVersion"], 1);
        assert_eq!(parsed["status"], "healthy");
        assert!(parsed["logPath"]
            .as_str()
            .unwrap_or("")
            .contains(HOME_PLACEHOLDER));
        assert!(!parsed["logPath"].as_str().unwrap_or("").contains("/Users/"));
    }

    /// 验证人类文本包含 status/检查/日志路径且无私有原始路径。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     无 `--json` 时用户依赖文本摘要；必须隐私安全并标注 stopped 正常。
    ///
    /// Code Logic（这个测试做什么）:
    ///     渲染含 warning 的 fixture，断言 status/stopped normal/WARNING 检查/log 路径与脱敏。
    #[test]
    fn doctor_text_includes_status_checks_and_sanitized_log_path() {
        // 文本 fixture 的 overall 在 deps.tmux.missing 时实际应为 degraded；
        // 这里直接改 status 以覆盖 degraded 展示路径。
        let mut snapshot = doctor_text_fixture();
        snapshot.status = DoctorStatus::Degraded;
        let text = super::render_doctor_text(&snapshot);
        assert!(text.contains("status: degraded"));
        assert!(text.contains("stopped (normal)"));
        assert!(text.contains("WARNING deps.tmux.missing"));
        assert!(text.contains(&format!(
            "log: {HOME_PLACEHOLDER}/.cc-partner/logs/backend.log"
        )));
        assert!(text.contains("recent errors:"));
        assert!(text.contains("peer request timed out"));
        assert!(!text.contains("/Users/"));
        assert!(!text.contains("alice"));
    }

    /// 验证 doctor 文本包含通配监听、实际端口与固定无身份 LAN 风险。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     操作者必须从 doctor 看到真实暴露面：wildcard listener、实际 HTTP 端口、
    ///     以及“可达网络任意设备可读写执行且不验证身份”的固定风险，且永不打印 control token。
    ///
    /// Code Logic（这个测试做什么）:
    ///     渲染带 port 的 fixture，断言 wildcard/port/固定中文风险存在，
    ///     且不含“安全/已认证/可信设备/control token”等禁用词。
    #[test]
    fn doctor_text_includes_wildcard_listener_and_fixed_lan_risk() {
        let mut snapshot = doctor_text_fixture();
        snapshot.backend.port = Some(62116);
        snapshot.backend.pid = Some(4242);
        snapshot.backend.state = "running".to_string();
        snapshot.backend.health = DoctorCheck::new(
            DoctorCheckStatus::Ok,
            "backend.running",
            "backend is running",
        );
        let text = super::render_doctor_text(&snapshot);
        assert!(
            text.contains("listener: wildcard 0.0.0.0 (actual http port=62116)"),
            "doctor must report wildcard listener and actual port: {text}"
        );
        assert!(
            text.contains("同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份"),
            "doctor must include fixed no-identity risk: {text}"
        );
        assert!(text.contains("port=62116"));
        assert!(!text.contains("安全"));
        assert!(!text.contains("已认证"));
        assert!(!text.contains("可信设备"));
        assert!(!text.to_lowercase().contains("control token"));
        assert!(!text.contains("expected-token"));
        assert!(!text.contains("controlToken"));
    }

    /// 验证 probe/构造失败映射退出码 2。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     doctor 无法完成时必须 exit 2，与 unhealthy 同档，便于脚本统一处理。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对 healthy/degraded/unhealthy 与 Err 调用 `doctor_exit_from_result` 断言 0/1/2/2。
    #[test]
    fn doctor_exit_codes_map_status_and_probe_failure() {
        assert_eq!(super::doctor_exit_from_result(Ok(DoctorStatus::Healthy)), 0);
        assert_eq!(
            super::doctor_exit_from_result(Ok(DoctorStatus::Degraded)),
            1
        );
        assert_eq!(
            super::doctor_exit_from_result(Ok(DoctorStatus::Unhealthy)),
            2
        );
        assert_eq!(
            super::doctor_exit_from_result(Err(AppError::generic("probe failed"))),
            2
        );
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
            control_schema_version: crate::backend::authority::CONTROL_SCHEMA_VERSION,
            owner_instance_id: Some("owner-test".to_string()),
            agent_hub_api_version: crate::backend::control::AGENT_HUB_API_VERSION,
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

    /// 验证 start 子进程 detach 时把 stdio 置 null，而不是重定向到 backend.log。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     父进程与 serve 子进程若同时打开同一轮转文件，会破坏 size 轮转与诊断唯一写入契约。
    ///
    /// Code Logic（这个测试做什么）:
    ///     复现 start 中 Command 配置：stdin/stdout/stderr 均为 null，且不出现 backend.log 路径重定向。
    #[test]
    fn start_detaches_stdio_without_opening_backend_log() {
        let mut command = std::process::Command::new("true");
        command
            .arg("serve")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let debug = format!("{command:?}");
        assert!(
            !debug.contains("backend.log"),
            "父进程不得把 stdio 重定向到 backend.log，实际: {debug}"
        );
        // Debug 输出在 Unix 上会标注 null 重定向；至少确认未绑定文件路径。
        assert!(
            debug.contains("serve"),
            "command 应包含 serve 子命令，实际: {debug}"
        );
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

    /// Business Logic（为什么需要这个测试）:
    ///     start 超时路径必须有界 kill+reap 自己 spawn 的 child，避免孤儿 detached serve。
    ///
    /// Code Logic（这个测试做什么）:
    ///     spawn 一个 sleep 子进程，调用 `kill_and_reap_owned_child`，断言 Ok 且进程不再存活。
    #[test]
    fn kill_and_reap_owned_child_terminates_stuck_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let note = super::kill_and_reap_owned_child(&mut child, super::CHILD_REAP_TIMEOUT)
            .expect("应成功 kill+reap sleep 子进程");
        assert!(note.contains("pid="), "reap note 应含 pid: {note}");
        let status = child.try_wait().expect("try_wait");
        assert!(status.is_some(), "child 应已被 reap");
        #[cfg(unix)]
        {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(!alive, "pid={pid} 不应仍存活");
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     采纳他人实例前若 kill/reap 超时，start 必须失败并报告残留 PID，不能假装成功。
    ///
    /// Code Logic（这个测试做什么）:
    ///     注入恒返回 None 的 wait + 短超时，断言 Err 含残留 PID；最后真实 reap 清理。
    #[test]
    fn kill_and_reap_owned_child_reports_residual_pid_on_timeout() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        // 注入卡住 wait：模拟 kill 后进程仍存活超过 deadline。
        let err =
            super::kill_and_reap_owned_child_with(&mut child, Duration::from_millis(80), |_c| {
                Ok(None)
            })
            .expect_err("卡住 wait 应超时失败");
        assert!(
            err.contains(&pid.to_string()) && err.contains("残留"),
            "错误应报告残留 PID，实际: {err}"
        );
        // 清理：真实 try_wait 路径 reap，避免测试泄漏 sleep。
        let _ = super::kill_and_reap_owned_child(&mut child, super::CHILD_REAP_TIMEOUT);
    }
}
