//! backend/doctor.rs — Doctor 健康检查快照 schema、状态聚合、隐私归一化与有界 probe。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户与 smoke/CI 需要一份可机器解析、可人工阅读的后端健康快照，
//!     同时保证路径/摘要中不泄露 home 用户名、项目名、Prompt 或凭据。
//!
//! Code Logic（这个模块做什么）:
//!     定义 camelCase `DoctorSnapshot` 及其子 DTO、检查严重度与 overall 状态计算，
//!     home/`<HOME>` 隐私 helper，以及 control/health/path/mDNS/dependency/recent-error
//!     的有界、非 mutating probe 采集（超时、脱敏、不枚举项目）。

use crate::backend::control::{
    control_file_path, process_is_alive, BackendControlFile, BackendStatus, BackendStatusKind,
};
use crate::config::{backend_log_path, data_dir};
use crate::error::AppError;
use crate::workbench::dependencies::{
    probe_claude_cli_non_mutating_with_budget, probe_git_non_mutating_with_budget,
    probe_tmux_non_mutating_with_budget, probe_wsl_non_mutating_with_budget,
    OptionalDependencyProbe, ProbeRuntimeGuard,
};
use chrono::Utc;
use mdns_sd::ServiceDaemon;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::net::{SocketAddr, TcpListener};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

/// Doctor 快照 schema 版本（当前固定为 1）。
pub const DOCTOR_SCHEMA_VERSION: u32 = 1;

/// 隐私归一化后 home 的占位符。
pub const HOME_PLACEHOLDER: &str = "<HOME>";

/// Doctor 整体健康状态。
///
/// Business Logic（为什么需要这个枚举）:
///     CLI 退出码与机器可读 JSON 需要把多种检查结果收敛为 healthy/degraded/unhealthy 三档。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 序列化为 `healthy` / `degraded` / `unhealthy`。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DoctorStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

impl DoctorStatus {
    /// 映射到 CLI 退出码契约。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `doctor` / `doctor --json` 的进程退出码必须与整体状态一致（0/1/2）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     healthy→0，degraded→1，unhealthy→2。
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Healthy => 0,
            Self::Degraded => 1,
            Self::Unhealthy => 2,
        }
    }
}

/// 应用/后端版本信息。
///
/// Business Logic（为什么需要这个结构）:
///     诊断报告需要标明当前 app 与 backend 二进制版本，便于跨设备对照。
///
/// Code Logic（这个结构做什么）:
///     camelCase 输出 `app` 与 `backend` 字符串。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorVersion {
    pub app: String,
    pub backend: String,
}

/// 运行平台信息。
///
/// Business Logic（为什么需要这个结构）:
///     路径/依赖/进程语义随 OS/arch 变化，快照必须标明当前平台。
///
/// Code Logic（这个结构做什么）:
///     camelCase 输出 `os` 与 `arch` 字符串。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorPlatform {
    pub os: String,
    pub arch: String,
}

/// 单项检查严重度。
///
/// Business Logic（为什么需要这个枚举）:
///     每项 probe 需要稳定的 ok/warning/error/info 级别，供 overall 聚合与 UI 着色。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 序列化为 `ok` / `warning` / `error` / `info`。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DoctorCheckStatus {
    Ok,
    Warning,
    Error,
    Info,
}

/// 单项检查结果。
///
/// Business Logic（为什么需要这个结构）:
///     每项 probe 需要稳定 code + 已脱敏 summary，便于脚本分支与人工阅读。
///
/// Code Logic（这个结构做什么）:
///     持有 status/code/summary；summary 应由调用方先过隐私归一化。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    pub status: DoctorCheckStatus,
    pub code: String,
    pub summary: String,
}

impl DoctorCheck {
    /// 构造单项检查结果。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     probe 与测试需要统一入口，避免字段漏填。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 status/code/summary 装入 `DoctorCheck`；summary 不做二次脱敏。
    pub fn new(
        status: DoctorCheckStatus,
        code: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            status,
            code: code.into(),
            summary: summary.into(),
        }
    }
}

/// 后端进程/控制面检查块。
///
/// Business Logic（为什么需要这个结构）:
///     doctor 需要同时暴露控制文件路径、pid/port 与 health 判定，而不是只给一个总状态。
///
/// Code Logic（这个结构做什么）:
///     持有 state 字符串、controlPath、可选 pid/port 与 health `DoctorCheck`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorBackendCheck {
    pub state: String,
    pub control_path: String,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub health: DoctorCheck,
}

/// 核心路径检查块（data / database / log）。
///
/// Business Logic（为什么需要这个结构）:
///     data/db/log 是核心可写路径；任一不可用应抬升为 unhealthy。
///
/// Code Logic（这个结构做什么）:
///     持有三个 `DoctorCheck`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorPathChecks {
    pub data: DoctorCheck,
    pub database: DoctorCheck,
    pub log: DoctorCheck,
}

/// 可选依赖检查块。
///
/// Business Logic（为什么需要这个结构）:
///     Git/tmux/WSL/Claude CLI 缺失只应 degraded，不得伪装成基础设施失败。
///
/// Code Logic（这个结构做什么）:
///     持有 git/tmux/wsl/claudeCli 四个 `DoctorCheck`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorDependencies {
    pub git: DoctorCheck,
    pub tmux: DoctorCheck,
    pub wsl: DoctorCheck,
    pub claude_cli: DoctorCheck,
}

/// 近期错误摘要条目。
///
/// Business Logic（为什么需要这个结构）:
///     doctor 需要展示最近有界条数的错误码与脱敏摘要，便于定位，不泄露正文。
///
/// Code Logic（这个结构做什么）:
///     持有 timestamp/code/summary 与可选 requestId。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorErrorSummary {
    pub timestamp: String,
    pub code: String,
    pub summary: String,
    pub request_id: Option<String>,
}

/// Doctor 完整快照（schemaVersion=1）。
///
/// Business Logic（为什么需要这个结构）:
///     `doctor --json` stdout 只输出这一份强类型 JSON，供脚本与人工排障消费。
///
/// Code Logic（这个结构做什么）:
///     camelCase 聚合 schemaVersion/generatedAt/status/version/platform/backend/paths/
///     mdns/dependencies/recentErrors/logPath；可选 logParseWarning 表示畸形日志 warning；
///     不含环境变量 map 或项目枚举。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorSnapshot {
    pub schema_version: u32,
    pub generated_at: String,
    pub status: DoctorStatus,
    pub version: DoctorVersion,
    pub platform: DoctorPlatform,
    pub backend: DoctorBackendCheck,
    pub paths: DoctorPathChecks,
    pub mdns: DoctorCheck,
    pub dependencies: DoctorDependencies,
    pub recent_errors: Vec<DoctorErrorSummary>,
    pub log_path: String,
    /// 读取 recent-errors 时发现的畸形受控 JSON 行 warning（无则省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_parse_warning: Option<DoctorCheck>,
}

/// 根据各检查结果计算 overall 状态。
///
/// Business Logic（为什么需要这个函数）:
///     healthy/degraded/unhealthy 必须按核心/可选问题严格分层，避免 stopped backend
///     被误报 warning，或 mDNS/可选依赖缺失被抬升为 unhealthy。
///
/// Code Logic（这个函数做什么）:
///     - 任一核心路径（data/database/log）为 error，或 backend.health 为 error → unhealthy
///     - 否则任一 warning（含 mDNS、可选依赖、可恢复 stale control 等）→ degraded
///     - 否则 healthy（info/ok 不抬升；stopped backend 的 info 仍属 healthy）
pub fn compute_overall_status(
    backend: &DoctorBackendCheck,
    paths: &DoctorPathChecks,
    mdns: &DoctorCheck,
    dependencies: &DoctorDependencies,
    log_parse_warning: Option<&DoctorCheck>,
) -> DoctorStatus {
    let core_checks = [&paths.data, &paths.database, &paths.log, &backend.health];
    if core_checks
        .iter()
        .any(|check| check.status == DoctorCheckStatus::Error)
    {
        return DoctorStatus::Unhealthy;
    }

    let optional_and_aux = [
        mdns,
        &dependencies.git,
        &dependencies.tmux,
        &dependencies.wsl,
        &dependencies.claude_cli,
    ];
    // backend.health 的 warning（如 recoverable stale）与畸形日志 warning 也算 degraded
    let has_warning = core_checks
        .iter()
        .chain(optional_and_aux.iter())
        .any(|check| check.status == DoctorCheckStatus::Warning)
        || log_parse_warning
            .map(|c| c.status == DoctorCheckStatus::Warning)
            .unwrap_or(false);

    if has_warning {
        DoctorStatus::Degraded
    } else {
        DoctorStatus::Healthy
    }
}

/// 解析当前进程可见的 home 目录。
///
/// Business Logic（为什么需要这个函数）:
///     隐私归一化需要把真实 home 前缀替换为占位符，避免泄露用户名。
///
/// Code Logic（这个函数做什么）:
///     调用 `dirs::home_dir()`；失败返回 None，调用方应跳过替换。
pub fn current_home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// 将路径中的 home 前缀归一化为 `<HOME>`。
///
/// Business Logic（为什么需要这个函数）:
///     doctor 快照中的 controlPath/logPath 等路径字段不得暴露真实用户名目录。
///
/// Code Logic（这个函数做什么）:
///     若 path 以 home 为前缀（组件级匹配），则输出以 `<HOME>` 开头的展示字符串；
///     否则返回 path 的 lossy 字符串。Windows/Unix 分隔符统一为 `/` 便于稳定 JSON。
pub fn normalize_home_in_path(path: &Path, home: Option<&Path>) -> String {
    let display = path_to_display(path);
    let Some(home) = home else {
        return display;
    };
    let home_display = path_to_display(home);
    if display == home_display {
        return HOME_PLACEHOLDER.to_string();
    }
    let prefix = format!("{home_display}/");
    if let Some(rest) = display.strip_prefix(&prefix) {
        return format!("{HOME_PLACEHOLDER}/{rest}");
    }
    // 兼容 Windows 反斜杠展示前的原始字符串匹配
    let raw = path.to_string_lossy();
    let home_raw = home.to_string_lossy();
    if raw == home_raw {
        return HOME_PLACEHOLDER.to_string();
    }
    #[cfg(windows)]
    {
        let prefix_bs = format!("{home_raw}\\");
        if let Some(rest) = raw.strip_prefix(prefix_bs.as_str()) {
            let rest = rest.replace('\\', "/");
            return format!("{HOME_PLACEHOLDER}/{rest}");
        }
    }
    display
}

/// 将任意文本中的 home 绝对路径前缀替换为 `<HOME>`。
///
/// Business Logic（为什么需要这个函数）:
///     summary/recentErrors 等自由文本也可能嵌入绝对路径，需与路径字段同样脱敏。
///
/// Code Logic（这个函数做什么）:
///     在 text 中查找 home 的展示串与原始 lossy 串，整段替换为 `<HOME>`；
///     home 未知时原样返回。
pub fn normalize_home_in_text(text: &str, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return text.to_string();
    };
    let home_display = path_to_display(home);
    let home_raw = home.to_string_lossy();
    let mut out = text.replace(&home_display, HOME_PLACEHOLDER);
    if home_raw.as_ref() != home_display.as_str() {
        out = out.replace(home_raw.as_ref(), HOME_PLACEHOLDER);
    }
    // Windows 原始路径可能混用反斜杠
    #[cfg(windows)]
    {
        let home_bs = home_raw.replace('/', "\\");
        if home_bs != home_raw.as_ref() {
            out = out.replace(&home_bs, HOME_PLACEHOLDER);
        }
    }
    out
}

/// 对 doctor 文本字段做隐私清洗（home + 敏感字面量）。
///
/// Business Logic（为什么需要这个函数）:
///     doctor JSON 不得泄露用户名路径、项目名、Prompt 正文、token 或文件内容 sentinel。
///
/// Code Logic（这个函数做什么）:
///     先 `normalize_home_in_text`，再按保守规则抹掉常见 secret/payload 形态与
///     用户工程路径段（`<HOME>/` 下非 `.cc-partner` 前缀的路径折叠为 `<HOME>/<REDACTED>`）。
pub fn sanitize_doctor_text(text: &str, home: Option<&Path>) -> String {
    let mut out = normalize_home_in_text(text, home);
    out = redact_secret_shapes(&out);
    out = redact_user_project_paths(&out);
    out
}

/// 抹掉 token/password/Authorization/Prompt 正文等敏感形态。
///
/// Business Logic（为什么需要这个函数）:
///     即便 probe 误把敏感片段写进 summary，序列化前也要挡住。
///
/// Code Logic（这个函数做什么）:
///     按字符扫描，ASCII 大小写无关匹配 key=value / Bearer / file-sentinel 形态并替换。
fn redact_secret_shapes(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let chars: Vec<char> = text.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();
    while i < chars.len() {
        if let Some((key_len, replacement)) = match_secret_key(&lower_chars, i) {
            out.push_str(replacement);
            i += key_len;
            // 跳过值（到空白）
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 在字符索引 i 匹配敏感键，返回 (key 长度含后续值前缀, 替换串)。
fn match_secret_key(lower: &[char], i: usize) -> Option<(usize, &'static str)> {
    const KEYS: &[&str] = &[
        "authorization:",
        "bearer ",
        "token=",
        "password=",
        "secret=",
        "api_key=",
        "apikey=",
        "prompt=",
        "file-sentinel-",
    ];
    for key in KEYS {
        let key_chars: Vec<char> = key.chars().collect();
        if lower[i..].starts_with(&key_chars) {
            return Some((key_chars.len(), "<REDACTED>"));
        }
    }
    None
}

/// 折叠 `<HOME>/` 下非应用数据目录的用户工程路径。
///
/// Business Logic（为什么需要这个函数）:
///     doctor 不应枚举或回显用户项目路径/名称。
///
/// Code Logic（这个函数做什么）:
///     将 `<HOME>/` 后紧跟且不以 `.cc-partner` 开头的路径段替换为 `<HOME>/<REDACTED>`。
fn redact_user_project_paths(text: &str) -> String {
    let marker = format!("{HOME_PLACEHOLDER}/");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find(&marker) {
        out.push_str(&rest[..idx]);
        out.push_str(&marker);
        let after = &rest[idx + marker.len()..];
        if after.starts_with(".cc-partner") {
            // 保留应用数据路径
            rest = after;
            continue;
        }
        // 折叠到下一空白或结束
        let end = after
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',')
            .unwrap_or(after.len());
        out.push_str("<REDACTED>");
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// 对快照中所有路径/文本字段就地做隐私归一化。
///
/// Business Logic（为什么需要这个函数）:
///     序列化前统一清洗，避免 probe 漏调 helper 导致泄露。
///
/// Code Logic（这个函数做什么）:
///     归一化 control_path、log_path、各 check.summary、log_parse_warning 与 recent_errors.summary。
pub fn sanitize_snapshot_privacy(snapshot: &mut DoctorSnapshot, home: Option<&Path>) {
    snapshot.backend.control_path =
        normalize_home_in_path(Path::new(&snapshot.backend.control_path), home);
    snapshot.log_path = normalize_home_in_path(Path::new(&snapshot.log_path), home);

    sanitize_check(&mut snapshot.backend.health, home);
    sanitize_check(&mut snapshot.paths.data, home);
    sanitize_check(&mut snapshot.paths.database, home);
    sanitize_check(&mut snapshot.paths.log, home);
    sanitize_check(&mut snapshot.mdns, home);
    sanitize_check(&mut snapshot.dependencies.git, home);
    sanitize_check(&mut snapshot.dependencies.tmux, home);
    sanitize_check(&mut snapshot.dependencies.wsl, home);
    sanitize_check(&mut snapshot.dependencies.claude_cli, home);
    if let Some(warning) = snapshot.log_parse_warning.as_mut() {
        sanitize_check(warning, home);
    }

    for err in &mut snapshot.recent_errors {
        err.summary = sanitize_doctor_text(&err.summary, home);
        err.code = sanitize_doctor_text(&err.code, home);
        if let Some(rid) = err.request_id.as_mut() {
            *rid = sanitize_doctor_text(rid, home);
        }
    }
}

/// 归一化单项检查 summary。
///
/// Business Logic（为什么需要这个函数）:
///     检查摘要可能内嵌绝对路径与敏感片段，需要统一清洗。
///
/// Code Logic（这个函数做什么）:
///     对 `check.summary` 调用 `sanitize_doctor_text`。
fn sanitize_check(check: &mut DoctorCheck, home: Option<&Path>) {
    check.summary = sanitize_doctor_text(&check.summary, home);
}

/// 将路径转为稳定的正斜杠展示串。
///
/// Business Logic（为什么需要这个函数）:
///     快照 JSON 在跨平台测试中需要稳定路径字符串，避免 Windows 反斜杠导致快照漂移。
///
/// Code Logic（这个函数做什么）:
///     按 Path component 重建，root/prefix 保留，其余用 `/` 连接；空路径返回空串。
fn path_to_display(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().into_owned());
            }
            Component::RootDir => {
                if parts.is_empty() {
                    parts.push(String::new());
                } else if !parts.last().map(|s| s.is_empty()).unwrap_or(false) {
                    // Windows: "C:" + RootDir → "C:"
                }
            }
            Component::Normal(seg) => {
                parts.push(seg.to_string_lossy().into_owned());
            }
            Component::CurDir => parts.push(".".to_string()),
            Component::ParentDir => parts.push("..".to_string()),
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    // Unix 绝对路径：RootDir 产生首个空串，join 后得到 "/a/b"
    // Windows 盘符：Prefix "C:" + RootDir + Normal → "C:/Users/..."
    if parts.len() == 1 && parts[0].is_empty() {
        return "/".to_string();
    }
    parts.join("/")
}

// ---------------------------------------------------------------------------
// Bounded probes（Task 5）
// ---------------------------------------------------------------------------

/// health / mDNS / 依赖等网络与进程 probe 的默认超时。
pub const DOCTOR_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// 整次 doctor 采集硬超时（覆盖 FS/依赖/mDNS 等可能阻塞探测）。
pub const DOCTOR_HARD_DEADLINE: Duration = Duration::from_secs(8);

/// recent errors 最多返回条数。
pub const DOCTOR_RECENT_ERROR_LIMIT: usize = 20;

/// recent error summary 最大字符数。
pub const DOCTOR_ERROR_SUMMARY_MAX_CHARS: usize = 240;

/// 读取 current + 最新历史日志时的最大 tail 字节数。
const DOCTOR_LOG_TAIL_BYTES: u64 = 64 * 1024;

/// Doctor 采集时可注入的依赖（便于 fixture 测试）。
///
/// Business Logic（为什么需要这个结构）:
///     单元测试需要覆盖 healthy/degraded/unhealthy 各态，不能依赖真实 backend/mDNS。
///
/// Code Logic（这个结构做什么）:
///     持有 control 状态、路径根、依赖探测结果、mDNS 结果与日志目录；生产路径填真实值。
#[derive(Debug, Clone)]
pub struct DoctorProbeInputs {
    pub backend_status: BackendStatus,
    pub control_path: PathBuf,
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
    pub log_dir: PathBuf,
    pub log_path: PathBuf,
    pub git: OptionalDependencyProbe,
    pub tmux: OptionalDependencyProbe,
    pub wsl: OptionalDependencyProbe,
    pub claude_cli: OptionalDependencyProbe,
    /// None 表示调用方希望运行真实 mDNS probe；Some 则使用注入结果。
    pub mdns_override: Option<DoctorCheck>,
    /// None 表示从 log_path/history 读取；Some 则直接注入 recent errors。
    pub recent_errors_override: Option<Vec<DoctorErrorSummary>>,
    /// 配合 `recent_errors_override` 注入 malformed warning；None 且 override 存在时表示无 warning。
    pub recent_errors_warning_override: Option<DoctorCheck>,
    pub app_version: String,
    pub backend_version: String,
}

/// 采集完整 doctor 快照（生产入口）。
///
/// Business Logic（为什么需要这个函数）:
///     `doctor` / `doctor --json` 需要一份有界、脱敏、可机器解析的健康快照；
///     卡住的 FS/探针绝不能让 CLI 进程长期挂起。
///
/// Code Logic（这个函数做什么）:
///     委托 `collect_doctor_snapshot_bounded`：在独立 OS 线程上跑完整采集
///     （含 `current_status`），主线程 `recv_timeout`；超时不 join 卡住线程，
///     保证返回并让 CLI 能 exit（不经 Tokio blocking pool，避免 runtime Drop 等待）。
pub async fn collect_doctor_snapshot() -> Result<DoctorSnapshot, AppError> {
    // 同步有界采集即可：内部自建 OS 线程 + 可选 current_thread runtime，
    // 不经过 spawn_blocking，避免超时后 Tokio runtime Drop 等待 blocking task。
    collect_doctor_snapshot_bounded(DOCTOR_HARD_DEADLINE)
}

/// 在硬 deadline 内采集 doctor 快照（进程退出保证入口）。
///
/// Business Logic（为什么需要这个函数）:
///     仅让 future 提前返回不够：若阻塞工作落在 Tokio blocking pool 且超时后仍 join，
///     CLI 进程仍会挂住。必须用 OS 线程 + `recv_timeout`，超时后放弃 join。
///
/// Code Logic（这个函数做什么）:
///     spawn 专用线程执行 `collect_doctor_snapshot_on_thread`；主线程 `recv_timeout`；
///     成功则 join；超时则 `mem::forget` JoinHandle（文档化泄漏卡住线程作为 last resort）
///     并返回 `AppError::Timeout`。
pub fn collect_doctor_snapshot_bounded(deadline: Duration) -> Result<DoctorSnapshot, AppError> {
    let guard = std::sync::Arc::new(ProbeRuntimeGuard::new());
    let overall_deadline = Instant::now() + deadline;
    let guard_for_thread = std::sync::Arc::clone(&guard);
    run_on_thread_with_deadline(deadline, move || {
        collect_doctor_snapshot_on_thread(overall_deadline, guard_for_thread)
    })
    .map_err(|err| {
        // 超时/断开前先 cancel + 有界 reap 依赖探测进程树，避免 forget 采集线程后遗留 git/tmux/wsl。
        guard.cancel_and_reap();
        if matches!(err, DeadlineRunError::Timeout) {
            AppError::timeout(format!("doctor 采集超时（{deadline:?}）"))
        } else {
            AppError::generic("doctor 采集线程异常退出（未回传结果）")
        }
    })?
}

/// 有界线程执行失败原因。
///
/// Business Logic（为什么需要这个枚举）:
///     测试与生产路径需要区分“超时放弃 join”和“线程断开未回传”。
///
/// Code Logic（这个枚举做什么）:
///     `Timeout` = recv_timeout 到期且已 detach；`Disconnected` = 通道断开且无结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeadlineRunError {
    Timeout,
    Disconnected,
}

/// 在独立 OS 线程上执行闭包，并用 `recv_timeout` 施加硬 deadline。
///
/// Business Logic（为什么需要这个函数）:
///     doctor 必须保证 CLI 在 deadline 后可退出；不能 join 可能永久阻塞的 FS/探针线程。
///
/// Code Logic（这个函数做什么）:
///     `std::thread::spawn` + `mpsc::recv_timeout`；完成则 join；超时 `mem::forget(handle)`
///     故意泄漏卡住线程（进程退出时 OS 回收），并返回 `DeadlineRunError::Timeout`。
fn run_on_thread_with_deadline<T, F>(deadline: Duration, work: F) -> Result<T, DeadlineRunError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = work();
        // 接收端可能已因超时丢弃；发送失败可忽略。
        let _ = tx.send(result);
    });
    match rx.recv_timeout(deadline) {
        Ok(value) => {
            // 工作已完成，回收线程，避免无意义泄漏。
            let _ = handle.join();
            Ok(value)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // LAST RESORT: 不 join 卡住的探测线程，否则 CLI 无法在硬截止后退出。
            // 泄漏的线程会在进程退出时被 OS 回收；禁止改为 join/park 等待。
            std::mem::forget(handle);
            Err(DeadlineRunError::Timeout)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            let _ = handle.join();
            Err(DeadlineRunError::Disconnected)
        }
    }
}

/// 在专用线程内完成 current_status + 同步探测。
///
/// Business Logic（为什么需要这个函数）:
///     硬 deadline 必须覆盖 health/control 与全部阻塞 probe，不能把 current_status 放在界外。
///
/// Code Logic（这个函数做什么）:
///     自建 current_thread Tokio runtime `block_on(current_status)`，再跑同步
///     `collect_doctor_snapshot_blocking` 组装快照。
fn collect_doctor_snapshot_on_thread(
    overall_deadline: Instant,
    guard: std::sync::Arc<ProbeRuntimeGuard>,
) -> Result<DoctorSnapshot, AppError> {
    if guard.is_cancelled() || Instant::now() >= overall_deadline {
        return Err(AppError::timeout("doctor 采集在启动前已超时/取消"));
    }
    let backend_status = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| AppError::generic(format!("doctor 采集 runtime 创建失败: {err}")))?
        .block_on(crate::backend::control::current_status());
    if guard.is_cancelled() || Instant::now() >= overall_deadline {
        return Err(AppError::timeout("doctor 采集在 health 后已超时/取消"));
    }
    let home = current_home_dir();
    collect_doctor_snapshot_blocking(backend_status, home.as_deref(), overall_deadline, guard)
}

/// 在 blocking 线程中完成同步探测并组装快照。
///
/// Business Logic（为什么需要这个函数）:
///     依赖命令、FS recv_timeout、mDNS 与日志 tail 都是同步阻塞；与 async health 分离后
///     便于把整段采集放进有界 OS 线程。
///
/// Code Logic（这个函数做什么）:
///     解析 data/db/log 路径 → 同步依赖/mDNS 探测 → 读 recent errors（含 malformed warning）
///     → assemble_snapshot_from_inputs。
fn collect_doctor_snapshot_blocking(
    backend_status: BackendStatus,
    home: Option<&Path>,
    overall_deadline: Instant,
    guard: std::sync::Arc<ProbeRuntimeGuard>,
) -> Result<DoctorSnapshot, AppError> {
    let inputs = gather_live_probe_inputs_sync(backend_status, overall_deadline, guard)?;
    Ok(assemble_snapshot_from_inputs(inputs, home))
}

/// 从 live 环境同步组装 probe 输入（不含 async health）。
///
/// Business Logic（为什么需要这个函数）:
///     生产路径需要把 control 状态、路径、依赖与日志位置一次收集，供组装快照。
///
/// Code Logic（这个函数做什么）:
///     解析 data/db/log 路径，跑非 mutating 依赖探测；路径解析失败上抛 Validation。
fn gather_live_probe_inputs_sync(
    backend_status: BackendStatus,
    overall_deadline: Instant,
    guard: std::sync::Arc<ProbeRuntimeGuard>,
) -> Result<DoctorProbeInputs, AppError> {
    let control_path = control_file_path()?;
    let data = data_dir()?;
    let default_database_path = data.join("data.db");
    let log_path = backend_log_path()?;
    let log_dir = log_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| AppError::validation("backend log 路径缺少父目录"))?;

    // 配置加载失败属于核心解析失败，不得静默吞掉后把路径回落相对目录。
    let cfg = crate::config::AppConfig::load()?;
    let db = if cfg.db_path.trim().is_empty() {
        default_database_path
    } else {
        PathBuf::from(&cfg.db_path)
    };
    let claude_path = cfg.github_trending.claude_cli_path.clone();

    if guard.is_cancelled() || Instant::now() >= overall_deadline {
        return Err(AppError::timeout("doctor 依赖探测前已超时/取消"));
    }

    Ok(DoctorProbeInputs {
        backend_status,
        control_path,
        data_dir: data,
        database_path: db,
        log_dir,
        log_path,
        git: probe_git_non_mutating_with_budget(Some(overall_deadline), Some(guard.as_ref())),
        tmux: probe_tmux_non_mutating_with_budget(Some(overall_deadline), Some(guard.as_ref())),
        wsl: probe_wsl_non_mutating_with_budget(Some(overall_deadline), Some(guard.as_ref())),
        claude_cli: probe_claude_cli_non_mutating_with_budget(
            &claude_path,
            Some(overall_deadline),
            Some(guard.as_ref()),
        ),
        mdns_override: None,
        recent_errors_override: None,
        // None 表示走真实读取并报告 malformed warning。
        recent_errors_warning_override: None,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        backend_version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// 根据 probe 输入组装快照（可测试入口）。
///
/// Business Logic（为什么需要这个函数）:
///     fixture 测试需要注入 control/path/dependency 结果验证 overall 与 code 映射。
///
/// Code Logic（这个函数做什么）:
///     映射 backend/path/mdns/deps/recent_errors → 合并 malformed warning →
///     compute_overall_status → 隐私清洗。
pub fn assemble_snapshot_from_inputs(
    inputs: DoctorProbeInputs,
    home: Option<&Path>,
) -> DoctorSnapshot {
    let backend = probe_backend_check(&inputs.backend_status, &inputs.control_path);
    let paths = probe_path_checks(&inputs.data_dir, &inputs.database_path, &inputs.log_dir);
    let mdns = inputs.mdns_override.unwrap_or_else(probe_mdns_bounded);
    let dependencies = DoctorDependencies {
        git: dependency_to_check("git", &inputs.git),
        tmux: dependency_to_check("tmux", &inputs.tmux),
        wsl: dependency_to_check("wsl", &inputs.wsl),
        claude_cli: dependency_to_check("claude_cli", &inputs.claude_cli),
    };
    let (recent_errors, log_warning) = match inputs.recent_errors_override {
        Some(errors) => (errors, inputs.recent_errors_warning_override),
        None => {
            let report = read_recent_errors_report(&inputs.log_path, &inputs.log_dir);
            (report.errors, report.malformed_warning)
        }
    };

    // 畸形受控 JSON 行产生 warning check，纳入 overall degraded。
    let log_parse_warning = log_warning;
    let status = compute_overall_status(
        &backend,
        &paths,
        &mdns,
        &dependencies,
        log_parse_warning.as_ref(),
    );
    let mut snapshot = DoctorSnapshot {
        schema_version: DOCTOR_SCHEMA_VERSION,
        generated_at: Utc::now().to_rfc3339(),
        status,
        version: DoctorVersion {
            app: inputs.app_version,
            backend: inputs.backend_version,
        },
        platform: DoctorPlatform {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        },
        backend,
        paths,
        mdns,
        dependencies,
        recent_errors,
        log_path: inputs.log_path.to_string_lossy().into_owned(),
        log_parse_warning,
    };
    sanitize_snapshot_privacy(&mut snapshot, home);
    snapshot
}

/// 将 control 状态映射为 `DoctorBackendCheck`。
///
/// Business Logic（为什么需要这个函数）:
///     stopped 是 info（healthy），可恢复 stale 是 warning，active 但 health 失败/端口冲突是 error。
///
/// Code Logic（这个函数做什么）:
///     按 `BackendStatusKind` 分支：Running→ok；Stopped→info；Stale→区分进程存活/端口占用；
///     Error→error。
pub fn probe_backend_check(status: &BackendStatus, control_path: &Path) -> DoctorBackendCheck {
    let (pid, port) = status
        .control
        .as_ref()
        .map(|c| (Some(c.pid), Some(c.port)))
        .unwrap_or((None, None));

    let (state, health) = match status.kind {
        BackendStatusKind::Running => (
            "running".to_string(),
            DoctorCheck::new(
                DoctorCheckStatus::Ok,
                "backend.health.ok",
                "backend health endpoint responded ok",
            ),
        ),
        BackendStatusKind::Stopped => (
            "stopped".to_string(),
            DoctorCheck::new(
                DoctorCheckStatus::Info,
                "backend.stopped",
                "backend is stopped",
            ),
        ),
        BackendStatusKind::Stale => classify_stale_backend(status.control.as_ref()),
        BackendStatusKind::Error => (
            "error".to_string(),
            DoctorCheck::new(
                DoctorCheckStatus::Error,
                "backend.control.error",
                status
                    .error
                    .clone()
                    .unwrap_or_else(|| "backend control reported an error".to_string()),
            ),
        ),
    };

    DoctorBackendCheck {
        state,
        control_path: control_path.to_string_lossy().into_owned(),
        pid,
        port,
        health,
    }
}

/// 细分 stale：可恢复（pid 死 + 端口空）warning；进程活/端口被占/health 不可达 error。
///
/// Business Logic（为什么需要这个函数）:
///     用户应能区分“残留控制文件可清理”与“端口被占/进程僵死不可达”。
///
/// Code Logic（这个函数做什么）:
///     无 control → warning stale；pid 不存活且端口可绑定 → warning recoverable；
///     pid 存活但 health 失败 → error unreachable；端口被占 → error port_conflict。
fn classify_stale_backend(control: Option<&BackendControlFile>) -> (String, DoctorCheck) {
    let Some(control) = control else {
        return (
            "stale".to_string(),
            DoctorCheck::new(
                DoctorCheckStatus::Warning,
                "backend.control.stale",
                "stale control metadata without process details",
            ),
        );
    };

    let alive = process_is_alive(control.pid);
    let port_free = is_local_port_free(control.port);

    if !alive && port_free {
        return (
            "stale".to_string(),
            DoctorCheck::new(
                DoctorCheckStatus::Warning,
                "backend.control.stale",
                "recoverable stale control: process exited and port is free",
            ),
        );
    }

    if !alive && !port_free {
        return (
            "error".to_string(),
            DoctorCheck::new(
                DoctorCheckStatus::Error,
                "backend.port.conflict",
                "control port is occupied by another process",
            ),
        );
    }

    // process alive but health failed (stale classification already implies health_ok=false)
    (
        "error".to_string(),
        DoctorCheck::new(
            DoctorCheckStatus::Error,
            "backend.health.unreachable",
            "active control process is unreachable on health endpoint",
        ),
    )
}

/// 探测本机 TCP 端口是否可绑定（非 mutating 短试）。
///
/// Business Logic（为什么需要这个函数）:
///     区分 stale 可恢复与端口冲突，避免误导用户清理仍被占用的端口。
///
/// Code Logic（这个函数做什么）:
///     尝试 `TcpListener::bind(127.0.0.1:port)`；成功即释放并返回 true。
fn is_local_port_free(port: u16) -> bool {
    if port == 0 {
        return true;
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpListener::bind(addr).is_ok()
}

/// 探测 data/db/log 核心路径可用性（最小 create/open/read，不删数据）。
///
/// Business Logic（为什么需要这个函数）:
///     核心路径不可用应抬升 unhealthy；检查不得删除或改写用户业务数据。
///
/// Code Logic（这个函数做什么）:
///     data/log：目录存在可创建 + 可写探测文件立即删除；database：文件存在则 open 读 1 字节，
///     不存在则检查父目录可写。
pub fn probe_path_checks(
    data_dir: &Path,
    database_path: &Path,
    log_dir: &Path,
) -> DoctorPathChecks {
    DoctorPathChecks {
        data: probe_directory_usable(data_dir, "paths.data"),
        database: probe_database_readable(database_path),
        log: probe_directory_usable(log_dir, "paths.log"),
    }
}

/// 检查目录可创建且可写。
///
/// Business Logic（为什么需要这个函数）:
///     data/log 目录必须可写，否则 backend 无法落库或写日志。
///
/// Code Logic（这个函数做什么）:
///     `create_dir_all` + 在目录内写/删 `.cc-partner-doctor-write-probe` 临时文件。
fn probe_directory_usable(dir: &Path, code_prefix: &str) -> DoctorCheck {
    match run_fs_probe_with_timeout(
        dir.to_path_buf(),
        code_prefix.to_string(),
        |dir, code_prefix| probe_directory_usable_inner(&dir, &code_prefix),
    ) {
        Ok(check) => check,
        Err(err) => DoctorCheck::new(
            DoctorCheckStatus::Error,
            format!("{code_prefix}.timeout"),
            format!("directory probe timed out or failed: {err}"),
        ),
    }
}

/// 目录可写探测实现：随机名 + create_new，仅删除本进程创建的探针。
///
/// Business Logic（为什么需要这个函数）:
///     固定名 truncate/delete 会破坏既有文件或 symlink，也与并发 doctor 互相截断；
///     探针必须非破坏且只清理自己创建的身份匹配文件。
///
/// Code Logic（这个函数做什么）:
///     create_dir_all → 随机探针路径 create_new(true) 写入 → 校验 metadata 后仅删除该路径。
fn probe_directory_usable_inner(dir: &Path, code_prefix: &str) -> DoctorCheck {
    if let Err(err) = fs::create_dir_all(dir) {
        return DoctorCheck::new(
            DoctorCheckStatus::Error,
            format!("{code_prefix}.inaccessible"),
            format!("directory inaccessible: {err}"),
        );
    }
    let token = uuid::Uuid::new_v4();
    let probe = dir.join(format!(".cc-partner-doctor-write-probe-{token}"));
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(mut f) => {
            use std::io::Write;
            let write_ok = f.write_all(b"ok").is_ok();
            drop(f);
            // 仅当本进程 create_new 成功创建时才删除，避免误删既有同名/竞态文件。
            if probe.exists() {
                let _ = fs::remove_file(&probe);
            }
            if write_ok {
                DoctorCheck::new(
                    DoctorCheckStatus::Ok,
                    format!("{code_prefix}.ok"),
                    "directory is usable",
                )
            } else {
                DoctorCheck::new(
                    DoctorCheckStatus::Error,
                    format!("{code_prefix}.inaccessible"),
                    "directory is not writable",
                )
            }
        }
        Err(err) => DoctorCheck::new(
            DoctorCheckStatus::Error,
            format!("{code_prefix}.inaccessible"),
            format!("directory is not writable: {err}"),
        ),
    }
}

/// 检查数据库文件可读（不删、不 migrate）。
///
/// Business Logic（为什么需要这个函数）:
///     data.db 不可读会使核心状态丢失；doctor 只需 open/read 校验。
///
/// Code Logic（这个函数做什么）:
///     文件存在 → 以只读打开并 try_read 1 字节；不存在 → 父目录可写视为 ok（尚未初始化）。
fn probe_database_readable(database_path: &Path) -> DoctorCheck {
    match run_fs_probe_with_timeout(
        database_path.to_path_buf(),
        "paths.database".to_string(),
        |database_path, _| probe_database_readable_inner(&database_path),
    ) {
        Ok(check) => check,
        Err(err) => DoctorCheck::new(
            DoctorCheckStatus::Error,
            "paths.database.timeout",
            format!("database probe timed out or failed: {err}"),
        ),
    }
}

/// 数据库可读探测实现（随机父目录探针，不 truncate 固定名）。
///
/// Business Logic（为什么需要这个函数）:
///     已有 db 只需只读 open/read；未创建时父目录探针不得破坏既有文件。
///
/// Code Logic（这个函数做什么）:
///     exists → 只读 open + try_read 1 字节；否则 create_new 随机探针并仅删除自己创建的文件。
fn probe_database_readable_inner(database_path: &Path) -> DoctorCheck {
    if database_path.exists() {
        match File::open(database_path) {
            Ok(mut f) => {
                let mut buf = [0u8; 1];
                match f.read(&mut buf) {
                    Ok(0) | Ok(1..) => DoctorCheck::new(
                        DoctorCheckStatus::Ok,
                        "paths.database.ok",
                        "database file is readable",
                    ),
                    Err(err) => DoctorCheck::new(
                        DoctorCheckStatus::Error,
                        "paths.database.inaccessible",
                        format!("database file is not readable: {err}"),
                    ),
                }
            }
            Err(err) => DoctorCheck::new(
                DoctorCheckStatus::Error,
                "paths.database.inaccessible",
                format!("database file cannot be opened: {err}"),
            ),
        }
    } else {
        let Some(parent) = database_path.parent() else {
            return DoctorCheck::new(
                DoctorCheckStatus::Error,
                "paths.database.inaccessible",
                "database parent directory is inaccessible",
            );
        };
        if !(parent.exists() || fs::create_dir_all(parent).is_ok()) {
            return DoctorCheck::new(
                DoctorCheckStatus::Error,
                "paths.database.inaccessible",
                "database parent directory is inaccessible",
            );
        }
        let token = uuid::Uuid::new_v4();
        let probe = parent.join(format!(".cc-partner-doctor-db-parent-probe-{token}"));
        match OpenOptions::new().write(true).create_new(true).open(&probe) {
            Ok(_) => {
                if probe.exists() {
                    let _ = fs::remove_file(&probe);
                }
                DoctorCheck::new(
                    DoctorCheckStatus::Ok,
                    "paths.database.ok",
                    "database parent directory is usable (db not yet created)",
                )
            }
            Err(err) => DoctorCheck::new(
                DoctorCheckStatus::Error,
                "paths.database.inaccessible",
                format!("database parent directory is not writable: {err}"),
            ),
        }
    }
}

/// 在独立线程中运行可能阻塞的 FS probe，并施加 `DOCTOR_PROBE_TIMEOUT`。
///
/// Business Logic（为什么需要这个函数）:
///     网络盘/故障盘上 create/open/read 可能无限阻塞，破坏 doctor 有界退出契约。
///
/// Code Logic（这个函数做什么）:
///     spawn 线程执行闭包，`recv_timeout` 等待结果；超时返回 Err 字符串。
fn run_fs_probe_with_timeout<F>(
    path: PathBuf,
    code_prefix: String,
    probe: F,
) -> Result<DoctorCheck, String>
where
    F: FnOnce(PathBuf, String) -> DoctorCheck + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = probe(path, code_prefix);
        let _ = tx.send(result);
    });
    match rx.recv_timeout(DOCTOR_PROBE_TIMEOUT) {
        Ok(check) => Ok(check),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(format!("exceeded {DOCTOR_PROBE_TIMEOUT:?}"))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("probe worker disconnected".to_string())
        }
    }
}

/// 有界 mDNS 初始化探测（不枚举设备/项目名）。
///
/// Business Logic（为什么需要这个函数）:
///     局域网发现失败只应 degraded，不得抬升 unhealthy。
///
/// Code Logic（这个函数做什么）:
///     在短超时内 `ServiceDaemon::new`，成功后立刻 `shutdown`；失败返回 warning。
pub fn probe_mdns_bounded() -> DoctorCheck {
    // ServiceDaemon::new 本身可能阻塞在 socket 创建；用 std thread + channel 包超时。
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = ServiceDaemon::new();
        let _ = tx.send(result);
    });
    match rx.recv_timeout(DOCTOR_PROBE_TIMEOUT) {
        Ok(Ok(daemon)) => {
            if let Err(err) = daemon.shutdown() {
                return DoctorCheck::new(
                    DoctorCheckStatus::Warning,
                    "mdns.shutdown_failed",
                    format!("mDNS initialized but shutdown failed: {err}"),
                );
            }
            DoctorCheck::new(
                DoctorCheckStatus::Ok,
                "mdns.ok",
                "mDNS discovery can initialize",
            )
        }
        Ok(Err(err)) => DoctorCheck::new(
            DoctorCheckStatus::Warning,
            "mdns.failed",
            format!("mDNS discovery failed to initialize: {err}"),
        ),
        Err(_) => DoctorCheck::new(
            DoctorCheckStatus::Warning,
            "mdns.timeout",
            "mDNS discovery initialization timed out",
        ),
    }
}

/// 把可选依赖探测结果映射为 DoctorCheck。
///
/// Business Logic（为什么需要这个函数）:
///     缺失可选依赖 → warning（degraded）；平台不适用 → info；可用 → ok。
///
/// Code Logic（这个函数做什么）:
///     按 applicable/available 分支生成稳定 code 与 summary。
fn dependency_to_check(name: &str, probe: &OptionalDependencyProbe) -> DoctorCheck {
    if !probe.applicable {
        return DoctorCheck::new(
            DoctorCheckStatus::Info,
            format!("deps.{name}.not_applicable"),
            probe.detail.clone(),
        );
    }
    if probe.available {
        let summary = match &probe.version {
            Some(v) if !v.is_empty() => format!("{} ({v})", probe.detail),
            _ => probe.detail.clone(),
        };
        DoctorCheck::new(DoctorCheckStatus::Ok, format!("deps.{name}.ok"), summary)
    } else {
        DoctorCheck::new(
            DoctorCheckStatus::Warning,
            format!("deps.{name}.missing"),
            probe.detail.clone(),
        )
    }
}

/// recent-errors 读取结果：错误列表 + 可选畸形 warning。
///
/// Business Logic（为什么需要这个结构）:
///     计划要求“ignore malformed lines with a warning check”；畸形不得静默丢弃，
///     否则日志损坏/schema 漂移会把 doctor 误报为 healthy。
///
/// Code Logic（这个结构做什么）:
///     持有已解析 error 摘要与可选 `DoctorCheck` warning。
#[derive(Debug, Clone)]
pub struct RecentErrorsReport {
    pub errors: Vec<DoctorErrorSummary>,
    pub malformed_warning: Option<DoctorCheck>,
}

/// 安全读取 recent errors（仅 current + 最新 history 的 tail）。
///
/// Business Logic（为什么需要这个函数）:
///     doctor 展示最近错误便于排障，但绝不能读任意用户文件或泄露正文。
///
/// Code Logic（这个函数做什么）:
///     委托 `read_recent_errors_report`，只返回 errors 列表（兼容旧调用点）。
pub fn read_recent_errors_from_logs(current_log: &Path, log_dir: &Path) -> Vec<DoctorErrorSummary> {
    read_recent_errors_report(current_log, log_dir).errors
}

/// 读取 recent errors 并报告畸形受控 JSON 行。
///
/// Business Logic（为什么需要这个函数）:
///     畸形受控 JSON 行必须产生 warning check 并纳入 overall degraded；
///     合法 info/warn/debug/trace 行是正常受控输出，不得误计 malformed 导致常态 degraded。
///
/// Code Logic（这个函数做什么）:
///     读 `backend.log` 与 `backend.log.1` 的有界 tail；每个文件若从中间 seek，
///     丢弃首个可能截断半行；按 `classify_controlled_log_line` 分类：error→收集、
///     合法非 error→忽略、JSON/schema 非法→malformed warning。
pub fn read_recent_errors_report(current_log: &Path, log_dir: &Path) -> RecentErrorsReport {
    let mut lines: Vec<String> = Vec::new();
    let history_1 = log_dir.join(format!(
        "{}.1",
        current_log
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("backend.log")
    ));
    // 先读历史再读 current，使 current 行在合并后更“新”
    for path in [&history_1, current_log] {
        if let Some(tail) = read_file_tail_lines(path, DOCTOR_LOG_TAIL_BYTES) {
            lines.extend(tail);
        }
    }

    let mut errors: Vec<DoctorErrorSummary> = Vec::new();
    let mut malformed = 0usize;
    for line in lines {
        match classify_controlled_log_line(&line) {
            ControlledLogLine::Error(err) => errors.push(err),
            ControlledLogLine::ValidNonError => {}
            ControlledLogLine::Malformed => malformed += 1,
            ControlledLogLine::Ignore => {}
        }
    }

    // 保留末尾 N 条（最新）
    if errors.len() > DOCTOR_RECENT_ERROR_LIMIT {
        let skip = errors.len() - DOCTOR_RECENT_ERROR_LIMIT;
        errors = errors.into_iter().skip(skip).collect();
    }

    let malformed_warning = if malformed > 0 {
        Some(DoctorCheck::new(
            DoctorCheckStatus::Warning,
            "logs.recent_errors.malformed",
            format!("ignored {malformed} malformed controlled JSON log line(s)"),
        ))
    } else {
        None
    };

    RecentErrorsReport {
        errors,
        malformed_warning,
    }
}

/// 受控日志行分类结果。
///
/// Business Logic（为什么需要这个枚举）:
///     doctor 必须区分“合法非 error”“error 摘要”“真正畸形”，避免 info/warn 常态 degraded。
///
/// Code Logic（这个枚举做什么）:
///     Error=收集；ValidNonError=忽略；Malformed=计 warning；Ignore=非 JSON 文本忽略。
#[derive(Debug)]
enum ControlledLogLine {
    Error(DoctorErrorSummary),
    ValidNonError,
    Malformed,
    Ignore,
}

/// 分类单行受控日志。
///
/// Business Logic（为什么需要这个函数）:
///     先独立解析 JSON 与受控 schema，再按 level 分流；只有语法/schema 非法才算 malformed。
///
/// Code Logic（这个函数做什么）:
///     非 `{` 开头 → Ignore；JSON 解析失败或非 object → Malformed；
///     level 为 info/warn/debug/trace → ValidNonError；level=error → 构造 summary；
///     缺失/未知 level → Malformed。
fn classify_controlled_log_line(line: &str) -> ControlledLogLine {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return ControlledLogLine::Ignore;
    }
    if !trimmed.starts_with('{') {
        return ControlledLogLine::Ignore;
    }
    let value: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return ControlledLogLine::Malformed,
    };
    let Some(obj) = value.as_object() else {
        return ControlledLogLine::Malformed;
    };
    let Some(level) = obj.get("level").and_then(|v| v.as_str()) else {
        // 受控 schema 要求稳定 level 字段；缺 level 视为 schema-invalid。
        return ControlledLogLine::Malformed;
    };
    match level {
        "error" => ControlledLogLine::Error(error_summary_from_controlled_object(obj)),
        "info" | "warn" | "debug" | "trace" => ControlledLogLine::ValidNonError,
        _ => ControlledLogLine::Malformed,
    }
}

/// 读取文件末尾最多 max_bytes 字节，按行切分并跳过可能截断的首半行。
///
/// Business Logic（为什么需要这个函数）:
///     大日志不得整文件读入；tail seek 后首行可能是半行，不能当 malformed 报警。
///
/// Code Logic（这个函数做什么）:
///     seek 到 max(0, len-max_bytes) 后 read_to_end；若从中间 seek 且缓冲不以 `\n` 开头，
///     丢弃第一行；返回非空行列表。metadata/seek/read 失败返回 None（有界失败，不 panic）。
fn read_file_tail_lines(path: &Path, max_bytes: u64) -> Option<Vec<String>> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut truncated = false;
    if len > max_bytes {
        file.seek(SeekFrom::Start(len - max_bytes)).ok()?;
        truncated = true;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    // seek 到文件中部时，第一行通常是半行截断，跳过以免误报 malformed。
    if truncated && !buf.is_empty() && buf[0] != b'\n' && !lines.is_empty() {
        lines.remove(0);
    }
    lines.retain(|l| !l.trim().is_empty());
    Some(lines)
}

/// 解析单行受控日志 JSON 为 error summary。
///
/// Business Logic（为什么需要这个函数）:
///     单测需要直接拿到 error 级摘要；非 error/畸形返回 None。
///
/// Code Logic（这个函数做什么）:
///     委托 `classify_controlled_log_line`，仅 `Error` 变体映射为 Some。
#[cfg(test)]
fn parse_error_log_line(line: &str) -> Option<DoctorErrorSummary> {
    match classify_controlled_log_line(line) {
        ControlledLogLine::Error(err) => Some(err),
        _ => None,
    }
}

/// 从已确认 level=error 的受控 JSON object 构造 error summary。
///
/// Business Logic（为什么需要这个函数）:
///     分类器已校验 schema/level，字段清洗逻辑需与历史 parse 行为一致且可复用。
///
/// Code Logic（这个函数做什么）:
///     取 timestamp/error_code|operation/message/request_id；sanitize + 截断后返回 DTO。
fn error_summary_from_controlled_object(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> DoctorErrorSummary {
    let home = current_home_dir();
    let home_ref = home.as_deref();

    let raw_ts = obj.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
    let timestamp = sanitize_timestamp_field(raw_ts);

    let raw_code = obj
        .get("error_code")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| obj.get("operation").and_then(|v| v.as_str()))
        .unwrap_or("error");
    let code = sanitize_code_field(raw_code, home_ref);

    let message = obj.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let mut summary = sanitize_doctor_text(message, home_ref);
    if summary.chars().count() > DOCTOR_ERROR_SUMMARY_MAX_CHARS {
        summary = summary
            .chars()
            .take(DOCTOR_ERROR_SUMMARY_MAX_CHARS)
            .collect::<String>()
            + "…";
    }

    let request_id = obj
        .get("request_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| sanitize_request_id_field(s, home_ref));

    DoctorErrorSummary {
        timestamp,
        code,
        summary,
        request_id,
    }
}

/// 严格清洗 timestamp：仅接受 RFC3339，否则替换为占位。
///
/// Business Logic（为什么需要这个函数）:
///     敌意日志可把 Prompt/路径塞进 timestamp 字段；recentErrors 必须全字段隐私门禁。
///
/// Code Logic（这个函数做什么）:
///     `DateTime::parse_from_rfc3339` 成功则规范化为 RFC3339 字符串，失败返回 `invalid-timestamp`。
fn sanitize_timestamp_field(raw: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(raw.trim()) {
        Ok(dt) => dt.to_rfc3339(),
        Err(_) => "invalid-timestamp".to_string(),
    }
}

/// 清洗 error code/operation：限制字符集与长度，并跑隐私 sanitizer。
///
/// Business Logic（为什么需要这个函数）:
///     code 字段若原样回显可携带路径/Prompt/凭据，绕过 summary-only 清洗。
///
/// Code Logic（这个函数做什么）:
///     仅保留 `[A-Za-z0-9._-]`，最长 64；空则 `error`；再 `sanitize_doctor_text`。
fn sanitize_code_field(raw: &str, home: Option<&Path>) -> String {
    // 只取前缀合法 token（遇非法字符即停），避免把路径/命令拼进 code。
    let token: String = raw
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(64)
        .collect();
    let base = if token.is_empty() {
        "error".to_string()
    } else {
        token
    };
    sanitize_doctor_text(&base, home)
}

/// 清洗 requestId：长度限制 + 隐私 sanitizer。
///
/// Business Logic（为什么需要这个函数）:
///     requestId 可被敌意日志写入任意字符串，必须与 summary 同等脱敏。
///
/// Code Logic（这个函数做什么）:
///     截到 128 字符后 `sanitize_doctor_text`。
fn sanitize_request_id_field(raw: &str, home: Option<&Path>) -> String {
    // requestId 只允许短标识字符集；路径/Prompt 形态直接拒绝。
    let token: String = raw
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
        .take(128)
        .collect();
    let base = if token.is_empty() {
        "invalid-request-id".to_string()
    } else {
        token
    };
    sanitize_doctor_text(&base, home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::control::classify_status;
    use crate::workbench::dependencies::{
        probe_claude_cli_non_mutating_with_budget, probe_git_non_mutating_with_budget,
        probe_tmux_non_mutating_with_budget, probe_wsl_non_mutating_with_budget,
    };
    use serde_json::{json, Value};
    use std::net::TcpListener;

    /// 构造固定时钟/平台的稳定快照 fixture。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     schema 回归测试必须用固定值，避免 wall-clock / 真实 home 导致抖动。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 schemaVersion=1 的 healthy 快照，路径已用 `<HOME>` 占位。
    fn stable_snapshot_fixture() -> DoctorSnapshot {
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
                state: "running".to_string(),
                control_path: format!("{HOME_PLACEHOLDER}/.cc-partner/backend-control.json"),
                pid: Some(4242),
                port: Some(62116),
                health: DoctorCheck::new(
                    DoctorCheckStatus::Ok,
                    "backend.health.ok",
                    "backend health endpoint responded ok",
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
                tmux: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.tmux.ok", "tmux is available"),
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

    #[test]
    fn serializes_stable_snapshot() {
        let snapshot = stable_snapshot_fixture();
        let value = serde_json::to_value(&snapshot).expect("serialize snapshot");

        assert_eq!(value["schemaVersion"], json!(1));
        assert_eq!(value["generatedAt"], json!("2026-07-11T12:00:00Z"));
        assert_eq!(value["status"], json!("healthy"));
        assert_eq!(value["version"]["app"], json!("0.6.7"));
        assert_eq!(value["version"]["backend"], json!("0.6.7"));
        assert_eq!(value["platform"]["os"], json!("macos"));
        assert_eq!(value["platform"]["arch"], json!("aarch64"));
        assert_eq!(value["backend"]["state"], json!("running"));
        assert_eq!(
            value["backend"]["controlPath"],
            json!(format!(
                "{HOME_PLACEHOLDER}/.cc-partner/backend-control.json"
            ))
        );
        assert_eq!(value["backend"]["pid"], json!(4242));
        assert_eq!(value["backend"]["port"], json!(62116));
        assert_eq!(value["backend"]["health"]["status"], json!("ok"));
        assert_eq!(
            value["backend"]["health"]["code"],
            json!("backend.health.ok")
        );
        assert_eq!(value["paths"]["data"]["status"], json!("ok"));
        assert_eq!(value["paths"]["database"]["status"], json!("ok"));
        assert_eq!(value["paths"]["log"]["status"], json!("ok"));
        assert_eq!(value["mdns"]["status"], json!("ok"));
        assert_eq!(value["dependencies"]["git"]["status"], json!("ok"));
        assert_eq!(value["dependencies"]["tmux"]["status"], json!("ok"));
        assert_eq!(value["dependencies"]["wsl"]["status"], json!("info"));
        assert_eq!(value["dependencies"]["claudeCli"]["status"], json!("ok"));
        assert_eq!(value["recentErrors"][0]["code"], json!("net.timeout"));
        assert_eq!(
            value["recentErrors"][0]["requestId"],
            json!("req-fixed-001")
        );
        assert_eq!(
            value["logPath"],
            json!(format!("{HOME_PLACEHOLDER}/.cc-partner/logs/backend.log"))
        );

        // 禁止环境变量 map
        assert!(value.get("environment").is_none());
        assert!(value.get("env").is_none());

        let text = serde_json::to_string(&snapshot).expect("to_string");
        let round_trip: DoctorSnapshot = serde_json::from_str(&text).expect("from_str round-trip");
        assert_eq!(round_trip, snapshot);

        // 再走 Value 断言 camelCase key 集合稳定
        let obj = value.as_object().expect("object");
        for key in [
            "schemaVersion",
            "generatedAt",
            "status",
            "version",
            "platform",
            "backend",
            "paths",
            "mdns",
            "dependencies",
            "recentErrors",
            "logPath",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn removes_private_values() {
        let home = PathBuf::from("/Users/alice");
        let mut snapshot = DoctorSnapshot {
            schema_version: DOCTOR_SCHEMA_VERSION,
            generated_at: "2026-07-11T12:00:00Z".to_string(),
            status: DoctorStatus::Degraded,
            version: DoctorVersion {
                app: "0.6.7".to_string(),
                backend: "0.6.7".to_string(),
            },
            platform: DoctorPlatform {
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
            },
            backend: DoctorBackendCheck {
                state: "stale".to_string(),
                control_path: "/Users/alice/.cc-partner/backend-control.json".to_string(),
                pid: Some(99),
                port: Some(62116),
                health: DoctorCheck::new(
                    DoctorCheckStatus::Warning,
                    "backend.control.stale",
                    "stale control at /Users/alice/.cc-partner/backend-control.json",
                ),
            },
            paths: DoctorPathChecks {
                data: DoctorCheck::new(
                    DoctorCheckStatus::Ok,
                    "paths.data.ok",
                    "data ok under /Users/alice/.cc-partner",
                ),
                database: DoctorCheck::new(
                    DoctorCheckStatus::Ok,
                    "paths.database.ok",
                    "db ok",
                ),
                log: DoctorCheck::new(DoctorCheckStatus::Ok, "paths.log.ok", "log ok"),
            },
            mdns: DoctorCheck::new(
                DoctorCheckStatus::Warning,
                "mdns.failed",
                "mDNS init failed",
            ),
            dependencies: DoctorDependencies {
                git: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.git.ok", "ok"),
                tmux: DoctorCheck::new(
                    DoctorCheckStatus::Warning,
                    "deps.tmux.missing",
                    "tmux missing",
                ),
                wsl: DoctorCheck::new(
                    DoctorCheckStatus::Info,
                    "deps.wsl.not_applicable",
                    "n/a",
                ),
                claude_cli: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.claude_cli.ok", "ok"),
            },
            recent_errors: vec![DoctorErrorSummary {
                timestamp: "2026-07-11T11:58:00Z".to_string(),
                code: "internal".to_string(),
                summary: "failed under /Users/alice/web_project/secret-app with token=sk-live-abc Prompt=do-not-leak file-sentinel-XYZ".to_string(),
                request_id: Some("req-private".to_string()),
            }],
            log_path: "/Users/alice/.cc-partner/logs/backend.log".to_string(),
            log_parse_warning: None,
        };

        sanitize_snapshot_privacy(&mut snapshot, Some(home.as_path()));

        let json = serde_json::to_string(&snapshot).expect("serialize");
        let value: Value = serde_json::from_str(&json).expect("parse");

        assert!(value.get("environment").is_none());
        assert!(value.get("projects").is_none());
        assert!(value.get("prompts").is_none());
        assert!(value.get("env").is_none());

        assert_eq!(
            snapshot.backend.control_path,
            format!("{HOME_PLACEHOLDER}/.cc-partner/backend-control.json")
        );
        assert_eq!(
            snapshot.log_path,
            format!("{HOME_PLACEHOLDER}/.cc-partner/logs/backend.log")
        );
        assert!(snapshot.backend.health.summary.contains(HOME_PLACEHOLDER));
        assert!(snapshot.paths.data.summary.contains(HOME_PLACEHOLDER));
        assert!(snapshot.recent_errors[0].summary.contains(HOME_PLACEHOLDER));

        // 敌对 fixture：username / project / Prompt / token / file sentinel 均不得出现
        for banned in [
            "alice",
            "/Users/alice",
            "web_project",
            "secret-app",
            "sk-live-abc",
            "do-not-leak",
            "file-sentinel-XYZ",
            "token=sk-live-abc",
            "Prompt=do-not-leak",
        ] {
            assert!(
                !json.contains(banned),
                "banned private fragment {banned:?} still present in {json}"
            );
        }
    }

    #[test]
    fn compute_status_healthy_when_only_info_and_ok() {
        let backend = DoctorBackendCheck {
            state: "stopped".to_string(),
            control_path: format!("{HOME_PLACEHOLDER}/.cc-partner/backend-control.json"),
            pid: None,
            port: None,
            health: DoctorCheck::new(
                DoctorCheckStatus::Info,
                "backend.stopped",
                "backend is stopped",
            ),
        };
        let paths = DoctorPathChecks {
            data: DoctorCheck::new(DoctorCheckStatus::Ok, "paths.data.ok", "ok"),
            database: DoctorCheck::new(DoctorCheckStatus::Ok, "paths.database.ok", "ok"),
            log: DoctorCheck::new(DoctorCheckStatus::Ok, "paths.log.ok", "ok"),
        };
        let mdns = DoctorCheck::new(DoctorCheckStatus::Ok, "mdns.ok", "ok");
        let deps = DoctorDependencies {
            git: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.git.ok", "ok"),
            tmux: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.tmux.ok", "ok"),
            wsl: DoctorCheck::new(DoctorCheckStatus::Info, "deps.wsl.not_applicable", "n/a"),
            claude_cli: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.claude_cli.ok", "ok"),
        };
        assert_eq!(
            compute_overall_status(&backend, &paths, &mdns, &deps, None),
            DoctorStatus::Healthy
        );
        assert_eq!(DoctorStatus::Healthy.exit_code(), 0);
    }

    #[test]
    fn compute_status_degraded_on_optional_warning() {
        let backend = DoctorBackendCheck {
            state: "running".to_string(),
            control_path: format!("{HOME_PLACEHOLDER}/.cc-partner/backend-control.json"),
            pid: Some(1),
            port: Some(1),
            health: DoctorCheck::new(DoctorCheckStatus::Ok, "backend.health.ok", "ok"),
        };
        let paths = DoctorPathChecks {
            data: DoctorCheck::new(DoctorCheckStatus::Ok, "paths.data.ok", "ok"),
            database: DoctorCheck::new(DoctorCheckStatus::Ok, "paths.database.ok", "ok"),
            log: DoctorCheck::new(DoctorCheckStatus::Ok, "paths.log.ok", "ok"),
        };
        let mdns = DoctorCheck::new(DoctorCheckStatus::Warning, "mdns.failed", "mDNS failed");
        let deps = DoctorDependencies {
            git: DoctorCheck::new(DoctorCheckStatus::Warning, "deps.git.missing", "missing"),
            tmux: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.tmux.ok", "ok"),
            wsl: DoctorCheck::new(DoctorCheckStatus::Info, "deps.wsl.not_applicable", "n/a"),
            claude_cli: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.claude_cli.ok", "ok"),
        };
        assert_eq!(
            compute_overall_status(&backend, &paths, &mdns, &deps, None),
            DoctorStatus::Degraded
        );
        assert_eq!(DoctorStatus::Degraded.exit_code(), 1);
    }

    #[test]
    fn compute_status_unhealthy_on_core_path_error() {
        let backend = DoctorBackendCheck {
            state: "running".to_string(),
            control_path: format!("{HOME_PLACEHOLDER}/.cc-partner/backend-control.json"),
            pid: Some(1),
            port: Some(1),
            health: DoctorCheck::new(DoctorCheckStatus::Ok, "backend.health.ok", "ok"),
        };
        let paths = DoctorPathChecks {
            data: DoctorCheck::new(
                DoctorCheckStatus::Error,
                "paths.data.inaccessible",
                "data dir inaccessible",
            ),
            database: DoctorCheck::new(DoctorCheckStatus::Ok, "paths.database.ok", "ok"),
            log: DoctorCheck::new(DoctorCheckStatus::Ok, "paths.log.ok", "ok"),
        };
        let mdns = DoctorCheck::new(DoctorCheckStatus::Ok, "mdns.ok", "ok");
        let deps = DoctorDependencies {
            git: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.git.ok", "ok"),
            tmux: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.tmux.ok", "ok"),
            wsl: DoctorCheck::new(DoctorCheckStatus::Info, "deps.wsl.not_applicable", "n/a"),
            claude_cli: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.claude_cli.ok", "ok"),
        };
        assert_eq!(
            compute_overall_status(&backend, &paths, &mdns, &deps, None),
            DoctorStatus::Unhealthy
        );
        assert_eq!(DoctorStatus::Unhealthy.exit_code(), 2);
    }

    #[test]
    fn compute_status_unhealthy_on_backend_health_error() {
        let backend = DoctorBackendCheck {
            state: "error".to_string(),
            control_path: format!("{HOME_PLACEHOLDER}/.cc-partner/backend-control.json"),
            pid: Some(1),
            port: Some(1),
            health: DoctorCheck::new(
                DoctorCheckStatus::Error,
                "backend.health.unreachable",
                "active control unreachable",
            ),
        };
        let paths = DoctorPathChecks {
            data: DoctorCheck::new(DoctorCheckStatus::Ok, "paths.data.ok", "ok"),
            database: DoctorCheck::new(DoctorCheckStatus::Ok, "paths.database.ok", "ok"),
            log: DoctorCheck::new(DoctorCheckStatus::Ok, "paths.log.ok", "ok"),
        };
        let mdns = DoctorCheck::new(DoctorCheckStatus::Ok, "mdns.ok", "ok");
        let deps = DoctorDependencies {
            git: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.git.ok", "ok"),
            tmux: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.tmux.ok", "ok"),
            wsl: DoctorCheck::new(DoctorCheckStatus::Info, "deps.wsl.not_applicable", "n/a"),
            claude_cli: DoctorCheck::new(DoctorCheckStatus::Ok, "deps.claude_cli.ok", "ok"),
        };
        assert_eq!(
            compute_overall_status(&backend, &paths, &mdns, &deps, None),
            DoctorStatus::Unhealthy
        );
    }

    #[test]
    fn normalize_home_path_and_text() {
        let home = PathBuf::from("/Users/bob");
        let path = home.join(".cc-partner").join("logs").join("backend.log");
        assert_eq!(
            normalize_home_in_path(&path, Some(home.as_path())),
            format!("{HOME_PLACEHOLDER}/.cc-partner/logs/backend.log")
        );
        assert_eq!(
            normalize_home_in_path(Path::new("/tmp/other"), Some(home.as_path())),
            "/tmp/other"
        );
        let text = "error in /Users/bob/.cc-partner/data.db";
        assert_eq!(
            normalize_home_in_text(text, Some(home.as_path())),
            format!("error in {HOME_PLACEHOLDER}/.cc-partner/data.db")
        );
    }

    // ----- Task 5 fixture tests for core doctor states -----

    /// 构造可写临时 data/db/log 路径。
    fn temp_paths() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        let log_dir = data.join("logs");
        fs::create_dir_all(&log_dir).expect("mkdir logs");
        let db = data.join("data.db");
        fs::write(&db, b"SQLite format 3\0").expect("write db");
        (tmp, data, db, log_dir)
    }

    fn ok_dep(name: &str) -> OptionalDependencyProbe {
        OptionalDependencyProbe {
            available: true,
            applicable: true,
            version: Some("1.0".into()),
            detail: format!("{name} is available"),
        }
    }

    fn missing_dep(name: &str) -> OptionalDependencyProbe {
        OptionalDependencyProbe {
            available: false,
            applicable: true,
            version: None,
            detail: format!("{name} is missing"),
        }
    }

    fn na_dep(detail: &str) -> OptionalDependencyProbe {
        OptionalDependencyProbe {
            available: false,
            applicable: false,
            version: None,
            detail: detail.to_string(),
        }
    }

    fn base_inputs(
        status: BackendStatus,
        data: PathBuf,
        db: PathBuf,
        log_dir: PathBuf,
    ) -> DoctorProbeInputs {
        let log_path = log_dir.join("backend.log");
        DoctorProbeInputs {
            backend_status: status,
            control_path: data.join("backend-control.json"),
            data_dir: data,
            database_path: db,
            log_dir: log_dir.clone(),
            log_path,
            git: ok_dep("git"),
            tmux: ok_dep("tmux"),
            wsl: na_dep("WSL is not applicable on this platform"),
            claude_cli: ok_dep("claude CLI"),
            mdns_override: Some(DoctorCheck::new(
                DoctorCheckStatus::Ok,
                "mdns.ok",
                "mDNS discovery can initialize",
            )),
            recent_errors_override: Some(vec![]),
            recent_errors_warning_override: None,
            app_version: "0.6.7".into(),
            backend_version: "0.6.7".into(),
        }
    }

    #[test]
    fn fixture_running_healthy() {
        let (_tmp, data, db, log_dir) = temp_paths();
        let control = BackendControlFile {
            pid: 4242,
            port: 62116,
            device_id: "dev-1".into(),
            device_name: "test".into(),
            started_at: "2026-07-11T00:00:00Z".into(),
            control_token: "tok".into(),
        };
        let status = classify_status(Some(control), true, true, None);
        let snap = assemble_snapshot_from_inputs(
            base_inputs(status, data, db, log_dir),
            Some(Path::new("/Users/alice")),
        );
        assert_eq!(snap.status, DoctorStatus::Healthy);
        assert_eq!(snap.backend.state, "running");
        assert_eq!(snap.backend.health.status, DoctorCheckStatus::Ok);
        assert_eq!(snap.backend.health.code, "backend.health.ok");
        assert_eq!(DoctorStatus::Healthy.exit_code(), 0);
    }

    #[test]
    fn fixture_stopped_healthy() {
        let (_tmp, data, db, log_dir) = temp_paths();
        let status = classify_status(None, false, false, None);
        let snap = assemble_snapshot_from_inputs(base_inputs(status, data, db, log_dir), None);
        assert_eq!(snap.status, DoctorStatus::Healthy);
        assert_eq!(snap.backend.state, "stopped");
        assert_eq!(snap.backend.health.status, DoctorCheckStatus::Info);
        assert_eq!(snap.backend.health.code, "backend.stopped");
    }

    #[test]
    fn fixture_recoverable_stale_degraded() {
        let (_tmp, data, db, log_dir) = temp_paths();
        // pid 不存在且端口可绑定 → recoverable stale warning
        let control = BackendControlFile {
            pid: 0,  // process_is_alive(0) == false
            port: 0, // is_local_port_free(0) == true
            device_id: "dev-1".into(),
            device_name: "test".into(),
            started_at: "2026-07-11T00:00:00Z".into(),
            control_token: "tok".into(),
        };
        let status = classify_status(Some(control), false, false, None);
        assert_eq!(status.kind, BackendStatusKind::Stale);
        let snap = assemble_snapshot_from_inputs(base_inputs(status, data, db, log_dir), None);
        assert_eq!(snap.status, DoctorStatus::Degraded);
        assert_eq!(snap.backend.health.status, DoctorCheckStatus::Warning);
        assert_eq!(snap.backend.health.code, "backend.control.stale");
        assert_eq!(DoctorStatus::Degraded.exit_code(), 1);
    }

    #[test]
    fn fixture_active_unreachable_unhealthy() {
        let (_tmp, data, db, log_dir) = temp_paths();
        // 进程存活但 health 失败 → Stale 分类，probe 映射为 unreachable error
        // 使用当前进程 pid 保证 alive=true；port 用高位空闲端口但 health 已失败
        let pid = std::process::id();
        let control = BackendControlFile {
            pid,
            port: 1, // 通常被占用或不可达；alive=true 走 unreachable 分支
            device_id: "dev-1".into(),
            device_name: "test".into(),
            started_at: "2026-07-11T00:00:00Z".into(),
            control_token: "tok".into(),
        };
        let status = classify_status(Some(control), true, false, None);
        assert_eq!(status.kind, BackendStatusKind::Stale);
        let snap = assemble_snapshot_from_inputs(base_inputs(status, data, db, log_dir), None);
        assert_eq!(snap.status, DoctorStatus::Unhealthy);
        assert_eq!(snap.backend.health.status, DoctorCheckStatus::Error);
        assert_eq!(snap.backend.health.code, "backend.health.unreachable");
        assert_eq!(DoctorStatus::Unhealthy.exit_code(), 2);
    }

    #[test]
    fn fixture_occupied_port_conflict_unhealthy() {
        let (_tmp, data, db, log_dir) = temp_paths();
        // 占用一个端口，pid 不存活 → port conflict
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let control = BackendControlFile {
            pid: 0,
            port,
            device_id: "dev-1".into(),
            device_name: "test".into(),
            started_at: "2026-07-11T00:00:00Z".into(),
            control_token: "tok".into(),
        };
        let status = classify_status(Some(control), false, false, None);
        let snap = assemble_snapshot_from_inputs(base_inputs(status, data, db, log_dir), None);
        assert_eq!(snap.status, DoctorStatus::Unhealthy);
        assert_eq!(snap.backend.health.status, DoctorCheckStatus::Error);
        assert_eq!(snap.backend.health.code, "backend.port.conflict");
        drop(listener);
    }

    #[test]
    fn fixture_unreadable_paths_unhealthy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // 指向不存在且无法创建的路径（父为文件）
        let blocker = tmp.path().join("not-a-dir");
        fs::write(&blocker, b"x").expect("write blocker");
        let data = blocker.join("data"); // 父是文件 → create_dir_all 失败
        let db = data.join("data.db");
        let log_dir = data.join("logs");
        let status = classify_status(None, false, false, None);
        let snap = assemble_snapshot_from_inputs(base_inputs(status, data, db, log_dir), None);
        assert_eq!(snap.status, DoctorStatus::Unhealthy);
        assert_eq!(snap.paths.data.status, DoctorCheckStatus::Error);
        assert!(snap.paths.data.code.contains("inaccessible"));
    }

    #[test]
    fn fixture_mdns_warning_degraded() {
        let (_tmp, data, db, log_dir) = temp_paths();
        let status = classify_status(None, false, false, None);
        let mut inputs = base_inputs(status, data, db, log_dir);
        inputs.mdns_override = Some(DoctorCheck::new(
            DoctorCheckStatus::Warning,
            "mdns.failed",
            "mDNS discovery failed to initialize",
        ));
        let snap = assemble_snapshot_from_inputs(inputs, None);
        assert_eq!(snap.status, DoctorStatus::Degraded);
        assert_eq!(snap.mdns.status, DoctorCheckStatus::Warning);
        // mDNS 失败不得单独抬升为 unhealthy
        assert_ne!(snap.status, DoctorStatus::Unhealthy);
    }

    #[test]
    fn fixture_missing_optional_deps_degraded() {
        let (_tmp, data, db, log_dir) = temp_paths();
        let status = classify_status(None, false, false, None);
        let mut inputs = base_inputs(status, data, db, log_dir);
        inputs.git = missing_dep("git");
        inputs.tmux = missing_dep("tmux");
        inputs.wsl = missing_dep("WSL"); // applicable missing → warning
        inputs.claude_cli = missing_dep("claude CLI");
        let snap = assemble_snapshot_from_inputs(inputs, None);
        assert_eq!(snap.status, DoctorStatus::Degraded);
        assert_eq!(snap.dependencies.git.status, DoctorCheckStatus::Warning);
        assert_eq!(snap.dependencies.tmux.status, DoctorCheckStatus::Warning);
        assert_eq!(snap.dependencies.wsl.status, DoctorCheckStatus::Warning);
        assert_eq!(
            snap.dependencies.claude_cli.status,
            DoctorCheckStatus::Warning
        );
        assert_eq!(snap.dependencies.git.code, "deps.git.missing");
    }

    #[test]
    fn fixture_platform_inapplicable_wsl_is_info_not_warning() {
        let (_tmp, data, db, log_dir) = temp_paths();
        let status = classify_status(None, false, false, None);
        let mut inputs = base_inputs(status, data, db, log_dir);
        inputs.wsl = na_dep("WSL is not applicable on this platform");
        let snap = assemble_snapshot_from_inputs(inputs, None);
        assert_eq!(snap.dependencies.wsl.status, DoctorCheckStatus::Info);
        assert_eq!(snap.dependencies.wsl.code, "deps.wsl.not_applicable");
        assert_eq!(snap.status, DoctorStatus::Healthy);
    }

    #[test]
    fn recent_errors_parse_only_error_level_and_cap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_dir = tmp.path().to_path_buf();
        let current = log_dir.join("backend.log");
        let mut body = String::new();
        body.push_str(r#"{"timestamp":"2026-07-11T11:00:00Z","level":"info","message":"ok"}"#);
        body.push('\n');
        body.push_str(
            r#"{"timestamp":"2026-07-11T11:01:00Z","level":"error","error_code":"net.timeout","message":"peer request timed out","request_id":"req-1"}"#,
        );
        body.push('\n');
        body.push_str("not-json-line\n");
        body.push_str(
            r#"{"timestamp":"2026-07-11T11:02:00Z","level":"error","error_code":"internal","message":"failed under /Users/alice/web_project/secret-app token=sk-live-abc"}"#,
        );
        body.push('\n');
        fs::write(&current, body).expect("write log");

        let errors = read_recent_errors_from_logs(&current, &log_dir);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].code, "net.timeout");
        assert_eq!(errors[0].request_id.as_deref(), Some("req-1"));
        // 第二行应脱敏
        let joined = errors
            .iter()
            .map(|e| e.summary.clone())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!joined.contains("sk-live-abc"));
        assert!(!joined.contains("secret-app") || joined.contains("<REDACTED>"));
    }

    #[test]
    fn dependency_probe_helpers_are_non_mutating() {
        // 仅确保 helper 可调用且返回 applicable 字段；不安装、不改 PATH
        let git = probe_git_non_mutating_with_budget(None, None);
        assert!(git.applicable);
        let tmux = probe_tmux_non_mutating_with_budget(None, None);
        assert!(tmux.applicable);
        let wsl = probe_wsl_non_mutating_with_budget(None, None);
        // 在 macOS/Linux 上 WSL 应 not applicable
        #[cfg(not(target_os = "windows"))]
        assert!(!wsl.applicable);
        let claude = probe_claude_cli_non_mutating_with_budget("claude", None, None);
        assert!(claude.applicable);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     recentErrors 若只清洗 summary，timestamp/code/requestId 可绕过隐私门禁。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造敌意 JSON 行，断言 parse 后各字段均不含 sentinel 且 timestamp 非法被替换。
    #[test]
    fn recent_errors_sanitize_all_string_fields() {
        let line = r#"{"timestamp":"NOT-A-TIMESTAMP Prompt=do-not-leak","level":"error","error_code":"net.timeout;rm -rf /Users/alice/secret","message":"token=sk-live-abc","request_id":"req-/Users/alice/prompt=leak"}"#;
        let err = super::parse_error_log_line(line).expect("应解析 error 行");
        assert_eq!(err.timestamp, "invalid-timestamp");
        assert!(!err.code.contains("Users"));
        assert!(!err.code.contains("secret"));
        assert!(!err.summary.contains("sk-live"));
        let rid = err.request_id.unwrap_or_default();
        assert!(!rid.contains("Users"), "requestId={rid}");
        assert!(!rid.contains("alice"), "requestId={rid}");
        assert!(!rid.contains("prompt"), "requestId={rid}");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     目录探针不得 truncate/删除固定名既有文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     预置固定名探针文件内容，跑 probe_directory_usable，断言固定名内容仍在。
    #[test]
    fn directory_probe_does_not_destroy_fixed_name_file() {
        let dir = tempfile::tempdir().unwrap();
        let fixed = dir.path().join(".cc-partner-doctor-write-probe");
        std::fs::write(&fixed, b"user-data-keep-me").unwrap();
        let check = super::probe_directory_usable(dir.path(), "paths.data");
        assert!(check.status == DoctorCheckStatus::Ok || check.status == DoctorCheckStatus::Error);
        let content = std::fs::read(&fixed).expect("固定名文件应仍存在");
        assert_eq!(content, b"user-data-keep-me");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     畸形受控 JSON 行不得静默忽略，必须产生 warning 并把 overall 抬升为 degraded。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入 malformed JSON 行 + 合法 error 行，断言 report 含 warning 且 assemble 后 status=degraded。
    #[test]
    fn malformed_recent_error_lines_produce_warning_and_degrade() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_dir = tmp.path().to_path_buf();
        let current = log_dir.join("backend.log");
        let body = concat!(
            r#"{"timestamp":"2026-07-11T11:00:00Z","level":"error","error_code":"net.timeout","message":"ok error"}"#,
            "\n",
            r#"{"timestamp":"bad","level":"error","broken"#,
            "\n",
            r#"{"not":"a-valid-error-schema"}"#,
            "\n",
        );
        fs::write(&current, body).expect("write log");

        let report = read_recent_errors_report(&current, &log_dir);
        assert_eq!(report.errors.len(), 1);
        assert!(report.malformed_warning.is_some());
        let warning = report.malformed_warning.unwrap();
        assert_eq!(warning.status, DoctorCheckStatus::Warning);
        assert_eq!(warning.code, "logs.recent_errors.malformed");

        let data = tmp.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let db = data.join("data.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let status = classify_status(None, false, false, None);
        let mut inputs = base_inputs(status, data, db, log_dir.clone());
        inputs.recent_errors_override = None;
        inputs.recent_errors_warning_override = None;
        let snap = assemble_snapshot_from_inputs(inputs, None);
        assert_eq!(snap.status, DoctorStatus::Degraded);
        assert!(snap.log_parse_warning.is_some());
        assert_eq!(
            snap.log_parse_warning.as_ref().unwrap().code,
            "logs.recent_errors.malformed"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     tail seek 后首个半行不得计为 malformed，避免永远 degraded。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造超 tail 长度的日志，首字节落在半行中部；断言仅完整畸形行计数。
    #[test]
    fn truncated_first_tail_line_is_not_counted_as_malformed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_dir = tmp.path().to_path_buf();
        let current = log_dir.join("backend.log");
        // 构造 >64KiB 日志：前缀垃圾 + 半行 + 换行 + 完整合法 error
        let mut body = "X".repeat((DOCTOR_LOG_TAIL_BYTES as usize) + 10);
        body.push_str(r#"partial-json-{"broken"#);
        body.push('\n');
        body.push_str(
            r#"{"timestamp":"2026-07-11T11:01:00Z","level":"error","error_code":"net.timeout","message":"peer request timed out"}"#,
        );
        body.push('\n');
        fs::write(&current, body).expect("write");
        let report = read_recent_errors_report(&current, &log_dir);
        assert_eq!(report.errors.len(), 1);
        // 半行应被 skip，不应因 truncated first line 产生 malformed
        assert!(
            report.malformed_warning.is_none(),
            "truncated first line must not count as malformed"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     doctor 硬超时必须在阻塞同步工作上抢占返回，且不得 join 卡住线程。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对 `run_on_thread_with_deadline` 注入长于 deadline 的 sleep，断言 wrapper 在
    ///     deadline+margin 内返回 Timeout，且不 join 阻塞线程。
    #[test]
    fn hard_deadline_wrapper_returns_without_joining_stuck_thread() {
        let started = std::time::Instant::now();
        let result = super::run_on_thread_with_deadline(Duration::from_millis(150), || {
            std::thread::sleep(Duration::from_secs(5));
            42_u32
        });
        assert_eq!(result, Err(super::DeadlineRunError::Timeout));
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "硬超时必须在 deadline 后立即返回，实际耗时 {:?}",
            started.elapsed()
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     合法 info/warn 受控 JSON 是正常输出，不得计为 malformed 把 doctor 常态 degraded。
    ///
    /// Code Logic（这个测试做什么）:
    ///     仅写入合法 info/warn 行，断言 report 无 malformed warning；在其它检查 ok 时 overall healthy。
    #[test]
    fn valid_info_and_warn_log_lines_do_not_count_as_malformed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_dir = tmp.path().to_path_buf();
        let current = log_dir.join("backend.log");
        let body = concat!(
            r#"{"timestamp":"2026-07-11T11:00:00Z","level":"info","domain":"http","operation":"serve","result":"ok","message":"listening"}"#,
            "\n",
            r#"{"timestamp":"2026-07-11T11:00:01Z","level":"warn","domain":"sync","operation":"pull","result":"degraded","message":"peer slow"}"#,
            "\n",
            r#"{"timestamp":"2026-07-11T11:00:02Z","level":"debug","message":"trace detail"}"#,
            "\n",
        );
        fs::write(&current, body).expect("write log");

        let report = read_recent_errors_report(&current, &log_dir);
        assert!(report.errors.is_empty(), "非 error 行不得进入 recentErrors");
        assert!(
            report.malformed_warning.is_none(),
            "合法 info/warn/debug 不得计为 malformed"
        );

        let data = tmp.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let db = data.join("data.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let status = classify_status(None, false, false, None);
        let mut inputs = base_inputs(status, data, db, log_dir.clone());
        inputs.recent_errors_override = None;
        inputs.recent_errors_warning_override = None;
        let snap = assemble_snapshot_from_inputs(inputs, None);
        assert_eq!(
            snap.status,
            DoctorStatus::Healthy,
            "仅合法非 error 日志时应 healthy"
        );
        assert!(snap.log_parse_warning.is_none());
    }
}
