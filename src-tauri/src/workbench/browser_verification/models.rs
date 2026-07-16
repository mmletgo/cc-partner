//! browser_verification/models.rs — 浏览器验证有界 DTO 与资源上限
//!
//! Business Logic（为什么需要这个模块）:
//!     local/remote/mobile/CLI 与 Orchestrator 需要同一份 session/command/evidence 契约；
//!     资源上限与脱敏规则必须在 DTO 层可测，防止 fill value、cookie、绝对路径泄漏进 evidence。
//!
//! Code Logic（这个模块做什么）:
//!     定义 camelCase serde DTO、命令枚举、结果类型、资源常量与校验/脱敏 helper；
//!     单元测试覆盖 node/snapshot/fill/timeout 边界与 console 脱敏。

use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// snapshot 最大节点数（含）。
pub const MAX_SNAPSHOT_NODES: u32 = 5_000;
/// snapshot JSON 最大字节数（2 MiB）。
pub const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
/// fill value 最大字节数（64 KiB）。
pub const MAX_FILL_VALUE_BYTES: usize = 64 * 1024;
/// wait timeout 下限（毫秒）。
pub const MIN_WAIT_TIMEOUT_MS: u64 = 100;
/// wait timeout 上限（毫秒）。
pub const MAX_WAIT_TIMEOUT_MS: u64 = 30_000;
/// console 最大条目数。
pub const MAX_CONSOLE_ENTRIES: usize = 1_000;
/// console 证据正文最大字节数（1 MiB）。
pub const MAX_CONSOLE_BYTES: usize = 1024 * 1024;
/// screenshot PNG 最大字节数（8 MiB）。
pub const MAX_SCREENSHOT_BYTES: usize = 8 * 1024 * 1024;
/// 单 run 最大 artifact 数量。
pub const MAX_ARTIFACTS_PER_RUN: usize = 20;
/// 单 run artifact 总字节上限（50 MiB）。
pub const MAX_ARTIFACT_BYTES_PER_RUN: usize = 50 * 1024 * 1024;
/// artifact 保留时长（24 小时）。
pub const ARTIFACT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
/// verification session 最大时长（30 分钟）。
pub const SESSION_MAX_DURATION: Duration = Duration::from_secs(30 * 60);
/// 空闲退出时长（60 秒）。
pub const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// 并发 verification run 上限。
pub const MAX_CONCURRENT_RUNS: usize = 2;

/// 浏览器验证会话状态。
///
/// Business Logic（为什么需要这个枚举）:
///     UI/Orchestrator 需要知道 run 是排队、运行、成功、失败还是已取消。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 序列化稳定状态 token。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrowserVerificationState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Canceled,
}

/// 等待条件。
///
/// Business Logic（为什么需要这个枚举）:
///     wait 只接受结构化条件，禁止任意脚本或 selector shell。
///
/// Code Logic（这个枚举做什么）:
///     标签式 enum，按条件类型携带有界字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BrowserWaitCondition {
    /// DOMContentLoaded 或 load 完成。
    #[serde(rename = "domContentLoaded")]
    DomContentLoaded,
    /// URL path 等于给定值。
    #[serde(rename = "urlPath")]
    UrlPath { path: String },
    /// 文本出现在页面（有界长度）。
    #[serde(rename = "textPresent")]
    TextPresent { text: String },
    /// 指定 role/name 的节点可见。
    #[serde(rename = "roleVisible")]
    RoleVisible { role: String, name: String },
    /// console error 数量不超过阈值。
    #[serde(rename = "consoleErrorCountAtMost")]
    ConsoleErrorCountAtMost { max: u32 },
}

/// 浏览器验证命令（结构化，无任意 selector/eval）。
///
/// Business Logic（为什么需要这个枚举）:
///     engine 只执行有限命令面；click/fill 必须绑定当前 generation 的 opaque nodeRef。
///
/// Code Logic（这个枚举做什么）:
///     标签式 enum；fill 的 value 永不进入 command result 序列化路径。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BrowserVerificationCommand {
    Snapshot {
        max_nodes: u32,
    },
    Click {
        node_ref: String,
    },
    Fill {
        node_ref: String,
        value: String,
    },
    WaitFor {
        condition: BrowserWaitCondition,
        timeout_ms: u64,
    },
    Screenshot {
        full_page: bool,
    },
    ReadConsole {
        after_sequence: u64,
    },
}

/// accessibility snapshot 节点。
///
/// Business Logic（为什么需要这个结构体）:
///     UI/断言需要 role/name/state/bounds；不得返回 password value。
///
/// Code Logic（这个结构体做什么）:
///     保存 opaque node_ref、role、name、可选 state/bounds/source_hint。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSnapshotNode {
    pub node_ref: String,
    pub role: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<BrowserNodeBounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hint: Option<String>,
}

/// 节点屏幕坐标（CSS 像素）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserNodeBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// snapshot 命令结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSnapshotResult {
    pub generation: u64,
    pub nodes: Vec<BrowserSnapshotNode>,
    pub truncated: bool,
    pub url_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_title: Option<String>,
}

/// console 条目级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrowserConsoleLevel {
    Log,
    Info,
    Warn,
    Error,
    Debug,
}

/// console 条目（正文已脱敏）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserConsoleEntry {
    pub sequence: u64,
    pub level: BrowserConsoleLevel,
    pub text: String,
    pub timestamp_ms: i64,
}

/// 断言结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserAssertionResult {
    pub name: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 结构化 command 结果（fill 永不携带输入 value）。
///
/// Business Logic（为什么需要这个枚举）:
///     click/fill 对账与 UI 展示只需要成功/失败元数据；fill value 禁止出现在结果 JSON。
///
/// Code Logic（这个枚举做什么）:
///     各变体仅含安全字段；`Filled` 只含 node_ref 与 generation。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BrowserCommandResult {
    Snapshot(BrowserSnapshotResult),
    Clicked {
        node_ref: String,
        generation: u64,
        hit_count: u32,
    },
    Filled {
        node_ref: String,
        generation: u64,
    },
    WaitSatisfied {
        timeout_ms: u64,
    },
    Screenshot {
        artifact_id: String,
        byte_len: usize,
        full_page: bool,
    },
    Console {
        entries: Vec<BrowserConsoleEntry>,
        truncated: bool,
    },
}

impl BrowserCommandResult {
    /// 构造 fill 成功结果（不包含输入 value）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     fill 命令完成后调用方只需确认节点与 generation；输入正文不得进入 evidence/日志 JSON。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回仅含 `node_ref` 与 `generation` 的 `Filled` 变体。
    pub fn filled(node_ref: impl Into<String>, generation: u64) -> Self {
        Self::Filled {
            node_ref: node_ref.into(),
            generation,
        }
    }

    /// 构造 click 成功结果。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     对账与安全审计需要 hit_count=1 的稳定结果形状。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `Clicked` 变体。
    pub fn clicked(node_ref: impl Into<String>, generation: u64, hit_count: u32) -> Self {
        Self::Clicked {
            node_ref: node_ref.into(),
            generation,
            hit_count,
        }
    }
}

/// 浏览器验证 evidence 摘要（中立，不引用 task 表）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserVerificationEvidence {
    pub session_id: String,
    pub url_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_title: Option<String>,
    pub assertions: Vec<BrowserAssertionResult>,
    pub console_errors: Vec<BrowserConsoleEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_id: Option<String>,
    pub truncated: bool,
    pub captured_at: String,
}

/// 浏览器验证会话 DTO。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserVerificationSession {
    pub id: String,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    pub preview_id: String,
    pub owner_instance_id: String,
    pub state: BrowserVerificationState,
    pub created_at: String,
    pub last_activity_at: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// 启动验证请求（只接受 previewId，禁止 target URL）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserVerificationStartRequest {
    pub preview_id: String,
    /// 幂等键：相同 key 复用同一 run。
    pub request_id: String,
    #[serde(default)]
    pub commands: Vec<BrowserVerificationCommand>,
    /// 请求指纹（命令摘要）；不同 fingerprint 同 request_id 冲突。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

/// 运行详情（含可选 evidence）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserVerificationRun {
    pub session: BrowserVerificationSession,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<BrowserVerificationEvidence>,
    #[serde(default)]
    pub command_results: Vec<BrowserCommandResult>,
}

/// 验证 artifact 传输 DTO（base64，有界）。
///
/// Business Logic（为什么需要这个结构体）:
///     desktop/mobile/remote 拉取截图时需要统一 camelCase 信封，避免路径穿越与裸字节。
///
/// Code Logic（这个结构体做什么）:
///     携带 run/artifact id、content_type、长度与 base64 正文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserVerificationArtifactDto {
    pub run_id: String,
    pub artifact_id: String,
    pub content_type: String,
    pub byte_len: usize,
    pub base64: String,
}

/// 校验 snapshot max_nodes 上限。
///
/// Business Logic（为什么需要这个函数）:
///     超过 5,000 节点会拖垮 owner 与传输预算，必须在入参层拒绝。
///
/// Code Logic（这个函数做什么）:
///     max_nodes 为 0 时回落默认上限；> MAX 返回 validation `resource_limit`。
pub fn validate_snapshot_max_nodes(max_nodes: u32) -> Result<u32, AppError> {
    let effective = if max_nodes == 0 {
        MAX_SNAPSHOT_NODES
    } else {
        max_nodes
    };
    if effective > MAX_SNAPSHOT_NODES {
        return Err(AppError::validation("resource_limit"));
    }
    Ok(effective)
}

/// 校验 fill value 字节上限。
///
/// Business Logic（为什么需要这个函数）:
///     超大 fill 可能用于 abuse CDP；value 本身也不应写入日志。
///
/// Code Logic（这个函数做什么）:
///     按 UTF-8 字节长度比较 MAX_FILL_VALUE_BYTES；超限返回 `resource_limit`。
pub fn validate_fill_value(value: &str) -> Result<(), AppError> {
    if value.len() > MAX_FILL_VALUE_BYTES {
        return Err(AppError::validation("resource_limit"));
    }
    Ok(())
}

/// 校验 fill 目标控件是否允许写入（password/file/hidden 禁止）。
///
/// Business Logic（为什么需要这个函数）:
///     Spec 禁止通过验证引擎向 password/file/hidden 控件注入内容，防止凭证与本地文件路径被自动化写入。
///
/// Code Logic（这个函数做什么）:
///     根据 tagName、input type、hidden 状态判断；命中禁止类型返回 `browser_fill_forbidden_control`。
///     可在无 DOM 的单元测试中单独覆盖。
pub fn validate_fill_control_kind(
    tag_name: Option<&str>,
    input_type: Option<&str>,
    is_hidden: bool,
) -> Result<(), AppError> {
    if is_hidden {
        return Err(AppError::validation("browser_fill_forbidden_control"));
    }
    let tag = tag_name.unwrap_or("").trim().to_ascii_lowercase();
    let ty = input_type.unwrap_or("text").trim().to_ascii_lowercase();
    if tag == "input" && (ty == "password" || ty == "file" || ty == "hidden") {
        return Err(AppError::validation("browser_fill_forbidden_control"));
    }
    // 无 type 的 hidden input 常见于 type="hidden"；role/state 已由 is_hidden 覆盖。
    if ty == "password" || ty == "file" || ty == "hidden" {
        return Err(AppError::validation("browser_fill_forbidden_control"));
    }
    Ok(())
}

/// 校验 wait timeout 范围。
///
/// Business Logic（为什么需要这个函数）:
///     timeout 过短无意义，过长会占用 ephemeral browser 预算。
///
/// Code Logic（这个函数做什么）:
///     要求 [MIN_WAIT_TIMEOUT_MS, MAX_WAIT_TIMEOUT_MS]，否则 `resource_limit`。
pub fn validate_wait_timeout_ms(timeout_ms: u64) -> Result<u64, AppError> {
    if !(MIN_WAIT_TIMEOUT_MS..=MAX_WAIT_TIMEOUT_MS).contains(&timeout_ms) {
        return Err(AppError::validation("resource_limit"));
    }
    Ok(timeout_ms)
}

/// 校验 snapshot 序列化字节预算。
///
/// Business Logic（为什么需要这个函数）:
///     即使节点数 ≤5,000，大 name/state 仍可能撑爆 2 MiB 传输预算。
///
/// Code Logic（这个函数做什么）:
///     对 `BrowserSnapshotResult` 做 JSON 序列化；超 2 MiB 返回 `resource_limit`。
pub fn validate_snapshot_byte_budget(snapshot: &BrowserSnapshotResult) -> Result<(), AppError> {
    let bytes = serde_json::to_vec(snapshot).map_err(|e| AppError::generic(e.to_string()))?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(AppError::validation("resource_limit"));
    }
    Ok(())
}

/// 截断 snapshot 节点列表并标记 truncated。
///
/// Business Logic（为什么需要这个函数）:
///     engine 产出可能略超上限，服务层需裁剪到 5,000 并标记截断。
///
/// Code Logic（这个函数做什么）:
///     若 nodes.len() > MAX，截断并设 truncated=true。
pub fn truncate_snapshot_nodes(mut snapshot: BrowserSnapshotResult) -> BrowserSnapshotResult {
    if snapshot.nodes.len() > MAX_SNAPSHOT_NODES as usize {
        snapshot.nodes.truncate(MAX_SNAPSHOT_NODES as usize);
        snapshot.truncated = true;
    }
    snapshot
}

/// 清洗 console/URL 文本中的 query、header 形态与 cookie。
///
/// Business Logic（为什么需要这个函数）:
///     console 可能打印带 token 的 URL、Authorization header 或 Cookie；evidence 必须脱敏。
///
/// Code Logic（这个函数做什么）:
///     去掉 URL query/fragment；替换 cookie/authorization/header 关键字赋值；截断过长正文。
pub fn redact_console_text(raw: &str) -> String {
    let mut out = raw.to_string();
    // 剥离 query / fragment
    if let Some(idx) = out.find('?') {
        // 仅在疑似 URL 片段时裁剪 query
        let before = &out[..idx];
        if before.contains("://") || before.contains("http") || before.contains('/') {
            if let Some(frag) =
                out[idx..].find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            {
                let end = idx + frag;
                out.replace_range(idx..end, "");
            } else {
                out.truncate(idx);
            }
        }
    }
    if let Some(idx) = out.find('#') {
        let before = &out[..idx];
        if before.contains("://") || before.contains('/') {
            if let Some(frag) =
                out[idx..].find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            {
                let end = idx + frag;
                out.replace_range(idx..end, "");
            } else {
                out.truncate(idx);
            }
        }
    }
    // header / cookie / authorization 形态
    for pattern in [
        "authorization:",
        "Authorization:",
        "cookie:",
        "Cookie:",
        "set-cookie:",
        "Set-Cookie:",
        "Bearer ",
        "bearer ",
    ] {
        if let Some(pos) = out.find(pattern) {
            let rest = &out[pos + pattern.len()..];
            let end_rel = rest.find(['\n', '\r', '"', '\'']).unwrap_or(rest.len());
            let end = pos + pattern.len() + end_rel;
            out.replace_range(pos..end, &format!("{pattern}[REDACTED]"));
        }
    }
    // 绝对路径粗略脱敏（Unix/macOS home 与 Windows 盘符）
    out = redact_absolute_paths(&out);
    if out.len() > 4_096 {
        out.truncate(4_096);
        out.push('…');
    }
    out
}

/// 粗略脱敏绝对路径。
///
/// Business Logic（为什么需要这个函数）:
///     stack trace 中的用户 home 路径不得进入 evidence。
///
/// Code Logic（这个函数做什么）:
///     将 `/Users/...`、`/home/...`、`C:\Users\...` 等替换为 `[PATH]`。
fn redact_absolute_paths(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/'
            && (input[i..].starts_with("/Users/")
                || input[i..].starts_with("/home/")
                || input[i..].starts_with("/private/var/")
                || input[i..].starts_with("/var/folders/"))
        {
            out.push_str("[PATH]");
            i += 1;
            while i < bytes.len()
                && !bytes[i].is_ascii_whitespace()
                && bytes[i] != b'"'
                && bytes[i] != b'\''
                && bytes[i] != b')'
            {
                i += 1;
            }
            continue;
        }
        // Windows 风格 C:\Users\...
        if i + 3 < bytes.len()
            && bytes[i].is_ascii_alphabetic()
            && bytes[i + 1] == b':'
            && (bytes[i + 2] == b'\\' || bytes[i + 2] == b'/')
        {
            out.push_str("[PATH]");
            i += 3;
            while i < bytes.len()
                && !bytes[i].is_ascii_whitespace()
                && bytes[i] != b'"'
                && bytes[i] != b'\''
            {
                i += 1;
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// 默认 smoke 命令序列：wait DOMContentLoaded → snapshot → console → screenshot。
///
/// Business Logic（为什么需要这个函数）:
///     一键验证不要求用户写脚本或选元素；默认产出 a11y 摘要、console 与截图。
///
/// Code Logic（这个函数做什么）:
///     返回固定有序命令列表。
pub fn default_smoke_commands() -> Vec<BrowserVerificationCommand> {
    vec![
        BrowserVerificationCommand::WaitFor {
            condition: BrowserWaitCondition::DomContentLoaded,
            timeout_ms: 15_000,
        },
        BrowserVerificationCommand::Snapshot {
            max_nodes: MAX_SNAPSHOT_NODES,
        },
        BrowserVerificationCommand::ReadConsole { after_sequence: 0 },
        BrowserVerificationCommand::Screenshot { full_page: false },
    ]
}

/// 计算命令列表的稳定指纹（用于幂等冲突检测）。
///
/// Business Logic（为什么需要这个函数）:
///     同一 request_id 若命令面不同必须冲突，不能静默复用错误 run。
///
/// Code Logic（这个函数做什么）:
///     对命令 JSON 做序列化后简易哈希（非密码学，仅冲突检测）。
pub fn command_fingerprint(commands: &[BrowserVerificationCommand]) -> String {
    let json = serde_json::to_string(commands).unwrap_or_default();
    // 不把 Fill.value 写入指纹以外的日志；指纹本身包含 value 哈希足够区分，
    // 但对外 API 只暴露 hex digest，不回显 value。
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fill 结果 JSON 永不包含输入 value 字段名或 secret 正文。
    #[test]
    fn fill_result_never_serializes_input_value() {
        let result = BrowserCommandResult::filled("node-1", 12);
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("secret value"));
        assert!(!json.contains("value"));
        assert!(json.contains("node-1"));
        assert!(json.contains("\"generation\":12") || json.contains("\"generation\": 12"));
    }

    /// 5,001 节点入参被拒绝。
    #[test]
    fn snapshot_rejects_more_than_5000_nodes_request() {
        let err = validate_snapshot_max_nodes(5_001).unwrap_err();
        assert_eq!(err.code(), "resource_limit");
    }

    /// 截断 helper 将 5,001 节点裁到 5,000 并标记 truncated。
    #[test]
    fn snapshot_truncates_5001_nodes() {
        let nodes: Vec<_> = (0..5_001)
            .map(|i| BrowserSnapshotNode {
                node_ref: format!("n{i}"),
                role: "button".into(),
                name: format!("b{i}"),
                state: None,
                bounds: None,
                source_hint: None,
            })
            .collect();
        let snapshot = truncate_snapshot_nodes(BrowserSnapshotResult {
            generation: 1,
            nodes,
            truncated: false,
            url_path: "/".into(),
            page_title: None,
        });
        assert_eq!(snapshot.nodes.len(), 5_000);
        assert!(snapshot.truncated);
    }

    /// 2 MiB+ snapshot 字节预算被拒绝。
    #[test]
    fn snapshot_rejects_over_2mib_payload() {
        // 构造超大 name 使 JSON > 2 MiB
        let big = "x".repeat(3 * 1024 * 1024);
        let snapshot = BrowserSnapshotResult {
            generation: 1,
            nodes: vec![BrowserSnapshotNode {
                node_ref: "n1".into(),
                role: "text".into(),
                name: big,
                state: None,
                bounds: None,
                source_hint: None,
            }],
            truncated: false,
            url_path: "/".into(),
            page_title: None,
        };
        let err = validate_snapshot_byte_budget(&snapshot).unwrap_err();
        assert_eq!(err.code(), "resource_limit");
    }

    /// fill 超过 64 KiB 被拒绝。
    #[test]
    fn fill_rejects_over_64kib_value() {
        let big = "a".repeat(MAX_FILL_VALUE_BYTES + 1);
        let err = validate_fill_value(&big).unwrap_err();
        assert_eq!(err.code(), "resource_limit");
        assert!(validate_fill_value(&"a".repeat(MAX_FILL_VALUE_BYTES)).is_ok());
    }

    /// fill 禁止 password / file / hidden 控件。
    #[test]
    fn fill_rejects_password_file_hidden_controls() {
        assert_eq!(
            validate_fill_control_kind(Some("input"), Some("password"), false)
                .unwrap_err()
                .code(),
            "browser_fill_forbidden_control"
        );
        assert_eq!(
            validate_fill_control_kind(Some("input"), Some("file"), false)
                .unwrap_err()
                .code(),
            "browser_fill_forbidden_control"
        );
        assert_eq!(
            validate_fill_control_kind(Some("input"), Some("hidden"), false)
                .unwrap_err()
                .code(),
            "browser_fill_forbidden_control"
        );
        assert_eq!(
            validate_fill_control_kind(Some("input"), Some("text"), true)
                .unwrap_err()
                .code(),
            "browser_fill_forbidden_control"
        );
        assert!(validate_fill_control_kind(Some("input"), Some("text"), false).is_ok());
        assert!(validate_fill_control_kind(Some("textarea"), None, false).is_ok());
    }

    /// wait timeout 99 / 30_001 被拒绝；边界 100 / 30_000 通过。
    #[test]
    fn wait_timeout_bounds() {
        assert_eq!(
            validate_wait_timeout_ms(99).unwrap_err().code(),
            "resource_limit"
        );
        assert_eq!(
            validate_wait_timeout_ms(30_001).unwrap_err().code(),
            "resource_limit"
        );
        assert_eq!(validate_wait_timeout_ms(100).unwrap(), 100);
        assert_eq!(validate_wait_timeout_ms(30_000).unwrap(), 30_000);
    }

    /// console 脱敏剥离 query 与 Authorization header。
    #[test]
    fn console_redacts_query_and_authorization_header() {
        let raw = "fetch failed http://127.0.0.1:5173/api?token=super-secret&x=1 Authorization: Bearer abc.def Cookie: sid=xyz";
        let cleaned = redact_console_text(raw);
        assert!(
            !cleaned.contains("super-secret"),
            "query secret must be redacted: {cleaned}"
        );
        assert!(
            !cleaned.contains("abc.def"),
            "bearer must be redacted: {cleaned}"
        );
        assert!(
            !cleaned.contains("sid=xyz"),
            "cookie must be redacted: {cleaned}"
        );
        assert!(cleaned.contains("[REDACTED]") || !cleaned.contains("token="));
    }

    /// console 脱敏剥离绝对路径。
    #[test]
    fn console_redacts_absolute_paths() {
        let raw = "Error at /Users/hans/project/src/app.ts:12";
        let cleaned = redact_console_text(raw);
        assert!(!cleaned.contains("/Users/hans"));
        assert!(cleaned.contains("[PATH]"));
    }

    /// start 请求 DTO 不含 targetUrl 字段。
    #[test]
    fn start_request_schema_has_no_target_url() {
        let req = BrowserVerificationStartRequest {
            preview_id: "p1".into(),
            request_id: "r1".into(),
            commands: default_smoke_commands(),
            fingerprint: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("targetUrl"));
        assert!(!json.contains("target_url"));
        assert!(json.contains("previewId"));
    }

    /// 默认 smoke 不含脚本或 selector 字段。
    #[test]
    fn default_smoke_has_no_script_or_selector() {
        let cmds = default_smoke_commands();
        let json = serde_json::to_string(&cmds).unwrap();
        assert!(!json.to_lowercase().contains("javascript"));
        assert!(!json.contains("selector"));
        assert!(!json.contains("eval"));
    }
}
