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
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const DEFAULT_CLAUDE_CLI: &str = "claude";
const DEFAULT_CLAUDE_MODEL: &str = "sonnet";
const MAX_ERROR_CHARS: usize = 500;
const MAX_ACTIVITY_CHARS: usize = 160;
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

/// 解析本机 Claude CLI 可执行路径。
///
/// Business Logic（为什么需要这个函数）:
///     打包后的 GUI/Tauri 进程只继承 launchd/系统 PATH，通常不含用户 shell 里的
///     `~/.local/bin` 等目录；默认配置 `claude` 会在 macOS 打包态直接 NotFound，
///     导致 GitHub 解说/Prompt 优化显示「启动 Claude CLI 失败: os error 2」。
///
/// Code Logic（这个函数做什么）:
///     归一化配置路径后：绝对/显式路径原样返回；裸命令名则在常见安装目录与当前
///     PATH 中查找可执行文件，命中返回绝对路径，未命中仍返回命令名让 OS 再解析。
pub(crate) fn resolve_cli_path(path: &str) -> String {
    resolve_cli_path_with_search(
        path,
        dirs::home_dir().as_deref(),
        std::env::var_os("PATH").as_deref(),
    )
}

/// 可测试入口：在给定 home / PATH 下解析 Claude CLI。
///
/// Code Logic（这个函数做什么）:
///     与 `resolve_cli_path` 相同语义，但搜索根与 PATH 由调用方注入。
pub(crate) fn resolve_cli_path_with_search(
    path: &str,
    home: Option<&Path>,
    path_env: Option<&std::ffi::OsStr>,
) -> String {
    let normalized = normalize_cli_path(path);
    let candidate = Path::new(&normalized);
    if candidate.is_absolute() || normalized.contains('/') || normalized.contains('\\') {
        return normalized;
    }

    for dir in cli_search_dirs(home, path_env) {
        if let Some(found) = executable_in_dir(&dir, &normalized) {
            return found.to_string_lossy().into_owned();
        }
    }
    normalized
}

/// 构造启动 Claude CLI 时应使用的 PATH（用户常见安装目录优先）。
///
/// Business Logic（为什么需要这个函数）:
///     即使已解析到绝对路径，Claude CLI 内部仍可能依赖同目录或用户 PATH 中的其它工具；
///     GUI 进程的稀疏 PATH 会导致二次失败。doctor 探测与正式调用必须共用同一 PATH。
///
/// Code Logic（这个函数做什么）:
///     将常见安装目录前置拼到现有 PATH 前，去重后 `join_paths`。
pub(crate) fn cli_command_path_env() -> OsString {
    cli_command_path_env_with(
        dirs::home_dir().as_deref(),
        std::env::var_os("PATH").as_deref(),
    )
}

fn cli_command_path_env_with(home: Option<&Path>, path_env: Option<&std::ffi::OsStr>) -> OsString {
    std::env::join_paths(gui_cli_search_dirs(home, path_env))
        .unwrap_or_else(|_| OsString::from("/usr/bin:/bin"))
}

/// GUI/sidecar 探测与 spawn CLI 共用的搜索目录。
///
/// Business Logic（为什么需要这个函数）:
///     Agent Hub 库存 probe 与 Prompt 优化必须在打包态稀疏 PATH 下找到
///     `~/.local/bin` 以及 nvm/fnm/volta/asdf 当前 bin 里的 Claude/Codex；
///     目录清单只能有一份。
///
/// Code Logic（这个函数做什么）:
///     用户常见安装目录 + Node 版本管理器当前 bin + 传入 PATH + 系统基础路径，去重保序。
pub(crate) fn gui_cli_search_dirs(
    home: Option<&Path>,
    path_env: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
    let mut dirs = cli_search_dirs(home, path_env);
    for system in [
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/sbin"),
    ] {
        if !dirs.iter().any(|d| d == &system) {
            dirs.push(system);
        }
    }
    dirs
}

/// 常见 Claude CLI 安装目录 + 当前 PATH 条目。
fn cli_search_dirs(home: Option<&Path>, path_env: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home {
        push_unique_dir(&mut dirs, home.join(".local").join("bin"));
        push_unique_dir(&mut dirs, home.join(".claude").join("local"));
        push_unique_dir(&mut dirs, home.join(".claude").join("bin"));
        for manager_bin in node_version_manager_bins(home) {
            push_unique_dir(&mut dirs, manager_bin);
        }
        #[cfg(windows)]
        {
            push_unique_dir(&mut dirs, home.join("AppData").join("Local").join("Claude"));
            push_unique_dir(&mut dirs, home.join("AppData").join("Roaming").join("npm"));
        }
    }
    push_unique_dir(&mut dirs, PathBuf::from("/opt/homebrew/bin"));
    push_unique_dir(&mut dirs, PathBuf::from("/usr/local/bin"));
    if let Some(path_env) = path_env {
        for entry in std::env::split_paths(path_env) {
            push_unique_dir(&mut dirs, entry);
        }
    }
    dirs
}

fn push_unique_dir(dirs: &mut Vec<PathBuf>, dir: PathBuf) {
    if dir.as_os_str().is_empty() {
        return;
    }
    if !dirs.iter().any(|existing| existing == &dir) {
        dirs.push(dir);
    }
}

/// nvm / fnm / volta / asdf 的当前 bin。只收录存在的目录，不扫全部历史 Node 版本。
fn node_version_manager_bins(home: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(nvm) = nvm_default_bin(home) {
        dirs.push(nvm);
    }
    for dir in [
        home.join(".local")
            .join("share")
            .join("fnm")
            .join("aliases")
            .join("default")
            .join("bin"),
        home.join(".fnm")
            .join("aliases")
            .join("default")
            .join("bin"),
        home.join(".volta").join("bin"),
        home.join(".asdf").join("shims"),
    ] {
        if dir.is_dir() {
            dirs.push(dir);
        }
    }
    dirs
}

/// 解析 nvm 默认 Node 的 bin（`~/.nvm/current` 或 `alias/default` → `versions/node/<ver>/bin`）。
fn nvm_default_bin(home: &Path) -> Option<PathBuf> {
    let nvm = home.join(".nvm");
    let current_bin = nvm.join("current").join("bin");
    if current_bin.is_dir() {
        return Some(current_bin);
    }
    let mut name = std::fs::read_to_string(nvm.join("alias").join("default")).ok()?;
    name = name.trim().to_string();
    if name.is_empty() {
        return None;
    }
    if let Ok(next) = std::fs::read_to_string(nvm.join("alias").join(&name)) {
        let trimmed = next.trim();
        if !trimmed.is_empty() {
            name = trimmed.to_string();
        }
    }
    let bin = nvm.join("versions").join("node").join(&name).join("bin");
    bin.is_dir().then_some(bin)
}

/// 在目录中查找可执行的 CLI 文件（Windows 额外尝试 .exe/.cmd/.bat）。
fn executable_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if is_runnable_file(&direct) {
        return Some(direct);
    }
    #[cfg(windows)]
    {
        for ext in ["exe", "cmd", "bat"] {
            let with_ext = dir.join(name).with_extension(ext);
            if is_runnable_file(&with_ext) {
                return Some(with_ext);
            }
        }
    }
    None
}

fn is_runnable_file(path: &Path) -> bool {
    path.is_file()
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

/// 把可选的隔离 `CLAUDE_CONFIG_DIR` 注入 Claude CLI spawn。
///
/// Business Logic（为什么需要这个函数）:
///     cc-partner 的内部 headless Claude 调用可选使用一个**不等于 OS 默认**的 cc-switch
///     provider。经查官方文档：进程 env 会被 settings.json 的 `env` 块覆盖、`--settings` 是
///     浅层 per-key merge（stale-key 泄露）；唯一无合并/无泄露的机制是 `CLAUDE_CONFIG_DIR`
///     整体重定位 `~/.claude`，使 claude 只读我们写入的隔离 settings.json，不改写 OS 默认配置。
///
/// Code Logic（这个函数做什么）:
///     `Some(dir)` 时设 `cmd.env("CLAUDE_CONFIG_DIR", dir)`；`None` 时不动（沿用 OS 默认）。
fn apply_provider_config_dir(cmd: &mut Command, provider_config_dir: Option<&Path>) {
    if let Some(dir) = provider_config_dir {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
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
    let cli = resolve_cli_path(cli_path);
    let mut cmd = Command::new(&cli);
    cmd.env("PATH", cli_command_path_env())
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output =
        match tokio::time::timeout(Duration::from_secs(CLI_VERSION_TIMEOUT_SECS), cmd.output())
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

/// Claude CLI `--mcp-config` 的空配置。
///
/// Business Logic（为什么需要这个常量）:
///     merge 冲突解决只开放 Read/Edit/Write/Grep/Glob，必须切断用户/项目 MCP，避免额外工具面。
///
/// Code Logic（这个常量做什么）:
///     CLI schema 要求 `mcpServers` 为 record。裸 `{}` 会把该字段当成 undefined，
///     启动即失败：`Invalid MCP configuration: mcpServers: expected record, received undefined`。
const EMPTY_MCP_CONFIG_JSON: &str = r#"{"mcpServers":{}}"#;

/// 构造允许 Claude 直接编辑当前隔离 worktree 的 headless 参数。
///
/// Business Logic（为什么需要这个函数）:
///     大型 merge 冲突不能把所有文件全文塞进 prompt 再要求模型完整回传；Claude 应在受后端隔离、
///     校验和回收的 integration worktree 内直接读取并编辑冲突文件。
///
/// Code Logic（这个函数做什么）:
///     使用 print/non-persistent 项目上下文，只开放 Read/Edit/Write/Grep/Glob；dontAsk 模式下只预批准
///     integration 项目根内读写与只读搜索，不开放 Bash，精确文件范围由调用方在 CLI 返回后用 Git 验证。
///     `--strict-mcp-config` + 空 `mcpServers` 切断用户/项目 MCP，且满足 CLI record schema。
///     stream-json + verbose 让调用方按行观察工具调用并做 idle timeout，而不是等整段 JSON。
pub(crate) fn build_project_edit_headless_args(model: &str) -> Vec<String> {
    vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--include-partial-messages".to_string(),
        "--no-session-persistence".to_string(),
        "--strict-mcp-config".to_string(),
        "--mcp-config".to_string(),
        EMPTY_MCP_CONFIG_JSON.to_string(),
        "--disable-slash-commands".to_string(),
        "--no-chrome".to_string(),
        "--permission-mode".to_string(),
        "dontAsk".to_string(),
        "--tools".to_string(),
        "Read,Edit,Write,Grep,Glob".to_string(),
        "--allowedTools".to_string(),
        "Read(/**)".to_string(),
        "Edit(/**)".to_string(),
        "Grep".to_string(),
        "Glob".to_string(),
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

/// 从 Claude CLI stream-json 行提取一行可读动向。
///
/// Business Logic（为什么需要这个函数）:
///     merge 冲突解决要在阶段条下展示 Claude Code 正在做什么，但不能把整段 JSON 事件甩给用户。
///
/// Code Logic（这个函数做什么）:
///     优先取 assistant/tool_use 的工具名+路径/pattern；否则取 assistant 文本最后一行；
///     thinking/result/其它元事件返回 None。空白行压成单行并截断。
pub(crate) fn activity_line_from_stream_json(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    match value.get("type").and_then(|item| item.as_str())? {
        "assistant" => activity_from_assistant(&value),
        "stream_event" => activity_from_stream_event(&value),
        _ => None,
    }
}

/// 从 assistant 事件提取工具调用或最后一行文本。
fn activity_from_assistant(value: &serde_json::Value) -> Option<String> {
    let content = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_array())?;
    let mut last_tool = None;
    let mut last_text = None;
    for item in content {
        match item.get("type").and_then(|kind| kind.as_str()) {
            Some("tool_use") => {
                if let Some(name) = item.get("name").and_then(|name| name.as_str()) {
                    last_tool = Some(format_tool_activity(name, item.get("input")));
                }
            }
            Some("text") => {
                if let Some(text) = item.get("text").and_then(|text| text.as_str()) {
                    last_text = text
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .next_back()
                        .map(collapse_activity_line);
                }
            }
            _ => {}
        }
    }
    last_tool.or(last_text).filter(|line| !line.is_empty())
}

/// 从 stream_event 的 tool_use 起始块提取动向。
fn activity_from_stream_event(value: &serde_json::Value) -> Option<String> {
    let event = value.get("event")?;
    if event.get("type").and_then(|item| item.as_str()) != Some("content_block_start") {
        return None;
    }
    let block = event.get("content_block")?;
    if block.get("type").and_then(|item| item.as_str()) != Some("tool_use") {
        return None;
    }
    let name = block.get("name").and_then(|item| item.as_str())?;
    Some(format_tool_activity(name, block.get("input"))).filter(|line| !line.is_empty())
}

/// 把工具名和关键参数压成一行动向。
fn format_tool_activity(name: &str, input: Option<&serde_json::Value>) -> String {
    let Some(input) = input else {
        return collapse_activity_line(name);
    };
    let detail = match name {
        "Grep" | "Glob" => input
            .get("pattern")
            .or_else(|| input.get("glob"))
            .or_else(|| input.get("query"))
            .or_else(|| input.get("path")),
        _ => input
            .get("file_path")
            .or_else(|| input.get("path"))
            .or_else(|| input.get("pattern")),
    }
    .and_then(|item| item.as_str())
    .unwrap_or("");
    if detail.is_empty() {
        collapse_activity_line(name)
    } else {
        collapse_activity_line(&format!("{name} {detail}"))
    }
}

/// 把多空白/换行压成单行并截断，供阶段条一行展示。
fn collapse_activity_line(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let truncated: String = chars.by_ref().take(MAX_ACTIVITY_CHARS).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
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
    provider_config_dir: Option<&Path>,
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
        provider_config_dir,
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
#[allow(clippy::too_many_arguments)] // 内部 helper：cli/model/provider/schema/prompt/cwd/timeout/label 8 段语义独立
pub(crate) async fn run_structured_json_with_cwd<T>(
    cli_path: &str,
    model: &str,
    provider_config_dir: Option<&Path>,
    schema: &str,
    prompt: &str,
    working_directory: Option<&Path>,
    timeout_secs: u64,
    task_label: &str,
) -> Result<T, AppError>
where
    T: DeserializeOwned,
{
    let cli = resolve_cli_path(cli_path);
    let mut cmd = Command::new(&cli);
    let args = if working_directory.is_some() {
        build_project_headless_args(model, schema)
    } else {
        build_pure_headless_args(model, schema)
    };
    cmd.env("PATH", cli_command_path_env())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    apply_provider_config_dir(&mut cmd, provider_config_dir);
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

/// 在指定项目目录执行允许受限文件编辑的 Claude CLI。
///
/// Business Logic（为什么需要这个函数）:
///     merge 冲突文件可能很大且很多，完整 JSON 回传会同时放大输入与输出并稳定触发超时；
///     integration worktree 已与真实 main/source 隔离，可让 Claude 在其中直接修改，随后由调用方严格验收。
///     解决冲突只要 CLI 还在产出输出就应继续等；只有连续无输出才算卡住。
///
/// Code Logic（这个函数做什么）:
///     以受限项目编辑参数启动 CLI，通过 stdin 写 prompt，按行读 stream-json；每行刷新 idle 计时，
///     可解析出动向时回调 on_activity。成功只表示 CLI 正常退出，不信任其文本结果。
#[allow(clippy::too_many_arguments)] // cli/model/provider/prompt/cwd/idle/label/on_activity 语义独立
pub(crate) async fn run_project_edit_with_cwd<F>(
    cli_path: &str,
    model: &str,
    provider_config_dir: Option<&Path>,
    prompt: &str,
    working_directory: &Path,
    idle_timeout_secs: u64,
    task_label: &str,
    mut on_activity: F,
) -> Result<(), AppError>
where
    F: FnMut(&str),
{
    let cli = resolve_cli_path(cli_path);
    let mut cmd = Command::new(&cli);
    cmd.env("PATH", cli_command_path_env())
        .args(build_project_edit_headless_args(model))
        .current_dir(working_directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    apply_provider_config_dir(&mut cmd, provider_config_dir);

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

    let idle = Duration::from_secs(idle_timeout_secs);
    let mut reader = BufReader::new(stdout).lines();
    let mut last_activity = String::new();
    loop {
        let line = match tokio::time::timeout(idle, reader.next_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => break,
            Ok(Err(e)) => return Err(AppError::generic(format!("读取 Claude CLI 输出失败: {e}"))),
            Err(_) => {
                return Err(AppError::generic(format!(
                    "Claude CLI {task_label}超时（{idle_timeout_secs} 秒无输出）"
                )))
            }
        };
        if let Some(activity) = activity_line_from_stream_json(&line) {
            if activity != last_activity {
                last_activity = activity.clone();
                on_activity(&activity);
            }
        }
    }

    let status = match tokio::time::timeout(idle, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => return Err(AppError::generic(format!("等待 Claude CLI 输出失败: {e}"))),
        Err(_) => {
            return Err(AppError::generic(format!(
                "Claude CLI {task_label}超时（{idle_timeout_secs} 秒无输出）"
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

/// 在可选工作目录中执行 Claude CLI 并流式返回 assistant 文本。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench Prompt 小组件希望优化结果生成时就进入当前终端，用户无需等待完整 JSON 后再一次性填入。
///
/// Code Logic（这个函数做什么）:
///     使用 Claude CLI `stream-json` 输出格式，逐行解析 assistant 文本增量并调用 on_chunk；
///     working_directory 存在时不加 `--bare`，从而允许 Claude Code 读取项目 CLAUDE.md 上下文。
#[allow(clippy::too_many_arguments)] // 内部 helper：cli/model/provider/prompt/cwd/timeout/label/on_chunk 8 段语义独立
pub(crate) async fn run_streaming_text_with_cwd<F>(
    cli_path: &str,
    model: &str,
    provider_config_dir: Option<&Path>,
    prompt: &str,
    working_directory: Option<&Path>,
    timeout_secs: u64,
    task_label: &str,
    mut on_chunk: F,
) -> Result<(), AppError>
where
    F: FnMut(&str) -> Result<(), AppError> + Send,
{
    let cli = resolve_cli_path(cli_path);
    let mut cmd = Command::new(&cli);
    cmd.env("PATH", cli_command_path_env())
        .args(build_streaming_text_args(
            model,
            working_directory.is_some(),
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    apply_provider_config_dir(&mut cmd, provider_config_dir);
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
        // pure 模式不带 mcp/slash/chrome 抑制参数——那些只属于
        // build_project_edit_headless_args（merge 冲突编辑），由其专属测试覆盖。
        assert!(!args.iter().any(|arg| arg == "--strict-mcp-config"));
        assert!(!args.iter().any(|arg| arg == "--disable-slash-commands"));
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
    fn project_edit_args_allow_only_scoped_file_edits_without_bash() {
        let args = build_project_edit_headless_args(" sonnet ");

        assert!(args.iter().any(|arg| arg == "-p"));
        assert!(args.iter().any(|arg| arg == "--no-session-persistence"));
        assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--mcp-config", EMPTY_MCP_CONFIG_JSON]));
        assert!(
            !args.windows(2).any(|pair| pair == ["--mcp-config", "{}"]),
            "裸空对象会被 Claude CLI 校验为 mcpServers=undefined"
        );
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "dontAsk"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--tools", "Read,Edit,Write,Grep,Glob"]));
        assert!(args.iter().any(|arg| arg == "Read(/**)"));
        assert!(args.iter().any(|arg| arg == "Edit(/**)"));
        assert!(args.iter().any(|arg| arg == "Grep"));
        assert!(args.iter().any(|arg| arg == "Glob"));
        assert!(args.windows(2).any(|pair| pair == ["--model", "sonnet"]));
        assert!(!args.iter().any(|arg| arg == "Bash"));
        assert!(!args.iter().any(|arg| arg == "--json-schema"));
        assert!(!args.iter().any(|arg| arg == "--bare"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-format", "stream-json"]));
        assert!(args.iter().any(|arg| arg == "--verbose"));
        assert!(args.iter().any(|arg| arg == "--include-partial-messages"));
    }

    #[test]
    fn activity_line_from_stream_json_prefers_tool_use_path() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Inspecting conflict"},{"type":"tool_use","name":"Read","input":{"file_path":"src/lib.rs"}}]}}"#;
        assert_eq!(
            activity_line_from_stream_json(line).as_deref(),
            Some("Read src/lib.rs")
        );
    }

    #[test]
    fn activity_line_from_stream_json_uses_last_assistant_text_line() {
        let line = "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first line\\nsecond line\\n\"}]}}";
        assert_eq!(
            activity_line_from_stream_json(line).as_deref(),
            Some("second line")
        );
    }

    #[test]
    fn activity_line_from_stream_json_formats_grep_and_ignores_result() {
        let grep = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Grep","input":{"pattern":"<<<<<<","path":"."}}]}}"#;
        assert_eq!(
            activity_line_from_stream_json(grep).as_deref(),
            Some("Grep <<<<<<")
        );
        assert_eq!(
            activity_line_from_stream_json(r#"{"type":"result","result":"done"}"#),
            None
        );
        assert_eq!(
            activity_line_from_stream_json(
                r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"internal"}}}"#
            ),
            None
        );
    }

    #[test]
    fn empty_mcp_config_json_is_record_shaped() {
        let value: serde_json::Value =
            serde_json::from_str(EMPTY_MCP_CONFIG_JSON).expect("empty mcp json");
        assert!(
            value
                .get("mcpServers")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|servers| servers.is_empty()),
            "Claude CLI 要求 mcpServers 为 record，空配置必须带空对象"
        );
    }

    #[test]
    fn normalizes_empty_cli_and_model_defaults() {
        assert_eq!(normalize_cli_path("  "), "claude");
        assert_eq!(normalize_cli_path("  /opt/claude  "), "/opt/claude");
        assert_eq!(normalize_model("  "), "sonnet");
        assert_eq!(normalize_model("  haiku  "), "haiku");
    }

    #[test]
    fn resolves_bare_claude_from_user_local_bin_when_gui_path_empty() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path();
        let local_bin = home.join(".local").join("bin");
        std::fs::create_dir_all(&local_bin).expect("mkdir");
        let fake_cli = local_bin.join("claude");
        std::fs::write(&fake_cli, b"#!/bin/sh\necho 1.0.0\n").expect("write fake cli");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_cli, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }

        // 模拟打包 GUI 的稀疏 PATH：不含 ~/.local/bin。
        let sparse_path = std::ffi::OsString::from("/usr/bin:/bin");
        let resolved =
            resolve_cli_path_with_search("claude", Some(home), Some(sparse_path.as_os_str()));
        assert_eq!(
            Path::new(&resolved),
            fake_cli.as_path(),
            "应解析到 home/.local/bin/claude，而不是留下裸命令名"
        );

        // 显式绝对路径不改写。
        assert_eq!(
            resolve_cli_path_with_search(
                fake_cli.to_str().expect("utf8"),
                Some(home),
                Some(sparse_path.as_os_str())
            ),
            fake_cli.to_string_lossy()
        );

        // 不存在时仍返回默认命令名，便于错误文案保留 `claude`。
        let missing_home = home.join("empty-home");
        std::fs::create_dir_all(&missing_home).expect("mkdir empty home");
        assert_eq!(
            resolve_cli_path_with_search(
                "claude",
                Some(missing_home.as_path()),
                Some(sparse_path.as_os_str())
            ),
            "claude"
        );
    }

    #[test]
    fn path_env_prefers_user_local_bin() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path();
        let path = cli_command_path_env_with(Some(home), Some(std::ffi::OsStr::new("/usr/bin")));
        let joined = path.to_string_lossy();
        let local = home
            .join(".local")
            .join("bin")
            .to_string_lossy()
            .into_owned();
        assert!(
            joined.contains(&local),
            "增强 PATH 应包含 ~/.local/bin: {joined}"
        );
        assert!(joined.contains("/usr/bin"), "增强 PATH 应保留系统路径");
    }

    #[test]
    fn sparse_path_includes_nvm_default_bin() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path();
        let nvm_bin = home
            .join(".nvm")
            .join("versions")
            .join("node")
            .join("v22.19.0")
            .join("bin");
        std::fs::create_dir_all(&nvm_bin).expect("mkdir nvm");
        std::fs::create_dir_all(home.join(".nvm").join("alias")).expect("mkdir alias");
        std::fs::write(
            home.join(".nvm").join("alias").join("default"),
            "v22.19.0\n",
        )
        .expect("write alias");
        let fake_cli = nvm_bin.join("codex");
        std::fs::write(&fake_cli, b"#!/bin/sh\necho 0.147.0\n").expect("write fake cli");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_cli, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        let sparse_path = std::ffi::OsString::from("/usr/bin:/bin");
        let resolved =
            resolve_cli_path_with_search("codex", Some(home), Some(sparse_path.as_os_str()));
        assert_eq!(
            Path::new(&resolved),
            fake_cli.as_path(),
            "稀疏 PATH 应解析到 nvm default bin 里的 codex"
        );
        let path_env = cli_command_path_env_with(Some(home), Some(sparse_path.as_os_str()));
        let joined = path_env.to_string_lossy();
        assert!(
            joined.contains(nvm_bin.to_string_lossy().as_ref()),
            "增强 PATH 应包含 nvm default bin: {joined}"
        );
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

    #[cfg(unix)]
    fn write_fake_claude_script(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("fake-claude");
        std::fs::write(&script, contents).expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        (dir, script)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn project_edit_keeps_waiting_while_cli_keeps_emitting_output() {
        let (_dir, script) = write_fake_claude_script(
            r#"#!/bin/sh
cat >/dev/null
i=0
while [ "$i" -lt 8 ]; do
  printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"f.rs"}}]}}'
  sleep 0.3
  i=$((i+1))
done
exit 0
"#,
        );
        let cwd = tempfile::tempdir().expect("cwd");
        let mut activities = Vec::new();
        let started = std::time::Instant::now();
        run_project_edit_with_cwd(
            script.to_str().expect("utf8 path"),
            "sonnet",
            None,
            "resolve",
            cwd.path(),
            1,
            "解决 merge 冲突",
            |line| activities.push(line.to_string()),
        )
        .await
        .expect("ongoing output must not idle-timeout");
        assert!(started.elapsed() >= std::time::Duration::from_millis(1500));
        assert!(
            activities.iter().any(|line| line == "Read f.rs"),
            "expected tool activity, got {activities:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn project_edit_idle_timeouts_after_silence() {
        let (_dir, script) = write_fake_claude_script(
            r#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"f.rs"}}]}}'
sleep 3
exit 0
"#,
        );
        let cwd = tempfile::tempdir().expect("cwd");
        let error = run_project_edit_with_cwd(
            script.to_str().expect("utf8 path"),
            "sonnet",
            None,
            "resolve",
            cwd.path(),
            1,
            "解决 merge 冲突",
            |_| {},
        )
        .await
        .expect_err("silence longer than idle timeout must fail");
        let message = error.to_string();
        assert!(
            message.contains("超时") && message.contains("无输出"),
            "unexpected timeout message: {message}"
        );
    }
}
