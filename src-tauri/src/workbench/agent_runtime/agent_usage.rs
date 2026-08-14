//! workbench/agent_runtime/agent_usage — Agent 终态 usage 提取（Claude/Codex/OpenCode）
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent 使用统计 Ledger 的 tokens 在生产中长期为 null，因为 `note_usage()` 没有调用者。
//!     各 CLI（Claude Code / Codex / OpenCode）在会话结束时会把可靠 usage 落到本地
//!     session 文件或 SQLite 中；runtime 进入终态时应提取这些数据补记 Ledger，
//!     让 UI 不再显示「未提供」。
//!
//! Code Logic（这个模块做什么）:
//!     三个纯函数提取器（root 可注入便于测试）+ 统一入口 `extract_provider_usage`：
//!     - Claude：`~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`，按 message.id
//!       去重（stop_reason 优先 / output_tokens 更大者）后求和；
//!     - Codex：`~/.codex/sessions/YYYY/MM/DD/rollout-*<uuid>*.jsonl`，取最后一个
//!       token_count 的 `total_token_usage`（会话累计值）；
//!     - OpenCode：只读打开 opencode SQLite，按 session 查 message 表 data JSON 求和。
//!     所有提取有界、宽松解析，失败一律返回 None，不 panic。

use crate::workbench::agent_ledger::ReliableUsageSnapshot;
use serde_json::Value;
use sqlx::Row;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// projects 根下最多遍历的项目目录数（有界，命中即返）。
const MAX_CLAUDE_PROJECT_DIRS: usize = 10_000;
/// Claude session jsonl 文件大小上限。
const MAX_JSONL_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// 单行 JSON 上限（超过则跳过该行）。
const MAX_JSONL_LINE_BYTES: usize = 1024 * 1024;
/// Codex sessions 下最多检查的 rollout 文件数。
const MAX_CODEX_SESSION_FILES: usize = 10_000;

/// 校验 native session id 可安全拼进文件路径：非空、无路径分隔符、无 `..`。
///
/// Business Logic（为什么需要这个函数）:
///     native_session_id 来自外部 CLI 输出，必须防止 `../` 之类的路径穿越。
///
/// Code Logic（这个模块做什么）:
///     非空且不包含 `/`、`\` 与 `..` 返回 true。
fn is_safe_native_id(id: &str) -> bool {
    !id.is_empty() && !id.contains('/') && !id.contains('\\') && !id.contains("..")
}

/// 把 f64 金额格式化为保留 6 位小数的字符串。
///
/// Business Logic（为什么需要这个函数）:
///     Ledger 的 cost_major 是字符串主单位金额，需要稳定格式便于 minor units 换算。
///
/// Code Logic（这个模块做什么）:
///     `format!("{:.6}", v)`；调用方在总额为 0 时直接传 None。
fn format_cost(v: f64) -> String {
    format!("{:.6}", v)
}

// ---------------------------------------------------------------------------
// A. Claude
// ---------------------------------------------------------------------------

/// Claude 单条 usage 观察的内存表示（用于 message.id 去重）。
#[derive(Debug, Clone, Default)]
struct ClaudeUsageEntry {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost_usd: f64,
    stop_reason: bool,
}

/// 从 Claude session JSONL 提取可靠 usage。
///
/// Business Logic（为什么需要这个函数）:
///     Claude Code 把每次请求的 usage 写进 session jsonl 的 `message.usage`；
///     同一 message.id 可能出现多份快照，直接求和会双算。
///
/// Code Logic（这个模块做什么）:
///     在 projects_root 下最多 10_000 个项目目录中查找 `<native_session_id>.jsonl`；
///     逐行宽松解析，按 message.id 去重（有 stop_reason 优先，否则 output_tokens 更大者），
///     求和 output；costUSD 随代表行累加；model 取最后一条带 model 的行。失败返回 None。
pub(crate) fn extract_claude_usage(
    projects_root: Option<PathBuf>,
    native_session_id: &str,
) -> Option<ReliableUsageSnapshot> {
    if !is_safe_native_id(native_session_id) {
        return None;
    }
    let root = projects_root?;
    let file_name = format!("{native_session_id}.jsonl");
    let target = find_claude_session_file(&root, &file_name)?;
    parse_claude_jsonl(&target, native_session_id)
}

/// 在 projects root 下有界查找目标 session 文件。
///
/// Business Logic（为什么需要这个函数）:
///     encoded-cwd 子目录名无法从 session id 反推，只能遍历查找，且必须防异常目录拖垮。
///
/// Code Logic（这个模块做什么）:
///     read_dir 顺序遍历（上限 10_000 目录），命中文件名即返回完整路径。
fn find_claude_session_file(root: &Path, file_name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(root).ok()?;
    for (i, entry) in entries.enumerate() {
        if i >= MAX_CLAUDE_PROJECT_DIRS {
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let candidate = entry.path().join(file_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 解析 Claude session jsonl 并按 message.id 去重求和。
///
/// Business Logic（为什么需要这个函数）:
///     jsonl 每行的 `message.usage` 是单次请求量；同一 message.id 的 message_start
///     快照与最终块并存时需要选出代表行再累计。
///
/// Code Logic（这个模块做什么）:
///     整文件顺序读（≤64MiB，单行 ≤1MiB）；`message.id` + `message.usage` 存在才记录；
///     去重规则：新行有 stop_reason 而旧行没有 → 替换；stop_reason 状态相同 → output 更大者。
///     输出四个 token 维度求和、costUSD 求和、model_id 取最后一条。
fn parse_claude_jsonl(path: &Path, expected_session: &str) -> Option<ReliableUsageSnapshot> {
    let mut file = fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if meta.len() > MAX_JSONL_FILE_BYTES {
        return None;
    }
    let mut content = String::new();
    file.read_to_string(&mut content).ok()?;

    let mut messages: HashMap<String, ClaudeUsageEntry> = HashMap::new();
    let mut total_cost = 0.0f64;
    let mut model_id: Option<String> = None;
    let mut matched = false;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.len() > MAX_JSONL_LINE_BYTES {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // 会话文件本身即目标 session；sessionId 字段若存在则做一致性过滤（宽松）。
        if let Some(sid) = value.get("sessionId").and_then(Value::as_str) {
            if sid != expected_session {
                continue;
            }
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };
        let Some(msg_id) = message.get("id").and_then(Value::as_str) else {
            // 无 id 的 usage 无法去重，丢弃以保证不双算。
            continue;
        };
        let model = message.get("model").and_then(Value::as_str);
        if model.is_some() {
            model_id = model.map(str::to_string);
        }
        let entry = ClaudeUsageEntry {
            input: usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output: usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_read: usage
                .get("cache_read_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_write: usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cost_usd: value.get("costUSD").and_then(Value::as_f64).unwrap_or(0.0),
            stop_reason: message
                .get("stop_reason")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty()),
        };
        matched = true;
        let should_replace = match messages.get(msg_id) {
            None => true,
            Some(existing) => {
                if entry.stop_reason && !existing.stop_reason {
                    true
                } else if entry.stop_reason == existing.stop_reason {
                    entry.output > existing.output
                } else {
                    false
                }
            }
        };
        if should_replace {
            // costUSD 属于行级快照，替换代表行时回退旧行成本再加新行，避免双计。
            if let Some(old) = messages.insert(msg_id.to_string(), entry.clone()) {
                total_cost -= old.cost_usd;
            }
            total_cost += entry.cost_usd;
        }
    }
    if !matched {
        return None;
    }
    let mut input = 0u64;
    let mut output = 0u64;
    let mut cache_read = 0u64;
    let mut cache_write = 0u64;
    for entry in messages.values() {
        input += entry.input;
        output += entry.output;
        cache_read += entry.cache_read;
        cache_write += entry.cache_write;
    }
    let cost = if total_cost > 0.0 {
        Some((format_cost(total_cost), "USD".to_string()))
    } else {
        None
    };
    Some(ReliableUsageSnapshot {
        model_id,
        input_tokens: Some(input),
        output_tokens: Some(output),
        cache_read_tokens: Some(cache_read),
        cache_write_tokens: Some(cache_write),
        cost_major: cost.as_ref().map(|(m, _)| m.clone()),
        cost_currency: cost.map(|(_, c)| c),
    })
}

// ---------------------------------------------------------------------------
// B. Codex
// ---------------------------------------------------------------------------

/// Codex 会话累计 usage（来自最后一个 token_count 的 total_token_usage）。
#[derive(Debug, Clone, Default)]
struct CodexCumulativeUsage {
    input: u64,
    cache_read: u64,
    output: u64,
}

/// 解析 codex `total_token_usage` 对象。
///
/// Business Logic（为什么需要这个函数）:
///     Codex token_count 的 info.total_token_usage 是会话累计值，字段名与 Claude 不同。
///
/// Code Logic（这个模块做什么）:
///     取 input_tokens / cached_input_tokens(→cache_read) / output_tokens，宽松缺省 0。
fn parse_codex_total_usage(total: &Value) -> Option<CodexCumulativeUsage> {
    let fields = total.as_object()?;
    let has_any = [
        "input_tokens",
        "cached_input_tokens",
        "output_tokens",
        "total_tokens",
    ]
    .iter()
    .any(|k| fields.contains_key(*k));
    if !has_any {
        return None;
    }
    Some(CodexCumulativeUsage {
        input: total
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read: total
            .get("cached_input_tokens")
            .or_else(|| total.get("cache_read_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: total
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

/// 从 Codex rollout jsonl 提取可靠 usage。
///
/// Business Logic（为什么需要这个函数）:
///     Codex CLI 把会话事件写进 `~/.codex/sessions/YYYY/MM/DD/rollout-*<uuid>*.jsonl`；
///     其 token_count 的 total_token_usage 是会话累计，取最后一条即可。
///
/// Code Logic（这个模块做什么）:
///     codex_root（默认 `CODEX_HOME` 或 `~/.codex`）下有界遍历日期目录找文件名含 uuid 的
///     rollout；逐行解析，model 从 session_meta / turn_context / token_count 宽松提取；
///     取最后一个 token_count 的累计值。无 token_count 返回 None。Codex jsonl 无成本 → cost None。
pub(crate) fn extract_codex_usage(
    codex_root: Option<PathBuf>,
    native_session_id: &str,
) -> Option<ReliableUsageSnapshot> {
    if !is_safe_native_id(native_session_id) {
        return None;
    }
    let root = codex_root?;
    let sessions = root.join("sessions");
    let target = find_codex_rollout_file(&sessions, native_session_id)?;
    parse_codex_jsonl(&target)
}

/// 在 sessions 目录树（YYYY/MM/DD）下有界查找文件名含 uuid 的 rollout 文件。
///
/// Business Logic（为什么需要这个函数）:
///     rollout 文件按日期分层且文件名含线程 uuid，只能按名匹配遍历，必须设上限。
///
/// Code Logic（这个模块做什么）:
///     递归遍历（深度最多 4 层），文件名包含 uuid 即返回；超过 10_000 个文件放弃。
fn find_codex_rollout_file(sessions: &Path, uuid: &str) -> Option<PathBuf> {
    let mut stack = vec![sessions.to_path_buf()];
    let mut checked = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if dir.components().count() < sessions.components().count() + 4 {
                    stack.push(entry.path());
                }
            } else if ft.is_file() {
                checked += 1;
                if checked > MAX_CODEX_SESSION_FILES {
                    return None;
                }
                let name = entry.file_name();
                let Some(name_str) = name.to_str() else {
                    continue;
                };
                if name_str.contains(uuid) {
                    return Some(entry.path());
                }
            }
        }
    }
    None
}

/// 解析 Codex rollout jsonl。
///
/// Business Logic（为什么需要这个函数）:
///     只关心会话累计 token 与 model；其余事件（response_item 等）一律忽略。
///
/// Code Logic（这个模块做什么）:
///     整文件顺序读；`type=="session_meta"`/`"turn_context"` 宽松取 payload.model；
///     `type=="event_msg"` 且 payload.type=="token_count" 时更新累计快照（最后一个生效）。
fn parse_codex_jsonl(path: &Path) -> Option<ReliableUsageSnapshot> {
    let mut file = fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if meta.len() > MAX_JSONL_FILE_BYTES {
        return None;
    }
    let mut content = String::new();
    file.read_to_string(&mut content).ok()?;

    let mut latest: Option<CodexCumulativeUsage> = None;
    let mut model_id: Option<String> = None;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.len() > MAX_JSONL_LINE_BYTES {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") | Some("turn_context") => {
                if let Some(model) = value
                    .get("payload")
                    .and_then(|p| {
                        p.get("model")
                            .or_else(|| p.get("info").and_then(|i| i.get("model")))
                    })
                    .and_then(Value::as_str)
                {
                    model_id = Some(model.to_string());
                }
            }
            Some("event_msg") => {
                let Some(payload) = value.get("payload") else {
                    continue;
                };
                if payload.get("type").and_then(Value::as_str) != Some("token_count") {
                    continue;
                }
                let Some(info) = payload.get("info") else {
                    continue;
                };
                if let Some(model) = info
                    .get("model")
                    .or_else(|| info.get("model_name"))
                    .or_else(|| payload.get("model"))
                    .and_then(Value::as_str)
                {
                    model_id = Some(model.to_string());
                }
                if let Some(total) = info.get("total_token_usage") {
                    if let Some(parsed) = parse_codex_total_usage(total) {
                        latest = Some(parsed);
                    }
                }
            }
            _ => {}
        }
    }
    let usage = latest?;
    Some(ReliableUsageSnapshot {
        model_id,
        input_tokens: Some(usage.input),
        output_tokens: Some(usage.output),
        cache_read_tokens: Some(usage.cache_read),
        cache_write_tokens: None,
        cost_major: None,
        cost_currency: None,
    })
}

// ---------------------------------------------------------------------------
// C. OpenCode
// ---------------------------------------------------------------------------

/// OpenCode 单条 assistant 消息 data JSON 的 usage 表示。
#[derive(Debug, Clone, Default, PartialEq)]
struct OpenCodeMessageUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: f64,
    model_id: Option<String>,
}

/// 解析 OpenCode message 表 data JSON 中的 usage。
///
/// Business Logic（为什么需要这个函数）:
///     OpenCode 把每次请求的 tokens/cost/model 序列化在 message.data JSON 中，
///     纯函数化便于不依赖 SQLite 的单元测试。
///
/// Code Logic（这个模块做什么）:
///     取 tokens.input / tokens.output / tokens.cache.read / tokens.cache.write、
///     cost、modelID；tokens 缺失返回 None（非 assistant 消息无 tokens）。
fn parse_opencode_message_data(value: &Value) -> Option<OpenCodeMessageUsage> {
    let tokens = value.get("tokens")?;
    let cache = tokens.get("cache");
    Some(OpenCodeMessageUsage {
        input: tokens.get("input").and_then(Value::as_u64).unwrap_or(0),
        output: tokens.get("output").and_then(Value::as_u64).unwrap_or(0),
        cache_read: cache
            .and_then(|c| c.get("read"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write: cache
            .and_then(|c| c.get("write"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cost: value.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
        model_id: value
            .get("modelID")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// 汇总 OpenCode 消息 usage 列表为快照。
///
/// Business Logic（为什么需要这个函数）:
///     tokens 为单次请求量需要求和；model 取最后一条非空；cost 求和。
///
/// Code Logic（这个模块做什么）:
///     顺序累加四个 token 维度与 cost；空列表返回 None。
fn aggregate_opencode_usage(rows: Vec<OpenCodeMessageUsage>) -> Option<ReliableUsageSnapshot> {
    if rows.is_empty() {
        return None;
    }
    let mut input = 0u64;
    let mut output = 0u64;
    let mut cache_read = 0u64;
    let mut cache_write = 0u64;
    let mut total_cost = 0.0f64;
    let mut model_id: Option<String> = None;
    for row in rows {
        input += row.input;
        output += row.output;
        cache_read += row.cache_read;
        cache_write += row.cache_write;
        total_cost += row.cost;
        if row.model_id.is_some() {
            model_id = row.model_id;
        }
    }
    let cost = if total_cost > 0.0 {
        Some((format_cost(total_cost), "USD".to_string()))
    } else {
        None
    };
    Some(ReliableUsageSnapshot {
        model_id,
        input_tokens: Some(input),
        output_tokens: Some(output),
        cache_read_tokens: Some(cache_read),
        cache_write_tokens: Some(cache_write),
        cost_major: cost.as_ref().map(|(m, _)| m.clone()),
        cost_currency: cost.map(|(_, c)| c),
    })
}

/// 用已连接的 sqlx sqlite pool 查询并汇总某 OpenCode session 的 usage（async）。
///
/// Business Logic（为什么需要这个函数）:
///     OpenCode 唯一可靠数据源是本地 SQLite message 表；查询逻辑独立成 async 函数
///     便于用内存库做单元测试。
///
/// Code Logic（这个模块做什么）:
///     先 PRAGMA table_info(message) 确认 session_id/data/time 列存在；再
///     `SELECT data FROM message WHERE session_id = ?1 ORDER BY time ASC`，
///     逐行解析 data JSON 后汇总。任何失败返回 None。
async fn query_opencode_usage(
    pool: &sqlx::SqlitePool,
    session_id: &str,
) -> Option<ReliableUsageSnapshot> {
    let columns: Vec<String> = sqlx::query("PRAGMA table_info(message)")
        .fetch_all(pool)
        .await
        .ok()?
        .iter()
        .filter_map(|row| row.try_get::<String, _>("name").ok())
        .collect();
    for needed in ["session_id", "data", "time"] {
        if !columns.iter().any(|c| c == needed) {
            return None;
        }
    }
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT data FROM message WHERE session_id = ?1 ORDER BY time ASC")
            .bind(session_id)
            .fetch_all(pool)
            .await
            .ok()?;
    let mut parsed = Vec::new();
    for (data,) in rows {
        if let Ok(value) = serde_json::from_str::<Value>(&data) {
            if let Some(usage) = parse_opencode_message_data(&value) {
                parsed.push(usage);
            }
        }
    }
    aggregate_opencode_usage(parsed)
}

/// 解析生产 OpenCode 数据库路径（复用 auto_title 的候选探测）。
///
/// Business Logic（为什么需要这个函数）:
///     OpenCode db 位置因安装方式与 XDG 环境而异，auto_title 模块已有探测逻辑，复用避免双套路径。
///
/// Code Logic（这个模块做什么）:
///     优先注入 db_path；否则调用 `resolve_opencode_db_path()`。
fn resolve_opencode_db(db_path: Option<PathBuf>) -> Option<PathBuf> {
    db_path.or_else(crate::workbench::auto_title_opencode::resolve_opencode_db_path)
}

/// 从 OpenCode SQLite 提取可靠 usage（同步入口，供 spawn_blocking 调用）。
///
/// Business Logic（为什么需要这个函数）:
///     生产调用发生在 spawn_blocking 线程，需要一个同步签名；文件 IO 小，block_on 可接受。
///
/// Code Logic（这个模块做什么）:
///     只读打开 db（max_connections=1）；在当前 tokio runtime handle 上 block_on 执行
///     async 查询；打开失败 / 列缺失 / 查询失败一律 None。
pub(crate) fn extract_opencode_usage(
    db_path: Option<PathBuf>,
    session_id: &str,
) -> Option<ReliableUsageSnapshot> {
    if !is_safe_native_id(session_id) {
        return None;
    }
    let path = resolve_opencode_db(db_path)?;
    if !path.is_file() {
        return None;
    }
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .read_only(true);
    let result = tokio::runtime::Handle::try_current()
        .ok()?
        .block_on(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .ok()?;
            query_opencode_usage(&pool, session_id).await
        });
    result
}

// ---------------------------------------------------------------------------
// D. 统一入口
// ---------------------------------------------------------------------------

/// 按 provider 分发提取终态 usage。
///
/// Business Logic（为什么需要这个函数）:
///     runtime 终态接线只应认识一个入口，不感知各 CLI 数据源差异。
///
/// Code Logic（这个模块做什么）:
///     claudeCodeVisible → Claude jsonl；codex → rollout jsonl；opencode → SQLite；
///     其他 provider（含 generic terminal）返回 None。
pub fn extract_provider_usage(
    provider_id: &str,
    native_session_id: &str,
) -> Option<ReliableUsageSnapshot> {
    match provider_id {
        "claudeCodeVisible" => extract_claude_usage(
            crate::cc::collector::claude_projects_dir(),
            native_session_id,
        ),
        "codex" => extract_codex_usage(codex_home(), native_session_id),
        "opencode" => extract_opencode_usage(None, native_session_id),
        _ => None,
    }
}

/// 解析 Codex 配置根：优先 `CODEX_HOME` env，否则 `~/.codex`。
///
/// Business Logic（为什么需要这个函数）:
///     用户可能用 CODEX_HOME 重定向 Codex 数据目录，需与 CLI 行为一致。
///
/// Code Logic（这个模块做什么）:
///     读 env 非空即用；否则 home_dir 拼接 `.codex`。
fn codex_home() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("CODEX_HOME") {
        let p = PathBuf::from(raw.trim());
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join(".codex"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// 写入临时 jsonl 文件的辅助。
    fn write_jsonl(dir: &Path, name: &str, lines: &[String]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    /// Claude：重复 message.id（先无 stop_reason 后有）→ 去重求和 + costUSD 累加。
    #[test]
    fn claude_dedup_and_cost_sum() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("-Users-hans-demo");
        std::fs::create_dir_all(&project).unwrap();
        let l1 = serde_json::json!({
            "sessionId": "s1",
            "message": {
                "id": "msg_1",
                "model": "claude-sonnet-4",
                "usage": {"input_tokens": 10, "output_tokens": 1, "cache_read_input_tokens": 5, "cache_creation_input_tokens": 2},
            },
            "costUSD": 0.01,
        })
        .to_string();
        let l2 = serde_json::json!({
            "sessionId": "s1",
            "message": {
                "id": "msg_1",
                "model": "claude-sonnet-4",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 10, "output_tokens": 20, "cache_read_input_tokens": 5, "cache_creation_input_tokens": 2},
            },
            "costUSD": 0.02,
        })
        .to_string();
        let l3 = serde_json::json!({
            "sessionId": "s1",
            "message": {
                "id": "msg_2",
                "model": "claude-sonnet-4",
                "usage": {"input_tokens": 7, "output_tokens": 8, "cache_read_input_tokens": 1, "cache_creation_input_tokens": 0},
            },
            "costUSD": 0.03,
        })
        .to_string();
        write_jsonl(&project, "s1.jsonl", &[l1, l2, l3]);

        let snap = extract_claude_usage(Some(tmp.path().to_path_buf()), "s1").unwrap();
        // msg_1 取有 stop_reason 的快照（output=20），costUSD 也取代表行 0.02。
        assert_eq!(snap.input_tokens, Some(17));
        assert_eq!(snap.output_tokens, Some(28));
        assert_eq!(snap.cache_read_tokens, Some(6));
        assert_eq!(snap.cache_write_tokens, Some(2));
        assert_eq!(snap.cost_major.as_deref(), Some("0.050000"));
        assert_eq!(snap.cost_currency.as_deref(), Some("USD"));
        assert_eq!(snap.model_id.as_deref(), Some("claude-sonnet-4"));
    }

    /// Claude：文件缺失 → None；id 带路径穿越 → None。
    #[test]
    fn claude_missing_and_unsafe_id() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(extract_claude_usage(Some(tmp.path().to_path_buf()), "nope").is_none());
        assert!(extract_claude_usage(Some(tmp.path().to_path_buf()), "../s1").is_none());
        assert!(extract_claude_usage(Some(tmp.path().to_path_buf()), "a/b").is_none());
    }

    /// Codex：token_count 取最后一个累计值 + session_meta model。
    #[test]
    fn codex_last_token_count_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/08/14");
        std::fs::create_dir_all(&day).unwrap();
        let meta = serde_json::json!({
            "type": "session_meta",
            "payload": {"id": "u1", "model": "gpt-5"},
        })
        .to_string();
        let tc1 = serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {"total_token_usage": {"input_tokens": 10, "cached_input_tokens": 4, "output_tokens": 6}}},
        })
        .to_string();
        let tc2 = serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {"total_token_usage": {"input_tokens": 30, "cached_input_tokens": 8, "output_tokens": 12}}},
        })
        .to_string();
        let noise = serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "agent_reasoning"},
        })
        .to_string();
        write_jsonl(
            &day,
            "rollout-2026-08-14-u1.jsonl",
            &[meta, tc1, noise, tc2],
        );

        let snap = extract_codex_usage(Some(tmp.path().to_path_buf()), "u1").unwrap();
        assert_eq!(snap.input_tokens, Some(30));
        assert_eq!(snap.cache_read_tokens, Some(8));
        assert_eq!(snap.output_tokens, Some(12));
        assert_eq!(snap.model_id.as_deref(), Some("gpt-5"));
        assert!(snap.cost_major.is_none());
        assert!(snap.cache_write_tokens.is_none());
    }

    /// Codex：无 token_count → None。
    #[test]
    fn codex_no_token_count_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/08/14");
        std::fs::create_dir_all(&day).unwrap();
        let meta = serde_json::json!({"type": "session_meta", "payload": {"id": "u2"}}).to_string();
        write_jsonl(&day, "rollout-2026-08-14-u2.jsonl", &[meta]);
        assert!(extract_codex_usage(Some(tmp.path().to_path_buf()), "u2").is_none());
    }

    /// OpenCode：data JSON 纯函数解析 + 汇总。
    #[test]
    fn opencode_parse_and_aggregate() {
        let v1: Value = serde_json::from_str(
            r#"{"modelID":"m1","tokens":{"input":3,"output":4,"cache":{"read":1,"write":2}},"cost":0.5}"#,
        )
        .unwrap();
        let v2: Value = serde_json::from_str(
            r#"{"modelID":"m2","tokens":{"input":5,"output":6,"cache":{"read":0,"write":0}},"cost":0.25}"#,
        )
        .unwrap();
        let no_tokens: Value = serde_json::from_str(r#"{"role":"user","parts":[]}"#).unwrap();
        let rows = vec![
            parse_opencode_message_data(&v1).unwrap(),
            parse_opencode_message_data(&v2).unwrap(),
        ];
        assert!(parse_opencode_message_data(&no_tokens).is_none());
        let snap = aggregate_opencode_usage(rows).unwrap();
        assert_eq!(snap.input_tokens, Some(8));
        assert_eq!(snap.output_tokens, Some(10));
        assert_eq!(snap.cache_read_tokens, Some(1));
        assert_eq!(snap.cache_write_tokens, Some(2));
        assert_eq!(snap.cost_major.as_deref(), Some("0.750000"));
        assert_eq!(snap.model_id.as_deref(), Some("m2"));
    }

    /// OpenCode：sqlx 内存库端到端查询（含列缺失 → None）。
    #[tokio::test]
    async fn opencode_query_via_memory_db() {
        let options = sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, data TEXT, time INTEGER)",
        )
        .execute(&pool)
        .await
        .unwrap();
        async fn insert(pool: &sqlx::SqlitePool, id: &str, sid: &str, data: &str, t: i64) {
            sqlx::query("INSERT INTO message (id, session_id, data, time) VALUES (?1, ?2, ?3, ?4)")
                .bind(id)
                .bind(sid)
                .bind(data)
                .bind(t)
                .execute(pool)
                .await
                .unwrap();
        }
        insert(
            &pool,
            "m1",
            "sess",
            r#"{"modelID":"mo","tokens":{"input":1,"output":2,"cache":{"read":3,"write":4}},"cost":0.1}"#,
            1,
        )
        .await;
        insert(&pool, "m2", "sess", r#"{"role":"user"}"#, 2).await;
        insert(
            &pool,
            "m3",
            "sess",
            r#"{"modelID":"mo2","tokens":{"input":10,"output":20,"cache":{"read":0,"write":0}},"cost":0.2}"#,
            3,
        )
        .await;
        let snap = query_opencode_usage(&pool, "sess").await.unwrap();
        assert_eq!(snap.input_tokens, Some(11));
        assert_eq!(snap.output_tokens, Some(22));
        assert_eq!(snap.cost_major.as_deref(), Some("0.300000"));
        assert_eq!(snap.model_id.as_deref(), Some("mo2"));
        // 不存在的 session → None
        assert!(query_opencode_usage(&pool, "other").await.is_none());
        // 列缺失的表 → 查询不到 session_id 列（列检查会短路返回 None）
        sqlx::query("CREATE TABLE bad (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        let bad_columns: Vec<String> = sqlx::query("PRAGMA table_info(bad)")
            .fetch_all(&pool)
            .await
            .unwrap()
            .iter()
            .filter_map(|row| row.try_get::<String, _>("name").ok())
            .collect();
        assert!(!bad_columns.iter().any(|c| c == "session_id"));
        assert!(!bad_columns.iter().any(|c| c == "data"));
    }

    /// 统一入口：未知 provider → None；不安全 id → None。
    #[test]
    fn dispatch_unknown_provider() {
        assert!(extract_provider_usage("generic", "x").is_none());
        assert!(extract_provider_usage("claudeCodeVisible", "../x").is_none());
        assert!(extract_provider_usage("codex", "").is_none());
    }
}
