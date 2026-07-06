//! claude_cli.rs — Claude Code CLI headless 调用共享 helper。
//!
//! Business Logic（为什么需要这个模块）:
//!     GitHub Trending 解说和 Prompt 优化都需要调用本机 Claude Code CLI 并解析结构化 JSON；
//!     Workbench Prompt 优化还需要可选项目上下文。共享参数、执行、解析和错误提取逻辑，
//!     避免不同功能出现不一致的 CLI 行为。
//!
//! Code Logic（这个模块做什么）:
//!     提供 pure 与项目上下文 headless 参数构造、路径/模型归一化、结构化输出解析、
//!     非零退出错误摘要和带 stdin/timeout 的 `Command` 执行入口。

use crate::error::AppError;
use serde::de::DeserializeOwned;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const DEFAULT_CLAUDE_CLI: &str = "claude";
const DEFAULT_CLAUDE_MODEL: &str = "sonnet";
const MAX_ERROR_CHARS: usize = 500;
const CLI_VERSION_TIMEOUT_SECS: u64 = 10;

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

/// Business Logic（为什么需要这个函数）:
///     多个功能（GitHub Trending 解说、Workbench session resume）都需要先确认本机 Claude CLI 可用，
///     避免功能启动后才发现 CLI 缺失导致用户体验中断。
///
/// Code Logic（这个函数做什么）:
///     接收 Claude CLI 路径（空则用 "claude"），执行 `<cli> --version`，
///     成功（exit 0）返回 `Ok(version)`（version 为 stdout trim 后的版本字符串，调用方按需丢弃即可）；
///     失败（命令不存在/非零退出）返回 `Err(中文错误描述)`；
///     命令成功但 stdout 为空也视为异常返回 Err。不读写任何配置，纯检测函数。
pub(crate) async fn check_claude_cli_available(cli_path: &str) -> Result<String, String> {
    let cli = normalize_cli_path(cli_path);
    let mut cmd = Command::new(&cli);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = match tokio::time::timeout(
        Duration::from_secs(CLI_VERSION_TIMEOUT_SECS),
        cmd.output(),
    )
    .await
    {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Err(if e.kind() == std::io::ErrorKind::NotFound {
                "未找到 Claude CLI，请确认已安装并配置 PATH".to_string()
            } else {
                format!("启动 Claude CLI 失败: {e}")
            });
        }
        Err(_) => {
            return Err(format!(
                "Claude CLI 检测超时（{} 秒）",
                CLI_VERSION_TIMEOUT_SECS
            ));
        }
    };

    if output.status.success() {
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if version.is_empty() {
            return Err("Claude CLI 未返回版本信息".to_string());
        }
        Ok(version)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            "命令返回非零状态".to_string()
        } else {
            stderr
        };
        Err(format!("Claude CLI 执行失败: {detail}"))
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
    let mut args = vec!["--bare".to_string()];
    args.extend(build_project_headless_args(model, schema));
    args
}

/// 构造 Claude Code CLI 项目上下文 headless 结构化输出参数。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 内嵌 Prompt 优化需要让 Claude Code 在项目根目录运行，从而按原生规则发现
///     项目 CLAUDE.md；此时不能启用 `--bare`，否则 CLI 会跳过 CLAUDE.md auto-discovery。
///
/// Code Logic（这个函数做什么）:
///     返回 non-interactive/json-schema 参数列表，保留无会话持久化和禁用工具，但不追加 `--bare`。
pub(crate) fn build_project_headless_args(model: &str, schema: &str) -> Vec<String> {
    vec![
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

/// 构造 Claude Code CLI 流式纯文本输出参数。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench Prompt 小组件需要把优化后的 Prompt 边生成边写入终端，不能等待完整 JSON 返回。
///
/// Code Logic（这个函数做什么）:
///     返回 print + stream-json + verbose + partial message 参数；项目上下文模式不加 `--bare`，纯模式才加。
pub(crate) fn build_streaming_text_args(model: &str, use_project_context: bool) -> Vec<String> {
    let mut args = Vec::new();
    if !use_project_context {
        args.push("--bare".to_string());
    }
    args.extend([
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--include-partial-messages".to_string(),
        "--no-session-persistence".to_string(),
        "--tools".to_string(),
        "".to_string(),
        "--model".to_string(),
        normalize_model(model),
    ]);
    args
}

/// Claude CLI stream-json 文本增量解析状态。
///
/// Business Logic（为什么需要这个结构）:
///     `--include-partial-messages` 可能输出累计文本快照，也可能输出独立文本块；写入终端时不能重复内容。
///
/// Code Logic（这个结构做什么）:
///     保存已写入的 assistant 文本；优先解析 stream_event text_delta 实时增量，最终 assistant 快照只用于兜底。
#[derive(Default)]
pub(crate) struct StreamingTextState {
    written_text: String,
}

impl StreamingTextState {
    /// Business Logic（为什么需要这个函数）:
    ///     Workbench 流式优化只应把模型生成的 Prompt 文本写入终端，忽略 system/result/thinking 等元事件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析一行 stream-json；stream_event text_delta 作为独立增量立即返回，assistant 完整快照只返回未写过的后缀。
    pub(crate) fn chunk_from_stream_json_line(
        &mut self,
        line: &str,
    ) -> Result<Option<String>, AppError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)?;
        if let Some(delta) = text_delta_from_stream_event(&value) {
            if delta.is_empty() {
                return Ok(None);
            }
            self.written_text.push_str(delta);
            return Ok(Some(delta.to_string()));
        }
        if value.get("type").and_then(|item| item.as_str()) != Some("assistant") {
            return Ok(None);
        }
        let Some(text) = assistant_text_from_stream_value(&value) else {
            return Ok(None);
        };
        if text.is_empty() {
            return Ok(None);
        }
        if let Some(delta) = text.strip_prefix(&self.written_text) {
            let delta = delta.to_string();
            self.written_text = text;
            return Ok((!delta.is_empty()).then_some(delta));
        }
        self.written_text.push_str(&text);
        Ok(Some(text))
    }
}

/// 从 Claude CLI stream-json 增量事件中提取文本 delta。
///
/// Business Logic（为什么需要这个函数）:
///     Claude CLI 的真实流式文本不是顶层 assistant 事件，而是 stream_event.content_block_delta.text_delta。
///
/// Code Logic（这个函数做什么）:
///     仅提取 text_delta.text，明确忽略 thinking_delta、signature_delta、message_delta 等非可见文本事件。
fn text_delta_from_stream_event(value: &serde_json::Value) -> Option<&str> {
    if value.get("type").and_then(|item| item.as_str()) != Some("stream_event") {
        return None;
    }
    let event = value.get("event")?;
    if event.get("type").and_then(|item| item.as_str()) != Some("content_block_delta") {
        return None;
    }
    let delta = event.get("delta")?;
    if delta.get("type").and_then(|item| item.as_str()) != Some("text_delta") {
        return None;
    }
    delta.get("text").and_then(|item| item.as_str())
}

/// 从 Claude CLI stream-json assistant 事件中提取文本。
///
/// Business Logic（为什么需要这个函数）:
///     stream-json 事件包含多种元数据，Workbench 只需要 assistant 文本块。
///
/// Code Logic（这个函数做什么）:
///     遍历 message.content 数组，把 `{type:"text", text:"..."}` 块拼接为字符串。
fn assistant_text_from_stream_value(value: &serde_json::Value) -> Option<String> {
    let content = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_array())?;
    let text = content
        .iter()
        .filter(|item| item.get("type").and_then(|kind| kind.as_str()) == Some("text"))
        .filter_map(|item| item.get("text").and_then(|text| text.as_str()))
        .collect::<Vec<_>>()
        .join("");
    Some(text)
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
    run_structured_json_with_cwd(
        cli_path,
        model,
        schema,
        prompt,
        None,
        timeout_secs,
        task_label,
    )
    .await
}

/// 在可选工作目录中执行 Claude CLI 并解析结构化 JSON 输出。
///
/// Business Logic（为什么需要这个函数）:
///     默认 Prompt 优化和 GitHub 解说需要隔离项目上下文；Workbench Prompt 优化则需要在当前
///     项目根目录运行，让 Claude Code 原生加载项目 CLAUDE.md。
///
/// Code Logic（这个函数做什么）:
///     working_directory 为空时使用 pure/bare 参数；非空时设置 Command.current_dir 并使用
///     不含 `--bare` 的项目上下文参数，其余 stdin/stdout/stderr/timeout/解析流程保持一致。
pub(crate) async fn run_structured_json_with_cwd<T>(
    cli_path: &str,
    model: &str,
    schema: &str,
    prompt: &str,
    working_directory: Option<&Path>,
    timeout_secs: u64,
    task_label: &str,
) -> Result<T, AppError>
where
    T: DeserializeOwned,
{
    let cli = normalize_cli_path(cli_path);
    let mut cmd = Command::new(cli);
    let args = if working_directory.is_some() {
        build_project_headless_args(model, schema)
    } else {
        build_pure_headless_args(model, schema)
    };
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(directory) = working_directory {
        cmd.current_dir(directory);
    }

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

/// 在可选工作目录中执行 Claude CLI 并流式返回 assistant 文本。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench Prompt 小组件希望优化结果生成时就进入当前终端，用户无需等待完整 JSON 后再一次性填入。
///
/// Code Logic（这个函数做什么）:
///     使用 Claude CLI `stream-json` 输出格式，逐行解析 assistant 文本增量并调用 on_chunk；
///     working_directory 存在时不加 `--bare`，从而允许 Claude Code 读取项目 CLAUDE.md 上下文。
pub(crate) async fn run_streaming_text_with_cwd<F>(
    cli_path: &str,
    model: &str,
    prompt: &str,
    working_directory: Option<&Path>,
    timeout_secs: u64,
    task_label: &str,
    mut on_chunk: F,
) -> Result<(), AppError>
where
    F: FnMut(&str) -> Result<(), AppError> + Send,
{
    let cli = normalize_cli_path(cli_path);
    let mut cmd = Command::new(cli);
    cmd.args(build_streaming_text_args(
        model,
        working_directory.is_some(),
    ))
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    if let Some(directory) = working_directory {
        cmd.current_dir(directory);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::generic(format!("启动 Claude CLI 失败: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| AppError::generic(format!("写入 Claude CLI prompt 失败: {e}")))?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::generic("Claude CLI stdout 不可用"))?;
    let stderr_task = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes).await;
            bytes
        })
    });

    let stream_future = async {
        let mut reader = BufReader::new(stdout).lines();
        let mut state = StreamingTextState::default();
        while let Some(line) = reader
            .next_line()
            .await
            .map_err(|e| AppError::generic(format!("读取 Claude CLI 流式输出失败: {e}")))?
        {
            if let Some(chunk) = state.chunk_from_stream_json_line(&line)? {
                on_chunk(&chunk)?;
            }
        }
        child
            .wait()
            .await
            .map_err(|e| AppError::generic(format!("等待 Claude CLI 输出失败: {e}")))
    };

    let status = match tokio::time::timeout(Duration::from_secs(timeout_secs), stream_future).await
    {
        Ok(result) => result?,
        Err(_) => {
            return Err(AppError::generic(format!(
                "Claude CLI {task_label}超时（{timeout_secs} 秒）"
            )))
        }
    };
    let stderr = match stderr_task {
        Some(task) => task.await.unwrap_or_default(),
        None => Vec::new(),
    };

    if !status.success() {
        return Err(AppError::generic(format!(
            "Claude CLI {task_label}失败: {}",
            failure_detail(&stderr, &[])
        )));
    }

    Ok(())
}

/// 解析 Claude CLI 结构化输出。
///
/// Business Logic（为什么需要这个函数）:
///     不同 Claude CLI 版本可能返回直接 JSON、`structured_output` 或 `result` 包装。
///
/// Code Logic（这个函数做什么）:
///     先剥离整段 fenced JSON，再解析 stdout 为 JSON Value；随后依次尝试直接反序列化、structured_output、
///     result object、result string（result string 也允许 fenced JSON）。
pub(crate) fn parse_structured_output<T>(stdout: &str) -> Result<T, AppError>
where
    T: DeserializeOwned,
{
    let stripped_stdout = strip_markdown_fenced_json(stdout);
    let value: serde_json::Value = serde_json::from_str(&stripped_stdout)?;
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
            let stripped_result = strip_markdown_fenced_json(text);
            return Ok(serde_json::from_str::<T>(&stripped_result)?);
        }
        return Err(AppError::generic("Claude CLI 输出 result 不是可解析 JSON"));
    }
    Err(AppError::generic(
        "Claude CLI 输出缺少结构化 JSON/structured_output/result 字段",
    ))
}

/// Business Logic（为什么需要这个函数）:
///     Claude CLI 或模型偶尔把结构化 JSON 放进 markdown fenced code block；结构化解析不应因此误判为失败。
///
/// Code Logic（这个函数做什么）:
///     如果整段文本是 fenced code block，则提取 fence 内容；否则返回 trim 后文本。
fn strip_markdown_fenced_json(value: &str) -> String {
    let trimmed = value.trim();
    let lines = trimmed.lines().collect::<Vec<_>>();
    if lines.len() >= 2
        && lines
            .first()
            .is_some_and(|line| line.trim_start().starts_with("```"))
    {
        if let Some(end) = lines
            .iter()
            .rposition(|line| line.trim_start().starts_with("```"))
            .filter(|index| *index > 0)
        {
            return lines[1..end].join("\n").trim().to_string();
        }
    }
    trimmed.to_string()
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
    fn builds_project_headless_args_without_bare_mode() {
        let args = build_project_headless_args("  sonnet  ", "{}");
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-format", "json"]));
        assert!(args.windows(2).any(|pair| pair == ["--json-schema", "{}"]));
        assert!(args.windows(2).any(|pair| pair == ["--tools", ""]));
        assert!(args.windows(2).any(|pair| pair == ["--model", "sonnet"]));
        assert!(args.iter().any(|arg| arg == "-p"));
        assert!(args.iter().any(|arg| arg == "--no-session-persistence"));
        assert!(!args.iter().any(|arg| arg == "--bare"));
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
        let fenced_direct: SampleOutput =
            parse_structured_output("```json\n{\"value\":\"fenced\"}\n```").expect("fenced");
        let fenced_result: SampleOutput = parse_structured_output(
            "{\"result\":\"```json\\n{\\\"value\\\":\\\"fenced-result\\\"}\\n```\"}",
        )
        .expect("fenced result");

        assert_eq!(direct.value, "direct");
        assert_eq!(structured.value, "wrapped");
        assert_eq!(object.value, "object");
        assert_eq!(string.value, "string");
        assert_eq!(fenced_direct.value, "fenced");
        assert_eq!(fenced_result.value, "fenced-result");
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

    #[test]
    fn streaming_text_state_emits_only_new_assistant_text() {
        let mut state = StreamingTextState::default();
        let first = state
            .chunk_from_stream_json_line(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"目标"}]}}"#,
            )
            .expect("first line");
        let second = state
            .chunk_from_stream_json_line(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"目标和上下文"}]}}"#,
            )
            .expect("second line");
        let result = state
            .chunk_from_stream_json_line(r#"{"type":"result","result":"目标和上下文"}"#)
            .expect("result line");

        assert_eq!(first.as_deref(), Some("目标"));
        assert_eq!(second.as_deref(), Some("和上下文"));
        assert_eq!(result, None);
    }

    #[test]
    fn streaming_text_state_accepts_independent_text_chunks() {
        let mut state = StreamingTextState::default();
        let first = state
            .chunk_from_stream_json_line(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"目标"}]}}"#,
            )
            .expect("first line");
        let second = state
            .chunk_from_stream_json_line(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"约束"}]}}"#,
            )
            .expect("second line");

        assert_eq!(first.as_deref(), Some("目标"));
        assert_eq!(second.as_deref(), Some("约束"));
    }

    #[test]
    fn streaming_text_state_emits_stream_event_text_delta_immediately() {
        let mut state = StreamingTextState::default();
        let first = state
            .chunk_from_stream_json_line(
                r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"目标"}}}"#,
            )
            .expect("first delta");
        let second = state
            .chunk_from_stream_json_line(
                r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"和约束"}}}"#,
            )
            .expect("second delta");
        let final_snapshot = state
            .chunk_from_stream_json_line(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"目标和约束"}]}}"#,
            )
            .expect("final assistant snapshot");

        assert_eq!(first.as_deref(), Some("目标"));
        assert_eq!(second.as_deref(), Some("和约束"));
        assert_eq!(final_snapshot, None);
    }

    #[test]
    fn streaming_text_state_ignores_thinking_stream_delta() {
        let mut state = StreamingTextState::default();
        let thinking = state
            .chunk_from_stream_json_line(
                r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"internal"}}}"#,
            )
            .expect("thinking delta");

        assert_eq!(thinking, None);
    }

    #[test]
    fn streaming_text_args_use_project_context_without_json_schema() {
        let args = build_streaming_text_args("sonnet", true);

        assert!(!args.iter().any(|arg| arg == "--bare"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-format", "stream-json"]));
        assert!(args.iter().any(|arg| arg == "--verbose"));
        assert!(args.iter().any(|arg| arg == "--include-partial-messages"));
        assert!(!args.iter().any(|arg| arg == "--json-schema"));
    }

    /// Business Logic（为什么需要这个 helper）:
    ///     `check_claude_cli_available` 的单测需要模拟真实 Claude CLI 的成功/非零退出/空输出等行为，
    ///     而系统自带的 echo/sh 无法控制退出码与 stdout，因此每个用例都用临时 shell 脚本模拟一个 CLI。
    ///
    /// Code Logic（这个 helper 做什么）:
    ///     在系统临时目录下写入一段 `#!/bin/sh` 脚本并 chmod 0755，返回脚本绝对路径字符串；
    ///     测试结束时通过返回的 PathBuf 由调用方删除。
    fn make_temp_cli_script(body: &str) -> std::io::Result<String> {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "cc-partner-test-cli-{}-{}.sh",
            std::process::id(),
            uuid_like_suffix()
        ));
        std::fs::write(&path, format!("#!/bin/sh\n{body}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms)?;
        }
        Ok(path.to_string_lossy().into_owned())
    }

    /// Business Logic（为什么需要这个 helper）:
    ///     临时脚本路径需要唯一后缀避免并发测试或残留文件冲突。
    ///
    /// Code Logic（这个 helper 做什么）:
    ///     返回一个由当前系统纳秒时间拼成的字符串，用作文件名后缀，足以保证单次测试运行内唯一。
    fn uuid_like_suffix() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().to_string())
            .unwrap_or_else(|_| "fixed".to_string())
    }

    /// Business Logic（为什么需要这个测试）:
    ///     CLI 探测的成功路径是最常被触发的分支，必须保证 `<cli> --version` 正常退出且 stdout 非空时返回 Ok。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建一个打印固定版本字符串并 exit 0 的临时脚本，断言 `check_claude_cli_available` 返回
    ///     `Ok("claude 1.2.3")`，测试结束删除临时脚本。
    #[tokio::test]
    async fn check_claude_cli_available_returns_version_on_success() {
        let script = make_temp_cli_script("echo \"claude 1.2.3\"").expect("write script");
        let result = check_claude_cli_available(&script).await;
        let _ = std::fs::remove_file(&script);
        assert_eq!(result.expect("available"), "claude 1.2.3");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     CLI 不存在是最常见的安装问题，必须给出友好提示而非原始 os 错误。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用一个确定不存在的路径 `/no/such/cli`，断言返回 Err 且错误信息包含"未找到 Claude CLI"。
    #[tokio::test]
    async fn check_claude_cli_available_reports_not_found() {
        let result = check_claude_cli_available("/no/such/cli").await;
        let err = result.expect_err("should error");
        assert!(err.contains("未找到 Claude CLI"), "unexpected error: {err}");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     当 CLI 路径实际是一个目录（权限/路径配错）时，os 返回的不是 NotFound，必须保留原始错误以便诊断，
    ///     不能误报为"未安装"。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 `std::env::temp_dir()`（一个目录）作为 CLI 路径，断言返回 Err 且错误信息包含"启动 Claude CLI 失败"。
    #[tokio::test]
    async fn check_claude_cli_available_reports_launch_failure_for_directory() {
        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        let result = check_claude_cli_available(&dir).await;
        let err = result.expect_err("should error");
        assert!(err.contains("启动 Claude CLI 失败"), "unexpected error: {err}");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     CLI 存在但执行失败（如内部错误退出非零码）时，必须把非零退出归一化为"执行失败"错误。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建一个 `exit 1` 的临时脚本，断言返回 Err 且错误信息包含"Claude CLI 执行失败"，
    ///     测试结束删除临时脚本。
    #[tokio::test]
    async fn check_claude_cli_available_reports_nonzero_exit() {
        let script = make_temp_cli_script("exit 1").expect("write script");
        let result = check_claude_cli_available(&script).await;
        let _ = std::fs::remove_file(&script);
        let err = result.expect_err("should error");
        assert!(err.contains("Claude CLI 执行失败"), "unexpected error: {err}");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     CLI 正常退出但 stdout 为空属于异常（真实 `claude --version` 必输出版本号），
    ///     必须拦截避免空字符串透传给前端。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建一个不输出任何内容直接 `exit 0` 的临时脚本，断言返回 Err 且错误信息包含"未返回版本信息"，
    ///     测试结束删除临时脚本。
    #[tokio::test]
    async fn check_claude_cli_available_reports_empty_stdout() {
        let script = make_temp_cli_script("exit 0").expect("write script");
        let result = check_claude_cli_available(&script).await;
        let _ = std::fs::remove_file(&script);
        let err = result.expect_err("should error");
        assert!(err.contains("未返回版本信息"), "unexpected error: {err}");
    }
}
