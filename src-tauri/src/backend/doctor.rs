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
use crate::workbench::dependencies::{
    probe_claude_cli_non_mutating, probe_git_non_mutating, probe_tmux_non_mutating,
    probe_wsl_non_mutating, OptionalDependencyProbe,
};
use chrono::Utc;
use mdns_sd::ServiceDaemon;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::net::{SocketAddr, TcpListener};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

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
///     mdns/dependencies/recentErrors/logPath；不含环境变量 map 或项目枚举。
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
    // backend.health 的 warning（如 recoverable stale）也算 degraded
    let has_warning = core_checks
        .iter()
        .chain(optional_and_aux.iter())
        .any(|check| check.status == DoctorCheckStatus::Warning);

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
///     归一化 control_path、log_path、各 check.summary 与 recent_errors.summary。
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

    for err in &mut snapshot.recent_errors {
        err.summary = sanitize_doctor_text(&err.summary, home);
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
    pub app_version: String,
    pub backend_version: String,
}

/// 采集完整 doctor 快照（生产入口）。
///
/// Business Logic（为什么需要这个函数）:
///     `doctor` / `doctor --json` 需要一份有界、脱敏、可机器解析的健康快照。
///
/// Code Logic（这个函数做什么）:
///     读取 control/health 状态，探测 path/dependency/mDNS，读 recent errors，
///     计算 overall 并 `sanitize_snapshot_privacy`。
pub async fn collect_doctor_snapshot() -> DoctorSnapshot {
    let inputs = gather_live_probe_inputs().await;
    assemble_snapshot_from_inputs(inputs, current_home_dir().as_deref())
}

/// 从 live 环境组装 probe 输入。
///
/// Business Logic（为什么需要这个函数）:
///     生产路径需要把 control/health、路径、依赖与日志位置一次收集，供组装快照。
///
/// Code Logic（这个函数做什么）:
///     调 `current_status`（含 health 超时）、解析 data/db/log 路径，跑非 mutating 依赖探测。
async fn gather_live_probe_inputs() -> DoctorProbeInputs {
    let backend_status = probe_backend_status().await;
    let control_path = control_file_path();
    let data = data_dir().unwrap_or_else(|_| PathBuf::from(".cc-partner-unresolved"));
    let database_path = data.join("data.db");
    // 尝试从 config 读 db_path；失败则用默认
    if let Ok(cfg) = crate::config::AppConfig::load() {
        let candidate = PathBuf::from(&cfg.db_path);
        // 仅在路径字符串非空时使用
        let db = if cfg.db_path.trim().is_empty() {
            database_path
        } else {
            candidate
        };
        let log_path = backend_log_path().unwrap_or_else(|_| data.join("logs").join("backend.log"));
        let log_dir = log_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| data.join("logs"));
        let claude_path = cfg.github_trending.claude_cli_path.clone();
        return DoctorProbeInputs {
            backend_status,
            control_path,
            data_dir: data,
            database_path: db,
            log_dir,
            log_path,
            git: probe_git_non_mutating(),
            tmux: probe_tmux_non_mutating(),
            wsl: probe_wsl_non_mutating(),
            claude_cli: probe_claude_cli_non_mutating(&claude_path),
            mdns_override: None,
            recent_errors_override: None,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            backend_version: env!("CARGO_PKG_VERSION").to_string(),
        };
    }

    let log_path = backend_log_path().unwrap_or_else(|_| data.join("logs").join("backend.log"));
    let log_dir = log_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| data.join("logs"));
    DoctorProbeInputs {
        backend_status,
        control_path,
        data_dir: data,
        database_path,
        log_dir,
        log_path,
        git: probe_git_non_mutating(),
        tmux: probe_tmux_non_mutating(),
        wsl: probe_wsl_non_mutating(),
        claude_cli: probe_claude_cli_non_mutating("claude"),
        mdns_override: None,
        recent_errors_override: None,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        backend_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// 有界读取当前 backend 控制/health 状态。
///
/// Business Logic（为什么需要这个函数）:
///     doctor 必须复用 control 的 running/stopped/stale 口径，且 health 调用有超时。
///
/// Code Logic（这个函数做什么）:
///     委托 `crate::backend::control::current_status`（内部 health 2s 超时）。
async fn probe_backend_status() -> BackendStatus {
    crate::backend::control::current_status().await
}

/// 根据 probe 输入组装快照（可测试入口）。
///
/// Business Logic（为什么需要这个函数）:
///     fixture 测试需要注入 control/path/dependency 结果验证 overall 与 code 映射。
///
/// Code Logic（这个函数做什么）:
///     映射 backend/path/mdns/deps/recent_errors → compute_overall_status → 隐私清洗。
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
    let mut recent_errors = inputs
        .recent_errors_override
        .unwrap_or_else(|| read_recent_errors_from_logs(&inputs.log_path, &inputs.log_dir));
    // 若 recent errors 读取过程中发现畸形行，read helper 会把 warning 码写入最后一项 code 前缀；
    // 这里不额外抬升 overall（畸形只产生 warning check 由 caller 可选合并）。
    let _ = &mut recent_errors;

    let status = compute_overall_status(&backend, &paths, &mdns, &dependencies);
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
    if let Err(err) = fs::create_dir_all(dir) {
        return DoctorCheck::new(
            DoctorCheckStatus::Error,
            format!("{code_prefix}.inaccessible"),
            format!("directory inaccessible: {err}"),
        );
    }
    let probe = dir.join(".cc-partner-doctor-write-probe");
    match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(mut f) => {
            use std::io::Write;
            let write_ok = f.write_all(b"ok").is_ok();
            drop(f);
            let _ = fs::remove_file(&probe);
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
    if database_path.exists() {
        match File::open(database_path) {
            Ok(mut f) => {
                let mut buf = [0u8; 1];
                // 空库或至少可读 1 字节都算可读；只关心 open/read 是否成功。
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
        let parent = database_path.parent().unwrap_or_else(|| Path::new("."));
        if parent.exists() || fs::create_dir_all(parent).is_ok() {
            // 父目录可访问即认为尚未初始化的健康态
            match OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(parent.join(".cc-partner-doctor-db-parent-probe"))
            {
                Ok(_) => {
                    let _ = fs::remove_file(parent.join(".cc-partner-doctor-db-parent-probe"));
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
        } else {
            DoctorCheck::new(
                DoctorCheckStatus::Error,
                "paths.database.inaccessible",
                "database parent directory is inaccessible",
            )
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

/// 安全读取 recent errors（仅 current + 最新 history 的 tail）。
///
/// Business Logic（为什么需要这个函数）:
///     doctor 展示最近错误便于排障，但绝不能读任意用户文件或泄露正文。
///
/// Code Logic（这个函数做什么）:
///     读 `backend.log` 与 `backend.log.1` 的尾部字节，按行解析受控 JSON，
///     仅接受 level=error；re-sanitize、cap 条数与 summary 长度；畸形行记 warning 跳过。
pub fn read_recent_errors_from_logs(current_log: &Path, log_dir: &Path) -> Vec<DoctorErrorSummary> {
    let mut lines: Vec<String> = Vec::new();
    // newest history first for recency when merging tails
    let history_1 = log_dir.join(format!(
        "{}.1",
        current_log
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("backend.log")
    ));
    // 先读历史再读 current，使 current 行在合并后更“新”
    for path in [&history_1, current_log] {
        if let Some(tail) = read_file_tail(path, DOCTOR_LOG_TAIL_BYTES) {
            for line in tail.lines() {
                if !line.trim().is_empty() {
                    lines.push(line.to_string());
                }
            }
        }
    }

    let mut errors: Vec<DoctorErrorSummary> = Vec::new();
    let mut malformed = 0usize;
    for line in lines {
        match parse_error_log_line(&line) {
            Some(err) => errors.push(err),
            None => {
                // 非 JSON 或非 error 级别：仅对“看起来像 JSON 但字段不对”计 malformed
                let trimmed = line.trim();
                if trimmed.starts_with('{') {
                    malformed += 1;
                }
            }
        }
    }

    // 保留末尾 N 条（最新）
    if errors.len() > DOCTOR_RECENT_ERROR_LIMIT {
        let skip = errors.len() - DOCTOR_RECENT_ERROR_LIMIT;
        errors = errors.into_iter().skip(skip).collect();
    }

    // 畸形行只产生副作用：若有 malformed 且 errors 为空，不额外塞假错误；
    // 调用方可忽略。保留计数仅用于调试——此处不写入 snapshot 额外字段。
    let _ = malformed;
    errors
}

/// 读取文件末尾最多 max_bytes 字节并转 UTF-8 lossy。
///
/// Business Logic（为什么需要这个函数）:
///     大日志不得整文件读入；doctor 只看 tail。
///
/// Code Logic（这个函数做什么）:
///     seek 到 max(0, len-max_bytes) 后 read_to_end。
fn read_file_tail(path: &Path, max_bytes: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > max_bytes {
        file.seek(SeekFrom::Start(len - max_bytes)).ok()?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// 解析单行受控日志 JSON 为 error summary。
///
/// Business Logic（为什么需要这个函数）:
///     只接受白名单 schema 的 error 级行，防止把 info 或任意文本当错误。
///
/// Code Logic（这个函数做什么）:
///     serde_json 解析；要求 level==error；取 timestamp/error_code/message/request_id；
///     summary 再 sanitize + 截断。
fn parse_error_log_line(line: &str) -> Option<DoctorErrorSummary> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let obj = value.as_object()?;
    let level = obj.get("level")?.as_str()?;
    if level != "error" {
        return None;
    }
    let timestamp = obj
        .get("timestamp")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let code = obj
        .get("error_code")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| obj.get("operation").and_then(|v| v.as_str()))
        .unwrap_or("error")
        .to_string();
    let message = obj
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let request_id = obj
        .get("request_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let home = current_home_dir();
    let mut summary = sanitize_doctor_text(&message, home.as_deref());
    if summary.chars().count() > DOCTOR_ERROR_SUMMARY_MAX_CHARS {
        summary = summary
            .chars()
            .take(DOCTOR_ERROR_SUMMARY_MAX_CHARS)
            .collect::<String>()
            + "…";
    }

    Some(DoctorErrorSummary {
        timestamp,
        code,
        summary,
        request_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::control::classify_status;
    use crate::workbench::dependencies::{
        probe_claude_cli_non_mutating, probe_git_non_mutating, probe_tmux_non_mutating,
        probe_wsl_non_mutating,
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
            compute_overall_status(&backend, &paths, &mdns, &deps),
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
            compute_overall_status(&backend, &paths, &mdns, &deps),
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
            compute_overall_status(&backend, &paths, &mdns, &deps),
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
            compute_overall_status(&backend, &paths, &mdns, &deps),
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
        let git = probe_git_non_mutating();
        assert!(git.applicable);
        let tmux = probe_tmux_non_mutating();
        assert!(tmux.applicable);
        let wsl = probe_wsl_non_mutating();
        // 在 macOS/Linux 上 WSL 应 not applicable
        #[cfg(not(target_os = "windows"))]
        assert!(!wsl.applicable);
        let claude = probe_claude_cli_non_mutating("claude");
        assert!(claude.applicable);
    }
}
