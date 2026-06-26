//! commands/prompt_optimizer.rs — Prompt 优化命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户需要把原始编程任务 prompt 优化成适合 Claude Code 执行的中英文版本，
//!     但不需要保存历史、入库、跨设备同步或缓存。
//!
//! Code Logic（这个模块做什么）:
//!     校验输入长度，复用 GitHub Trending 设置中的 Claude CLI 路径和模型，调用共享
//!     `claude_cli` headless helper，并返回 camelCase DTO；Workbench 可传项目目录加载 CLAUDE.md。

use crate::claude_cli;
use crate::commands::workbench::device_base_url;
use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::{
    remote_client::RemoteWorkbenchClient, remote_ids::parse_remote_entity_id,
    remote_protocol::RemotePromptOptimizerReq,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
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

/// 单语 Prompt 优化响应 DTO（仅供 Workbench 小组件内部映射）。
///
/// Business Logic（为什么需要这个结构）:
///     Workbench 小组件只需要按设置页语种生成一个 Prompt，避免 CLI 同时生成中英两版造成等待和噪音。
///
/// Code Logic（这个结构做什么）:
///     serde 使用 camelCase 暴露 `optimizedPrompt`，命令层再映射回前端既有 `PromptOptimizeResponseDto`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SinglePromptOptimizeResponseDto {
    optimized_prompt: String,
}

/// Workbench 小组件单语优化目标语种。
///
/// Business Logic（为什么需要这个枚举）:
///     设置页只允许用户选择中文或英文填入终端，后端单语 schema 需要一个受控目标语种。
///
/// Code Logic（这个枚举做什么）:
///     用 Zh / En 表达前端 `zh` / `en`，避免在 schema、指令和结果映射中散落字符串判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptOptimizeTargetLanguage {
    Zh,
    En,
}

impl PromptOptimizeTargetLanguage {
    /// Business Logic（为什么需要这个函数）:
    ///     设置页只允许 Workbench 小组件选择中文或英文填入，后端需要拒绝未知语言值。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将前端传入的 `zh` / `en` 解析为枚举；None 表示普通双语优化模式。
    fn parse(input: Option<String>) -> Result<Option<Self>, AppError> {
        let Some(raw) = input else {
            return Ok(None);
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        match trimmed {
            "zh" => Ok(Some(Self::Zh)),
            "en" => Ok(Some(Self::En)),
            _ => Err(AppError::generic("Prompt 优化目标语种仅支持 zh 或 en")),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Claude CLI 单语优化指令需要明确目标语言，避免生成双语结果。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回写入英文指令的语言描述。
    fn instruction_label(self) -> &'static str {
        match self {
            Self::Zh => "Simplified Chinese",
            Self::En => "English",
        }
    }
}

/// 调用 Claude Code CLI 优化用户输入的编程任务 Prompt。
///
/// Business Logic（为什么需要这个命令）:
///     用户在本机把零散需求整理成适合 Claude Code 执行的结构化 prompt；普通页面得到中英两版，
///     Workbench 小组件只得到设置页选择的单语版本。结果只在当前页面展示，不入库、不缓存、不跨设备同步。
///
/// Code Logic（这个命令做什么）:
///     校验输入长度；读取 GitHub Trending 的 Claude CLI 路径和模型；按 target_language 构造 schema 与任务指令；
///     未传工作目录时执行 pure/bare CLI 调用，传入工作目录时执行项目上下文 CLI 调用。
#[tauri::command]
pub async fn optimize_prompt(
    state: State<'_, AppState>,
    prompt: String,
    working_directory: Option<String>,
    target_language: Option<String>,
) -> Result<PromptOptimizeResponseDto, AppError> {
    validate_prompt_input(&prompt)?;
    let working_directory = resolve_working_directory(working_directory)?;
    let target_language = PromptOptimizeTargetLanguage::parse(target_language)?;
    let (cli_path, model) = {
        let cfg = state.config.read().unwrap();
        (
            cfg.github_trending.claude_cli_path.clone(),
            cfg.github_trending.claude_model.clone(),
        )
    };
    let schema = prompt_optimize_schema_for_target(target_language)?;
    let instruction = build_optimize_instruction_for_target(&prompt, target_language);

    if let Some(target_language) = target_language {
        let result = claude_cli::run_structured_json_with_cwd::<SinglePromptOptimizeResponseDto>(
            &cli_path,
            &model,
            &schema.to_string(),
            &instruction,
            working_directory.as_deref(),
            PROMPT_OPTIMIZE_TIMEOUT_SECS,
            "优化 Prompt",
        )
        .await?;
        return Ok(single_prompt_response_to_full(target_language, result));
    }

    claude_cli::run_structured_json_with_cwd::<PromptOptimizeResponseDto>(
        &cli_path,
        &model,
        &schema.to_string(),
        &instruction,
        working_directory.as_deref(),
        PROMPT_OPTIMIZE_TIMEOUT_SECS,
        "优化 Prompt",
    )
    .await
}

/// 流式优化 Prompt 并写入 Workbench 终端。
///
/// Business Logic（为什么需要这个命令）:
///     Workbench 快捷键小组件需要在当前 Claude Code/终端输入位置下方优化 prompt，并把生成内容边生成边填入当前终端，
///     让用户看到实时输出而不是等待完整结果。
///
/// Code Logic（这个命令做什么）:
///     校验 prompt 与目标语种，使用当前项目目录运行 Claude CLI stream-json 输出；每个 assistant 文本增量通过
///     WorkbenchSessionRegistry 写入指定 session，不返回优化文本给前端。
#[tauri::command]
pub async fn stream_optimize_prompt_to_workbench_session(
    state: State<'_, AppState>,
    prompt: String,
    working_directory: Option<String>,
    target_language: String,
    session_id: String,
) -> Result<Value, AppError> {
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        return remote_stream_optimize_prompt_to_workbench_session(
            &state,
            prompt,
            working_directory,
            target_language,
            session_id,
            parsed.device_id,
            parsed.inner_id,
        )
        .await;
    }

    local_stream_optimize_prompt_to_workbench_session(
        &state,
        prompt,
        working_directory,
        target_language,
        session_id,
    )
    .await
}

/// 在本机流式优化 Prompt 并写入 Workbench 终端。
///
/// Business Logic（为什么需要这个函数）:
///     本机终端和远端 HTTP 路由都需要复用同一套本地 Claude CLI 流式优化逻辑。
///
/// Code Logic（这个函数做什么）:
///     校验 prompt/session/目标语种，解析本机工作目录后运行 Claude CLI，并把文本增量写入本机 session。
pub(crate) async fn local_stream_optimize_prompt_to_workbench_session(
    state: &AppState,
    prompt: String,
    working_directory: Option<String>,
    target_language: String,
    session_id: String,
) -> Result<Value, AppError> {
    validate_prompt_input(&prompt)?;
    if session_id.trim().is_empty() {
        return Err(AppError::generic("工作台终端会话不能为空"));
    }
    let (target_language, _) = parse_required_target_language(target_language)?;
    let working_directory = resolve_working_directory(working_directory)?;
    let (cli_path, model) = {
        let cfg = state.config.read().unwrap();
        (
            cfg.github_trending.claude_cli_path.clone(),
            cfg.github_trending.claude_model.clone(),
        )
    };
    let instruction = build_streaming_optimize_instruction(&prompt, target_language);
    let sessions = state.workbench_sessions.clone();
    let write_session_id = session_id.clone();

    claude_cli::run_streaming_text_with_cwd(
        &cli_path,
        &model,
        &instruction,
        working_directory.as_deref(),
        PROMPT_OPTIMIZE_TIMEOUT_SECS,
        "流式优化 Prompt",
        move |chunk| sessions.write_input(&write_session_id, chunk),
    )
    .await?;

    Ok(json!({ "ok": true, "sessionId": session_id }))
}

/// 把 Workbench Prompt 优化转发到远端设备。
///
/// Business Logic（为什么需要这个函数）:
///     用户操作 remote shortcut 时，Prompt 优化应读取远端项目上下文并写入远端 terminal，而不是在本机执行。
///
/// Code Logic（这个函数做什么）:
///     校验 prompt、目标语种和远端工作目录；剥离 remote session 前缀后调用对端 HTTP 路由，返回本机 remote sessionId。
async fn remote_stream_optimize_prompt_to_workbench_session(
    state: &AppState,
    prompt: String,
    working_directory: Option<String>,
    target_language: String,
    local_session_id: String,
    device_id: String,
    remote_session_id: String,
) -> Result<Value, AppError> {
    validate_prompt_input(&prompt)?;
    let (_, target_language) = parse_required_target_language(target_language)?;
    let working_directory = remote_working_directory(working_directory)?;
    let base_url = device_base_url(state, &device_id)?;
    let client = RemoteWorkbenchClient::new();
    let _ = client
        .stream_prompt_optimizer_to_session(
            &base_url,
            RemotePromptOptimizerReq {
                prompt,
                working_directory,
                target_language,
                session_id: remote_session_id,
            },
        )
        .await?;

    Ok(json!({ "ok": true, "sessionId": local_session_id }))
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

/// 解析可选 Claude Code 工作目录。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench Prompt 优化需要在当前项目根目录执行 Claude Code，使其读取项目 CLAUDE.md；
///     普通 Prompt 优化页不绑定项目，因此不传目录时必须保留原 pure/headless 行为。
///
/// Code Logic（这个函数做什么）:
///     None 或空白字符串返回 None；非空路径要求存在且是目录，随后 canonicalize 成稳定绝对路径。
fn resolve_working_directory(input: Option<String>) -> Result<Option<PathBuf>, AppError> {
    let Some(raw) = input else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(trimmed);
    if !path.is_dir() {
        return Err(AppError::generic("Prompt 优化工作目录不存在或不是文件夹"));
    }
    path.canonicalize()
        .map(Some)
        .map_err(|error| AppError::generic(format!("解析 Prompt 优化工作目录失败: {error}")))
}

/// 解析 Workbench 流式 Prompt 优化必填目标语种。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 小组件必须明确写入中文或英文，远端转发还需要保留规范化后的语种字符串。
///
/// Code Logic（这个函数做什么）:
///     trim 输入并解析为目标语种枚举；空值或未知值返回业务错误，同时返回规范化 `zh` / `en` 字符串。
fn parse_required_target_language(
    input: String,
) -> Result<(PromptOptimizeTargetLanguage, String), AppError> {
    let canonical = input.trim().to_string();
    let target = PromptOptimizeTargetLanguage::parse(Some(canonical.clone()))?
        .ok_or_else(|| AppError::generic("Workbench Prompt 优化必须指定目标语种"))?;
    Ok((target, canonical))
}

/// 解析远端 Prompt 优化工作目录。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的工作目录是远端设备路径，本机不能提前 canonicalize，但必须确保传给对端的是非空远端路径。
///
/// Code Logic（这个函数做什么）:
///     None 或空白字符串返回业务错误；非空时只 trim 字符串并原样交给远端设备校验。
fn remote_working_directory(input: Option<String>) -> Result<String, AppError> {
    let Some(raw) = input else {
        return Err(AppError::generic("远端 Prompt 优化必须指定远端工作目录"));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::generic("远端 Prompt 优化必须指定远端工作目录"));
    }
    Ok(trimmed.to_string())
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

/// 按目标语种构造 Prompt 优化结构化输出 schema。
///
/// Business Logic（为什么需要这个函数）:
///     普通 Prompt 优化页仍需要中英双语结果；Workbench 小组件只需要一个设置语种的 Prompt。
///
/// Code Logic（这个函数做什么）:
///     target 为 None 时返回双语 schema；target 为 Some 时返回仅包含 `optimizedPrompt` 的单语 schema。
fn prompt_optimize_schema_for_target(
    target: Option<PromptOptimizeTargetLanguage>,
) -> Result<Value, AppError> {
    if target.is_none() {
        return Ok(prompt_optimize_schema());
    }
    Ok(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["optimizedPrompt"],
        "properties": {
            "optimizedPrompt": { "type": "string" }
        }
    }))
}

/// 把单语优化结果映射回前端既有 DTO。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 小组件只消费设置语种的结果，但前端 API 类型仍复用 `{optimizedZh, optimizedEn}`。
///
/// Code Logic（这个函数做什么）:
///     中文目标填充 optimized_zh；英文目标填充 optimized_en；另一个字段保持空字符串。
fn single_prompt_response_to_full(
    target: PromptOptimizeTargetLanguage,
    result: SinglePromptOptimizeResponseDto,
) -> PromptOptimizeResponseDto {
    match target {
        PromptOptimizeTargetLanguage::Zh => PromptOptimizeResponseDto {
            optimized_zh: result.optimized_prompt,
            optimized_en: String::new(),
        },
        PromptOptimizeTargetLanguage::En => PromptOptimizeResponseDto {
            optimized_zh: String::new(),
            optimized_en: result.optimized_prompt,
        },
    }
}

/// 构造发给 Claude CLI 的优化指令。
///
/// Business Logic（为什么需要这个函数）:
///     优化目标固定面向 Claude Code 编程任务，要求保留原意并用需求方视角直接表达，
///     不能把结果写成继续向用户追问意见的澄清问题。
///
/// Code Logic（这个函数做什么）:
///     把原始 prompt 作为 fenced code block 嵌入系统化指令，要求输出目标、上下文、约束、验收标准；
///     缺失信息只能写成待补充占位或执行假设，不能新增原始需求没有要求的文档/文件输出确认。
fn build_optimize_instruction(prompt: &str) -> String {
    format!(
        "You optimize user prompts for Claude Code programming tasks.\n\
         Return only data matching the JSON schema.\n\
         Requirements:\n\
         - Preserve the user's intent and do not invent external facts.\n\
         - optimizedZh must be a clear Simplified Chinese prompt.\n\
         - optimizedEn must be an equivalent English prompt.\n\
         - Both versions must include: goal, context, constraints, and acceptance criteria.\n\
         - Write both optimized prompts from the requester's perspective, as a direct prompt they can paste into Claude Code.\n\
         - Do not ask the requester clarifying questions or include confirmation requests.\n\
         - If details are missing, express them as bracketed placeholders or execution assumptions inside the prompt, not as questions to the requester.\n\
         - Do not add documentation, docs/, file-writing, persistence, or confirmation requirements unless the original prompt explicitly asks for those outputs.\n\
         - Keep the prompt actionable for a coding agent.\n\n\
         Original prompt:\n```text\n{}\n```",
        prompt
    )
}

/// 按目标语种构造发给 Claude CLI 的优化指令。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 小组件只需要设置页选择的一种语言，普通 Prompt 优化页仍保留双语输出。
///
/// Code Logic（这个函数做什么）:
///     target 为 None 时复用双语指令；target 为 Some 时构造只返回 `optimizedPrompt` 的单语指令。
fn build_optimize_instruction_for_target(
    prompt: &str,
    target: Option<PromptOptimizeTargetLanguage>,
) -> String {
    let Some(target) = target else {
        return build_optimize_instruction(prompt);
    };
    format!(
        "You optimize user prompts for Claude Code programming tasks.\n\
         Return only data matching the JSON schema.\n\
         Requirements:\n\
         - Preserve the user's intent and do not invent external facts.\n\
         - optimizedPrompt must be a clear {} prompt.\n\
         - optimizedPrompt must include: goal, context, constraints, and acceptance criteria.\n\
         - Write optimizedPrompt from the requester's perspective, as a direct prompt they can paste into Claude Code.\n\
         - Do not ask the requester clarifying questions or include confirmation requests.\n\
         - If details are missing, express them as bracketed placeholders or execution assumptions inside the prompt, not as questions to the requester.\n\
         - Do not add documentation, docs/, file-writing, persistence, or confirmation requirements unless the original prompt explicitly asks for those outputs.\n\
         - Keep the prompt actionable for a coding agent.\n\
         - Do not generate a second language version.\n\n\
         Original prompt:\n```text\n{}\n```",
        target.instruction_label(),
        prompt
    )
}

/// 构造 Workbench 流式优化指令。
///
/// Business Logic（为什么需要这个函数）:
///     快捷键小组件会直接把模型输出写进终端，因此输出必须是单语纯 Prompt 文本，不能包含 JSON 包装或解释。
///
/// Code Logic（这个函数做什么）:
///     按设置页目标语种生成 stream-json 纯文本任务指令；提示 Claude Code 使用当前项目目录自动加载的上下文，
///     并禁止生成第二语言版本、澄清问题和未经原始需求要求的 docs/ 文档输出。
fn build_streaming_optimize_instruction(
    prompt: &str,
    target: PromptOptimizeTargetLanguage,
) -> String {
    format!(
        "You optimize user prompts for Claude Code programming tasks.\n\
         Output only the optimized prompt text in {}.\n\
         Do not output JSON, Markdown fences, headings about your answer, explanations, or metadata.\n\
         Requirements:\n\
         - Preserve the user's intent and do not invent external facts.\n\
         - Use any project instructions/context Claude Code auto-loads from the current working directory, including CLAUDE.md, when relevant.\n\
         - The optimized prompt must include: goal, context, constraints, and acceptance criteria.\n\
         - Write the optimized prompt from the requester's perspective, as a direct prompt they can paste into Claude Code.\n\
         - Do not ask the requester clarifying questions or include confirmation requests.\n\
         - If details are missing, express them as bracketed placeholders or execution assumptions inside the prompt, not as questions to the requester.\n\
         - Do not add documentation, docs/, file-writing, persistence, or confirmation requirements unless the original prompt explicitly asks for those outputs.\n\
         - Keep the prompt actionable for a coding agent.\n\
         - Do not generate a second language version.\n\n\
         Original prompt:\n```text\n{}\n```",
        target.instruction_label(),
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
#[cfg(test)]
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
    fn resolves_optional_working_directory() {
        assert!(resolve_working_directory(None).unwrap().is_none());
        assert!(resolve_working_directory(Some(" \n".to_string()))
            .unwrap()
            .is_none());

        let cwd = std::env::current_dir().expect("current dir");
        let resolved = resolve_working_directory(Some(cwd.to_string_lossy().to_string()))
            .expect("existing dir")
            .expect("some dir");
        assert!(resolved.is_dir());

        let missing = cwd.join("__cc_partner_missing_prompt_context_dir__");
        assert!(resolve_working_directory(Some(missing.to_string_lossy().to_string())).is_err());
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

    #[test]
    fn target_language_schema_requires_single_output() {
        let target = PromptOptimizeTargetLanguage::parse(Some("zh".to_string()))
            .expect("parse zh")
            .expect("zh target");
        let schema = prompt_optimize_schema_for_target(Some(target)).expect("zh schema");

        assert_eq!(schema["required"][0], "optimizedPrompt");
        assert_eq!(schema["properties"]["optimizedPrompt"]["type"], "string");
        assert!(schema["properties"].get("optimizedZh").is_none());
        assert!(schema["properties"].get("optimizedEn").is_none());

        assert!(PromptOptimizeTargetLanguage::parse(Some("fr".to_string())).is_err());
    }

    #[test]
    fn required_target_language_trims_and_rejects_blank() {
        let (target, canonical) =
            parse_required_target_language("  en  ".to_string()).expect("english target");

        assert_eq!(target, PromptOptimizeTargetLanguage::En);
        assert_eq!(canonical, "en");
        assert!(parse_required_target_language("   ".to_string()).is_err());
        assert!(parse_required_target_language("fr".to_string()).is_err());
    }

    #[test]
    fn remote_working_directory_requires_non_empty_path_without_local_resolution() {
        let path =
            remote_working_directory(Some("  /remote/project  ".to_string())).expect("remote path");

        assert_eq!(path, "/remote/project");
        assert!(remote_working_directory(None).is_err());
        assert!(remote_working_directory(Some("\n\t".to_string())).is_err());
    }

    #[test]
    fn single_language_response_maps_to_selected_field() {
        let zh = single_prompt_response_to_full(
            PromptOptimizeTargetLanguage::Zh,
            SinglePromptOptimizeResponseDto {
                optimized_prompt: "中文版本".to_string(),
            },
        );
        let en = single_prompt_response_to_full(
            PromptOptimizeTargetLanguage::En,
            SinglePromptOptimizeResponseDto {
                optimized_prompt: "English version".to_string(),
            },
        );

        assert_eq!(zh.optimized_zh, "中文版本");
        assert_eq!(zh.optimized_en, "");
        assert_eq!(en.optimized_zh, "");
        assert_eq!(en.optimized_en, "English version");
    }

    #[test]
    fn instruction_requires_requester_perspective_without_clarifying_questions() {
        let instruction = build_optimize_instruction("修复工作台 Prompt 优化浮层");

        assert!(instruction.contains(
            "Do not add documentation, docs/, file-writing, persistence, or confirmation requirements unless the original prompt explicitly asks for those outputs."
        ));
        assert!(instruction.contains(
            "Write both optimized prompts from the requester's perspective, as a direct prompt they can paste into Claude Code."
        ));
        assert!(instruction.contains(
            "Do not ask the requester clarifying questions or include confirmation requests."
        ));
    }

    #[test]
    fn streaming_instruction_returns_plain_single_language_prompt() {
        let instruction = build_streaming_optimize_instruction(
            "修复工作台 Prompt 优化浮层",
            PromptOptimizeTargetLanguage::Zh,
        );

        assert!(instruction.contains("Output only the optimized prompt text"));
        assert!(instruction.contains("Do not output JSON"));
        assert!(instruction.contains("Do not generate a second language version"));
        assert!(instruction.contains("Simplified Chinese"));
        assert!(!instruction.contains("optimizedPrompt"));
    }
}
