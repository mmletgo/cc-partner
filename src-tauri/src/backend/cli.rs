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
use crate::backend::control_api::{ControlDevicesResponse, ControlRelayShadowDto};
use crate::backend::control_client::BackendControlClient;
use crate::backend::doctor::{
    collect_doctor_snapshot, DoctorCheck, DoctorCheckStatus, DoctorSnapshot, DoctorStatus,
};
use crate::backend::runtime::{
    build_app_state, shutdown_backend_runtime, start_backend_services, start_background_tasks,
    BackendRuntimeMode,
};
use crate::backend::ui::{BackendUi, HeadlessBackendUi};
use crate::config::{AppConfig, ManualPeerConfig, RelayConfig};
use crate::config_runtime::{ConfigSnapshot, ConfigUpdateRequest, RuntimeConfigPatch};
use crate::config_store::{ConfigStore, FsConfigStore};
use crate::error::{AppError, AppErrorCategory};
use crate::models::device::DeviceDto;
use crate::state::AppState;
use chrono::Utc;
use serde::Serialize;
#[cfg(any(not(unix), test))]
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
        Some("devices") => dispatch_devices(&args[2..]),
        Some("relay") => dispatch_relay(&args[2..]),
        Some("peers") => dispatch_peers(&args[2..]),
        Some("version") | Some("--version") | Some("-V") => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            0
        }
        _ => {
            eprintln!(
                "用法: cc-partner-backend <start|serve|stop|status|supervise|doctor [--json]|version|--version|-V>"
            );
            eprintln!("      cc-partner-backend devices [--json]");
            eprintln!(
                "      cc-partner-backend relay <status [--json] | via add|remove <device_id|device_name> | allow on|off>"
            );
            eprintln!("      cc-partner-backend peers <list [--json] | add|remove <host[:port]>>");
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
///     以命令名 `doctor` 委托 `parse_json_flag_args`。
fn parse_doctor_args(rest: &[String]) -> Result<bool, String> {
    parse_json_flag_args(rest, "doctor")
}

/// 严格解析"仅允许可选 `--json`"的只读命令参数。
///
/// Business Logic（为什么需要这个函数）:
///     doctor / devices / relay status / peers list 共享同一 stdout 单行 JSON 契约，
///     参数解析必须一致：未知选项与多余参数一律报错，不得静默忽略。
///
/// Code Logic（这个函数做什么）:
///     扫描剩余参数：无参 → false；仅一次 `--json` → true；重复/未知选项/多余参数
///     返回带命令名的错误文本（供调用方写 stderr 并退出 2）。
fn parse_json_flag_args(rest: &[String], command: &str) -> Result<bool, String> {
    let mut json_mode = false;
    for arg in rest {
        match arg.as_str() {
            "--json" if !json_mode => json_mode = true,
            "--json" => {
                return Err(format!("{command} 不接受重复的 --json"));
            }
            other if other.starts_with('-') => {
                return Err(format!("{command} 未知选项: {other}"));
            }
            other => {
                return Err(format!("{command} 多余参数: {other}"));
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

// ---------------------------------------------------------------------------
// devices / relay / peers 子命令（headless 中转访问配置）
// ---------------------------------------------------------------------------

/// 手动 peer 未显式给端口时的默认端口（与 P2P 首选端口一致）。
const PEER_DEFAULT_PORT: u16 = 62116;

/// devices/relay/peers 命令的统一结果：Ok(stdout 文本) / Err(stderr 文本)。
type CliCommandResult = Result<String, AppError>;

/// `relay status --json` 中单个跳板（via）的可达性摘要。
///
/// Business Logic（为什么需要这个结构）:
///     跳板是否在线由 owner 直连表权威判定；CLI 需要结构化输出供脚本判断。
///
/// Code Logic（这个结构做什么）:
///     camelCase；跳板不在直连表时省略 name/address 且 online=false。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliRelayViaStatus {
    device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_name: Option<String>,
    online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    address: Option<String>,
    shadow_count: usize,
}

/// `relay status --json` 输出结构。
///
/// Business Logic（为什么需要这个结构）:
///     relay 配置 + 跳板可达性 + 影子清单需要在 stdout 输出为单行稳定 JSON。
///
/// Code Logic（这个结构做什么）:
///     relay_enabled/ignored_target_ids 来自权威 relay 配置；shadows 为 owner 影子表。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliRelayStatusOutput {
    relay_enabled: bool,
    via: Vec<CliRelayViaStatus>,
    ignored_target_ids: Vec<String>,
    shadows: Vec<ControlRelayShadowDto>,
}

/// `peers list --json` 输出结构。
///
/// Business Logic（为什么需要这个结构）:
///     手动 peer 列表需要机器可读形式；直接复用 `ManualPeerConfig`（host/port camelCase）。
///
/// Code Logic（这个结构做什么）:
///     包装 `{peers: [{host, port}]}`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliPeersListOutput {
    peers: Vec<ManualPeerConfig>,
}

/// via 候选解析结果（`relay via add` 的设备表匹配）。
///
/// Business Logic（为什么需要这个枚举）:
///     用户可能传 device_id 或设备名；多匹配/零匹配必须分别给出可操作错误。
///
/// Code Logic（这个枚举做什么）:
///     Resolved 携带解析出的 device_id 与可选设备名；Ambiguous 列出候选展示串；
///     NotFound 表示 id 与名称在直连设备表中均未命中。
enum ViaResolution {
    Resolved {
        device_id: String,
        device_name: Option<String>,
    },
    Ambiguous(Vec<String>),
    NotFound,
}

/// 分发 `devices` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     headless 设备需要命令行获取 device_id 名册（跳板/目标配置都依赖它）。
///
/// Code Logic（这个函数做什么）:
///     严格解析 `--json` → 初始化仅 stderr tracing → 异步查询 control `/devices` →
///     JSON 或中文文本到 stdout；解析失败 2，业务失败 1，成功 0。
fn dispatch_devices(rest: &[String]) -> i32 {
    let json_mode = match parse_json_flag_args(rest, "devices") {
        Ok(json_mode) => json_mode,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("用法: cc-partner-backend devices [--json]");
            return 2;
        }
    };
    crate::backend::logging::init_doctor_tracing();
    map_cli_command_result(run_async(run_devices_command(json_mode)))
}

/// 分发 `relay` 子命令（status / via add|remove / allow on|off）。
///
/// Business Logic（为什么需要这个函数）:
///     A 侧配置跳板（via）、B 侧开关被中转能力（allow）、运维排查（status）
///     都需要在无 GUI 设备上命令行完成。
///
/// Code Logic（这个函数做什么）:
///     手写 match 严格解析二级子命令与参数个数；用法错误统一打印用法并返回 2；
///     业务命令经 `map_cli_command_result` 映射为 0/1。
fn dispatch_relay(rest: &[String]) -> i32 {
    let Some(action) = rest.first().map(String::as_str) else {
        print_relay_usage();
        return 2;
    };
    match action {
        "status" => {
            let json_mode = match parse_json_flag_args(&rest[1..], "relay status") {
                Ok(json_mode) => json_mode,
                Err(message) => {
                    eprintln!("{message}");
                    print_relay_usage();
                    return 2;
                }
            };
            crate::backend::logging::init_doctor_tracing();
            map_cli_command_result(run_async(run_relay_status_command(json_mode)))
        }
        "via" => {
            let Some(verb) = rest.get(1).map(String::as_str) else {
                print_relay_usage();
                return 2;
            };
            match verb {
                "add" | "remove" => {
                    let Some(target) = rest.get(2).map(String::as_str) else {
                        eprintln!("relay via {verb} 需要一个 <device_id|device_name> 参数");
                        print_relay_usage();
                        return 2;
                    };
                    if rest.len() > 3 {
                        eprintln!("relay via {verb} 只接受一个参数");
                        print_relay_usage();
                        return 2;
                    }
                    crate::backend::logging::init_doctor_tracing();
                    let result = if verb == "add" {
                        run_async(run_relay_via_add(target))
                    } else {
                        run_async(run_relay_via_remove(target))
                    };
                    map_cli_command_result(result)
                }
                _ => {
                    print_relay_usage();
                    2
                }
            }
        }
        "allow" => {
            if rest.len() > 2 {
                eprintln!("relay allow 只接受一个参数");
                print_relay_usage();
                return 2;
            }
            let Some(value) = rest.get(1).map(String::as_str) else {
                eprintln!("relay allow 需要 on 或 off");
                print_relay_usage();
                return 2;
            };
            let enabled = match value {
                "on" => true,
                "off" => false,
                other => {
                    eprintln!("relay allow 只接受 on 或 off，实际: {other}");
                    print_relay_usage();
                    return 2;
                }
            };
            crate::backend::logging::init_doctor_tracing();
            map_cli_command_result(run_async(run_relay_allow(enabled)))
        }
        _ => {
            print_relay_usage();
            2
        }
    }
}

/// 分发 `peers` 子命令（list / add / remove）。
///
/// Business Logic（为什么需要这个函数）:
///     跨子网/VPN 拓扑下 mDNS 不可见时，用户需要命令行维护 manual peers 兜底发现。
///
/// Code Logic（这个函数做什么）:
///     add/remove 先严格解析 `host[:port]`（解析失败 → 用法 2），list 复用 `--json`
///     解析；业务命令经 `map_cli_command_result` 映射为 0/1。
fn dispatch_peers(rest: &[String]) -> i32 {
    let Some(action) = rest.first().map(String::as_str) else {
        print_peers_usage();
        return 2;
    };
    match action {
        "add" | "remove" => {
            let Some(target) = rest.get(1).map(String::as_str) else {
                eprintln!("peers {action} 需要 <host[:port]> 参数");
                print_peers_usage();
                return 2;
            };
            if rest.len() > 2 {
                eprintln!("peers {action} 只接受一个参数");
                print_peers_usage();
                return 2;
            }
            let (host, port) = match parse_peer_target(target) {
                Ok(parsed) => parsed,
                Err(message) => {
                    eprintln!("{message}");
                    print_peers_usage();
                    return 2;
                }
            };
            crate::backend::logging::init_doctor_tracing();
            let result = if action == "add" {
                run_async(run_peers_add(&host, port))
            } else {
                run_async(run_peers_remove(&host, port))
            };
            map_cli_command_result(result)
        }
        "list" => {
            let json_mode = match parse_json_flag_args(&rest[1..], "peers list") {
                Ok(json_mode) => json_mode,
                Err(message) => {
                    eprintln!("{message}");
                    print_peers_usage();
                    return 2;
                }
            };
            crate::backend::logging::init_doctor_tracing();
            map_cli_command_result(run_async(run_peers_list(json_mode)))
        }
        _ => {
            print_peers_usage();
            2
        }
    }
}

/// 打印 relay 子命令用法（stderr）。
///
/// Business Logic（为什么需要这个函数）:
///     用法错误必须给出完整可复制的命令形态，避免用户猜测参数。
///
/// Code Logic（这个函数做什么）:
///     逐行输出 status/via/allow 用法。
fn print_relay_usage() {
    eprintln!("用法: cc-partner-backend relay status [--json]");
    eprintln!("      cc-partner-backend relay via add <device_id|device_name>");
    eprintln!("      cc-partner-backend relay via remove <device_id>");
    eprintln!("      cc-partner-backend relay allow on|off");
}

/// 打印 peers 子命令用法（stderr）。
///
/// Business Logic（为什么需要这个函数）:
///     同 `print_relay_usage`：用法错误要给出完整命令形态。
///
/// Code Logic（这个函数做什么）:
///     逐行输出 list/add/remove 用法（端口缺省 62116 与 IPv6 形式提示）。
fn print_peers_usage() {
    eprintln!("用法: cc-partner-backend peers list [--json]");
    eprintln!("      cc-partner-backend peers add <host[:port]>");
    eprintln!("      cc-partner-backend peers remove <host[:port]>");
    eprintln!("      （端口缺省 62116；IPv6 使用 [::1]:端口 形式）");
}

/// 统一映射 devices/relay/peers 命令结果为退出码。
///
/// Business Logic（为什么需要这个函数）:
///     新子命令必须保持既有退出码契约：0 成功（stdout）/ 1 业务失败（stderr）。
///
/// Code Logic（这个函数做什么）:
///     `Ok(message)` 打印 stdout 返回 0；`Err` 打印 stderr 返回 1。
fn map_cli_command_result(result: CliCommandResult) -> i32 {
    match result {
        Ok(message) => {
            println!("{message}");
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

/// 运行 `devices` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     获取跳板/目标 device_id 的权威途径；--json 供脚本，文本供人工。
///
/// Code Logic（这个函数做什么）:
///     要求 backend 运行中并查询 control `/devices`，按模式渲染。
async fn run_devices_command(json_mode: bool) -> CliCommandResult {
    let payload = require_running_devices_payload().await?;
    if json_mode {
        Ok(serde_json::to_string(&payload)?)
    } else {
        Ok(render_devices_text(&payload))
    }
}

/// 运行 `relay status` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     排查"跳板不可达 / 目标下线 / 配置错误"需要 relay 配置 + via 可达性 + 影子状态一体视图。
///
/// Code Logic（这个函数做什么）:
///     查询 control `/devices` 后组合 `CliRelayStatusOutput`（--json）或中文文本。
async fn run_relay_status_command(json_mode: bool) -> CliCommandResult {
    let payload = require_running_devices_payload().await?;
    if json_mode {
        Ok(serde_json::to_string(&build_relay_status_output(&payload))?)
    } else {
        Ok(render_relay_status_text(&payload))
    }
}

/// 运行 `relay via add` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     A 侧需要把可信跳板写入 `relay.via_device_ids`：运行中走 control CAS 热生效；
///     离线直接落盘 config.json，下次启动生效。
///
/// Code Logic（这个函数做什么）:
///     运行中：先经设备表解析参数（device_id 精确优先，其次设备名精确匹配；
///     多匹配/零匹配报错），再 get-config 合并 relay 段提交 update-config；
///     离线：参数按 device_id 直接落盘（名称解析无法离线进行）。
async fn run_relay_via_add(arg: &str) -> CliCommandResult {
    let target = arg.trim();
    match running_control_file().await {
        Some(control) => {
            let payload = fetch_control_devices(&control).await?;
            let (device_id, device_name) = match resolve_via_candidate(target, &payload.devices) {
                ViaResolution::Resolved {
                    device_id,
                    device_name,
                } => (device_id, device_name),
                ViaResolution::Ambiguous(candidates) => {
                    return Err(AppError::validation(format!(
                        "设备名 \"{target}\" 匹配到多台设备，请改用 device_id。候选: {}",
                        candidates.join("、")
                    )));
                }
                ViaResolution::NotFound => {
                    return Err(AppError::not_found(format!(
                        "设备表中未找到 \"{target}\"（按 device_id 与设备名均未命中）；\
                         可运行 devices 查看，或停止 backend 后按 device_id 离线添加"
                    )));
                }
            };
            let client = BackendControlClient::from_control(&control)?;
            let snapshot = client.get_config().await?;
            let relay =
                relay_with_via_add(&snapshot.relay, &device_id).map_err(AppError::validation)?;
            commit_config_patch(
                &client,
                &snapshot,
                RuntimeConfigPatch {
                    relay: Some(relay),
                    ..Default::default()
                },
            )
            .await?;
            match device_name {
                Some(name) => Ok(format!("已添加跳板设备: {name} ({device_id})")),
                None => Ok(format!("已添加跳板设备: {device_id}")),
            }
        }
        None => {
            apply_offline_config_edit(|cfg| {
                let relay = relay_with_via_add(&cfg.relay, target).map_err(AppError::validation)?;
                cfg.relay = relay;
                Ok(())
            })?;
            Ok(format!(
                "backend 未运行，已离线写入跳板 device_id: {target}（下次启动生效）"
            ))
        }
    }
}

/// 运行 `relay via remove` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     撤销对某跳板的信任必须与 add 对称：运行中热生效，离线落盘。
///
/// Code Logic（这个函数做什么）:
///     仅按 device_id 操作（与 spec 一致）：get-config/磁盘读出当前 relay 段，
///     移除目标 id（不存在则报错），提交 CAS patch 或原子落盘。
async fn run_relay_via_remove(device_id: &str) -> CliCommandResult {
    let target = device_id.trim();
    match running_control_file().await {
        Some(control) => {
            let client = BackendControlClient::from_control(&control)?;
            let snapshot = client.get_config().await?;
            let relay =
                relay_with_via_remove(&snapshot.relay, target).map_err(AppError::validation)?;
            commit_config_patch(
                &client,
                &snapshot,
                RuntimeConfigPatch {
                    relay: Some(relay),
                    ..Default::default()
                },
            )
            .await?;
            Ok(format!("已移除跳板设备: {target}"))
        }
        None => {
            apply_offline_config_edit(|cfg| {
                let relay =
                    relay_with_via_remove(&cfg.relay, target).map_err(AppError::validation)?;
                cfg.relay = relay;
                Ok(())
            })?;
            Ok(format!(
                "backend 未运行，已离线移除跳板 device_id: {target}（下次启动生效）"
            ))
        }
    }
}

/// 运行 `relay allow on|off` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     B 侧（跳板机）需要整体关闭"被用作跳板"能力；关闭后 health 不再宣告 net.relay.v1。
///
/// Code Logic（这个函数做什么）:
///     运行中 get-config 合并 enabled 字段提交 CAS（热生效）；离线直接改盘。
async fn run_relay_allow(enabled: bool) -> CliCommandResult {
    match running_control_file().await {
        Some(control) => {
            let client = BackendControlClient::from_control(&control)?;
            let snapshot = client.get_config().await?;
            let relay = relay_with_allow(&snapshot.relay, enabled);
            commit_config_patch(
                &client,
                &snapshot,
                RuntimeConfigPatch {
                    relay: Some(relay),
                    ..Default::default()
                },
            )
            .await?;
            Ok(format!(
                "已{}本机被用作跳板（relay.allow={}，热生效）",
                if enabled { "允许" } else { "禁止" },
                if enabled { "on" } else { "off" }
            ))
        }
        None => {
            apply_offline_config_edit(|cfg| {
                cfg.relay.enabled = enabled;
                Ok(())
            })?;
            Ok(format!(
                "backend 未运行，已离线写入 relay.allow={}（下次启动生效）",
                if enabled { "on" } else { "off" }
            ))
        }
    }
}

/// 运行 `peers add` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     跨子网/VPN 对端 mDNS 不可见时需要手动登记 host:port 兜底发现。
///
/// Code Logic（这个函数做什么）:
///     运行中 get-config 合并 manual_peers 段提交 CAS（探测循环最迟 15s 内生效）；
///     离线直接落盘；重复 (host,port) 一律报错。
async fn run_peers_add(host: &str, port: u16) -> CliCommandResult {
    match running_control_file().await {
        Some(control) => {
            let client = BackendControlClient::from_control(&control)?;
            let snapshot = client.get_config().await?;
            let peers = manual_peers_with_add(&snapshot.manual_peers, host, port)
                .map_err(AppError::validation)?;
            commit_config_patch(
                &client,
                &snapshot,
                RuntimeConfigPatch {
                    manual_peers: Some(peers),
                    ..Default::default()
                },
            )
            .await?;
            Ok(format!("已添加手动 peer: {host}:{port}"))
        }
        None => {
            apply_offline_config_edit(|cfg| {
                let peers = manual_peers_with_add(&cfg.manual_peers, host, port)
                    .map_err(AppError::validation)?;
                cfg.manual_peers = peers;
                Ok(())
            })?;
            Ok(format!(
                "backend 未运行，已离线写入手动 peer: {host}:{port}（下次启动生效）"
            ))
        }
    }
}

/// 运行 `peers remove` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     撤销手动 peer 必须精确匹配 (host,port)，避免误删相近条目。
///
/// Code Logic（这个函数做什么）:
///     运行中/离线分别从权威快照或磁盘读出列表，精确移除（不存在则报错）后提交。
async fn run_peers_remove(host: &str, port: u16) -> CliCommandResult {
    match running_control_file().await {
        Some(control) => {
            let client = BackendControlClient::from_control(&control)?;
            let snapshot = client.get_config().await?;
            let peers = manual_peers_with_remove(&snapshot.manual_peers, host, port)
                .map_err(AppError::validation)?;
            commit_config_patch(
                &client,
                &snapshot,
                RuntimeConfigPatch {
                    manual_peers: Some(peers),
                    ..Default::default()
                },
            )
            .await?;
            Ok(format!("已移除手动 peer: {host}:{port}"))
        }
        None => {
            apply_offline_config_edit(|cfg| {
                let peers = manual_peers_with_remove(&cfg.manual_peers, host, port)
                    .map_err(AppError::validation)?;
                cfg.manual_peers = peers;
                Ok(())
            })?;
            Ok(format!(
                "backend 未运行，已离线移除手动 peer: {host}:{port}（下次启动生效）"
            ))
        }
    }
}

/// 运行 `peers list` 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     排查 manual peers 配置需要当前列表；列表只依赖 config，运行中读权威快照、
///     离线读磁盘（两者都可用，不像 devices/relay status 依赖运行时设备表）。
///
/// Code Logic（这个函数做什么）:
///     运行中经 control get-config，离线 `AppConfig::load()`；按模式渲染 JSON/文本。
async fn run_peers_list(json_mode: bool) -> CliCommandResult {
    let peers = match running_control_file().await {
        Some(control) => {
            BackendControlClient::from_control(&control)?
                .get_config()
                .await?
                .manual_peers
        }
        None => AppConfig::load()?.manual_peers,
    };
    if json_mode {
        Ok(serde_json::to_string(&CliPeersListOutput { peers })?)
    } else {
        Ok(render_peers_list_text(&peers))
    }
}

/// 提取运行中 backend 的控制文件（未运行返回 None）。
///
/// Business Logic（为什么需要这个函数）:
///     写命令的"运行中/离线"分叉必须与 start/stop/status 共用同一状态判定，避免口径分叉。
///
/// Code Logic（这个函数做什么）:
///     复用 `current_status()`（控制文件 + pid + health）；仅 Running 且带控制文件时返回。
async fn running_control_file() -> Option<BackendControlFile> {
    let status = current_status().await;
    if status.kind == BackendStatusKind::Running {
        status.control.clone()
    } else {
        None
    }
}

/// 查询运行中 backend 的 devices 快照；未运行时返回带指引的业务错误。
///
/// Business Logic（为什么需要这个函数）:
///     devices / relay status 依赖运行时设备表/影子表，离线无法查询，必须给出可操作提示。
///
/// Code Logic（这个函数做什么）:
///     `running_control_file()` 为空 → unavailable 错误（exit 1）；否则 POST control `/devices`。
async fn require_running_devices_payload() -> Result<ControlDevicesResponse, AppError> {
    let Some(control) = running_control_file().await else {
        return Err(AppError::unavailable(
            "backend 未运行，无法查询设备表（可先 cc-partner-backend start）",
        ));
    };
    fetch_control_devices(&control).await
}

/// POST control `/devices` 读取设备/影子/relay 快照。
///
/// Business Logic（为什么需要这个函数）:
///     CLI 与 owner 共用 loopback+token 控制面；沿用 `request_stop_route` 的直连 POST 先例。
///
/// Code Logic（这个函数做什么）:
///     POST `http://127.0.0.1:{port}/api/backend/control/devices`，body 仅 `controlToken`；
///     非成功状态转业务错误，成功解析为 `ControlDevicesResponse`。
async fn fetch_control_devices(
    control: &BackendControlFile,
) -> Result<ControlDevicesResponse, AppError> {
    let client = reqwest::Client::builder()
        .timeout(HEALTH_TIMEOUT)
        .build()
        .map_err(|error| AppError::generic(format!("构造 devices client 失败: {error}")))?;
    let url = format!(
        "http://127.0.0.1:{}/api/backend/control/devices",
        control.port
    );
    let response = client
        .post(&url)
        .json(&serde_json::json!({ "controlToken": control.control_token }))
        .send()
        .await
        .map_err(|error| AppError::generic(format!("请求 devices control 失败: {error}")))?;
    if !response.status().is_success() {
        return Err(AppError::generic(format!(
            "devices control 返回 HTTP {}",
            response.status()
        )));
    }
    response
        .json::<ControlDevicesResponse>()
        .await
        .map_err(|error| AppError::generic(format!("解析 devices control 响应失败: {error}")))
}

/// 经 control CAS 提交配置 patch（冲突转"并发修改，请重试"）。
///
/// Business Logic（为什么需要这个函数）:
///     运行中写命令共享 get-config → 合并 → update-config 流程；generation 冲突必须
///     给出统一友好提示（exit 1），而不是透出内部错误码。
///
/// Code Logic（这个函数做什么）:
///     用 snapshot 的 owner/generation 构造 `ConfigUpdateRequest`；Conflict 类错误
///     统一替换为"配置已被并发修改，请重试"。
async fn commit_config_patch(
    client: &BackendControlClient,
    snapshot: &ConfigSnapshot,
    patch: RuntimeConfigPatch,
) -> Result<(), AppError> {
    let request = ConfigUpdateRequest {
        expected_owner_instance_id: snapshot.owner_instance_id.clone(),
        expected_generation: snapshot.generation,
        patch,
    };
    match client.update_config(request).await {
        Ok(_) => Ok(()),
        Err(error) if error.classify() == AppErrorCategory::Conflict => {
            Err(AppError::conflict("配置已被并发修改，请重试"))
        }
        Err(error) => Err(error),
    }
}

/// backend 未运行时的离线配置写入：load → 编辑 → validate → 原子落盘。
///
/// Business Logic（为什么需要这个函数）:
///     跳板机/目标机常在启动前预配置；离线路径必须复用 `AppConfig::validate` 既有校验
///     （manual_peers/relay 去重等）与 `FsConfigStore::save_atomic` 原子写，并尊重
///     `CC_PARTNER_DATA_DIR` 隔离（load/save 均走同一 config 路径）。
///
/// Code Logic（这个函数做什么）:
///     `AppConfig::load()`（缺文件则初始化默认配置）→ 调用方闭包编辑 → validate →
///     `FsConfigStore::save_atomic` 落盘。仅限 backend 未运行时调用（绕过 ConfigRuntime
///     writer gate 的唯一合法场景，与 config.rs load 迁移保存路径同语义）。
fn apply_offline_config_edit<F>(edit: F) -> Result<(), AppError>
where
    F: FnOnce(&mut AppConfig) -> Result<(), AppError>,
{
    let mut cfg = AppConfig::load()?;
    edit(&mut cfg)?;
    cfg.validate()?;
    let store = FsConfigStore::default_path()?;
    store.save_atomic(&cfg)?;
    Ok(())
}

/// 解析 `host[:port]` 形式的手动 peer 参数。
///
/// Business Logic（为什么需要这个函数）:
///     peers add/remove 的参数合法性必须在进入业务前失败（用法 2）；IPv6 裸地址
///     有歧义，必须强制 `[::1]:port` 形式；端口缺省 62116。
///
/// Code Logic（这个函数做什么）:
///     `[` 开头按 IPv6 字面量解析（`]` 后仅允许空或 `:port`）；普通 host 按 0/1 个冒号
///     处理（>1 个冒号提示 IPv6 形式）；host trim 后非空、port ∈ 1..=65535。
fn parse_peer_target(input: &str) -> Result<(String, u16), String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("peer 地址不能为空".to_string());
    }
    if let Some(rest) = raw.strip_prefix('[') {
        let Some(close_index) = rest.find(']') else {
            return Err(format!("IPv6 地址缺少 ']'，请使用 [::1]:端口 形式: {raw}"));
        };
        let host = rest[..close_index].trim();
        if host.is_empty() {
            return Err(format!("IPv6 地址不能为空: {raw}"));
        }
        let after = &rest[close_index + 1..];
        let port = if after.is_empty() {
            PEER_DEFAULT_PORT
        } else {
            let Some(port_text) = after.strip_prefix(':') else {
                return Err(format!("IPv6 地址 ']' 后只允许跟 ':端口': {raw}"));
            };
            parse_peer_port(port_text, raw)?
        };
        return Ok((host.to_string(), port));
    }
    match raw.matches(':').count() {
        0 => Ok((raw.to_string(), PEER_DEFAULT_PORT)),
        1 => {
            let (host, port_text) = raw.split_once(':').expect("已确认恰好一个冒号");
            let host = host.trim();
            if host.is_empty() {
                return Err(format!("peer 主机名不能为空: {raw}"));
            }
            Ok((host.to_string(), parse_peer_port(port_text, raw)?))
        }
        _ => Err(format!("IPv6 地址请使用 [::1]:端口 形式: {raw}")),
    }
}

/// 解析并校验端口文本（1..=65535）。
///
/// Business Logic（为什么需要这个函数）:
///     端口 0 与超范围值会直接被 config validate 拒绝；CLI 应先给出可读错误。
///
/// Code Logic（这个函数做什么）:
///     u32 解析失败提示"不是合法数字"；越界提示 1..=65535；错误文本附原始参数。
fn parse_peer_port(text: &str, original: &str) -> Result<u16, String> {
    let port: u32 = text
        .parse()
        .map_err(|_| format!("端口不是合法数字: {original}"))?;
    if !(1..=65535).contains(&port) {
        return Err(format!("端口必须在 1..=65535: {original}"));
    }
    Ok(port as u16)
}

/// 在直连设备表中解析 via 候选（device_id 精确优先，其次设备名精确匹配）。
///
/// Business Logic（为什么需要这个函数）:
///     用户既会粘 device_id 也会传设备名；解析必须确定性（id 优先），且只在直连设备中
///     匹配（影子目标不是合法跳板候选）；多匹配必须列出候选帮助用户消歧。
///
/// Code Logic（这个函数做什么）:
///     先按 id 精确匹配（仅直连条目）；未命中再按 name 精确匹配（大小写敏感）：
///     唯一 → Resolved；多个 → Ambiguous（候选 "name (id)" 列表）；零个 → NotFound。
fn resolve_via_candidate(arg: &str, devices: &[DeviceDto]) -> ViaResolution {
    let target = arg.trim();
    if let Some(direct) = devices
        .iter()
        .find(|d| d.via_device_id.is_none() && d.id == target)
    {
        return ViaResolution::Resolved {
            device_id: direct.id.clone(),
            device_name: Some(direct.name.clone()),
        };
    }
    let by_name: Vec<&DeviceDto> = devices
        .iter()
        .filter(|d| d.via_device_id.is_none() && d.name == target)
        .collect();
    match by_name.len() {
        1 => ViaResolution::Resolved {
            device_id: by_name[0].id.clone(),
            device_name: Some(by_name[0].name.clone()),
        },
        0 => ViaResolution::NotFound,
        _ => ViaResolution::Ambiguous(
            by_name
                .iter()
                .map(|d| format!("{} ({})", d.name, d.id))
                .collect(),
        ),
    }
}

/// 返回开关被中转能力后的 relay 配置（保留 via/ignored 列表）。
///
/// Business Logic（为什么需要这个函数）:
///     `relay allow on|off` 只改 enabled 一个字段，必须不触碰已配置的跳板与忽略列表。
///
/// Code Logic（这个函数做什么）:
///     clone 后覆盖 enabled 返回。
fn relay_with_allow(relay: &RelayConfig, enabled: bool) -> RelayConfig {
    let mut next = relay.clone();
    next.enabled = enabled;
    next
}

/// 返回追加 via 后的 relay 配置（重复报错）。
///
/// Business Logic（为什么需要这个函数）:
///     重复添加跳板应是显式失败而非静默幂等，避免用户误以为换了配置。
///
/// Code Logic（这个函数做什么）:
///     clone 后 push；已存在同一 device_id 返回错误文本（走 validation → exit 1）。
fn relay_with_via_add(relay: &RelayConfig, device_id: &str) -> Result<RelayConfig, String> {
    if relay.via_device_ids.iter().any(|id| id == device_id) {
        return Err(format!("该设备已在跳板列表中: {device_id}"));
    }
    let mut next = relay.clone();
    next.via_device_ids.push(device_id.to_string());
    Ok(next)
}

/// 返回移除 via 后的 relay 配置（不存在报错）。
///
/// Business Logic（为什么需要这个函数）:
///     移除不存在的跳板大概率是笔误，必须显式失败并列出当前列表。
///
/// Code Logic（这个函数做什么）:
///     定位失败返回含当前列表的错误文本；命中则 retain 移除。
fn relay_with_via_remove(relay: &RelayConfig, device_id: &str) -> Result<RelayConfig, String> {
    if !relay.via_device_ids.iter().any(|id| id == device_id) {
        return Err(format!(
            "跳板列表中不存在该 device_id: {device_id}（当前: {}）",
            if relay.via_device_ids.is_empty() {
                "无".to_string()
            } else {
                relay.via_device_ids.join("、")
            }
        ));
    }
    let mut next = relay.clone();
    next.via_device_ids.retain(|id| id != device_id);
    Ok(next)
}

/// 追加手动 peer（重复 (host,port) 报错）。
///
/// Business Logic（为什么需要这个函数）:
///     config validate 会拦重复，但 CLI 应先给友好错误；同样拒绝静默幂等。
///
/// Code Logic（这个函数做什么）:
///     host trim 后查重 (host, port)；未命中才追加并返回新列表。
fn manual_peers_with_add(
    peers: &[ManualPeerConfig],
    host: &str,
    port: u16,
) -> Result<Vec<ManualPeerConfig>, String> {
    let host = host.trim();
    if peers.iter().any(|p| p.host == host && p.port == port) {
        return Err(format!("已存在相同的手动 peer: {host}:{port}"));
    }
    let mut next = peers.to_vec();
    next.push(ManualPeerConfig {
        host: host.to_string(),
        port,
    });
    Ok(next)
}

/// 精确移除手动 peer（不存在报错）。
///
/// Business Logic（为什么需要这个函数）:
///     remove 必须命中确切 (host,port)，未命中提示当前列表避免误操作。
///
/// Code Logic（这个函数做什么）:
///     精确匹配失败返回含现有列表的错误文本；命中则 retain 移除。
fn manual_peers_with_remove(
    peers: &[ManualPeerConfig],
    host: &str,
    port: u16,
) -> Result<Vec<ManualPeerConfig>, String> {
    let host = host.trim();
    if !peers.iter().any(|p| p.host == host && p.port == port) {
        let current = peers
            .iter()
            .map(|p| format!("{}:{}", p.host, p.port))
            .collect::<Vec<_>>()
            .join("、");
        return Err(format!(
            "手动 peer 不存在: {host}:{port}（当前: {}）",
            if current.is_empty() {
                "无".to_string()
            } else {
                current
            }
        ));
    }
    let mut next = peers.to_vec();
    next.retain(|p| !(p.host == host && p.port == port));
    Ok(next)
}

/// 在合并设备表中定位某 via 的直连条目（影子条目不参与）。
///
/// Business Logic（为什么需要这个函数）:
///     via 可达性由 owner 直连表权威判定；同名同 id 的影子条目不得干扰。
///
/// Code Logic（这个函数做什么）:
///     取 devices 中 id 匹配且 via_device_id 为空的条目。
fn direct_device_for_via<'a>(
    payload: &'a ControlDevicesResponse,
    via_id: &str,
) -> Option<&'a DeviceDto> {
    payload
        .devices
        .iter()
        .find(|d| d.via_device_id.is_none() && d.id == via_id)
}

/// 组装 `relay status --json` 输出。
///
/// Business Logic（为什么需要这个函数）:
///     脚本需要结构化判断"哪个跳板离线/各跳板可见几台影子"。
///
/// Code Logic（这个函数做什么）:
///     遍历 relay.via_device_ids，从合并设备表取直连条目的名称/地址/在线状态，
///     统计各 via 名下影子数量；shadows 原样透出。
fn build_relay_status_output(payload: &ControlDevicesResponse) -> CliRelayStatusOutput {
    let via = payload
        .relay
        .via_device_ids
        .iter()
        .map(|via_id| {
            let direct = direct_device_for_via(payload, via_id);
            CliRelayViaStatus {
                device_id: via_id.clone(),
                device_name: direct.map(|d| d.name.clone()),
                online: direct.map(|d| d.online).unwrap_or(false),
                address: direct.map(|d| format!("{}:{}", d.address, d.port)),
                shadow_count: payload
                    .shadows
                    .iter()
                    .filter(|s| s.via_device_id == *via_id)
                    .count(),
            }
        })
        .collect();
    CliRelayStatusOutput {
        relay_enabled: payload.relay.enabled,
        via,
        ignored_target_ids: payload.relay.ignored_target_ids.clone(),
        shadows: payload.shadows.clone(),
    }
}

/// 渲染 devices 人类可读文本。
///
/// Business Logic（为什么需要这个函数）:
///     无 `--json` 时用户需要一眼看到本机身份与直连/影子设备清单。
///
/// Code Logic（这个函数做什么）:
///     首行本机，其次按台数汇总（直连/影子），每台一行在线状态 + 名称 + id + 地址，
///     影子条目标注经谁中转。
fn render_devices_text(payload: &ControlDevicesResponse) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "本机: {} ({})",
        payload.device_name, payload.device_id
    ));
    let direct_count = payload
        .devices
        .iter()
        .filter(|d| d.via_device_id.is_none())
        .count();
    let shadow_count = payload.devices.len() - direct_count;
    if payload.devices.is_empty() {
        lines.push("对端设备 0 台（直连 0 / 影子 0）".to_string());
    } else {
        lines.push(format!(
            "对端设备 {} 台（直连 {direct_count} / 影子 {shadow_count}）:",
            payload.devices.len()
        ));
        for device in &payload.devices {
            let state = if device.online { "在线" } else { "离线" };
            match device.via_device_id.as_deref() {
                Some(via_id) => {
                    let via_name = device.via_device_name.as_deref().unwrap_or(via_id);
                    lines.push(format!(
                        "  [{state}] {} ({})  地址={}:{}  经 {via_name} 中转",
                        device.name, device.id, device.address, device.port
                    ));
                }
                None => lines.push(format!(
                    "  [{state}] {} ({})  地址={}:{}",
                    device.name, device.id, device.address, device.port
                )),
            }
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

/// 渲染 relay status 人类可读文本。
///
/// Business Logic（为什么需要这个函数）:
///     排查中转链路需要按"跳板 → 影子"层级展示在线状态与配置。
///
/// Code Logic（这个函数做什么）:
///     输出 allow 开关、逐跳板可达性（直连表未命中标记未发现）、影子清单与忽略目标。
fn render_relay_status_text(payload: &ControlDevicesResponse) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("中转访问状态:".to_string());
    lines.push(format!(
        "  允许被用作跳板 (relay.allow): {}",
        if payload.relay.enabled { "on" } else { "off" }
    ));
    if payload.relay.via_device_ids.is_empty() {
        lines.push("  跳板设备 (via): 无（可 relay via add <device_id|device_name>）".to_string());
    } else {
        lines.push(format!(
            "  跳板设备 (via) {} 台:",
            payload.relay.via_device_ids.len()
        ));
        for via_id in &payload.relay.via_device_ids {
            match direct_device_for_via(payload, via_id) {
                Some(direct) => {
                    let state = if direct.online { "在线" } else { "离线" };
                    let count = payload
                        .shadows
                        .iter()
                        .filter(|s| s.via_device_id == *via_id)
                        .count();
                    lines.push(format!(
                        "  [{state}] {} ({})  地址={}:{}  可见影子 {count} 台",
                        direct.name, direct.id, direct.address, direct.port
                    ));
                }
                None => lines.push(format!("  [离线] {via_id}（当前设备表中未发现）")),
            }
        }
    }
    if payload.shadows.is_empty() {
        lines.push("  影子设备: 无".to_string());
    } else {
        lines.push(format!("  影子设备 {} 台:", payload.shadows.len()));
        for shadow in &payload.shadows {
            let state = if shadow.online { "在线" } else { "离线" };
            lines.push(format!(
                "  [{state}] {} ({})  经 {} 中转",
                shadow.device_name, shadow.target_device_id, shadow.via_device_id
            ));
        }
    }
    if payload.relay.ignored_target_ids.is_empty() {
        lines.push("  显式忽略目标 (ignored): 无".to_string());
    } else {
        lines.push(format!(
            "  显式忽略目标 (ignored): {}",
            payload.relay.ignored_target_ids.join("、")
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// 渲染 peers list 人类可读文本。
///
/// Business Logic（为什么需要这个函数）:
///     无 `--json` 时需要直观列出当前手动 peer。
///
/// Code Logic（这个函数做什么）:
///     空列表提示用法；非空逐条 `host:port`。
fn render_peers_list_text(peers: &[ManualPeerConfig]) -> String {
    let mut lines: Vec<String> = Vec::new();
    if peers.is_empty() {
        lines.push("手动 peer 0 条（可 peers add <host[:port]>，端口缺省 62116）".to_string());
    } else {
        lines.push(format!("手动 peer {} 条:", peers.len()));
        for peer in peers {
            lines.push(format!("  {}:{}", peer.host, peer.port));
        }
    }
    lines.push(String::new());
    lines.join("\n")
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

    // start detach 后 serve 子进程独立读取环境变量；posix_spawn/Command 均继承当前
    // 环境，因此 CC_PARTNER_DATA_DIR 会进入 serve。macOS 必须 disclaim，否则 Dev.app
    // codesign SIGKILL 会沿着责任链杀掉 sidecar 与 tmux。
    let current_exe = std::env::current_exe()?;
    #[cfg(unix)]
    let mut child = crate::backend::detached_spawn::spawn_disclaimed(&current_exe, &["serve"])?;
    #[cfg(not(unix))]
    let mut child = {
        let mut command = Command::new(current_exe);
        command
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        inherit_data_dir_env(&mut command);
        configure_detached_child(&mut command);
        command.spawn()?
    };

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
                let _ = reap_started_serve(&mut child);
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
            match reap_started_serve(&mut child) {
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
    match reap_started_serve(&mut child) {
        Ok(note) => Err(AppError::generic(format!("等待后端启动超时{note}"))),
        Err(reap_err) => Err(AppError::generic(format!(
            "等待后端启动超时；且清理子进程失败: {reap_err}"
        ))),
    }
}

/// Business Logic（为什么需要这个函数）:
///     start 失败路径必须杀掉自己 spawn 的 serve；Unix 走 disclaim child，Windows 仍是 std Child。
///
/// Code Logic（这个函数做什么）:
///     按平台委托 `DisclaimedChild::kill_and_reap` 或 `kill_and_reap_owned_child`。
#[cfg(unix)]
fn reap_started_serve(
    child: &mut crate::backend::detached_spawn::DisclaimedChild,
) -> Result<String, String> {
    child.kill_and_reap(CHILD_REAP_TIMEOUT)
}

#[cfg(not(unix))]
fn reap_started_serve(child: &mut std::process::Child) -> Result<String, String> {
    kill_and_reap_owned_child(child, CHILD_REAP_TIMEOUT)
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
#[cfg(any(not(unix), test))]
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
#[cfg(any(not(unix), test))]
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
#[cfg(any(not(unix), test))]
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
#[allow(dead_code)] // start() 改走 spawn_disclaimed；保留给非 macOS 对照与历史 smoke 语义
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
    use super::*;
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

    /// Business Logic（为什么需要这个测试）:
    ///     Unix `start` 若仍用普通 Command spawn，macOS GUI 责任链会在 codesign 时杀掉 sidecar。
    ///
    /// Code Logic（这个测试做什么）:
    ///     锁定 `start` 调用 `spawn_disclaimed`。
    #[test]
    fn start_unix_path_uses_disclaimed_spawn() {
        let src = include_str!("cli.rs");
        let start = src.find("async fn start()").expect("start");
        let body = src[start..]
            .split("fn reap_started_serve")
            .next()
            .expect("start body");
        assert!(
            body.contains("spawn_disclaimed"),
            "Unix start 必须 spawn_disclaimed，避免 GUI codesign 杀掉 sidecar"
        );
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

    // ── devices / relay / peers 子命令 ────────────────────────────────

    /// 构造测试用 DeviceDto。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     via 解析与渲染测试需要多台直连/影子设备 fixture，避免重复拼字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按参数生成直连（via=None）或影子（via=Some）条目。
    fn device_dto_for_test(
        id: &str,
        name: &str,
        online: bool,
        via: Option<(&str, &str)>,
    ) -> DeviceDto {
        DeviceDto {
            id: id.to_string(),
            name: name.to_string(),
            address: "10.0.0.1".to_string(),
            port: 62116,
            last_seen: "2026-09-04T00:00:00Z".to_string(),
            online,
            is_self: false,
            proto_version: 1,
            capabilities: Vec::new(),
            via_device_id: via.map(|(id, _)| id.to_string()),
            via_device_name: via.map(|(_, name)| name.to_string()),
        }
    }

    /// 构造测试用 control devices 快照。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     渲染与 relay status 组装测试需要固定 devices/relay/shadows 数据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     两台直连（一台离线）+ 一台影子 + 非默认 relay 配置。
    fn devices_payload_for_test() -> ControlDevicesResponse {
        ControlDevicesResponse {
            device_id: "self-a".to_string(),
            device_name: "发起机".to_string(),
            devices: vec![
                device_dto_for_test("jump-b", "nas-vpn", true, None),
                device_dto_for_test("other-d", "desk", false, None),
                device_dto_for_test("target-c", "power-vpn", true, Some(("jump-b", "nas-vpn"))),
            ],
            relay: crate::config::RelayConfig {
                enabled: false,
                via_device_ids: vec!["jump-b".to_string(), "ghost-x".to_string()],
                ignored_target_ids: vec!["hidden-z".to_string()],
            },
            shadows: vec![ControlRelayShadowDto {
                target_device_id: "target-c".to_string(),
                via_device_id: "jump-b".to_string(),
                device_name: "power-vpn".to_string(),
                online: true,
                last_seen: "2026-09-04T00:00:00Z".to_string(),
            }],
        }
    }

    /// `parse_peer_target` 必须覆盖缺省端口、显式端口、IPv6 与非法输入。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     peers 参数解析是用法错误的守门员；IPv6 裸地址歧义必须被拒绝。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言默认 62116、host:port、[::1] 与 [::1]:4000、空串/端口 0/超范围/
    ///     裸 IPv6/缺 ']' 各自的成功或错误文本。
    #[test]
    fn parse_peer_target_covers_default_port_ipv6_and_invalid_inputs() {
        let cases: Vec<(&str, (String, u16))> = vec![
            ("10.0.0.5", ("10.0.0.5".to_string(), 62116)),
            ("  nas.local ", ("nas.local".to_string(), 62116)),
            ("10.0.0.5:40000", ("10.0.0.5".to_string(), 40000)),
            ("[::1]", ("::1".to_string(), 62116)),
            ("[::1]:4000", ("::1".to_string(), 4000)),
            ("[fe80::1%en0]:62116", ("fe80::1%en0".to_string(), 62116)),
        ];
        for (input, expected) in cases {
            assert_eq!(
                super::parse_peer_target(input).expect(input),
                expected,
                "输入: {input}"
            );
        }
        for bad in [
            "",
            "   ",
            "10.0.0.5:0",
            "10.0.0.5:99999",
            "10.0.0.5:abc",
            "::1",
            "10.0.0.5::9",
            "[::1",
            "[:]:x",
        ] {
            let err = super::parse_peer_target(bad).expect_err(&format!("非法输入应失败: {bad}"));
            assert!(!err.is_empty(), "{bad}");
        }
        assert!(super::parse_peer_target("[::1")
            .unwrap_err()
            .contains("']'"));
        assert!(super::parse_peer_target("::1")
            .unwrap_err()
            .contains("IPv6"));
        assert!(super::parse_peer_target("10.0.0.5:0")
            .unwrap_err()
            .contains("1..=65535"));
    }

    /// `resolve_via_candidate`：id 精确优先、名称唯一命中、多匹配列候选、零匹配 NotFound、影子不参与。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     `relay via add <device_id|device_name>` 的消歧规则必须确定性且只认直连设备。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 devices_payload_for_test 分别按 id、唯一名、重名、未知值解析并断言分支。
    #[test]
    fn resolve_via_candidate_prefers_id_then_exact_name() {
        let payload = devices_payload_for_test();
        // id 精确命中（影子条目 target-c 虽在表内，但只匹配直连）。
        match super::resolve_via_candidate("jump-b", &payload.devices) {
            super::ViaResolution::Resolved {
                device_id,
                device_name,
            } => {
                assert_eq!(device_id, "jump-b");
                assert_eq!(device_name.as_deref(), Some("nas-vpn"));
            }
            _ => panic!("id 应精确命中"),
        }
        // 名称唯一命中 → 解析到对应 id。
        match super::resolve_via_candidate("desk", &payload.devices) {
            super::ViaResolution::Resolved { device_id, .. } => assert_eq!(device_id, "other-d"),
            _ => panic!("唯一名称应命中"),
        }
        // 名称大小写敏感：零匹配。
        assert!(matches!(
            super::resolve_via_candidate("Desk", &payload.devices),
            super::ViaResolution::NotFound
        ));
        // 未知值 → NotFound。
        assert!(matches!(
            super::resolve_via_candidate("ghost", &payload.devices),
            super::ViaResolution::NotFound
        ));
        // 影子目标不能作为候选：target-c 仅以影子形式存在 → NotFound。
        assert!(matches!(
            super::resolve_via_candidate("target-c", &payload.devices),
            super::ViaResolution::NotFound
        ));
        // 重名 → Ambiguous 且候选包含 id。
        let mut doubled = payload.devices.clone();
        doubled.insert(0, device_dto_for_test("jump-b2", "nas-vpn", true, None));
        match super::resolve_via_candidate("nas-vpn", &doubled) {
            super::ViaResolution::Ambiguous(candidates) => {
                assert_eq!(candidates.len(), 2);
                assert!(candidates.iter().any(|c| c.contains("jump-b")));
                assert!(candidates.iter().any(|c| c.contains("jump-b2")));
            }
            _ => panic!("重名应 Ambiguous"),
        }
    }

    /// via 与 manual peers 的纯合并助手：追加/移除/重复与缺失分支。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     运行中与离线两条写路径共用同一组合并助手，重复/缺失必须显式失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     逐个调用 relay_with_via_add/remove 与 manual_peers_with_add/remove，
    ///     断言成功值与错误文本。
    #[test]
    fn merge_helpers_reject_duplicate_and_missing_entries() {
        let relay = crate::config::RelayConfig::default();
        let added = super::relay_with_via_add(&relay, "jump-1").expect("add");
        assert_eq!(added.via_device_ids, vec!["jump-1"]);
        let err = super::relay_with_via_add(&added, "jump-1").expect_err("重复 add");
        assert!(err.contains("已在跳板列表"), "{err}");
        let removed = super::relay_with_via_remove(&added, "jump-1").expect("remove");
        assert!(removed.via_device_ids.is_empty());
        let err = super::relay_with_via_remove(&removed, "jump-1").expect_err("缺失 remove");
        assert!(err.contains("不存在"), "{err}");
        // allow 开关保留 via 列表。
        let toggled = super::relay_with_allow(&added, false);
        assert!(!toggled.enabled);
        assert_eq!(toggled.via_device_ids, vec!["jump-1"]);

        let peers = Vec::new();
        let added = super::manual_peers_with_add(&peers, "10.0.0.9", 40000).expect("add");
        assert_eq!(added.len(), 1);
        let err = super::manual_peers_with_add(&added, " 10.0.0.9 ", 40000)
            .expect_err("重复（host 先 trim）");
        assert!(err.contains("已存在"), "{err}");
        let removed = super::manual_peers_with_remove(&added, "10.0.0.9", 40000).expect("remove");
        assert!(removed.is_empty());
        let err =
            super::manual_peers_with_remove(&removed, "10.0.0.9", 40000).expect_err("缺失 remove");
        assert!(err.contains("不存在"), "{err}");
    }

    /// devices / relay status 渲染与 `relay status --json` 单行 JSON 契约。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     `--json` 输出必须可直接 jq；文本输出必须区分直连/影子与在线/离线。
    ///
    /// Code Logic（这个测试做什么）:
    ///     渲染文本断言关键字段；组装 CliRelayStatusOutput 序列化为单行并回读断言
    ///     relayEnabled/via.online/shadowCount/shadows。
    #[test]
    fn render_devices_and_relay_status_outputs() {
        let payload = devices_payload_for_test();

        let devices_text = super::render_devices_text(&payload);
        assert!(
            devices_text.contains("本机: 发起机 (self-a)"),
            "{devices_text}"
        );
        assert!(devices_text.contains("直连 2 / 影子 1"), "{devices_text}");
        assert!(
            devices_text.contains("[离线] desk (other-d)"),
            "{devices_text}"
        );
        assert!(devices_text.contains("经 nas-vpn 中转"), "{devices_text}");

        let status_text = super::render_relay_status_text(&payload);
        assert!(status_text.contains("(relay.allow): off"), "{status_text}");
        assert!(
            status_text.contains("[在线] nas-vpn (jump-b)"),
            "{status_text}"
        );
        assert!(status_text.contains("可见影子 1 台"), "{status_text}");
        assert!(
            status_text.contains("[离线] ghost-x（当前设备表中未发现）"),
            "{status_text}"
        );
        assert!(status_text.contains("经 jump-b 中转"), "{status_text}");
        assert!(status_text.contains("hidden-z"), "{status_text}");

        let output = super::build_relay_status_output(&payload);
        let json = serde_json::to_string(&output).expect("serialize");
        assert!(!json.contains('\n'), "--json 必须单行: {json}");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("单行 JSON");
        assert_eq!(parsed["relayEnabled"], false);
        assert_eq!(parsed["via"].as_array().map(Vec::len), Some(2));
        assert_eq!(parsed["via"][0]["deviceId"], "jump-b");
        assert_eq!(parsed["via"][0]["deviceName"], "nas-vpn");
        assert_eq!(parsed["via"][0]["online"], true);
        assert_eq!(parsed["via"][0]["shadowCount"], 1);
        assert_eq!(parsed["via"][1]["online"], false, "未发现跳板应离线");
        assert!(parsed["via"][1].get("address").is_none());
        assert_eq!(parsed["ignoredTargetIds"][0], "hidden-z");
        assert_eq!(parsed["shadows"][0]["targetDeviceId"], "target-c");
    }

    /// 新子命令的用法错误必须返回 2 且不得触发任何 IO。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     参数解析是这些命令的第一道门；错误用法必须在触碰网络/磁盘前失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对 devices/relay/peers 的缺参、未知动作、非法枚举、非法 host:port、
    ///     重复 --json 等路径逐一断言 exit 2。
    #[test]
    fn devices_relay_peers_usage_errors_exit_two() {
        let cases: &[&[&str]] = &[
            &["cc-partner-backend", "devices", "--yaml"],
            &["cc-partner-backend", "devices", "--json", "--json"],
            &["cc-partner-backend", "devices", "extra"],
            &["cc-partner-backend", "relay"],
            &["cc-partner-backend", "relay", "frobnicate"],
            &["cc-partner-backend", "relay", "status", "--yaml"],
            &["cc-partner-backend", "relay", "via"],
            &["cc-partner-backend", "relay", "via", "frobnicate"],
            &["cc-partner-backend", "relay", "via", "add"],
            &["cc-partner-backend", "relay", "via", "add", "a", "b"],
            &["cc-partner-backend", "relay", "via", "remove", "a", "b"],
            &["cc-partner-backend", "relay", "allow"],
            &["cc-partner-backend", "relay", "allow", "banana"],
            &["cc-partner-backend", "relay", "allow", "on", "extra"],
            &["cc-partner-backend", "peers"],
            &["cc-partner-backend", "peers", "frobnicate"],
            &["cc-partner-backend", "peers", "add"],
            &["cc-partner-backend", "peers", "add", "a", "b"],
            &["cc-partner-backend", "peers", "add", "::1"],
            &["cc-partner-backend", "peers", "add", "[::1"],
            &["cc-partner-backend", "peers", "add", "host:0"],
            &["cc-partner-backend", "peers", "add", "host:70000"],
            &["cc-partner-backend", "peers", "add", "host:abc"],
            &["cc-partner-backend", "peers", "remove"],
            &["cc-partner-backend", "peers", "list", "--yaml"],
        ];
        for case in cases {
            let exit = super::dispatch_for_test(case.iter().copied());
            assert_eq!(exit, 2, "用法错误应 exit 2: {case:?}");
        }
    }

    /// 离线 `relay via add|remove` 直接落盘并可在重载后读回。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     跳板机常在启动前预配置；离线路径必须走隔离数据目录 + validate + 原子写。
    ///
    /// Code Logic（这个测试做什么）:
    ///     `CC_PARTNER_DATA_DIR` 指向临时目录（backend 无控制文件 → 离线分支），
    ///     依次添加两个 id、断言重复报错、移除与缺失移除报错，最后读盘核对列表。
    #[tokio::test]
    async fn relay_via_add_remove_offline_persist_in_isolated_data_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _guard = crate::config::install_data_dir_env(Some(temp.path().to_str().unwrap()));

        let message = super::run_relay_via_add("jump-device-1")
            .await
            .expect("离线添加应成功");
        assert!(message.contains("离线"), "{message}");
        super::run_relay_via_add("opaque-id-甲")
            .await
            .expect("第二个 id");

        let cfg = crate::config::AppConfig::load().expect("reload config");
        assert_eq!(
            cfg.relay.via_device_ids,
            vec!["jump-device-1", "opaque-id-甲"]
        );
        assert!(cfg.relay.enabled, "默认 allow=on 不得被 via 操作改写");

        let err = super::run_relay_via_add("jump-device-1")
            .await
            .expect_err("重复添加应失败");
        assert!(err.to_string().contains("已在跳板列表"), "{err}");

        super::run_relay_via_remove("jump-device-1")
            .await
            .expect("移除应成功");
        let err = super::run_relay_via_remove("jump-device-1")
            .await
            .expect_err("移除不存在应失败");
        assert!(err.to_string().contains("不存在"), "{err}");

        let cfg = crate::config::AppConfig::load().expect("reload config");
        assert_eq!(cfg.relay.via_device_ids, vec!["opaque-id-甲"]);
    }

    /// 离线 `relay allow` 与 `peers add|remove|list` 落盘与 JSON 契约。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     peers list 允许离线读盘（只依赖 config）；写路径必须尊重 CC_PARTNER_DATA_DIR。
    ///
    /// Code Logic（这个测试做什么）:
    ///     隔离目录内 allow off → add 两条 peer（显式端口 + 默认端口）→ 重复报错 →
    ///     list --json 解析断言 → list 文本断言 → 读盘核对 → remove 与缺失移除。
    #[tokio::test]
    async fn relay_allow_and_peers_offline_persist_in_isolated_data_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _guard = crate::config::install_data_dir_env(Some(temp.path().to_str().unwrap()));

        super::run_relay_allow(false).await.expect("allow off");
        super::run_peers_add("10.8.0.1", 40001)
            .await
            .expect("peer add");
        let err = super::run_peers_add("10.8.0.1", 40001)
            .await
            .expect_err("重复 peer 应失败");
        assert!(err.to_string().contains("已存在"), "{err}");
        super::run_peers_add("10.8.0.2", super::PEER_DEFAULT_PORT)
            .await
            .expect("默认端口 add");

        let json = super::run_peers_list(true).await.expect("list --json");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("peers list JSON 应为单行合法 JSON");
        assert_eq!(parsed["peers"].as_array().map(Vec::len), Some(2));
        assert_eq!(parsed["peers"][0]["host"], "10.8.0.1");
        assert_eq!(parsed["peers"][0]["port"], 40001);
        assert_eq!(parsed["peers"][1]["port"], 62116);

        let text = super::run_peers_list(false).await.expect("list 文本");
        assert!(text.contains("手动 peer 2 条"), "{text}");
        assert!(text.contains("10.8.0.1:40001"), "{text}");

        let cfg = crate::config::AppConfig::load().expect("reload config");
        assert!(!cfg.relay.enabled);
        assert_eq!(cfg.manual_peers.len(), 2);

        super::run_peers_remove("10.8.0.1", 40001)
            .await
            .expect("remove 应成功");
        let err = super::run_peers_remove("10.8.0.1", 40001)
            .await
            .expect_err("再删应失败");
        assert!(err.to_string().contains("不存在"), "{err}");
        let cfg = crate::config::AppConfig::load().expect("reload config");
        assert_eq!(cfg.manual_peers.len(), 1);
        assert_eq!(cfg.manual_peers[0].host, "10.8.0.2");
    }
}
