//! backend/doctor.rs — Doctor 健康检查快照 schema、状态聚合与隐私归一化。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户与 smoke/CI 需要一份可机器解析、可人工阅读的后端健康快照，
//!     同时保证路径/摘要中不泄露 home 用户名、项目名、Prompt 或凭据。
//!
//! Code Logic（这个模块做什么）:
//!     定义 camelCase `DoctorSnapshot` 及其子 DTO、检查严重度与 overall 状态计算，
//!     以及把 home 前缀替换为 `<HOME>` 的隐私 helper；完整 probe 接线留给后续任务。

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

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
    let core_checks = [
        &paths.data,
        &paths.database,
        &paths.log,
        &backend.health,
    ];
    if core_checks
        .iter()
        .any(|check| check.status == DoctorCheckStatus::Error)
    {
        return DoctorStatus::Unhealthy;
    }

    let optional_and_aux = [
        &mdns,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

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
            json!(format!("{HOME_PLACEHOLDER}/.cc-partner/backend-control.json"))
        );
        assert_eq!(value["backend"]["pid"], json!(4242));
        assert_eq!(value["backend"]["port"], json!(62116));
        assert_eq!(value["backend"]["health"]["status"], json!("ok"));
        assert_eq!(value["backend"]["health"]["code"], json!("backend.health.ok"));
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
        let round_trip: DoctorSnapshot =
            serde_json::from_str(&text).expect("from_str round-trip");
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
}
