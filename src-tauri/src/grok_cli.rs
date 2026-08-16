//! grok_cli.rs — Grok Build CLI headless 调用 helper。
//!
//! Business Logic（为什么需要这个模块）:
//!     Prompt 优化可选 Grok Build 作为 HeadlessCompletion 后端；调用约定与 Claude 不同
//!     （`-p` 传 prompt、`--output-format json`），需要独立的有界 timeout 与 JSON 解析。
//!
//! Code Logic（这个模块做什么）:
//!     写死 `grok -p <prompt> --output-format json --json-schema <schema>`；
//!     在可选项目根 cwd 下执行，超时后杀进程，并把 stdout 解析成与 Claude 相同的结构化 DTO。

use crate::claude_cli;
use crate::error::AppError;
use serde::de::DeserializeOwned;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const DEFAULT_GROK_CLI: &str = "grok";
const MAX_ERROR_CHARS: usize = 500;

/// 构造 Grok headless JSON 参数。
///
/// Business Logic（为什么需要这个函数）:
///     Prompt 优化必须稳定产出与 Claude 相同的 schema 字段；Grok 用 `-p` 吃完整指令，
///     输出格式写死 json，避免 streaming-json 与 json 混用。
///
/// Code Logic（这个函数做什么）:
///     返回 `-p <prompt> --output-format json --json-schema <schema>`；
///     `--json-schema` 约束模型输出，`--output-format json` 按 spec 显式写死。
pub(crate) fn build_headless_json_args(prompt: &str, schema: &str) -> Vec<String> {
    vec![
        "-p".to_string(),
        prompt.to_string(),
        "--output-format".to_string(),
        "json".to_string(),
        "--json-schema".to_string(),
        schema.to_string(),
        "--max-turns".to_string(),
        "1".to_string(),
        "--no-memory".to_string(),
        "--permission-mode".to_string(),
        "dontAsk".to_string(),
    ]
}

/// 解析本机 Grok CLI 可执行路径。
///
/// Business Logic（为什么需要这个函数）:
///     打包 GUI 的 PATH 通常不含 `~/.grok/bin`；默认命令名 `grok` 需要按常见安装目录查找。
///
/// Code Logic（这个函数做什么）:
///     先查 `~/.grok/bin/grok`，未命中则复用 Claude helper 的常见目录 / PATH 搜索。
pub(crate) fn resolve_grok_cli_path() -> String {
    resolve_grok_cli_path_with_search(
        dirs::home_dir().as_deref(),
        std::env::var_os("PATH").as_deref(),
    )
}

/// 可测试入口：在给定 home / PATH 下解析 Grok CLI。
///
/// Code Logic（这个函数做什么）:
///     与 `resolve_grok_cli_path` 相同语义，搜索根由调用方注入。
pub(crate) fn resolve_grok_cli_path_with_search(
    home: Option<&Path>,
    path_env: Option<&std::ffi::OsStr>,
) -> String {
    if let Some(home) = home {
        let candidate = home.join(".grok").join("bin").join(DEFAULT_GROK_CLI);
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    claude_cli::resolve_cli_path_with_search(DEFAULT_GROK_CLI, home, path_env)
}

/// 在可选工作目录中执行 Grok CLI 并解析结构化 JSON。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench Prompt 优化需要在当前项目根运行，让 Grok 读到项目上下文；
///     普通优化页不绑项目，cwd 为空时在进程默认目录执行。
///
/// Code Logic（这个函数做什么）:
///     spawn `grok`，prompt 走 `-p` 参数（不写 stdin）；timeout 包裹 `wait_with_output`；
///     非零退出提取 stderr/stdout 摘要；成功则按直接 JSON / result 包装解析。
pub(crate) async fn run_structured_json_with_cwd<T>(
    prompt: &str,
    schema: &str,
    working_directory: Option<&Path>,
    timeout_secs: u64,
    task_label: &str,
) -> Result<T, AppError>
where
    T: DeserializeOwned,
{
    let cli = resolve_grok_cli_path();
    let mut cmd = Command::new(&cli);
    cmd.env("PATH", grok_command_path_env())
        .args(build_headless_json_args(prompt, schema))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(directory) = working_directory {
        cmd.current_dir(directory);
    }

    let child = cmd
        .spawn()
        .map_err(|e| AppError::generic(format!("启动 Grok CLI 失败: {e}")))?;

    let output =
        match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
            .await
        {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(AppError::generic(format!("等待 Grok CLI 输出失败: {e}"))),
            Err(_) => {
                return Err(AppError::generic(format!(
                    "Grok CLI {task_label}超时（{timeout_secs} 秒）"
                )))
            }
        };

    if !output.status.success() {
        return Err(AppError::generic(format!(
            "Grok CLI {task_label}失败: {}",
            failure_detail(&output.stderr, &output.stdout)
        )));
    }

    parse_structured_output(&String::from_utf8_lossy(&output.stdout))
}

/// 构造启动 Grok CLI 时应使用的 PATH（`~/.grok/bin` 优先）。
///
/// Business Logic（为什么需要这个函数）:
///     即使已解析到绝对路径，Grok 内部仍可能依赖同目录工具；GUI 稀疏 PATH 会导致二次失败。
///
/// Code Logic（这个函数做什么）:
///     将 `~/.grok/bin` 前置到 Claude helper 已增强的 PATH 前。
fn grok_command_path_env() -> std::ffi::OsString {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".grok").join("bin"));
    }
    for entry in std::env::split_paths(&claude_cli::cli_command_path_env()) {
        if !entry.as_os_str().is_empty() && !dirs.iter().any(|d| d == &entry) {
            dirs.push(entry);
        }
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| claude_cli::cli_command_path_env())
}

/// 解析 Grok CLI 结构化输出。
///
/// Business Logic（为什么需要这个函数）:
///     Grok `--output-format json` 可能返回 schema 对象本身，或包在 `result` / `structured_output` 里。
///
/// Code Logic（这个函数做什么）:
///     先剥离 fenced JSON，再依次尝试直接反序列化、structured_output、result object、result string。
pub(crate) fn parse_structured_output<T>(stdout: &str) -> Result<T, AppError>
where
    T: DeserializeOwned,
{
    let stripped_stdout = strip_markdown_fenced_json(stdout);
    let value: serde_json::Value = serde_json::from_str(&stripped_stdout)
        .map_err(|err| AppError::generic(format!("Grok CLI 输出不是合法 JSON: {err}")))?;
    if let Ok(parsed) = serde_json::from_value::<T>(value.clone()) {
        return Ok(parsed);
    }
    if let Some(structured_output) = value.get("structured_output") {
        return serde_json::from_value::<T>(structured_output.clone()).map_err(|err| {
            AppError::generic(format!("Grok CLI structured_output 无法解析: {err}"))
        });
    }
    if let Some(result) = value.get("result") {
        if result.is_object() {
            return serde_json::from_value::<T>(result.clone())
                .map_err(|err| AppError::generic(format!("Grok CLI result 无法解析: {err}")));
        }
        if let Some(text) = result.as_str() {
            let stripped_result = strip_markdown_fenced_json(text);
            return serde_json::from_str::<T>(&stripped_result).map_err(|err| {
                AppError::generic(format!("Grok CLI result 字符串不是合法 JSON: {err}"))
            });
        }
        return Err(AppError::generic("Grok CLI 输出 result 不是可解析 JSON"));
    }
    Err(AppError::generic(
        "Grok CLI 输出缺少结构化 JSON/structured_output/result 字段",
    ))
}

/// 如果整段文本是 fenced code block，则提取 fence 内容。
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

/// 从 Grok CLI 非零退出输出中提取用户可读错误。
fn failure_detail(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr_text = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr_text.is_empty() {
        return truncate_error_text(&stderr_text);
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
                return truncate_error_text(&joined);
            }
        }
        if let Some(result) = value.get("result").and_then(|v| v.as_str()) {
            if !result.trim().is_empty() {
                return truncate_error_text(result.trim());
            }
        }
    }
    truncate_error_text(&stdout_text)
}

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
    use std::path::PathBuf;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "camelCase")]
    struct SampleOptimizeOutput {
        optimized_zh: String,
        optimized_en: String,
    }

    #[test]
    fn builds_headless_json_args_with_output_format_json() {
        let args = build_headless_json_args("optimize this", r#"{"type":"object"}"#);
        assert!(args.windows(2).any(|pair| pair == ["-p", "optimize this"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-format", "json"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--json-schema", r#"{"type":"object"}"#]));
        assert!(!args.iter().any(|arg| arg.contains("streaming")));
    }

    #[test]
    fn parses_direct_json_fixture_stdout() {
        let parsed = parse_structured_output::<SampleOptimizeOutput>(
            r#"{"optimizedZh":"中文优化","optimizedEn":"English optimized"}"#,
        )
        .expect("direct json");
        assert_eq!(parsed.optimized_zh, "中文优化");
        assert_eq!(parsed.optimized_en, "English optimized");
    }

    #[test]
    fn parses_wrapped_result_object_fixture_stdout() {
        let parsed = parse_structured_output::<SampleOptimizeOutput>(
            r#"{"type":"result","result":{"optimizedZh":"结构化中文","optimizedEn":"Structured English"}}"#,
        )
        .expect("wrapped result");
        assert_eq!(parsed.optimized_zh, "结构化中文");
        assert_eq!(parsed.optimized_en, "Structured English");
    }

    #[test]
    fn parses_fenced_result_string_fixture_stdout() {
        let parsed = parse_structured_output::<SampleOptimizeOutput>(
            "{\n  \"result\": \"```json\\n{\\\"optimizedZh\\\":\\\"围栏中文\\\",\\\"optimizedEn\\\":\\\"Fenced English\\\"}\\n```\"\n}",
        )
        .expect("fenced result string");
        assert_eq!(parsed.optimized_zh, "围栏中文");
        assert_eq!(parsed.optimized_en, "Fenced English");
    }

    #[test]
    fn rejects_non_json_stdout() {
        let err =
            parse_structured_output::<SampleOptimizeOutput>("not-json").expect_err("non-json");
        assert!(err.to_string().contains("Grok CLI"));
    }

    #[test]
    fn resolve_prefers_grok_home_bin() {
        let tmp = std::env::temp_dir().join(format!("cc-partner-grok-cli-{}", std::process::id()));
        let bin = tmp.join(".grok").join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        let exe = bin.join("grok");
        std::fs::write(&exe, b"").expect("touch grok");
        let resolved = resolve_grok_cli_path_with_search(Some(&tmp), None);
        assert_eq!(PathBuf::from(resolved), exe);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
