//! workbench/agent_runtime/agent_usage — Agent 终态 usage 提取
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent 使用统计 Ledger 的 tokens 在生产中长期为 null，因为 `note_usage()` 没有调用者。
//!     各 CLI 在会话结束时会把可靠 usage 落到本地 session 文件或 SQLite 中；
//!     runtime 进入终态时应提取这些数据补记 Ledger，让 UI 不再显示「未提供」。
//!
//! Code Logic（这个模块做什么）:
//!     五个纯函数提取器（root 可注入便于测试）+ 统一入口 `extract_provider_usage`：
//!     - Claude：`~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`，按 message.id
//!       去重（stop_reason 优先 / output_tokens 更大者）后求和 billed tokens；
//!       `context_length` 取末条主链占用（input+cache），compact_boundary 后取压缩后占用；
//!     - Codex：`~/.codex/sessions/YYYY/MM/DD/rollout-*<uuid>*.jsonl`，取最后一个
//!       token_count 的 `total_token_usage`（会话累计值）；`context_length` 取
//!       `last_token_usage` occupancy，`context_window` 取 `model_context_window`；
//!     - OpenCode：只读打开 opencode SQLite，按 session 查 message 表 data JSON 求和；
//!       `context_length` 取末条 message occupancy。
//!     - Grok：`~/.grok/sessions/<group>/<session-id>/signals.json` 宽松解析
//!       input/output/cache 与 context 字段；缺字段保持 None，对不上返回 None。
//!     - Gemini：`~/.gemini/tmp/*/chats/*.json` 仅当能稳定读到 input/output/cached
//!       时返回 Some，否则 None。
//!     所有提取有界、宽松解析，失败一律返回 None，不 panic；禁止把缺失写成 0。

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
/// Grok sessions 下最多检查的一级 group 目录数。
const MAX_GROK_GROUP_DIRS: usize = 10_000;
/// Grok sessions 下最多检查的二级 session 目录数。
const MAX_GROK_SESSION_DIRS: usize = 10_000;
/// Gemini tmp 下最多检查的 project hash 目录数。
const MAX_GEMINI_PROJECT_DIRS: usize = 10_000;
/// Gemini chats 下最多检查的 json 文件数。
const MAX_GEMINI_CHAT_FILES: usize = 10_000;

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

/// USD ISO 4217 exponent（分）。provider 提取的 cost 目前都是美元。
const USD_COST_EXPONENT: usize = 2;

/// 把 provider 给出的 f64 金额格式成可无损转为 USD minor units 的十进制字符串。
///
/// Business Logic（为什么需要这个函数）:
///     Ledger 只接受 ISO 4217 exponent 以内的 cost_major。`{:.6}` 会把 `0.05`
///     写成 `0.050000`，USD exponent=2 的无损换算全部失败，会话明细成本变成「—」。
///
/// Code Logic（这个函数做什么）:
///     先格式化为 12 位小数字符串，再按 USD exponent=2 四舍五入并去掉尾随零。
///     调用方在总额为 0 时直接传 None。
fn format_cost(v: f64) -> String {
    if !v.is_finite() || v <= 0.0 {
        return "0".to_string();
    }
    round_decimal_to_exponent(&format!("{v:.12}"), USD_COST_EXPONENT)
        .unwrap_or_else(|| format!("{v:.2}"))
}

/// 去掉十进制字符串小数部分的尾随零（以及空小数点）。
fn trim_trailing_decimal_zeros(major: &str) -> String {
    if !major.contains('.') {
        return major.to_string();
    }
    let trimmed = major.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 将十进制主单位字符串四舍五入到 `exp` 位小数。
///
/// Business Logic（为什么需要这个函数）:
///     provider 的 costUSD 是 f64，常带分以下精度；Ledger 按 ISO 4217 分存储，
///     必须在进入无损换算前把字符串收敛到 exponent，而不是 `f64 * 100`。
///
/// Code Logic（这个函数做什么）:
///     解析 `整数.小数`；超出 `exp` 的下一位 ≥5 则进位；失败返回 None。
fn round_decimal_to_exponent(major: &str, exp: usize) -> Option<String> {
    let s = major.trim();
    if s.is_empty() || s.starts_with('-') || s.contains(['e', 'E', '+']) {
        return None;
    }
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => return Some(s.to_string()),
    };
    if int_part.is_empty() || !int_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !frac_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if frac_part.len() <= exp {
        return Some(trim_trailing_decimal_zeros(&format!(
            "{int_part}.{frac_part}"
        )));
    }
    let keep = &frac_part[..exp];
    let round_up = frac_part.as_bytes().get(exp).is_some_and(|b| *b >= b'5');
    if !round_up {
        return Some(trim_trailing_decimal_zeros(&format!("{int_part}.{keep}")));
    }
    let mut digits: Vec<u8> = int_part.bytes().chain(keep.bytes()).collect();
    if digits.is_empty() {
        digits.push(b'0');
    }
    let mut i = digits.len();
    let mut carry = true;
    while carry && i > 0 {
        i -= 1;
        if digits[i] == b'9' {
            digits[i] = b'0';
        } else {
            digits[i] += 1;
            carry = false;
        }
    }
    if carry {
        digits.insert(0, b'1');
    }
    let rounded = String::from_utf8(digits).ok()?;
    if exp == 0 {
        return Some(trim_trailing_decimal_zeros(&rounded));
    }
    let padded = if rounded.len() <= exp {
        format!("{:0>width$}", rounded, width = exp + 1)
    } else {
        rounded
    };
    let split = padded.len() - exp;
    Some(trim_trailing_decimal_zeros(&format!(
        "{}.{}",
        &padded[..split],
        &padded[split..]
    )))
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
    assistant_ts_ms: Option<i64>,
    user_ts_ms: Option<i64>,
    is_main_chain: bool,
}

impl ClaudeUsageEntry {
    fn occupancy(&self) -> u64 {
        occupancy_tokens(self.input, self.cache_read, self.cache_write)
    }
}

/// 当前上下文占用：input + cache_read + cache_write（不含 output）。
///
/// Business Logic（为什么需要这个函数）:
///     ccstatusline-zh ContextLength 用末轮占用对照 window，不是累计计费 token。
///
/// Code Logic（这个函数做什么）:
///     三端 saturating 相加。
fn occupancy_tokens(input: u64, cache_read: u64, cache_write: u64) -> u64 {
    input.saturating_add(cache_read).saturating_add(cache_write)
}

/// 解析 jsonl `timestamp` 为毫秒。
fn parse_jsonl_ts_ms(value: &Value) -> Option<i64> {
    let raw = value.get("timestamp")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// 合并重叠区间，返回总毫秒（对齐 ccstatusline speed metrics）。
fn merge_intervals_ms(intervals: &[(i64, i64)]) -> Option<u64> {
    if intervals.is_empty() {
        return None;
    }
    let mut sorted = intervals.to_vec();
    sorted.sort_by_key(|(start, _)| *start);
    let mut total = 0i64;
    let (mut cur_start, mut cur_end) = sorted[0];
    for &(start, end) in sorted.iter().skip(1) {
        if start <= cur_end {
            cur_end = cur_end.max(end);
        } else {
            total += cur_end - cur_start;
            cur_start = start;
            cur_end = end;
        }
    }
    total += cur_end - cur_start;
    if total > 0 {
        Some(total as u64)
    } else {
        None
    }
}

/// 区间算术平均（不对重叠做 merge）。
///
/// Business Logic（为什么需要这个函数）:
///     首 token 平均是「用户发出指令 → 本轮第一条助手回复」的均值，
///     不能用合并总时长或工具回环当单次等待。
///
/// Code Logic（这个函数做什么）:
///     只统计 end>start 的区间；平均向下取整毫秒。
fn average_interval_ms(intervals: &[(i64, i64)]) -> Option<u64> {
    let mut sum = 0i64;
    let mut count = 0u64;
    for &(start, end) in intervals {
        if end > start {
            sum += end - start;
            count += 1;
        }
    }
    (sum as u64).checked_div(count)
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
    let mut last_main_msg_id: Option<String> = None;
    let mut last_main_after_compact: Option<String> = None;
    let mut saw_compact = false;
    let mut last_post_compact: Option<u64> = None;
    let mut last_user_ts_ms: Option<i64> = None;
    let mut last_human_ts_ms: Option<i64> = None;
    let mut awaiting_first_assistant = false;
    let mut first_token_intervals: Vec<(i64, i64)> = Vec::new();
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
        if is_claude_compact_boundary(&value) {
            saw_compact = true;
            last_main_after_compact = None;
            last_post_compact = compact_boundary_post_tokens(&value);
            continue;
        }
        let rec_type = value.get("type").and_then(Value::as_str);
        let is_sidechain = value.get("isSidechain") == Some(&Value::Bool(true));
        if rec_type == Some("user") && !is_sidechain {
            if let Some(ts) = parse_jsonl_ts_ms(&value) {
                last_user_ts_ms = Some(ts);
                if is_claude_human_prompt(&value) {
                    last_human_ts_ms = Some(ts);
                    awaiting_first_assistant = true;
                }
            }
            continue;
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
        let mut entry = ClaudeUsageEntry {
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
            assistant_ts_ms: parse_jsonl_ts_ms(&value),
            user_ts_ms: last_user_ts_ms,
            is_main_chain: false,
        };
        matched = true;
        let is_api_error = value.get("isApiErrorMessage") == Some(&Value::Bool(true));
        if !is_sidechain && !is_api_error {
            entry.is_main_chain = true;
            if awaiting_first_assistant {
                if let (Some(human_ts), Some(assistant_ts)) =
                    (last_human_ts_ms, entry.assistant_ts_ms)
                {
                    if assistant_ts > human_ts {
                        first_token_intervals.push((human_ts, assistant_ts));
                    }
                }
                awaiting_first_assistant = false;
            }
            last_main_msg_id = Some(msg_id.to_string());
            if saw_compact {
                last_main_after_compact = Some(msg_id.to_string());
            }
        }
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
    let context_length = if saw_compact {
        last_main_after_compact
            .and_then(|id| messages.get(&id).map(ClaudeUsageEntry::occupancy))
            .or(last_post_compact)
    } else {
        last_main_msg_id.and_then(|id| messages.get(&id).map(ClaudeUsageEntry::occupancy))
    };
    let intervals: Vec<(i64, i64)> = messages
        .values()
        .filter(|entry| entry.is_main_chain)
        .filter_map(|entry| {
            let start = entry.user_ts_ms?;
            let end = entry.assistant_ts_ms?;
            (end > start).then_some((start, end))
        })
        .collect();
    Some(ReliableUsageSnapshot {
        model_id: model_id.clone(),
        input_tokens: Some(input),
        output_tokens: Some(output),
        cache_read_tokens: Some(cache_read),
        cache_write_tokens: Some(cache_write),
        cost_major: cost.as_ref().map(|(m, _)| m.clone()),
        cost_currency: cost.map(|(_, c)| c),
        context_length,
        context_window: infer_context_window_from_model(model_id.as_deref()),
        active_duration_ms: merge_intervals_ms(&intervals),
        first_token_avg_ms: average_interval_ms(&first_token_intervals),
    })
}

/// 判断 jsonl 行是否为用户发出的指令。
///
/// Business Logic（为什么需要这个函数）:
///     首 token 耗时从「用户发出指令」起算，不能把 tool_result 回环当成新指令。
///
/// Code Logic（这个函数做什么）:
///     content 为字符串，或数组含 text 且不含 tool_result → true。
fn is_claude_human_prompt(value: &Value) -> bool {
    let Some(content) = value.get("message").and_then(|m| m.get("content")) else {
        return false;
    };
    if content.as_str().is_some() {
        return true;
    }
    let Some(items) = content.as_array() else {
        return false;
    };
    let has_tool_result = items
        .iter()
        .any(|item| item.get("type").and_then(Value::as_str) == Some("tool_result"));
    let has_text = items.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("text")
            || item.get("text").and_then(Value::as_str).is_some()
    });
    !has_tool_result && has_text
}

/// 从 model id 解析窗口（`[1m]` / 已知 grok 族）；否则 None 交前端查表。
fn infer_context_window_from_model(model_id: Option<&str>) -> Option<u64> {
    let raw = model_id?.trim();
    if raw.is_empty() {
        return None;
    }
    parse_model_window_hint(raw).or_else(|| {
        let lower = raw.to_ascii_lowercase();
        if lower.contains("grok-4.6") {
            Some(1_000_000)
        } else if lower.starts_with("grok-4") {
            Some(256_000)
        } else {
            None
        }
    })
}

/// 解析 model 字符串中的显式窗口提示：`[1M]` / `(200k)`。
fn parse_model_window_hint(model_id: &str) -> Option<u64> {
    let lower = model_id.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if c != b'[' && c != b'(' {
            continue;
        }
        if let Some(n) = parse_k_or_m_number(&lower[i + 1..]) {
            return Some(n);
        }
    }
    None
}

/// 解析 `1m` / `200k` / `1.0m` 前缀。
fn parse_k_or_m_number(s: &str) -> Option<u64> {
    let trimmed = s.trim_start();
    let mut end = 0;
    for (i, ch) in trimmed.char_indices() {
        if ch.is_ascii_digit() || ch == '.' || ch == ',' || ch == '_' {
            end = i + ch.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    let num: f64 = trimmed[..end].replace([',', '_'], "").parse().ok()?;
    if !num.is_finite() || num <= 0.0 {
        return None;
    }
    let unit = trimmed[end..]
        .trim_start()
        .chars()
        .next()?
        .to_ascii_lowercase();
    let mult = match unit {
        'm' => 1_000_000.0,
        'k' => 1_000.0,
        _ => return None,
    };
    Some((num * mult).round() as u64)
}
fn is_claude_compact_boundary(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("system")
        && value.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
        && value.get("isSidechain") != Some(&Value::Bool(true))
}

/// 读取 compact_boundary 的 postTokens（压缩后占用）。
fn compact_boundary_post_tokens(value: &Value) -> Option<u64> {
    let meta = value.get("compactMetadata")?;
    meta.get("postTokens")
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
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
    let mut last_turn: Option<CodexCumulativeUsage> = None;
    let mut context_window: Option<u64> = None;
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
                if let Some(last) = info.get("last_token_usage") {
                    if let Some(parsed) = parse_codex_total_usage(last) {
                        last_turn = Some(parsed);
                    }
                }
                if let Some(window) = info
                    .get("model_context_window")
                    .or_else(|| payload.get("model_context_window"))
                    .and_then(Value::as_u64)
                {
                    if window > 0 {
                        context_window = Some(window);
                    }
                }
            }
            _ => {}
        }
    }
    let usage = latest?;
    let context_length = last_turn.map(|turn| occupancy_tokens(turn.input, turn.cache_read, 0));
    Some(ReliableUsageSnapshot {
        model_id: model_id.clone(),
        input_tokens: Some(usage.input),
        output_tokens: Some(usage.output),
        cache_read_tokens: Some(usage.cache_read),
        cache_write_tokens: None,
        cost_major: None,
        cost_currency: None,
        context_length,
        context_window: context_window
            .or_else(|| infer_context_window_from_model(model_id.as_deref())),
        active_duration_ms: None,
        first_token_avg_ms: None,
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
    let mut context_length: Option<u64> = None;
    for row in rows {
        input += row.input;
        output += row.output;
        cache_read += row.cache_read;
        cache_write += row.cache_write;
        total_cost += row.cost;
        if row.model_id.is_some() {
            model_id = row.model_id.clone();
        }
        context_length = Some(occupancy_tokens(row.input, row.cache_read, row.cache_write));
    }
    let cost = if total_cost > 0.0 {
        Some((format_cost(total_cost), "USD".to_string()))
    } else {
        None
    };
    Some(ReliableUsageSnapshot {
        model_id: model_id.clone(),
        input_tokens: Some(input),
        output_tokens: Some(output),
        cache_read_tokens: Some(cache_read),
        cache_write_tokens: Some(cache_write),
        cost_major: cost.as_ref().map(|(m, _)| m.clone()),
        cost_currency: cost.map(|(_, c)| c),
        context_length,
        context_window: infer_context_window_from_model(model_id.as_deref()),
        active_duration_ms: None,
        first_token_avg_ms: None,
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
// D. 宽松 token 字段 + Grok
// ---------------------------------------------------------------------------

/// 从磁盘读取有界 JSON 对象；空文件 / 非对象 / 超限一律 None。
///
/// Business Logic（为什么需要这个函数）:
///     Grok signals 与 Gemini chat 都是单文件 JSON，读取必须有上限且不得 panic。
///
/// Code Logic（这个函数做什么）:
///     文件 ≤64MiB；trim 后反序列化为 object。
fn read_json_object(path: &Path) -> Option<Value> {
    let mut file = fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if meta.len() > MAX_JSONL_FILE_BYTES {
        return None;
    }
    let mut content = String::new();
    file.read_to_string(&mut content).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    value.is_object().then_some(value)
}

/// 把 JSON 数值宽松读成非负 token 计数；缺席或非法返回 None。
///
/// Business Logic（为什么需要这个函数）:
///     禁止把缺失字段写成 0；只有显式出现的非负数才可信。
///
/// Code Logic（这个函数做什么）:
///     接受 u64 / 非负 i64 / 有限非负 f64（四舍五入）。
fn as_nonneg_u64(value: &Value) -> Option<u64> {
    if let Some(n) = value.as_u64() {
        return Some(n);
    }
    if let Some(n) = value.as_i64() {
        return (n >= 0).then_some(n as u64);
    }
    if let Some(f) = value.as_f64() {
        if f.is_finite() && f >= 0.0 {
            return Some(f.round() as u64);
        }
    }
    None
}

/// 按候选键读取第一个可解析的非负 token。
fn json_token_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(as_nonneg_u64))
}

/// 宽松 usage 字段（缺席保持 None，不填 0）。
#[derive(Debug, Clone, Default)]
struct LooseUsageFields {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    model_id: Option<String>,
    context_length: Option<u64>,
    context_window: Option<u64>,
    cost_usd: Option<f64>,
    first_token_avg_ms: Option<u64>,
}

impl LooseUsageFields {
    fn has_token_dim(&self) -> bool {
        self.input.is_some()
            || self.output.is_some()
            || self.cache_read.is_some()
            || self.cache_write.is_some()
            || self.context_length.is_some()
            || self.context_window.is_some()
    }

    fn has_gemini_stable(&self) -> bool {
        self.input.is_some() || self.output.is_some() || self.cache_read.is_some()
    }

    fn fill_missing_from(&mut self, other: &Self) {
        if self.input.is_none() {
            self.input = other.input;
        }
        if self.output.is_none() {
            self.output = other.output;
        }
        if self.cache_read.is_none() {
            self.cache_read = other.cache_read;
        }
        if self.cache_write.is_none() {
            self.cache_write = other.cache_write;
        }
        if self.model_id.is_none() {
            self.model_id = other.model_id.clone();
        }
        if self.context_length.is_none() {
            self.context_length = other.context_length;
        }
        if self.context_window.is_none() {
            self.context_window = other.context_window;
        }
        if self.cost_usd.is_none() {
            self.cost_usd = other.cost_usd;
        }
        if self.first_token_avg_ms.is_none() {
            self.first_token_avg_ms = other.first_token_avg_ms;
        }
    }

    fn add_assign(&mut self, other: &Self) {
        self.input = sum_opt_u64(self.input, other.input);
        self.output = sum_opt_u64(self.output, other.output);
        self.cache_read = sum_opt_u64(self.cache_read, other.cache_read);
        self.cache_write = sum_opt_u64(self.cache_write, other.cache_write);
        if other.model_id.is_some() {
            self.model_id = other.model_id.clone();
        }
        if other.context_length.is_some() {
            self.context_length = other.context_length;
        }
        if other.context_window.is_some() {
            self.context_window = other.context_window;
        }
        self.cost_usd = match (self.cost_usd, other.cost_usd) {
            (None, None) => None,
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (Some(a), Some(b)) => Some(a + b),
        };
        if other.first_token_avg_ms.is_some() {
            self.first_token_avg_ms = other.first_token_avg_ms;
        }
    }

    fn into_snapshot(self) -> ReliableUsageSnapshot {
        let model_id = self.model_id.filter(|s| !s.is_empty());
        let cost = self
            .cost_usd
            .filter(|c| *c > 0.0)
            .map(|c| (format_cost(c), "USD".to_string()));
        ReliableUsageSnapshot {
            model_id: model_id.clone(),
            input_tokens: self.input,
            output_tokens: self.output,
            cache_read_tokens: self.cache_read,
            cache_write_tokens: self.cache_write,
            cost_major: cost.as_ref().map(|(m, _)| m.clone()),
            cost_currency: cost.map(|(_, c)| c),
            context_length: self.context_length,
            context_window: self
                .context_window
                .or_else(|| infer_context_window_from_model(model_id.as_deref())),
            active_duration_ms: None,
            first_token_avg_ms: self.first_token_avg_ms,
        }
    }
}

fn sum_opt_u64(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => Some(x.saturating_add(y)),
    }
}

/// 从 JSON 对象宽松读取 token / model / context；缺席保持 None。
///
/// Business Logic（为什么需要这个函数）:
///     Grok signals 与 Gemini chat 字段名不稳定，只能认常见别名，不能把缺失写成 0。
///
/// Code Logic（这个函数做什么）:
///     识别 input/output/cache、prompt/completion、usageMetadata 与 Grok context 键。
fn read_loose_usage(value: &Value) -> LooseUsageFields {
    let mut fields = LooseUsageFields {
        input: json_token_u64(
            value,
            &[
                "input",
                "input_tokens",
                "inputTokens",
                "prompt",
                "prompt_tokens",
                "promptTokens",
                "promptTokenCount",
                "prompt_token_count",
            ],
        ),
        output: json_token_u64(
            value,
            &[
                "output",
                "output_tokens",
                "outputTokens",
                "completion",
                "completion_tokens",
                "completionTokens",
                "candidatesTokenCount",
                "candidates_token_count",
            ],
        ),
        cache_read: None,
        cache_write: json_token_u64(
            value,
            &[
                "cache_write",
                "cache_write_tokens",
                "cacheWriteTokens",
                "cache_creation_input_tokens",
            ],
        ),
        model_id: [
            "primaryModelId",
            "model_id",
            "modelId",
            "current_model_id",
            "model",
        ]
        .iter()
        .find_map(|key| {
            value
                .get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        }),
        context_length: json_token_u64(
            value,
            &[
                "contextTokensUsed",
                "context_tokens_used",
                "context_length",
                "contextLength",
            ],
        ),
        context_window: json_token_u64(
            value,
            &[
                "contextWindowTokens",
                "context_window_tokens",
                "context_window",
                "contextWindow",
                "model_context_window",
            ],
        ),
        cost_usd: value
            .get("costUSD")
            .or_else(|| value.get("cost_usd"))
            .or_else(|| value.get("cost"))
            .and_then(Value::as_f64),
        first_token_avg_ms: json_token_u64(
            value,
            &[
                "avgTimeToFirstTokenMs",
                "first_token_avg_ms",
                "firstTokenAvgMs",
            ],
        ),
    };
    if let Some(cache) = value.get("cache") {
        if cache.is_object() {
            if fields.cache_read.is_none() {
                fields.cache_read = cache
                    .get("read")
                    .or_else(|| cache.get("cache_read"))
                    .and_then(as_nonneg_u64);
            }
            if fields.cache_write.is_none() {
                fields.cache_write = cache
                    .get("write")
                    .or_else(|| cache.get("cache_write"))
                    .and_then(as_nonneg_u64);
            }
        } else if fields.cache_read.is_none() {
            fields.cache_read = as_nonneg_u64(cache);
        }
    }
    if fields.cache_read.is_none() {
        fields.cache_read = json_token_u64(
            value,
            &[
                "cached",
                "cache_read",
                "cache_read_tokens",
                "cacheReadTokens",
                "cached_input_tokens",
                "cachedInputTokens",
                "cached_tokens",
                "cachedTokens",
                "cachedContentTokenCount",
                "cached_content_token_count",
            ],
        );
    }
    if fields.model_id.is_none() {
        if let Some(first) = value
            .get("modelsUsed")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter()
                    .find_map(|item| item.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
        {
            fields.model_id = Some(first.to_string());
        }
    }
    fields
}

/// 解析 Grok 配置根：注入路径 / `GROK_HOME` / `~/.grok`。
///
/// Business Logic（为什么需要这个函数）:
///     Grok CLI 可用 `GROK_HOME` 重定向数据目录，测试则注入临时根。
///
/// Code Logic（这个函数做什么）:
///     非空注入优先，其次非空 `GROK_HOME`，否则 home 下 `.grok`。
fn resolve_grok_home(injected: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(p) = injected {
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    if let Ok(raw) = std::env::var("GROK_HOME") {
        let p = PathBuf::from(raw.trim());
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join(".grok"))
}

/// 从 Grok `signals.json` 提取可靠 usage。
///
/// Business Logic（为什么需要这个函数）:
///     Grok Build 把 token / context 写在 session 目录的 `signals.json`；
///     ledger 只能读这份磁盘真值，抽不到必须返回 None，禁止把缺失写成 0。
///
/// Code Logic（这个函数做什么）:
///     校验 native id 后，在 `sessions/<group>/<session-id>/signals.json` 有界查找；
///     宽松解析 input/output/cache 与 context 字段，缺字段保持 None。
pub(crate) fn extract_grok_usage(
    grok_home: Option<PathBuf>,
    native_session_id: &str,
) -> Option<ReliableUsageSnapshot> {
    if !is_safe_native_id(native_session_id) {
        return None;
    }
    let root = resolve_grok_home(grok_home)?;
    let target = find_grok_signals_file(&root.join("sessions"), native_session_id)?;
    parse_grok_signals(&target)
}

/// 在 `sessions` 下按一级 group、二级 session 目录有界查找 `signals.json`。
///
/// Business Logic（为什么需要这个函数）:
///     group 名是 url-encoded cwd，不能从 session id 反推，但不得无限递归。
///
/// Code Logic（这个函数做什么）:
///     先试 `<group>/<id>/signals.json`，再扫二级目录名等于 native id 的项。
fn find_grok_signals_file(sessions: &Path, native_session_id: &str) -> Option<PathBuf> {
    let groups = fs::read_dir(sessions).ok()?;
    let mut checked_sessions = 0usize;
    for (i, group) in groups.enumerate() {
        if i >= MAX_GROK_GROUP_DIRS {
            break;
        }
        let Ok(group) = group else { continue };
        if !group.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let direct = group.path().join(native_session_id).join("signals.json");
        if direct.is_file() {
            return Some(direct);
        }
        let Ok(session_dirs) = fs::read_dir(group.path()) else {
            continue;
        };
        for entry in session_dirs {
            if checked_sessions >= MAX_GROK_SESSION_DIRS {
                return None;
            }
            checked_sessions += 1;
            let Ok(entry) = entry else { continue };
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if entry.file_name().to_str() != Some(native_session_id) {
                continue;
            }
            let candidate = entry.path().join("signals.json");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// 解析 Grok `signals.json`。
///
/// Business Logic（为什么需要这个函数）:
///     字段名可能在根级或 tokens/usage 嵌套对象里；对不上任何 token 维则整文件丢弃。
///
/// Code Logic（这个函数做什么）:
///     优先读 tokens/usage 子对象，再用根级补缺；无 token/context 维返回 None。
fn parse_grok_signals(path: &Path) -> Option<ReliableUsageSnapshot> {
    let value = read_json_object(path)?;
    let nested = ["tokens", "usage", "token_usage", "tokenUsage", "signals"]
        .iter()
        .find_map(|key| {
            let obj = value.get(*key)?;
            let parsed = read_loose_usage(obj);
            parsed.has_token_dim().then_some(parsed)
        });
    let mut fields = nested.unwrap_or_default();
    fields.fill_missing_from(&read_loose_usage(&value));
    if !fields.has_token_dim() {
        return None;
    }
    Some(fields.into_snapshot())
}

// ---------------------------------------------------------------------------
// E. Gemini
// ---------------------------------------------------------------------------

/// 解析 Gemini 配置根：注入路径或 `~/.gemini`。
///
/// Business Logic（为什么需要这个函数）:
///     测试注入临时根；生产读用户主目录下的 Gemini CLI 数据。
///
/// Code Logic（这个函数做什么）:
///     非空注入优先，否则 `home/.gemini`。
fn resolve_gemini_home(injected: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(p) = injected {
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join(".gemini"))
}

/// 从 Gemini chat/session JSON 提取可靠 usage。
///
/// Business Logic（为什么需要这个函数）:
///     Gemini CLI 会话 JSON 并不保证有 token 字段；只有稳定读到
///     input/output 或 cached 时才能写入 ledger，否则显示「未提供」。
///
/// Code Logic（这个函数做什么）:
///     校验 native id 后扫描 `{root}/tmp/*/chats/*.json`，按 `id` /
///     `sessionId` / 文件 stem 匹配；抽不到稳定字段返回 None。
pub(crate) fn extract_gemini_usage(
    gemini_home: Option<PathBuf>,
    native_session_id: &str,
) -> Option<ReliableUsageSnapshot> {
    if !is_safe_native_id(native_session_id) {
        return None;
    }
    let root = resolve_gemini_home(gemini_home)?;
    let target = find_gemini_chat_file(&root, native_session_id)?;
    parse_gemini_chat(&target, native_session_id)
}

/// 在 `{home}/tmp/*/chats/*.json` 有界查找匹配 session 的文件。
///
/// Business Logic（为什么需要这个函数）:
///     project hash 不能猜，只能枚举 tmp 下的 chats，且必须设上限。
///
/// Code Logic（这个函数做什么）:
///     先试 `<id>.json` 文件名，再读 `id` / `sessionId`；超过文件上限放弃。
fn find_gemini_chat_file(gemini_home: &Path, native_session_id: &str) -> Option<PathBuf> {
    let tmp = gemini_home.join("tmp");
    let projects = fs::read_dir(&tmp).ok()?;
    let mut checked_files = 0usize;
    for (i, project) in projects.enumerate() {
        if i >= MAX_GEMINI_PROJECT_DIRS {
            break;
        }
        let Ok(project) = project else { continue };
        if !project.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let chats = project.path().join("chats");
        if !chats.is_dir() {
            continue;
        }
        let exact = chats.join(format!("{native_session_id}.json"));
        if exact.is_file() {
            return Some(exact);
        }
        let Ok(entries) = fs::read_dir(&chats) else {
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            checked_files += 1;
            if checked_files > MAX_GEMINI_CHAT_FILES {
                return None;
            }
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if path.file_stem().and_then(|s| s.to_str()) == Some(native_session_id) {
                return Some(path);
            }
            if gemini_json_session_id(&path).as_deref() == Some(native_session_id) {
                return Some(path);
            }
        }
    }
    None
}

/// 读取 Gemini chat JSON 内的 session id（`id` / `sessionId`）。
fn gemini_json_session_id(path: &Path) -> Option<String> {
    let value = read_json_object(path)?;
    value
        .get("id")
        .or_else(|| value.get("sessionId"))
        .or_else(|| value.pointer("/session/id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// 解析 Gemini chat JSON 的稳定 usage。
///
/// Business Logic（为什么需要这个函数）:
///     只接受能稳定对应 input/output/cached 的字段；无这些键则整文件 None。
///
/// Code Logic（这个函数做什么）:
///     先看根级 usage/tokens/usageMetadata；否则累加 messages/history 中的同类对象。
fn parse_gemini_chat(path: &Path, expected_session: &str) -> Option<ReliableUsageSnapshot> {
    let value = read_json_object(path)?;
    if let Some(sid) = value
        .get("id")
        .or_else(|| value.get("sessionId"))
        .or_else(|| value.pointer("/session/id"))
        .and_then(Value::as_str)
    {
        let stem_ok = path.file_stem().and_then(|s| s.to_str()) == Some(expected_session);
        if sid != expected_session && !stem_ok {
            return None;
        }
    }
    collect_gemini_usage(&value).map(LooseUsageFields::into_snapshot)
}

/// 收集 Gemini JSON 中可稳定识别的 input/output/cached。
fn collect_gemini_usage(value: &Value) -> Option<LooseUsageFields> {
    let candidates = [
        value.get("usage"),
        value.get("tokens"),
        value.get("usageMetadata"),
        value.pointer("/metrics/tokens"),
        value.pointer("/session/usage"),
        value.pointer("/session/tokens"),
        value.pointer("/session/usageMetadata"),
    ];
    for candidate in candidates.into_iter().flatten() {
        let mut fields = read_loose_usage(candidate);
        if fields.has_gemini_stable() {
            fields.fill_missing_from(&read_loose_usage(value));
            return Some(fields);
        }
    }
    let messages = value
        .get("messages")
        .or_else(|| value.get("history"))
        .and_then(Value::as_array)?;
    let mut acc = LooseUsageFields::default();
    let mut any = false;
    for msg in messages {
        let part_src = msg
            .get("usage")
            .or_else(|| msg.get("tokens"))
            .or_else(|| msg.get("usageMetadata"))
            .unwrap_or(msg);
        let part = read_loose_usage(part_src);
        if part.has_gemini_stable() {
            acc.add_assign(&part);
            any = true;
        }
    }
    if !any {
        return None;
    }
    acc.fill_missing_from(&read_loose_usage(value));
    Some(acc)
}

// ---------------------------------------------------------------------------
// F. 统一入口
// ---------------------------------------------------------------------------

/// 判断 provider 是否可从 CLI 本地会话文件/库提取 usage。
///
/// Business Logic（为什么需要这个函数）:
///     交互式行的 wire id 是 `codexVisible` / `grokBuildVisible` 等，历史抽取入口
///     还认 catalog 短码；漏掉任一别名会导致该 provider 永远抽不到 tokens。
///
/// Code Logic（这个函数做什么）:
///     接受 Claude / Codex / OpenCode / Grok / Gemini 的稳定 id 与历史别名。
pub fn is_usage_extractable_provider(provider_id: &str) -> bool {
    matches!(
        provider_id,
        "claudeCodeVisible"
            | "codex"
            | "codexVisible"
            | "opencode"
            | "openCodeVisible"
            | "grokBuildVisible"
            | "grok"
            | "geminiCliVisible"
            | "gemini"
            | "cursorCliVisible"
            | "cursor"
            | "piVisible"
            | "pi"
    )
}

/// 按 provider 分发提取 usage（终态 Ledger 与 live 投影共用）。
///
/// Business Logic（为什么需要这个函数）:
///     runtime 接线只应认识一个入口，不感知各 CLI 数据源差异。
///
/// Code Logic（这个模块做什么）:
///     claudeCodeVisible → Claude jsonl；codex/codexVisible → rollout jsonl；
///     opencode/openCodeVisible → SQLite；grokBuildVisible/grok → signals.json；
///     geminiCliVisible/gemini → chat JSON；其他 provider 返回 None。
pub fn extract_provider_usage(
    provider_id: &str,
    native_session_id: &str,
) -> Option<ReliableUsageSnapshot> {
    match provider_id {
        "claudeCodeVisible" => extract_claude_usage(
            crate::cc::collector::claude_projects_dir(),
            native_session_id,
        ),
        "codex" | "codexVisible" => extract_codex_usage(codex_home(), native_session_id),
        "opencode" | "openCodeVisible" => extract_opencode_usage(None, native_session_id),
        "grokBuildVisible" | "grok" => extract_grok_usage(None, native_session_id),
        "geminiCliVisible" | "gemini" => extract_gemini_usage(None, native_session_id),
        _ => None,
    }
}

/// 定位 CLI 会话文件（Claude jsonl / Codex rollout / Grok signals / Gemini chat）。
///
/// Business Logic（为什么需要这个函数）:
///     live 轮询必须缓存路径，避免每 2s 遍历最多 10_000 个项目目录。
///
/// Code Logic（这个函数做什么）:
///     校验 native id 后按 provider 调用既有有界查找；找不到或非文件源返回 None。
pub(crate) fn locate_provider_session_file(
    provider_id: &str,
    native_session_id: &str,
) -> Option<PathBuf> {
    if !is_safe_native_id(native_session_id) {
        return None;
    }
    match provider_id {
        "claudeCodeVisible" => {
            let root = crate::cc::collector::claude_projects_dir()?;
            find_claude_session_file(&root, &format!("{native_session_id}.jsonl"))
        }
        "codex" | "codexVisible" => {
            find_codex_rollout_file(&codex_home()?.join("sessions"), native_session_id)
        }
        "grokBuildVisible" | "grok" => find_grok_signals_file(
            &resolve_grok_home(None)?.join("sessions"),
            native_session_id,
        ),
        "geminiCliVisible" | "gemini" => {
            find_gemini_chat_file(&resolve_gemini_home(None)?, native_session_id)
        }
        _ => None,
    }
}

/// 从已定位的会话文件（或 OpenCode SQLite）提取 usage。
///
/// Business Logic（为什么需要这个函数）:
///     live cache 在路径命中后应跳过目录遍历，只重解析变更文件。
///
/// Code Logic（这个函数做什么）:
///     Claude/Codex/Grok/Gemini 解析给定 path；OpenCode 忽略 path 走 SQLite。
pub(crate) fn extract_provider_usage_from_path(
    provider_id: &str,
    path: &Path,
    native_session_id: &str,
) -> Option<ReliableUsageSnapshot> {
    match provider_id {
        "claudeCodeVisible" => parse_claude_jsonl(path, native_session_id),
        "codex" | "codexVisible" => parse_codex_jsonl(path),
        "opencode" | "openCodeVisible" => extract_opencode_usage(None, native_session_id),
        "grokBuildVisible" | "grok" => parse_grok_signals(path),
        "geminiCliVisible" | "gemini" => parse_gemini_chat(path, native_session_id),
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
    use crate::workbench::agent_ledger::models::convert_major_to_minor_units;
    use std::str::FromStr;

    /// Business Logic: 提取出的 cost_major 必须能按 USD 分无损入库，否则明细全是「—」。
    #[test]
    fn format_cost_is_convertible_to_usd_cents() {
        assert_eq!(format_cost(0.05), "0.05");
        assert_eq!(
            convert_major_to_minor_units(&format_cost(0.05), "USD").unwrap(),
            5
        );
        assert_eq!(format_cost(0.012345), "0.01");
        assert_eq!(
            convert_major_to_minor_units(&format_cost(0.012345), "USD").unwrap(),
            1
        );
        assert_eq!(format_cost(0.75), "0.75");
        assert_eq!(
            convert_major_to_minor_units(&format_cost(0.75), "USD").unwrap(),
            75
        );
    }

    /// Business Logic: 分以下精度按十进制字符串四舍五入，不走 f64*100。
    #[test]
    fn round_decimal_to_exponent_half_up() {
        assert_eq!(
            round_decimal_to_exponent("0.015", 2).as_deref(),
            Some("0.02")
        );
        assert_eq!(
            round_decimal_to_exponent("0.014", 2).as_deref(),
            Some("0.01")
        );
        assert_eq!(
            round_decimal_to_exponent("0.050000", 2).as_deref(),
            Some("0.05")
        );
        assert_eq!(round_decimal_to_exponent("9.995", 2).as_deref(), Some("10"));
    }

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
        assert_eq!(snap.cost_major.as_deref(), Some("0.05"));
        assert_eq!(snap.cost_currency.as_deref(), Some("USD"));
        assert_eq!(snap.model_id.as_deref(), Some("claude-sonnet-4"));
        // 末轮占用 = msg_2 的 7+1+0，不是累计 17+6+2。
        assert_eq!(snap.context_length, Some(8));
        assert_eq!(snap.context_window, None);
    }

    /// Claude：compact_boundary 后占用取压缩后一轮，禁止泄漏压缩前占用。
    #[test]
    fn claude_context_length_uses_post_compact_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("-Users-hans-demo");
        std::fs::create_dir_all(&project).unwrap();
        let pre = serde_json::json!({
            "sessionId": "s-c",
            "timestamp": "2026-08-16T10:00:00Z",
            "message": {
                "id": "msg_pre",
                "model": "claude-sonnet-4",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 80_000, "output_tokens": 10, "cache_read_input_tokens": 20_000, "cache_creation_input_tokens": 0},
            },
        })
        .to_string();
        let boundary = serde_json::json!({
            "type": "system",
            "subtype": "compact_boundary",
            "sessionId": "s-c",
            "compactMetadata": {"postTokens": 12_000, "preTokens": 100_000},
        })
        .to_string();
        write_jsonl(&project, "s-c.jsonl", &[pre, boundary]);
        let snap = extract_claude_usage(Some(tmp.path().to_path_buf()), "s-c").unwrap();
        assert_eq!(snap.input_tokens, Some(80_000));
        assert_eq!(snap.context_length, Some(12_000));
    }

    /// 有效生成时长 = 用户→助手区间，不是墙钟；grok-4.6-build 窗口为 1M。
    #[test]
    fn claude_active_duration_and_grok_window() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("-Users-hans-demo");
        std::fs::create_dir_all(&project).unwrap();
        let user = serde_json::json!({
            "type": "user",
            "sessionId": "s-g",
            "timestamp": "2026-08-16T10:00:00.000Z",
            "message": {"role": "user", "content": "hi"},
        })
        .to_string();
        let asst = serde_json::json!({
            "type": "assistant",
            "sessionId": "s-g",
            "timestamp": "2026-08-16T10:00:10.000Z",
            "message": {
                "id": "msg_g",
                "model": "grok-4.6-build",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 100, "output_tokens": 20, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0},
            },
        })
        .to_string();
        write_jsonl(&project, "s-g.jsonl", &[user, asst]);
        let snap = extract_claude_usage(Some(tmp.path().to_path_buf()), "s-g").unwrap();
        assert_eq!(snap.active_duration_ms, Some(10_000));
        assert_eq!(snap.first_token_avg_ms, Some(10_000));
        assert_eq!(snap.context_window, Some(1_000_000));
        assert_eq!(snap.model_id.as_deref(), Some("grok-4.6-build"));
    }

    /// 首 token 只计用户指令到本轮第一条助手回复，忽略 tool_result 回环。
    #[test]
    fn claude_first_token_ignores_tool_loop() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("-Users-hans-demo");
        std::fs::create_dir_all(&project).unwrap();
        let human = serde_json::json!({
            "type": "user",
            "sessionId": "s-ttft",
            "timestamp": "2026-08-16T10:00:00.000Z",
            "message": {"role": "user", "content": "do it"},
        })
        .to_string();
        let first = serde_json::json!({
            "type": "assistant",
            "sessionId": "s-ttft",
            "timestamp": "2026-08-16T10:00:05.000Z",
            "message": {
                "id": "msg_a",
                "model": "claude-sonnet-4-5",
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 10, "output_tokens": 4, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0},
            },
        })
        .to_string();
        let tool_user = serde_json::json!({
            "type": "user",
            "sessionId": "s-ttft",
            "timestamp": "2026-08-16T10:00:06.000Z",
            "message": {"role": "user", "content": [{"type": "tool_result", "content": "ok"}]},
        })
        .to_string();
        let second = serde_json::json!({
            "type": "assistant",
            "sessionId": "s-ttft",
            "timestamp": "2026-08-16T10:00:20.000Z",
            "message": {
                "id": "msg_b",
                "model": "claude-sonnet-4-5",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 10, "output_tokens": 4, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0},
            },
        })
        .to_string();
        write_jsonl(&project, "s-ttft.jsonl", &[human, first, tool_user, second]);
        let snap = extract_claude_usage(Some(tmp.path().to_path_buf()), "s-ttft").unwrap();
        assert_eq!(snap.first_token_avg_ms, Some(5_000));
        assert_eq!(snap.active_duration_ms, Some(19_000));
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
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {"input_tokens": 30, "cached_input_tokens": 8, "output_tokens": 12},
                    "last_token_usage": {"input_tokens": 9, "cached_input_tokens": 8, "output_tokens": 4},
                    "model_context_window": 400000
                }
            },
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
        assert_eq!(snap.context_length, Some(17));
        assert_eq!(snap.context_window, Some(400_000));
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
        assert_eq!(snap.cost_major.as_deref(), Some("0.75"));
        assert_eq!(snap.model_id.as_deref(), Some("m2"));
        // 末条 5+0+0，不是累计 8+1+2。
        assert_eq!(snap.context_length, Some(5));
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
        assert_eq!(snap.cost_major.as_deref(), Some("0.3"));
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
        assert!(extract_provider_usage("codexVisible", "").is_none());
    }

    /// 交互式 wire id 必须与历史短码一样可抽取。
    #[test]
    fn extractable_provider_aliases() {
        assert!(is_usage_extractable_provider("claudeCodeVisible"));
        assert!(is_usage_extractable_provider("codex"));
        assert!(is_usage_extractable_provider("codexVisible"));
        assert!(is_usage_extractable_provider("opencode"));
        assert!(is_usage_extractable_provider("openCodeVisible"));
        assert!(is_usage_extractable_provider("grokBuildVisible"));
        assert!(is_usage_extractable_provider("grok"));
        assert!(is_usage_extractable_provider("geminiCliVisible"));
        assert!(is_usage_extractable_provider("gemini"));
        assert!(!is_usage_extractable_provider("genericTerminal"));
    }

    /// Grok：signals.json 抽出 tokens；缺字段保持 None。
    #[test]
    fn grok_signals_extracts_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("sessions/encoded-cwd/sess-g");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            session.join("signals.json"),
            serde_json::json!({
                "input_tokens": 11,
                "output_tokens": 22,
                "cache_read_tokens": 3,
                "primaryModelId": "grok-4.6",
                "contextTokensUsed": 100,
                "contextWindowTokens": 500000,
                "avgTimeToFirstTokenMs": 1629
            })
            .to_string(),
        )
        .unwrap();

        let snap = extract_grok_usage(Some(tmp.path().to_path_buf()), "sess-g").unwrap();
        assert_eq!(snap.input_tokens, Some(11));
        assert_eq!(snap.output_tokens, Some(22));
        assert_eq!(snap.cache_read_tokens, Some(3));
        assert!(snap.cache_write_tokens.is_none());
        assert_eq!(snap.model_id.as_deref(), Some("grok-4.6"));
        assert_eq!(snap.context_length, Some(100));
        assert_eq!(snap.context_window, Some(500000));
        assert_eq!(snap.first_token_avg_ms, Some(1629));
    }

    /// Grok：缺文件 / 空 json / 不安全 id → None。
    #[test]
    fn grok_missing_or_empty_signals_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(extract_grok_usage(Some(tmp.path().to_path_buf()), "nope").is_none());

        let session = tmp.path().join("sessions/g/empty-s");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("signals.json"), "{}").unwrap();
        assert!(extract_grok_usage(Some(tmp.path().to_path_buf()), "empty-s").is_none());

        std::fs::write(session.join("signals.json"), "").unwrap();
        assert!(extract_grok_usage(Some(tmp.path().to_path_buf()), "empty-s").is_none());

        assert!(extract_grok_usage(Some(tmp.path().to_path_buf()), "../x").is_none());
        assert!(extract_grok_usage(Some(tmp.path().to_path_buf()), "a/b").is_none());
    }

    /// Gemini：稳定 input/output/cached 才抽取；按 sessionId 匹配。
    #[test]
    fn gemini_extracts_stable_usage() {
        let tmp = tempfile::tempdir().unwrap();
        let chats = tmp.path().join("tmp/proj-hash/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(
            chats.join("chat-001.json"),
            serde_json::json!({
                "sessionId": "sess-gem",
                "usageMetadata": {
                    "promptTokenCount": 8,
                    "candidatesTokenCount": 13,
                    "cachedContentTokenCount": 2
                },
                "model": "gemini-2.5-pro"
            })
            .to_string(),
        )
        .unwrap();

        let snap = extract_gemini_usage(Some(tmp.path().to_path_buf()), "sess-gem").unwrap();
        assert_eq!(snap.input_tokens, Some(8));
        assert_eq!(snap.output_tokens, Some(13));
        assert_eq!(snap.cache_read_tokens, Some(2));
        assert!(snap.cache_write_tokens.is_none());
        assert_eq!(snap.model_id.as_deref(), Some("gemini-2.5-pro"));
    }

    /// Gemini：无 token 字段 → None。
    #[test]
    fn gemini_without_token_fields_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let chats = tmp.path().join("tmp/proj/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(
            chats.join("sess-g.json"),
            serde_json::json!({
                "id": "sess-g",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        )
        .unwrap();
        assert!(extract_gemini_usage(Some(tmp.path().to_path_buf()), "sess-g").is_none());
        assert!(extract_gemini_usage(Some(tmp.path().to_path_buf()), "../x").is_none());
    }
}
