//! claude_cli.rs — Claude Code CLI pure/headless 调用共享 helper。
//!
//! Business Logic（为什么需要这个模块）:
//!     GitHub Trending 解说和 Prompt 优化都需要调用本机 Claude Code CLI 并解析结构化 JSON。
//!     共享参数、执行、解析和错误提取逻辑，避免不同功能出现不一致的 CLI 行为。
//!
//! Code Logic（这个模块做什么）:
//!     提供 pure/headless 参数构造、路径/模型归一化、结构化输出解析、非零退出错误摘要和
//!     带 stdin/timeout 的 `Command` 执行入口。

use crate::error::AppError;
use serde::de::DeserializeOwned;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const DEFAULT_CLAUDE_CLI: &str = "claude";
const DEFAULT_CLAUDE_MODEL: &str = "sonnet";
const MAX_ERROR_CHARS: usize = 500;

/// 归一化 Claude CLI 路径。
///
/// Business Logic（为什么需要这个函数）:
///     用户可在设置中留空 CLI 路径，此时应回退到 PATH 中的 `claude`。
///
/// Code Logic（这个函数做什么）:
///     trim 输入，空值返回默认命令名，非空返回去首尾空白后的路径字符串。
pub(crate) fn normalize_cli_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        DEFAULT_CLAUDE_CLI.to_string()
    } else {
        trimmed.to_string()
    }
}

/// 归一化 Claude 模型名。
///
/// Business Logic（为什么需要这个函数）:
///     多个 Claude CLI 功能复用同一份模型配置；用户留空时需要稳定默认值。
///
/// Code Logic（这个函数做什么）:
///     trim 输入，空值返回 `sonnet`，非空返回去首尾空白后的模型名。
pub(crate) fn normalize_model(model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        DEFAULT_CLAUDE_MODEL.to_string()
    } else {
        trimmed.to_string()
    }
}

/// 构造 Claude Code CLI pure/headless 结构化输出参数。
///
/// Business Logic（为什么需要这个函数）:
///     应用内部结构化生成任务不需要加载项目上下文、会话持久化或工具。
///
/// Code Logic（这个函数做什么）:
///     返回 bare/headless/json-schema 参数列表，且不包含预算参数。
pub(crate) fn build_pure_headless_args(model: &str, schema: &str) -> Vec<String> {
    vec![
        "--bare".to_string(),
        "-p".to_string(),
        "--output-format".to_string(),
        "json".to_string(),
        "--json-schema".to_string(),
        schema.to_string(),
        "--no-session-persistence".to_string(),
        "--tools".to_string(),
        "".to_string(),
        "--model".to_string(),
        normalize_model(model),
    ]
}

/// 执行 Claude CLI 并解析结构化 JSON 输出。
///
/// Business Logic（为什么需要这个函数）:
///     多个功能都需要把输入通过 stdin 交给本机 Claude CLI，并得到严格 schema 输出。
///
/// Code Logic（这个函数做什么）:
///     使用 `Command::new(cli)` 直接启动进程，不经过 shell；stdin/stdout/stderr 均管道化；
///     写入 prompt 后用 timeout 包裹 `wait_with_output()`。
pub(crate) async fn run_structured_json<T>(
    cli_path: &str,
    model: &str,
    schema: &str,
    prompt: &str,
    timeout_secs: u64,
    task_label: &str,
) -> Result<T, AppError>
where
    T: DeserializeOwned,
{
    let cli = normalize_cli_path(cli_path);
    let mut cmd = Command::new(cli);
    cmd.args(build_pure_headless_args(model, schema))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::generic(format!("启动 Claude CLI 失败: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| AppError::generic(format!("写入 Claude CLI prompt 失败: {e}")))?;
    }

    let output =
        match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
            .await
        {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(AppError::generic(format!("等待 Claude CLI 输出失败: {e}"))),
            Err(_) => {
                return Err(AppError::generic(format!(
                    "Claude CLI {task_label}超时（{timeout_secs} 秒）"
                )))
            }
        };

    if !output.status.success() {
        return Err(AppError::generic(format!(
            "Claude CLI {task_label}失败: {}",
            failure_detail(&output.stderr, &output.stdout)
        )));
    }

    parse_structured_output(&String::from_utf8_lossy(&output.stdout))
}

/// 解析 Claude CLI 结构化输出。
///
/// Business Logic（为什么需要这个函数）:
///     不同 Claude CLI 版本可能返回直接 JSON、`structured_output` 或 `result` 包装。
///
/// Code Logic（这个函数做什么）:
///     先解析 stdout 为 JSON Value，再依次尝试直接反序列化、structured_output、result object、
///     result string。
pub(crate) fn parse_structured_output<T>(stdout: &str) -> Result<T, AppError>
where
    T: DeserializeOwned,
{
    let value: serde_json::Value = serde_json::from_str(stdout.trim())?;
    if let Ok(parsed) = serde_json::from_value::<T>(value.clone()) {
        return Ok(parsed);
    }
    if let Some(structured_output) = value.get("structured_output") {
        return Ok(serde_json::from_value::<T>(structured_output.clone())?);
    }
    if let Some(result) = value.get("result") {
        if result.is_object() {
            return Ok(serde_json::from_value::<T>(result.clone())?);
        }
        if let Some(text) = result.as_str() {
            return Ok(serde_json::from_str::<T>(text.trim())?);
        }
        return Err(AppError::generic("Claude CLI 输出 result 不是可解析 JSON"));
    }
    Err(AppError::generic(
        "Claude CLI 输出缺少结构化 JSON/structured_output/result 字段",
    ))
}

/// 从 Claude CLI 非零退出输出中提取用户可读错误。
///
/// Business Logic（为什么需要这个函数）:
///     Claude CLI 在部分失败场景会把错误写入 stdout JSON 而非 stderr。
///
/// Code Logic（这个函数做什么）:
///     优先 stderr；否则解析 stdout JSON 的 errors/result/subtype；仍无结构化错误时截断 stdout。
pub(crate) fn failure_detail(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr_text = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr_text.is_empty() {
        return stderr_text;
    }
    let stdout_text = String::from_utf8_lossy(stdout).trim().to_string();
    if stdout_text.is_empty() {
        return "命令返回非零状态".to_string();
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&stdout_text) {
        if let Some(errors) = value.get("errors").and_then(|v| v.as_array()) {
            let joined = errors
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            if !joined.is_empty() {
                return joined;
            }
        }
        if let Some(result) = value.get("result").and_then(|v| v.as_str()) {
            if !result.trim().is_empty() {
                return result.trim().to_string();
            }
        }
        if let Some(subtype) = value.get("subtype").and_then(|v| v.as_str()) {
            return subtype.to_string();
        }
    }
    truncate_error_text(&stdout_text)
}

/// 截断过长 CLI 错误输出。
///
/// Business Logic（为什么需要这个函数）:
///     前端错误区只需要诊断摘要，不能被完整 stdout 撑爆。
///
/// Code Logic（这个函数做什么）:
///     保留前 500 个 Unicode scalar，超出追加 `...`。
fn truncate_error_text(text: &str) -> String {
    let mut chars = text.chars();
    let truncated = chars.by_ref().take(MAX_ERROR_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct SampleOutput {
        value: String,
    }

    #[test]
    fn builds_pure_headless_args_without_budget_limit() {
        let args = build_pure_headless_args("  opus  ", "{}");
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-format", "json"]));
        assert!(args.windows(2).any(|pair| pair == ["--json-schema", "{}"]));
        assert!(args.windows(2).any(|pair| pair == ["--tools", ""]));
        assert!(args.windows(2).any(|pair| pair == ["--model", "opus"]));
        assert!(args.iter().any(|arg| arg == "--bare"));
        assert!(args.iter().any(|arg| arg == "-p"));
        assert!(args.iter().any(|arg| arg == "--no-session-persistence"));
        assert!(!args.iter().any(|arg| arg == "--max-budget-usd"));
    }

    #[test]
    fn normalizes_empty_cli_and_model_defaults() {
        assert_eq!(normalize_cli_path("  "), "claude");
        assert_eq!(normalize_cli_path("  /opt/claude  "), "/opt/claude");
        assert_eq!(normalize_model("  "), "sonnet");
        assert_eq!(normalize_model("  haiku  "), "haiku");
    }

    #[test]
    fn parses_direct_and_wrapped_outputs() {
        let direct: SampleOutput =
            parse_structured_output(r#"{"value":"direct"}"#).expect("direct");
        let structured: SampleOutput =
            parse_structured_output(r#"{"structured_output":{"value":"wrapped"}}"#)
                .expect("structured_output");
        let object: SampleOutput =
            parse_structured_output(r#"{"result":{"value":"object"}}"#).expect("result object");
        let string: SampleOutput =
            parse_structured_output(r#"{"result":"{\"value\":\"string\"}"}"#)
                .expect("result string");

        assert_eq!(direct.value, "direct");
        assert_eq!(structured.value, "wrapped");
        assert_eq!(object.value, "object");
        assert_eq!(string.value, "string");
    }

    #[test]
    fn extracts_failure_details_and_truncates_long_text() {
        assert_eq!(failure_detail(b" stderr says no \n", b""), "stderr says no");
        assert_eq!(
            failure_detail(&[], br#"{"errors":["first","second"]}"#),
            "first; second"
        );
        assert_eq!(
            failure_detail(&[], br#"{"result":"model refused"}"#),
            "model refused"
        );
        assert_eq!(
            failure_detail(&[], br#"{"subtype":"error_max_budget_usd"}"#),
            "error_max_budget_usd"
        );

        let long = "中".repeat(520);
        let detail = failure_detail(&[], long.as_bytes());
        assert_eq!(detail.chars().count(), 503);
        assert!(detail.ends_with("..."));
    }
}
