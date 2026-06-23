//! commands/prompt_optimizer.rs — Prompt 优化命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户需要把原始编程任务 prompt 优化成适合 Claude Code 执行的中英文版本，
//!     但不需要保存历史、入库、跨设备同步或缓存。
//!
//! Code Logic（这个模块做什么）:
//!     校验输入长度，复用 GitHub Trending 设置中的 Claude CLI 路径和模型，调用共享
//!     `claude_cli` pure/headless helper，并返回 camelCase DTO。

use crate::claude_cli;
use crate::error::AppError;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::State;

const MAX_PROMPT_CHARS: usize = 20_000;
const PROMPT_OPTIMIZE_TIMEOUT_SECS: u64 = 180;

/// Prompt 优化响应 DTO（camelCase，对齐前端类型）。
///
/// Business Logic（为什么需要这个结构）:
///     前端 Prompt 优化页需要同时展示中文优化版和等价英文优化版，且不保存历史。
///
/// Code Logic（这个结构做什么）:
///     serde 使用 camelCase 暴露 `optimizedZh` / `optimizedEn`，Rust 内部保持 snake_case。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptOptimizeResponseDto {
    pub optimized_zh: String,
    pub optimized_en: String,
}

/// 调用 Claude Code CLI 优化用户输入的编程任务 Prompt。
///
/// Business Logic（为什么需要这个命令）:
///     用户在本机把零散需求整理成适合 Claude Code 执行的结构化 prompt，并得到中文与英文两版。
///     结果只在当前页面展示，不入库、不缓存、不跨设备同步。
///
/// Code Logic（这个命令做什么）:
///     校验输入长度；读取 GitHub Trending 的 Claude CLI 路径和模型；构造 schema 与任务指令；
///     通过共享 `claude_cli::run_structured_json` 执行 pure/headless CLI 调用。
#[tauri::command]
pub async fn optimize_prompt(
    state: State<'_, AppState>,
    prompt: String,
) -> Result<PromptOptimizeResponseDto, AppError> {
    validate_prompt_input(&prompt)?;
    let (cli_path, model) = {
        let cfg = state.config.read().unwrap();
        (
            cfg.github_trending.claude_cli_path.clone(),
            cfg.github_trending.claude_model.clone(),
        )
    };
    let schema = prompt_optimize_schema();
    let instruction = build_optimize_instruction(&prompt);

    claude_cli::run_structured_json::<PromptOptimizeResponseDto>(
        &cli_path,
        &model,
        &schema.to_string(),
        &instruction,
        PROMPT_OPTIMIZE_TIMEOUT_SECS,
        "优化 Prompt",
    )
    .await
}

/// 校验原始 Prompt 输入。
///
/// Business Logic（为什么需要这个函数）:
///     空输入无法优化；过长输入会造成本地 CLI 调用耗时和上下文成本不可控，需要提前拦截。
///
/// Code Logic（这个函数做什么）:
///     trim 后为空返回业务错误；按 Unicode scalar 计数，超过 20,000 字符返回业务错误。
fn validate_prompt_input(prompt: &str) -> Result<(), AppError> {
    if prompt.trim().is_empty() {
        return Err(AppError::generic("Prompt 不能为空"));
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(AppError::generic("Prompt 不能超过 20,000 字符"));
    }
    Ok(())
}

/// 构造 Prompt 优化结构化输出 schema。
///
/// Business Logic（为什么需要这个函数）:
///     前端只需要两个只读结果框，必须强制 Claude CLI 返回稳定的中英文字段。
///
/// Code Logic（这个函数做什么）:
///     返回 JSON Schema，要求 `optimizedZh` 与 `optimizedEn` 都存在且禁止额外字段。
fn prompt_optimize_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["optimizedZh", "optimizedEn"],
        "properties": {
            "optimizedZh": { "type": "string" },
            "optimizedEn": { "type": "string" }
        }
    })
}

/// 构造发给 Claude CLI 的优化指令。
///
/// Business Logic（为什么需要这个函数）:
///     优化目标固定面向 Claude Code 编程任务，要求保留原意并显式标出缺失信息，不能编造外部事实。
///
/// Code Logic（这个函数做什么）:
///     把原始 prompt 作为 fenced code block 嵌入系统化指令，要求输出目标、上下文、约束、验收标准。
fn build_optimize_instruction(prompt: &str) -> String {
    format!(
        "You optimize user prompts for Claude Code programming tasks.\n\
         Return only data matching the JSON schema.\n\
         Requirements:\n\
         - Preserve the user's intent and do not invent external facts.\n\
         - optimizedZh must be a clear Simplified Chinese prompt.\n\
         - optimizedEn must be an equivalent English prompt.\n\
         - Both versions must include: goal, context, constraints, and acceptance criteria.\n\
         - If the original prompt lacks needed details, keep explicit placeholders or questions to fill in; do not fabricate them.\n\
         - Keep the prompt actionable for a coding agent.\n\n\
         Original prompt:\n```text\n{}\n```",
        prompt
    )
}

/// 解析 Prompt 优化输出。
///
/// Business Logic（为什么需要这个函数）:
///     单测锁定命令输出契约，确保直接 JSON 与 Claude CLI wrapper 都能解析成前端 DTO。
///
/// Code Logic（这个函数做什么）:
///     委托共享 `claude_cli::parse_structured_output`，返回 `PromptOptimizeResponseDto`。
fn parse_prompt_optimize_output(stdout: &str) -> Result<PromptOptimizeResponseDto, AppError> {
    claude_cli::parse_structured_output::<PromptOptimizeResponseDto>(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_prompt_length_and_empty_input() {
        assert!(validate_prompt_input(" \n\t ").is_err());
        assert!(validate_prompt_input(&"a".repeat(20_001)).is_err());
        assert!(validate_prompt_input(&"a".repeat(20_000)).is_ok());
    }

    #[test]
    fn parses_direct_and_wrapped_prompt_optimizer_output() {
        let direct = parse_prompt_optimize_output(
            r#"{"optimizedZh":"中文优化","optimizedEn":"English optimized"}"#,
        )
        .expect("direct");
        let wrapped = parse_prompt_optimize_output(
            r#"{"structured_output":{"optimizedZh":"结构化中文","optimizedEn":"Structured English"}}"#,
        )
        .expect("wrapped");

        assert_eq!(direct.optimized_zh, "中文优化");
        assert_eq!(direct.optimized_en, "English optimized");
        assert_eq!(wrapped.optimized_zh, "结构化中文");
        assert_eq!(wrapped.optimized_en, "Structured English");
    }

    #[test]
    fn schema_requires_both_outputs() {
        let schema = prompt_optimize_schema();
        assert_eq!(schema["required"][0], "optimizedZh");
        assert_eq!(schema["required"][1], "optimizedEn");
        assert_eq!(schema["additionalProperties"], false);
    }
}
