//! Claude Code visible runtime association.
//!
//! Business Logic（为什么需要这个模块）:
//!     Orchestrator 可见 Runner 会在 Workbench terminal 中启动 Claude Code，任务详情需要尽量关联
//!     Claude Code 自身的 session/transcript/runtime 信息，方便用户审计和接管。
//!
//! Code Logic（这个模块做什么）:
//!     扫描本机 Claude Code `~/.claude/projects` 下的 jsonl transcript，按 jsonl 行中的 cwd 匹配
//!     当前任务 worktree，并提取最新 session、transcript 路径和最后运行时事件摘要；扫描失败时保持 best-effort。

use crate::error::AppError;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

const MAX_RUNTIME_MESSAGE_CHARS: usize = 500;

/// Claude Code 可见运行时摘要。
///
/// Business Logic（为什么需要这个结构体）:
///     任务行需要保存 Claude Code 自身 runtime 线索，帮助用户从 Orchestrator 任务追溯到真实 Claude 会话。
///
/// Code Logic（这个结构体做什么）:
///     保存 session id、transcript path、最后活动时间、最后事件类型与最后可读消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeRuntimeSummary {
    pub claude_session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub last_activity_at: Option<String>,
    pub last_runtime_event: Option<String>,
    pub last_runtime_message: Option<String>,
}

/// Claude Code jsonl 单行的宽松解析结构。
///
/// Business Logic（为什么需要这个结构体）:
///     Claude Code transcript 字段会随 CLI 版本变化，runtime association 只能依赖 cwd/session/timestamp/message 等稳定线索。
///
/// Code Logic（这个结构体做什么）:
///     用 serde default 忽略未知字段，保留顶层 type/cwd/timestamp/sessionId 和 message.content。
#[derive(Debug, Default, Deserialize)]
struct ClaudeRuntimeLine {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    message: Option<ClaudeRuntimeMessage>,
}

/// Claude Code jsonl message 字段的宽松解析结构。
///
/// Business Logic（为什么需要这个结构体）:
///     任务详情只需要展示最后一条可读消息，不应因 message 字段带有 role 或复杂 content 结构而解析失败。
///
/// Code Logic（这个结构体做什么）:
///     保留 role 与任意 JSON content，content 后续通过 helper 提取字符串摘要。
#[derive(Debug, Default, Deserialize)]
struct ClaudeRuntimeMessage {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<Value>,
}

/// 单个 transcript 扫描出的候选结果。
///
/// Business Logic（为什么需要这个结构体）:
///     同一个 worktree 可能存在多个 Claude Code session，关联时需要选最后活动的 transcript。
///
/// Code Logic（这个结构体做什么）:
///     保存 summary 与 transcript 文件 mtime，timestamp 缺失时用 mtime 做跨文件兜底排序。
struct TranscriptCandidate {
    summary: ClaudeRuntimeSummary,
    modified_millis: i128,
}

/// Business Logic（为什么需要这个函数）:
///     Runner 写入 terminal input 后，需要 best-effort 关联 Claude Code 已创建的可见 session。
///
/// Code Logic（这个函数做什么）:
///     优先扫描 encoded worktree project 目录，再 fallback 扫描 home/.claude/projects 下一级项目目录中的 jsonl 文件；
///     逐行匹配 cwd，并可按 runtime_started_at 过滤旧 transcript；无目录、无匹配或扫描错误返回 Ok(None)。
pub fn associate_claude_runtime(
    home_dir: Option<&Path>,
    worktree_path: &str,
    runtime_started_at: Option<&str>,
) -> Result<Option<ClaudeRuntimeSummary>, AppError> {
    let Some(home_dir) = home_dir else {
        return Ok(None);
    };
    let worktree_path = worktree_path.trim();
    if worktree_path.is_empty() {
        return Ok(None);
    }

    let projects_dir = home_dir.join(".claude").join("projects");
    if !projects_dir.is_dir() {
        return Ok(None);
    }

    let min_activity_at = runtime_started_at.and_then(parse_runtime_timestamp);
    let mut best: Option<TranscriptCandidate> = None;
    let preferred_dir = projects_dir.join(encode_claude_project_path(worktree_path));
    if preferred_dir.is_dir() {
        scan_claude_project_dir(&preferred_dir, worktree_path, min_activity_at, &mut best);
        if best.is_some() {
            return Ok(best.map(|candidate| candidate.summary));
        }
    }

    let project_dirs = match fs::read_dir(&projects_dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::debug!("读取 Claude projects 目录失败 {:?}: {err}", projects_dir);
            return Ok(None);
        }
    };

    for entry in project_dirs.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        scan_claude_project_dir(&entry.path(), worktree_path, min_activity_at, &mut best);
    }

    Ok(best.map(|candidate| candidate.summary))
}

/// Business Logic（为什么需要这个函数）:
///     测试和诊断需要按 Claude Code projects 目录规则构造 encoded cwd 子目录名。
///
/// Code Logic（这个函数做什么）:
///     把路径分隔符与非安全字符转成 `-`，保留常见 ASCII 文件名字符；空路径回退为 `-`。
pub fn encode_claude_project_path(path: &str) -> String {
    let encoded: String = path
        .chars()
        .map(|ch| {
            if ch == '/' || ch == '\\' {
                '-'
            } else if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if encoded.is_empty() {
        "-".to_string()
    } else {
        encoded
    }
}

/// Business Logic（为什么需要这个函数）:
///     runtime association 需要同时支持 encoded 目录优先扫描与旧全量 fallback，单目录扫描逻辑必须复用。
///
/// Code Logic（这个函数做什么）:
///     枚举指定 Claude project 目录下的 jsonl 文件，按 cwd 匹配 transcript，并用最新候选更新 best；目录读取失败只记录 debug。
fn scan_claude_project_dir(
    project_dir: &Path,
    worktree_path: &str,
    min_activity_at: Option<DateTime<Utc>>,
    best: &mut Option<TranscriptCandidate>,
) {
    let Ok(files) = fs::read_dir(project_dir) else {
        tracing::debug!("读取 Claude project transcript 目录失败 {:?}", project_dir);
        return;
    };

    for file in files.flatten() {
        let path = file.path();
        if path.extension().and_then(|item| item.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(candidate) = scan_transcript_for_worktree(&path, worktree_path) else {
            continue;
        };
        if !candidate_is_after_runtime_start(&candidate, min_activity_at) {
            continue;
        }
        if transcript_candidate_is_newer(best.as_ref(), &candidate) {
            *best = Some(candidate);
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     runtime association 需要从单个 Claude transcript 中判断是否属于当前任务 worktree。
///
/// Code Logic（这个函数做什么）:
///     顺序读取 jsonl，跳过 malformed 行，只要某行 cwd 匹配就持续更新 summary，最后返回该 transcript 候选。
fn scan_transcript_for_worktree(path: &Path, worktree_path: &str) -> Option<TranscriptCandidate> {
    let file = fs::File::open(path)
        .map_err(|err| {
            tracing::debug!("打开 Claude transcript 失败 {:?}: {err}", path);
            err
        })
        .ok()?;
    let modified_millis = transcript_modified_millis(path);
    let mut summary: Option<ClaudeRuntimeSummary> = None;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let Ok(line) = line else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<ClaudeRuntimeLine>(&line) else {
            continue;
        };
        if parsed.cwd.as_deref() != Some(worktree_path) {
            continue;
        }
        let session_id = parsed
            .session_id
            .clone()
            .or_else(|| session_id_from_transcript_path(path));
        let last_runtime_event = runtime_event_name(&parsed);
        let last_runtime_message = runtime_message_summary(&parsed);
        summary = Some(ClaudeRuntimeSummary {
            claude_session_id: session_id,
            transcript_path: Some(path.to_string_lossy().to_string()),
            last_activity_at: parsed.timestamp.clone(),
            last_runtime_event,
            last_runtime_message,
        });
    }

    summary.map(|summary| TranscriptCandidate {
        summary,
        modified_millis,
    })
}

/// Business Logic（为什么需要这个函数）:
///     修复轮会复用同一 worktree，旧 Claude transcript 不能被新 attempt 重新关联。
///
/// Code Logic（这个函数做什么）:
///     当存在 runtime_started_at 下限时，优先用 transcript timestamp 判断；缺失或不可解析时回退文件 mtime。
fn candidate_is_after_runtime_start(
    candidate: &TranscriptCandidate,
    min_activity_at: Option<DateTime<Utc>>,
) -> bool {
    let Some(min_activity_at) = min_activity_at else {
        return true;
    };

    if let Some(last_activity_at) = candidate.summary.last_activity_at.as_deref() {
        if let Some(activity_at) = parse_runtime_timestamp(last_activity_at) {
            return activity_at >= min_activity_at;
        }
    }

    candidate.modified_millis >= min_activity_at.timestamp_millis() as i128
}

/// Business Logic（为什么需要这个函数）:
///     不同 transcript 可能同时匹配同一 worktree，任务行应关联最后活动的 Claude session。
///
/// Code Logic（这个函数做什么）:
///     优先比较 last_activity_at 字符串（ISO 时间戳按字典序可排序），缺失时比较文件 mtime。
fn transcript_candidate_is_newer(
    current: Option<&TranscriptCandidate>,
    candidate: &TranscriptCandidate,
) -> bool {
    let Some(current) = current else {
        return true;
    };
    match (
        current.summary.last_activity_at.as_deref(),
        candidate.summary.last_activity_at.as_deref(),
    ) {
        (Some(left), Some(right)) => {
            right > left
                || (right == left
                    && candidate.summary.transcript_path > current.summary.transcript_path)
        }
        (None, Some(_)) => true,
        (Some(_), None) => false,
        (None, None) => candidate.modified_millis > current.modified_millis,
    }
}

/// Business Logic（为什么需要这个函数）:
///     某些旧 transcript 行可能缺少 sessionId，仍应尽量从 jsonl 文件名提供可追溯 session 线索。
///
/// Code Logic（这个函数做什么）:
///     取 transcript 文件 stem 作为 session id，空文件名返回 None。
fn session_id_from_transcript_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|item| item.to_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
}

/// Business Logic（为什么需要这个函数）:
///     runtime event 需要用用户可读且稳定的值说明最后一条 Claude transcript 行类型。
///
/// Code Logic（这个函数做什么）:
///     优先使用顶层 type，缺失时回退 message.role；两者都为空则返回 None。
fn runtime_event_name(line: &ClaudeRuntimeLine) -> Option<String> {
    line.r#type
        .as_deref()
        .or_else(|| {
            line.message
                .as_ref()
                .and_then(|message| message.role.as_deref())
        })
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
}

/// Business Logic（为什么需要这个函数）:
///     任务详情需要显示最后 runtime 消息摘要，但 Claude content 可能是字符串、数组或对象。
///
/// Code Logic（这个函数做什么）:
///     从 message.content 中递归提取 text/content 字段；提取不到时转成短 JSON 字符串兜底。
fn runtime_message_summary(line: &ClaudeRuntimeLine) -> Option<String> {
    let content = line.message.as_ref()?.content.as_ref()?;
    content_to_summary(content)
}

/// Business Logic（为什么需要这个函数）:
///     Claude transcript 的 content 结构不稳定，关联逻辑必须容错并产生短摘要。
///
/// Code Logic（这个函数做什么）:
///     处理字符串、数组、对象和基础 JSON 值；数组/对象优先提取文字，最后用 JSON compact 字符串兜底。
fn content_to_summary(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => normalize_summary_text(text),
        Value::Array(items) => {
            let joined = items
                .iter()
                .filter_map(extract_text_from_value)
                .collect::<Vec<_>>()
                .join("\n");
            normalize_summary_text(&joined).or_else(|| compact_json_summary(value))
        }
        Value::Object(_) => extract_text_from_value(value).or_else(|| compact_json_summary(value)),
        Value::Null => None,
        _ => compact_json_summary(value),
    }
}

/// Business Logic（为什么需要这个函数）:
///     array/object content 中常见文本会藏在 `text` 或 `content` 字段，直接 JSON 化可读性较差。
///
/// Code Logic（这个函数做什么）:
///     递归查找字符串 text/content 字段，并把数组中的文本块按换行合并。
fn extract_text_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => normalize_summary_text(text),
        Value::Array(items) => {
            let joined = items
                .iter()
                .filter_map(extract_text_from_value)
                .collect::<Vec<_>>()
                .join("\n");
            normalize_summary_text(&joined)
        }
        Value::Object(map) => {
            for key in ["text", "content"] {
                if let Some(found) = map.get(key).and_then(extract_text_from_value) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Business Logic（为什么需要这个函数）:
///     runtime 摘要写入任务行，不能保存过长 transcript 内容影响列表接口和数据库可读性。
///
/// Code Logic（这个函数做什么）:
///     trim 空白并按 Unicode char 截断到 MAX_RUNTIME_MESSAGE_CHARS；空字符串返回 None。
fn normalize_summary_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_chars(trimmed, MAX_RUNTIME_MESSAGE_CHARS))
}

/// Business Logic（为什么需要这个函数）:
///     无可提取 text 字段时仍应给用户一个短的结构化内容线索，而不是直接丢失最后消息。
///
/// Code Logic（这个函数做什么）:
///     将 JSON Value compact 序列化并复用文本归一化截断。
fn compact_json_summary(value: &Value) -> Option<String> {
    serde_json::to_string(value)
        .ok()
        .and_then(|text| normalize_summary_text(&text))
}

/// Business Logic（为什么需要这个函数）:
///     字符串截断不能切坏中文、emoji 或其它多字节字符。
///
/// Code Logic（这个函数做什么）:
///     按 char 计数截断，超过上限时追加省略号。
fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            output.push('…');
            return output;
        }
        output.push(ch);
    }
    output
}

/// Business Logic（为什么需要这个函数）:
///     transcript 缺少 timestamp 时仍要有稳定的跨文件候选排序兜底。
///
/// Code Logic（这个函数做什么）:
///     读取文件 modified time 并转换成毫秒；失败返回 0。
fn transcript_modified_millis(path: &Path) -> i128 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i128)
        .unwrap_or(0)
}

/// Business Logic（为什么需要这个函数）:
///     Claude transcript timestamp 与任务 runtime_started_at 都是字符串，过滤旧 transcript 前需要统一成 UTC 时间。
///
/// Code Logic（这个函数做什么）:
///     解析 RFC3339 时间戳并转换为 UTC；解析失败记录 debug 并返回 None，让调用方按 best-effort 处理。
fn parse_runtime_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .inspect_err(|err| {
            tracing::debug!("解析 Claude runtime timestamp 失败 {value:?}: {err}");
        })
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Business Logic（为什么需要这个函数）:
    ///     Claude runtime 单测需要模拟真实 `~/.claude/projects/<encoded-cwd>/<session>.jsonl` 目录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在临时 home 下创建 encoded project 目录，写入指定 jsonl 文件并返回 transcript 路径。
    fn write_transcript(home: &Path, cwd: &str, session: &str, lines: &[&str]) -> PathBuf {
        let project_dir = home
            .join(".claude")
            .join("projects")
            .join(encode_claude_project_path(cwd));
        fs::create_dir_all(&project_dir).expect("create project dir");
        let transcript = project_dir.join(format!("{session}.jsonl"));
        fs::write(&transcript, lines.join("\n")).expect("write transcript");
        transcript
    }

    /// Business Logic（为什么需要这个函数）:
    ///     扫描优先级测试需要在非标准项目目录里放置匹配 cwd 的 transcript，模拟全量 fallback 中的历史文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在指定 Claude projects 子目录下写入 jsonl transcript 并返回路径。
    fn write_transcript_in_project_dir(
        home: &Path,
        project_dir_name: &str,
        session: &str,
        lines: &[&str],
    ) -> PathBuf {
        let project_dir = home.join(".claude").join("projects").join(project_dir_name);
        fs::create_dir_all(&project_dir).expect("create custom project dir");
        let transcript = project_dir.join(format!("{session}.jsonl"));
        fs::write(&transcript, lines.join("\n")).expect("write transcript");
        transcript
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Runner 写入 Claude 后应能把任务关联到匹配 cwd 的最新 Claude transcript，供任务详情显示 session/runtime。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入两个同 cwd transcript，断言返回最后活动时间更新的 session、transcript path、event 和消息摘要。
    #[test]
    fn matching_cwd_finds_latest_jsonl() {
        let temp = tempfile::tempdir().expect("tempdir");
        let worktree = "/tmp/project-a";
        write_transcript(
            temp.path(),
            worktree,
            "session-old",
            &[
                r#"{"type":"user","cwd":"/tmp/project-a","timestamp":"2026-07-06T00:00:00Z","sessionId":"session-old","message":{"role":"user","content":"old prompt"}}"#,
            ],
        );
        let latest = write_transcript(
            temp.path(),
            worktree,
            "session-new",
            &[
                r#"{"type":"user","cwd":"/tmp/project-a","timestamp":"2026-07-06T00:01:00Z","sessionId":"session-new","message":{"role":"user","content":"start task"}}"#,
                r#"{"type":"assistant","cwd":"/tmp/project-a","timestamp":"2026-07-06T00:02:00Z","sessionId":"session-new","message":{"role":"assistant","content":[{"type":"text","text":"finished work"}]}}"#,
            ],
        );

        let summary = associate_claude_runtime(Some(temp.path()), worktree, None)
            .expect("association")
            .expect("matching runtime");

        assert_eq!(summary.claude_session_id.as_deref(), Some("session-new"));
        assert_eq!(
            summary.transcript_path.as_deref(),
            Some(latest.to_string_lossy().as_ref())
        );
        assert_eq!(
            summary.last_activity_at.as_deref(),
            Some("2026-07-06T00:02:00Z")
        );
        assert_eq!(summary.last_runtime_event.as_deref(), Some("assistant"));
        assert_eq!(
            summary.last_runtime_message.as_deref(),
            Some("finished work")
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Runner 刚启动后应优先查当前 worktree 对应的 encoded Claude project 目录，避免旧全量扫描选到其它目录里的同 cwd 历史 transcript。
    ///
    /// Code Logic（这个测试做什么）:
    ///     同时写入 encoded 目录和非标准目录的匹配 transcript，非标准目录 timestamp 更新但断言仍返回 encoded 目录结果。
    #[test]
    fn encoded_project_dir_is_preferred_before_global_fallback() {
        let temp = tempfile::tempdir().expect("tempdir");
        let worktree = "/tmp/project-a";
        let preferred = write_transcript(
            temp.path(),
            worktree,
            "session-preferred",
            &[
                r#"{"type":"assistant","cwd":"/tmp/project-a","timestamp":"2026-07-06T00:01:00Z","sessionId":"session-preferred","message":{"role":"assistant","content":"preferred"}}"#,
            ],
        );
        write_transcript_in_project_dir(
            temp.path(),
            "legacy-global-dir",
            "session-global",
            &[
                r#"{"type":"assistant","cwd":"/tmp/project-a","timestamp":"2026-07-06T00:09:00Z","sessionId":"session-global","message":{"role":"assistant","content":"global"}}"#,
            ],
        );

        let summary = associate_claude_runtime(Some(temp.path()), worktree, None)
            .expect("association")
            .expect("matching runtime");

        assert_eq!(
            summary.claude_session_id.as_deref(),
            Some("session-preferred")
        );
        assert_eq!(
            summary.transcript_path.as_deref(),
            Some(preferred.to_string_lossy().as_ref())
        );
        assert_eq!(summary.last_runtime_message.as_deref(), Some("preferred"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     无匹配 Claude transcript 时 Runner 不能失败，任务仍应继续执行且 runtime 字段保持 unknown。
    ///
    /// Code Logic（这个测试做什么）:
    ///     只写入其它 cwd 的 transcript，断言关联结果为 None。
    #[test]
    fn no_matching_cwd_returns_none() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_transcript(
            temp.path(),
            "/tmp/other",
            "session-other",
            &[
                r#"{"type":"user","cwd":"/tmp/other","timestamp":"2026-07-06T00:00:00Z","sessionId":"session-other","message":{"role":"user","content":"other"}}"#,
            ],
        );

        let summary = associate_claude_runtime(Some(temp.path()), "/tmp/project-a", None)
            .expect("association");

        assert!(summary.is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Claude jsonl 可能包含半写入或损坏行，runtime association 不能因为单行格式错误阻断任务。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在 malformed 行后追加有效匹配行，断言扫描跳过坏行并返回有效 runtime 摘要。
    #[test]
    fn malformed_jsonl_lines_are_skipped() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_transcript(
            temp.path(),
            "/tmp/project-a",
            "session-a",
            &[
                "{not-json",
                r#"{"type":"assistant","cwd":"/tmp/project-a","timestamp":"2026-07-06T00:03:00Z","sessionId":"session-a","message":{"role":"assistant","content":{"text":"object text"}}}"#,
            ],
        );

        let summary = associate_claude_runtime(Some(temp.path()), "/tmp/project-a", None)
            .expect("association")
            .expect("valid line");

        assert_eq!(summary.claude_session_id.as_deref(), Some("session-a"));
        assert_eq!(summary.last_runtime_message.as_deref(), Some("object text"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     修复轮复用同一 worktree 时，新 attempt 不能重新关联上一轮早于 runtime_started_at 的 Claude transcript。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入早于 runtime_started_at 的匹配 transcript，断言带时间下限的关联返回 None。
    #[test]
    fn runtime_started_threshold_ignores_reused_worktree_old_transcript() {
        let temp = tempfile::tempdir().expect("tempdir");
        let worktree = "/tmp/project-a";
        write_transcript(
            temp.path(),
            worktree,
            "session-old",
            &[
                r#"{"type":"assistant","cwd":"/tmp/project-a","timestamp":"2026-07-06T00:01:00Z","sessionId":"session-old","message":{"role":"assistant","content":"old attempt"}}"#,
            ],
        );

        let summary =
            associate_claude_runtime(Some(temp.path()), worktree, Some("2026-07-06T00:05:00Z"))
                .expect("association");

        assert!(summary.is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本轮 Claude transcript 出现后，即使同 worktree 有上一轮历史 transcript，也应关联本轮 session。
    ///
    /// Code Logic（这个测试做什么）:
    ///     同时写入 runtime_started_at 前后的匹配 transcript，断言只返回启动时间之后的 session。
    #[test]
    fn runtime_started_threshold_accepts_current_attempt_transcript() {
        let temp = tempfile::tempdir().expect("tempdir");
        let worktree = "/tmp/project-a";
        write_transcript(
            temp.path(),
            worktree,
            "session-old",
            &[
                r#"{"type":"assistant","cwd":"/tmp/project-a","timestamp":"2026-07-06T00:01:00Z","sessionId":"session-old","message":{"role":"assistant","content":"old attempt"}}"#,
            ],
        );
        write_transcript(
            temp.path(),
            worktree,
            "session-new",
            &[
                r#"{"type":"assistant","cwd":"/tmp/project-a","timestamp":"2026-07-06T00:06:00Z","sessionId":"session-new","message":{"role":"assistant","content":"new attempt"}}"#,
            ],
        );

        let summary =
            associate_claude_runtime(Some(temp.path()), worktree, Some("2026-07-06T00:05:00Z"))
                .expect("association")
                .expect("current attempt transcript");

        assert_eq!(summary.claude_session_id.as_deref(), Some("session-new"));
        assert_eq!(summary.last_runtime_message.as_deref(), Some("new attempt"));
    }
}
