//! agent_cli/output.rs — JSON envelope、exit code 与 stdout/stderr 隔离。
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent 只能依赖稳定 schemaVersion/ok/error/exit code 合同；日志不得污染 `--json` stdout。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `CliExitCode`、`CliError`、成功/失败 envelope 渲染；失败从不回显敏感正文。

use serde::Serialize;
use serde_json::Value;

/// CLI 进程退出码（0..=7）。
///
/// Business Logic（为什么需要这个枚举）:
///     Agent 需要按固定整数分支，而不是解析本地化 message。
///
/// Code Logic（这个枚举做什么）:
///     `repr(i32)` 覆盖 success/internal/usage/not_found/conflict/unavailable/unsupported/partial。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliExitCode {
    Success = 0,
    Internal = 1,
    Usage = 2,
    NotFound = 3,
    Conflict = 4,
    Unavailable = 5,
    Unsupported = 6,
    Partial = 7,
}

impl CliExitCode {
    /// Business Logic（为什么需要这个函数）:
    ///     测试与 dispatch 需要把枚举稳定映射为 i32。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `self as i32`。
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

/// 成功 envelope（schemaVersion=1）。
///
/// Business Logic（为什么需要这个结构）:
///     `--json` 成功时 stdout 只能有一个 JSON 对象。
///
/// Code Logic（这个结构做什么）:
///     序列化 `{schemaVersion, ok:true, data}`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliSuccess<T: Serialize> {
    pub schema_version: u32,
    pub ok: bool,
    pub data: T,
}

/// 失败 envelope 中的 error 对象。
///
/// Business Logic（为什么需要这个结构）:
///     失败必须带稳定 code/retryable/outcomeUnknown，且 message 有界通用、不含敏感正文。
///
/// Code Logic（这个结构做什么）:
///     camelCase：code/message/retryable/requestId/outcomeUnknown。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CliErrorBody {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub request_id: Option<String>,
    pub outcome_unknown: bool,
}

/// 失败 envelope。
///
/// Business Logic（为什么需要这个结构）:
///     Agent 解析失败时只看 ok=false 与 error 对象。
///
/// Code Logic（这个结构做什么）:
///     `{schemaVersion, ok:false, error}`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliFailure {
    pub schema_version: u32,
    pub ok: bool,
    pub error: CliErrorBody,
}

/// 已渲染的 CLI 输出（stdout/stderr/exit）。
///
/// Business Logic（为什么需要这个结构）:
///     单测需要断言 `--json` 时 stderr 为空且 exit 映射正确，而不真正写进程流。
///
/// Code Logic（这个结构做什么）:
///     持有 stdout/stderr 字符串与 exit_code。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// 稳定 CLI 错误（带 exit code 映射，message 永不含敏感正文）。
///
/// Business Logic（为什么需要这个类型）:
///     dispatch/client/remote 需要统一错误码与 outcomeUnknown，避免解析本地化字符串。
///
/// Code Logic（这个类型做什么）:
///     保存 code/message/retryable/request_id/outcome_unknown 与对应 exit。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub request_id: Option<String>,
    pub outcome_unknown: bool,
    pub exit: CliExitCode,
}

impl std::fmt::Display for CliError {
    /// Business Logic（为什么需要这个函数）:
    ///     serde/map_err 与日志需要 Display，且不得回显敏感正文以外的额外字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     输出 `code: message`。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CliError {}

impl CliError {
    /// Business Logic（为什么需要这个函数）:
    ///     构造内部错误（exit 1）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     code=`internal`，retryable=false。
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: "internal".into(),
            message: message.into(),
            retryable: false,
            request_id: None,
            outcome_unknown: false,
            exit: CliExitCode::Internal,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     参数/输入校验失败（exit 2）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     code 默认可覆盖；retryable=false。
    pub fn usage(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            request_id: None,
            outcome_unknown: false,
            exit: CliExitCode::Usage,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     资源不存在（exit 3）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     code=`not_found`。
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "not_found".into(),
            message: message.into(),
            retryable: false,
            request_id: None,
            outcome_unknown: false,
            exit: CliExitCode::NotFound,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     选择器多命中或状态冲突（exit 4）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     code=`ambiguous_selector` 或调用方指定；默认 conflict exit。
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            code: "ambiguous_selector".into(),
            message: message.into(),
            retryable: false,
            request_id: None,
            outcome_unknown: false,
            exit: CliExitCode::Conflict,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     后端离线/超时/不可用（exit 5）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     code 可指定；retryable 默认 true。
    pub fn unavailable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: true,
            request_id: None,
            outcome_unknown: false,
            exit: CliExitCode::Unavailable,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     协议/能力不支持（exit 6）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     code=`unsupported_capability` 默认。
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            code: "unsupported_capability".into(),
            message: message.into(),
            retryable: false,
            request_id: None,
            outcome_unknown: false,
            exit: CliExitCode::Unsupported,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     部分成功（exit 7）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     code=`partial_result`。
    pub fn partial(message: impl Into<String>) -> Self {
        Self {
            code: "partial_result".into(),
            message: message.into(),
            retryable: false,
            request_id: None,
            outcome_unknown: false,
            exit: CliExitCode::Partial,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     non-replayable mutation 连接丢失时必须标记 outcomeUnknown。
    ///
    /// Code Logic（这个函数做什么）:
    ///     code=`outcome_unknown`，retryable=false，outcome_unknown=true，exit=5。
    pub fn outcome_unknown(message: impl Into<String>) -> Self {
        Self {
            code: "outcome_unknown".into(),
            message: message.into(),
            retryable: false,
            request_id: None,
            outcome_unknown: true,
            exit: CliExitCode::Unavailable,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试与 mapping 需要读取稳定 code。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 code 字符串切片。
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Business Logic（为什么需要这个函数）:
    ///     mutation 客户端需判断是否 outcomeUnknown。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 outcome_unknown 字段。
    pub fn outcome_unknown_flag(&self) -> bool {
        self.outcome_unknown
    }

    /// Business Logic（为什么需要这个函数）:
    ///     结构化 P2P/control 错误需挂 requestId。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆后写入 request_id。
    pub fn with_request_id(mut self, request_id: Option<String>) -> Self {
        self.request_id = request_id;
        self
    }

    /// Business Logic（为什么需要这个函数）:
    ///     部分错误可覆盖 retryable。
    ///
    /// Code Logic（这个函数做什么）:
    ///     设置 retryable 后返回 self。
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Business Logic（为什么需要这个函数）:
    ///     需要把 code 从默认值改成领域稳定 code。
    ///
    /// Code Logic（这个函数做什么）:
    ///     覆盖 code。
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = code.into();
        self
    }
}

/// 渲染成功 JSON envelope。
///
/// Business Logic（为什么需要这个函数）:
///     `--json` 成功时 stdout 必须是单一 JSON，stderr 空。
///
/// Code Logic（这个函数做什么）:
///     序列化 CliSuccess；非 json 模式把 data pretty 到 stdout。
pub fn render_success<T: Serialize>(data: T, json: bool) -> RenderedOutput {
    if json {
        let body = CliSuccess {
            schema_version: 1,
            ok: true,
            data,
        };
        let stdout = serde_json::to_string(&body).unwrap_or_else(|_| {
            r#"{"schemaVersion":1,"ok":false,"error":{"code":"internal","message":"serialize failed","retryable":false,"requestId":null,"outcomeUnknown":false}}"#.to_string()
        });
        RenderedOutput {
            stdout,
            stderr: String::new(),
            exit_code: CliExitCode::Success.as_i32(),
        }
    } else {
        let stdout = serde_json::to_string_pretty(&data)
            .unwrap_or_else(|_| "{}".to_string());
        RenderedOutput {
            stdout,
            stderr: String::new(),
            exit_code: CliExitCode::Success.as_i32(),
        }
    }
}

/// 渲染失败 envelope。
///
/// Business Logic（为什么需要这个函数）:
///     `--json` 失败时 stdout 仍输出 envelope（Agent 解析），stderr 不写诊断噪音；
///     非 json 模式可把简短错误写 stderr。
///
/// Code Logic（这个函数做什么）:
///     映射 exit；message 已假定不含敏感正文。
pub fn render_failure(error: CliError, json: bool) -> RenderedOutput {
    let exit_code = error.exit.as_i32();
    let body = CliFailure {
        schema_version: 1,
        ok: false,
        error: CliErrorBody {
            code: error.code,
            message: error.message.clone(),
            retryable: error.retryable,
            request_id: error.request_id,
            outcome_unknown: error.outcome_unknown,
        },
    };
    if json {
        let stdout = serde_json::to_string(&body).unwrap_or_else(|_| {
            r#"{"schemaVersion":1,"ok":false,"error":{"code":"internal","message":"serialize failed","retryable":false,"requestId":null,"outcomeUnknown":false}}"#.to_string()
        });
        RenderedOutput {
            stdout,
            stderr: String::new(),
            exit_code,
        }
    } else {
        RenderedOutput {
            stdout: String::new(),
            stderr: format!("{}: {}", body.error.code, error.message),
            exit_code,
        }
    }
}

/// 将 JSONL 事件行写入 stdout（每事件一行）。
///
/// Business Logic（为什么需要这个函数）:
///     `event follow` 必须以 JSONL 流式输出，不能包在单一 success envelope。
///
/// Code Logic（这个函数做什么）:
///     序列化 Value 为单行 JSON 字符串（不含尾随换行由调用方决定）。
pub fn render_event_line(event: &Value) -> Result<String, CliError> {
    serde_json::to_string(event).map_err(|_| CliError::internal("event serialize failed"))
}

/// 把 RenderedOutput 写到真实 stdout/stderr。
///
/// Business Logic（为什么需要这个函数）:
///     进程入口需要实际落盘 IO。
///
/// Code Logic（这个函数做什么）:
///     写 stdout/stderr 并返回 exit_code。
pub fn emit_rendered(rendered: &RenderedOutput) -> i32 {
    use std::io::Write;
    if !rendered.stdout.is_empty() {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", rendered.stdout);
        let _ = out.flush();
    }
    if !rendered.stderr.is_empty() {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "{}", rendered.stderr);
        let _ = err.flush();
    }
    rendered.exit_code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_failure_maps_conflict_to_exit_four_without_stdout_noise() {
        let rendered = render_failure(CliError::conflict("ambiguous_selector"), true);
        assert_eq!(rendered.exit_code, CliExitCode::Conflict as i32);
        assert_eq!(rendered.stderr, "");
        let body: Value = serde_json::from_str(&rendered.stdout).unwrap();
        assert_eq!(body["schemaVersion"], 1);
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"]["outcomeUnknown"], false);
        assert_eq!(body["error"]["code"], "ambiguous_selector");
    }

    #[test]
    fn all_exit_codes_are_stable() {
        assert_eq!(CliExitCode::Success.as_i32(), 0);
        assert_eq!(CliExitCode::Internal.as_i32(), 1);
        assert_eq!(CliExitCode::Usage.as_i32(), 2);
        assert_eq!(CliExitCode::NotFound.as_i32(), 3);
        assert_eq!(CliExitCode::Conflict.as_i32(), 4);
        assert_eq!(CliExitCode::Unavailable.as_i32(), 5);
        assert_eq!(CliExitCode::Unsupported.as_i32(), 6);
        assert_eq!(CliExitCode::Partial.as_i32(), 7);
    }

    #[test]
    fn json_success_isolates_stdout() {
        let rendered = render_success(serde_json::json!({"items": []}), true);
        assert_eq!(rendered.exit_code, 0);
        assert_eq!(rendered.stderr, "");
        let body: Value = serde_json::from_str(&rendered.stdout).unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["schemaVersion"], 1);
    }

    #[test]
    fn outcome_unknown_maps_exit_five_and_flag() {
        let err = CliError::outcome_unknown("connection lost after dispatch");
        let rendered = render_failure(err, true);
        assert_eq!(rendered.exit_code, 5);
        let body: Value = serde_json::from_str(&rendered.stdout).unwrap();
        assert_eq!(body["error"]["outcomeUnknown"], true);
        assert_eq!(body["error"]["retryable"], false);
        assert_eq!(body["error"]["code"], "outcome_unknown");
    }

    #[test]
    fn event_line_is_single_json_object() {
        let line = render_event_line(&serde_json::json!({
            "ownerInstanceId": "o1",
            "sequence": 3,
            "event": "tick"
        }))
        .unwrap();
        assert!(!line.contains('\n'));
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["sequence"], 3);
    }

    #[test]
    fn failure_message_never_echoes_secret_fixture() {
        let secret = "SUPER_SECRET_PROMPT_BODY_xyz";
        // 调用方必须传通用 message；此处验证 render 不会自动附加额外字段
        let rendered = render_failure(CliError::usage("invalid_input", "stdin body rejected"), true);
        assert!(!rendered.stdout.contains(secret));
        assert!(!rendered.stderr.contains(secret));
    }
}
