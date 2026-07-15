//! Claude session 索引/搜索/watcher 单测模块（由 `claude_sessions.rs` 以 `#[path]` 挂载）。
//!
//! Business Logic:
//!     将大体量 `#[cfg(test)]` 从生产源文件拆出，避免 module-boundary no-growth 与测试膨胀互相绑死，
//!     同时保留子模块对 `claude_sessions` 私有 helper 的可见性。
//!
//! Code Logic:
//!     文件本体即为 `mod tests` 的模块体；仅在 `cfg(test)` 下由父模块 `#[path]` 引入。

//! claude_sessions 单测：覆盖 jsonl 解析、worktree 扫描、搜索语义、文件监听降级。

use super::*;
use std::fs;
use std::io::Write;

/// 生成唯一临时目录路径（避免并发测试竞争，参考 Phase 0 flaky test 教训）。
///
/// Business Logic（为什么需要这个函数）:
///     多个测试用同一固定路径会并发竞争，必须每个测试用唯一路径。
///
/// Code Logic（这个函数做什么）:
///     temp_dir + 函数名 + 进程 id + 纳秒时间组合，保证跨测试跨运行唯一。
fn unique_temp_dir(test_name: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "cc-partner-claude-sessions-{}-{}-{}",
        test_name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    fs::create_dir_all(&dir).expect("创建临时目录失败");
    dir
}

/// 写一个 jsonl 文件（每行一个 JSON 对象），返回文件路径。
///
/// Business Logic（为什么需要这个函数）:
///     测试需要构造 Claude transcript 文件来验证解析逻辑。
///
/// Code Logic（这个函数做什么）:
///     在 dir 下创建 session_id.jsonl，逐行写入 lines。
fn write_jsonl(dir: &Path, session_id: &str, lines: &[&str]) -> PathBuf {
    let path = dir.join(format!("{session_id}.jsonl"));
    let mut f = fs::File::create(&path).expect("创建 jsonl 失败");
    for line in lines {
        writeln!(f, "{line}").expect("写入 jsonl 行失败");
    }
    path
}

/// Business Logic（为什么需要这个测试）:
///     WorktreeSessionIndex 的 encoded_cwd 必须复用 Phase 0 的 encode_claude_project_path 共享 helper，
///     否则扫描会落到错误的 transcript 目录。
///
/// Code Logic（这个测试做什么）:
///     构造一个不存在的 worktree path，扫描后断言 encoded_cwd 字段等于 helper 直接编码结果。
#[test]
fn encode_uses_shared_helper() {
    let tmp = unique_temp_dir("encode_uses_shared_helper");
    let worktree = tmp.join("my-project");
    let index = scan_worktree_sessions(&worktree);
    let canonical = worktree.canonicalize().unwrap_or_else(|_| worktree.clone());
    let expected = encode_claude_project_path(&canonical.to_string_lossy());
    assert_eq!(index.encoded_cwd, expected);
    assert!(index.sessions.is_empty());
}

/// Business Logic（为什么需要这个测试）:
///     session 标题应取 lastPrompt（最后一条 last-prompt 行），让用户看到最近一次输入的摘要。
///
/// Code Logic（这个测试做什么）:
///     构造含两条 last-prompt 行的 jsonl，断言 title = 最后一条 lastPrompt。
#[test]
fn parse_extracts_last_prompt_as_title() {
    let tmp = unique_temp_dir("parse_extracts_last_prompt_as_title");
    let path = write_jsonl(
        &tmp,
        "sess-1",
        &[
            r#"{"type":"user","message":{"role":"user","content":"first prompt"},"timestamp":"2026-01-01T00:00:00Z","cwd":"/tmp/p"}"#,
            r#"{"type":"last-prompt","lastPrompt":"earlier summary"}"#,
            r#"{"type":"last-prompt","lastPrompt":"final summary"}"#,
        ],
    );
    let index = build_session_index(&path).expect("应解析成功");
    assert_eq!(index.title, "final summary");
}

/// Business Logic（为什么需要这个测试）:
///     无 last-prompt 行时标题应回退为第一条有效 user 文本，保证无 lastPrompt 的旧 transcript 也有可读标题。
///
/// Code Logic（这个测试做什么）:
///     构造无 last-prompt 行的 jsonl，断言 title = 第一条 user 文本。
#[test]
fn parse_falls_back_to_first_user_when_no_last_prompt() {
    let tmp = unique_temp_dir("parse_falls_back_to_first_user_when_no_last_prompt");
    let path = write_jsonl(
        &tmp,
        "sess-2",
        &[
            r#"{"type":"user","message":{"role":"user","content":"first user text"},"timestamp":"2026-01-01T00:00:00Z"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"reply"}]},"timestamp":"2026-01-01T00:01:00Z"}"#,
        ],
    );
    let index = build_session_index(&path).expect("应解析成功");
    assert_eq!(index.title, "first user text");
}

/// Business Logic（为什么需要这个测试）:
///     user 的 content 是纯字符串时必须正确提取文本进 user_text 和 recent_messages。
///
/// Code Logic（这个测试做什么）:
///     构造 content 为 string 的 user 行，断言 user_text 含该文本。
#[test]
fn parse_extracts_user_text_from_string_content() {
    let tmp = unique_temp_dir("parse_extracts_user_text_from_string_content");
    let path = write_jsonl(
        &tmp,
        "sess-3",
        &[
            r#"{"type":"user","message":{"role":"user","content":"hello from string"},"timestamp":"2026-01-01T00:00:00Z"}"#,
        ],
    );
    let index = build_session_index(&path).expect("应解析成功");
    assert!(index.user_text.contains("hello from string"));
    assert_eq!(index.message_count, 1);
}

/// Business Logic（为什么需要这个测试）:
///     user 的 content 是数组时（带 text 块）必须只取 type==text 块拼接，忽略 tool_result 等其它块。
///
/// Code Logic（这个测试做什么）:
///     构造 content 为含 text 和 tool_result 块的数组，断言 user_text 只含 text 块内容。
#[test]
fn parse_extracts_user_text_from_array_text_blocks() {
    let tmp = unique_temp_dir("parse_extracts_user_text_from_array_text_blocks");
    let path = write_jsonl(
        &tmp,
        "sess-4",
        &[
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"array user text"},{"type":"tool_result","content":"noise"}]},"timestamp":"2026-01-01T00:00:00Z"}"#,
        ],
    );
    let index = build_session_index(&path).expect("应解析成功");
    assert!(index.user_text.contains("array user text"));
    assert!(!index.user_text.contains("noise"));
}

/// Business Logic（为什么需要这个测试）:
///     assistant 文本只应包含 text 块，thinking 和 tool_use 必须被忽略，避免内部推理噪声进搜索。
///
/// Code Logic（这个测试做什么）:
///     构造 assistant 行含 text/thinking/tool_use 块，断言 assistant_text 只含 text 块。
#[test]
fn parse_extracts_assistant_text_ignoring_thinking_and_tool_use() {
    let tmp = unique_temp_dir("parse_extracts_assistant_text_ignoring_thinking_and_tool_use");
    let path = write_jsonl(
        &tmp,
        "sess-5",
        &[
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"internal"},{"type":"text","text":"visible reply"},{"type":"tool_use","name":"Bash","input":{}}]},"timestamp":"2026-01-01T00:01:00Z"}"#,
        ],
    );
    let index = build_session_index(&path).expect("应解析成功");
    assert!(index.assistant_text.contains("visible reply"));
    assert!(!index.assistant_text.contains("internal"));
    assert!(!index.assistant_text.contains("tool_use"));
}

/// Business Logic（为什么需要这个测试）:
///     `/help`、`!ls` 这类命令不应进入 user_text 和 recent_messages，避免污染搜索结果。
///
/// Code Logic（这个测试做什么）:
///     构造含 slash 和 bash 命令的 user 行，断言它们不进 user_text 与 recent_messages。
#[test]
fn parse_ignores_slash_and_bash_commands() {
    let tmp = unique_temp_dir("parse_ignores_slash_and_bash_commands");
    let path = write_jsonl(
        &tmp,
        "sess-6",
        &[
            r#"{"type":"user","message":{"role":"user","content":"/clear"},"timestamp":"2026-01-01T00:00:00Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":"!ls -la"},"timestamp":"2026-01-01T00:01:00Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":"real question"},"timestamp":"2026-01-01T00:02:00Z"}"#,
        ],
    );
    let index = build_session_index(&path).expect("应解析成功");
    assert!(!index.user_text.contains("/clear"));
    assert!(!index.user_text.contains("!ls"));
    assert!(index.user_text.contains("real question"));
    // 只有 real question 一条进了 recent
    assert_eq!(index.message_count, 1);
}

/// Business Logic（为什么需要这个测试）:
///     jsonl 可能含 malformed 行（被截断、非法 JSON），解析必须跳过它们不 panic，正常行仍要解析。
///
/// Code Logic（这个测试做什么）:
///     构造含非法 JSON 行的 jsonl，断言不 panic 且正常 user 文本仍被提取。
#[test]
fn parse_skips_malformed_lines_without_panicking() {
    let tmp = unique_temp_dir("parse_skips_malformed_lines_without_panicking");
    let path = write_jsonl(
        &tmp,
        "sess-7",
        &[
            r#"{"type":"user","message":{"role":"user","content":"good line"},"timestamp":"2026-01-01T00:00:00Z"}"#,
            "this is not json {{{",
            "",
            r#"{"type":"user","message":{"role":"user","content":"another good"},"timestamp":"2026-01-01T00:01:00Z"}"#,
        ],
    );
    let index = build_session_index(&path).expect("应解析成功");
    assert!(index.user_text.contains("good line"));
    assert!(index.user_text.contains("another good"));
    assert_eq!(index.message_count, 2);
}

/// Business Logic（为什么需要这个测试）:
///     空 query 应返回全部 session 并按 last_activity_at 倒序，让最近活动的 session 排在最前。
///
/// Code Logic（这个测试做什么）:
///     构造一个含 3 个不同活动时间 session 的 WorktreeSessionIndex，空 query 搜索断言顺序为最新到最旧。
#[test]
fn search_empty_query_returns_all_sorted_by_last_activity_desc() {
    let tmp = unique_temp_dir("search_empty_query_returns_all_sorted_by_last_activity_desc");
    let _p1 = write_jsonl(
        &tmp,
        "old",
        &[
            r#"{"type":"user","message":{"role":"user","content":"old"},"timestamp":"2026-01-01T00:00:00Z"}"#,
        ],
    );
    let _p2 = write_jsonl(
        &tmp,
        "mid",
        &[
            r#"{"type":"user","message":{"role":"user","content":"mid"},"timestamp":"2026-06-01T00:00:00Z"}"#,
        ],
    );
    let _p3 = write_jsonl(
        &tmp,
        "new",
        &[
            r#"{"type":"user","message":{"role":"user","content":"new"},"timestamp":"2026-07-01T00:00:00Z"}"#,
        ],
    );

    let mut sessions = HashMap::new();
    for entry in fs::read_dir(&tmp).unwrap().flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            if let Some(idx) = build_session_index(&p) {
                sessions.insert(idx.session_id.clone(), idx);
            }
        }
    }
    let index = WorktreeSessionIndex {
        worktree_path: tmp.clone(),
        encoded_cwd: "test".to_string(),
        sessions,
        last_scan_at: Utc::now().to_rfc3339(),
        truncated: false,
        diagnostics: SessionSearchDiagnostics::ok(0, 0, 0),
    };

    let hits = search_sessions(&index, "", 50);
    assert_eq!(hits.len(), 3);
    // 最新的 new 排第一
    assert_eq!(hits[0].session_id, "new");
    assert_eq!(hits[1].session_id, "mid");
    assert_eq!(hits[2].session_id, "old");
}

/// Business Logic（为什么需要这个测试）:
///     关键词命中应按 title_hit > user_hit > assistant_hit 优先级排序，帮助用户更快定位。
///
/// Code Logic（这个测试做什么）:
///     构造三个 session 分别在 title/user/assistant 命中同一关键词，断言排序为 title、user、assistant。
#[test]
fn search_keyword_prioritizes_title_hit_over_user_over_assistant() {
    let tmp = unique_temp_dir("search_keyword_prioritizes_title");
    // s-title：title（lastPrompt）命中 "fix"，user 文本不含 fix
    let _p1 = write_jsonl(
        &tmp,
        "s-title",
        &[
            r#"{"type":"last-prompt","lastPrompt":"fix auth bug"}"#,
            r#"{"type":"user","message":{"role":"user","content":"misc question"},"timestamp":"2026-01-01T00:00:00Z"}"#,
        ],
    );
    // s-user：title（lastPrompt）不含 fix，user 文本命中 "fix"
    let _p2 = write_jsonl(
        &tmp,
        "s-user",
        &[
            r#"{"type":"last-prompt","lastPrompt":"deploy notes"}"#,
            r#"{"type":"user","message":{"role":"user","content":"please fix the login"},"timestamp":"2026-01-01T00:00:00Z"}"#,
        ],
    );
    // s-assistant：title 与 user 都不含 fix，assistant 文本命中 "fix"
    let _p3 = write_jsonl(
        &tmp,
        "s-assistant",
        &[
            r#"{"type":"last-prompt","lastPrompt":"random topic"}"#,
            r#"{"type":"user","message":{"role":"user","content":"hello there"},"timestamp":"2026-01-01T00:00:00Z"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I will fix that now"}]},"timestamp":"2026-01-01T00:00:00Z"}"#,
        ],
    );

    let mut sessions = HashMap::new();
    for entry in fs::read_dir(&tmp).unwrap().flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            if let Some(idx) = build_session_index(&p) {
                sessions.insert(idx.session_id.clone(), idx);
            }
        }
    }
    let index = WorktreeSessionIndex {
        worktree_path: tmp.clone(),
        encoded_cwd: "test".to_string(),
        sessions,
        last_scan_at: Utc::now().to_rfc3339(),
        truncated: false,
        diagnostics: SessionSearchDiagnostics::ok(0, 0, 0),
    };

    let hits = search_sessions(&index, "fix", 50);
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].session_id, "s-title");
    assert!(hits[0].title_hit);
    assert_eq!(hits[1].session_id, "s-user");
    assert!(!hits[1].title_hit);
    assert!(hits[1].user_hit);
    assert_eq!(hits[2].session_id, "s-assistant");
    assert!(hits[2].assistant_hit);
}

/// Business Logic（为什么需要这个测试）:
///     limit 应截断结果数量，防止超长列表。
///
/// Code Logic（这个测试做什么）:
///     构造 3 个 session，limit=2 断言只返回 2 条。
#[test]
fn search_respects_limit() {
    let tmp = unique_temp_dir("search_respects_limit");
    for i in 0..3 {
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":"item {i}"}},"timestamp":"2026-01-0{i}T00:00:00Z"}}"#
        );
        let _ = write_jsonl(&tmp, &format!("s{i}"), &[line.as_str()]);
    }

    let mut sessions = HashMap::new();
    for entry in fs::read_dir(&tmp).unwrap().flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            if let Some(idx) = build_session_index(&p) {
                sessions.insert(idx.session_id.clone(), idx);
            }
        }
    }
    let index = WorktreeSessionIndex {
        worktree_path: tmp.clone(),
        encoded_cwd: "test".to_string(),
        sessions,
        last_scan_at: Utc::now().to_rfc3339(),
        truncated: false,
        diagnostics: SessionSearchDiagnostics::ok(0, 0, 0),
    };

    let hits = search_sessions(&index, "", 2);
    assert_eq!(hits.len(), 2);
}

/// Business Logic（为什么需要这个测试）:
///     preview_snippets 应取命中位置前后各 30 字符的上下文片段，帮助用户预判是否是目标 session。
///     且片段必须保留原文大小写（不能返回全小写文本，否则用户预览失真）。
///
/// Code Logic（这个测试做什么）:
///     构造一段含大小写的 user 文本（"foo Target bar"），用小写关键词 "target" 搜索，
///     断言 preview_snippets 非空、含关键词、且保留原文大写 "Target"。
#[test]
fn search_preview_snippets_extract_context_around_hit() {
    let tmp = unique_temp_dir("search_preview_snippets_extract_context_around_hit");
    // 构造一段足够长的文本，关键词在中间，且含大小写以验证片段保留原文大小写
    let prefix = "x".repeat(40);
    let suffix = "y".repeat(40);
    let content = format!("{prefix}foo Target bar{suffix}");
    let line = format!(
        r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-01-01T00:00:00Z"}}"#,
        serde_json::Value::String(content)
    );
    let path = write_jsonl(&tmp, "s-snippet", &[&line]);

    let mut sessions = HashMap::new();
    if let Some(idx) = build_session_index(&path) {
        sessions.insert(idx.session_id.clone(), idx);
    }
    let index = WorktreeSessionIndex {
        worktree_path: tmp.clone(),
        encoded_cwd: "test".to_string(),
        sessions,
        last_scan_at: Utc::now().to_rfc3339(),
        truncated: false,
        diagnostics: SessionSearchDiagnostics::ok(0, 0, 0),
    };

    // 用小写关键词搜索，应命中大写的 "Target"
    let hits = search_sessions(&index, "target", 50);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].user_hit);
    assert!(!hits[0].preview_snippets.is_empty());
    let snippet = &hits[0].preview_snippets[0];
    // 片段应包含关键词（小写匹配命中大写原文）
    assert!(snippet.to_lowercase().contains("target"));
    // 关键：片段保留原文大小写，不是全小写
    assert!(
        snippet.contains("Target"),
        "snippet 应保留原文大小写，实际: {snippet}"
    );
    // 片段长度应 <= 关键词长度 + 前后各 30 字符
    assert!(
        snippet.chars().count() <= "foo Target bar".chars().count() + 2 * PREVIEW_SNIPPET_RADIUS
    );
    // 应包含关键词前面的部分上下文
    assert!(snippet.contains('x'));
}

/// Business Logic（为什么需要这个测试）:
///     recent_messages 上限为 20（spec 3.1），超过时只保留按时间排序的尾部 20 条，
///     保证 preview 面板数据量可控。
///
/// Code Logic（这个测试做什么）:
///     构造一个含 25 条 user/assistant 交替消息的 jsonl（带递增 timestamp），调用
///     build_session_index，断言 recent_messages.len() == 20，message_count == 25，
///     且 recent_messages 恰好是最后 20 条（首条文本含序号 6，末条含序号 25）。
#[test]
fn recent_messages_capped_at_twenty() {
    let tmp = unique_temp_dir("recent_messages_capped_at_twenty");
    let mut lines: Vec<String> = Vec::new();
    // 25 条 user/assistant 交替消息，timestamp 递增，文本带序号便于断言尾部
    for i in 1..=25 {
        let role = if i % 2 == 1 { "user" } else { "assistant" };
        let text = format!("msg-{i:02}");
        let ts = format!("2026-01-01T00:{:02}:00Z", i); // 每分钟一条，严格递增
        let content = if role == "user" {
            format!(r#""{}""#, text)
        } else {
            format!(r#"[{{"type":"text","text":"{}"}}]"#, text)
        };
        lines.push(format!(
                r#"{{"type":"{role}","message":{{"role":"{role}","content":{content}}},"timestamp":"{ts}"}}"#
            ));
    }
    let line_refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    let path = write_jsonl(&tmp, "sess-many", &line_refs);

    let index = build_session_index(&path).expect("应解析成功");
    // message_count 记录全部有效消息（25 条，user 13 + assistant 12）
    assert_eq!(index.message_count, 25, "message_count 应为全部消息数");
    // recent_messages 被截断为 20
    assert_eq!(
        index.recent_messages.len(),
        RECENT_MESSAGES_MAX,
        "recent_messages 应被截断为 {}",
        RECENT_MESSAGES_MAX
    );
    // 首条应是序号 6（25-20+1=6），末条是序号 25
    assert_eq!(
        index.recent_messages[0].text, "msg-06",
        "recent_messages 首条应是按时间排序的第 6 条"
    );
    assert_eq!(
        index.recent_messages[19].text, "msg-25",
        "recent_messages 末条应是最后一条"
    );
    // 首末时间戳也应是第 6 条和第 25 条的时间
    assert_eq!(index.recent_messages[0].timestamp, "2026-01-01T00:06:00Z");
    assert_eq!(index.recent_messages[19].timestamp, "2026-01-01T00:25:00Z");
}

/// Business Logic（为什么需要这个测试）:
///     N7 性能基线：在引入 spawn_blocking / 文件数 / 字节 / 行长 / 缓存文本预算之前，
///     必须可重复记录「当前同步全量索引」的耗时、处理字节、会话数与截断语义，
///     后续任务才能证明预算化与非阻塞改造真正改善了热点，而不是仅改代码结构。
///
/// Code Logic（这个测试做什么）:
///     1. 用 temp JSONL fixture（非用户目录）生成多 session + 长正文 + 超 20 条消息；
///     2. 同步调用 build_session_index 扫描全部文件，累计 wall 时间与读取字节；
///     3. 并行 heartbeat 线程每 2ms 自增，记录索引期间心跳次数（表征调用线程占用）；
///     4. 断言当前行为：全量入库（无 file/byte budget 截断）、user/assistant 全文缓存、
///        recent_messages 仅截到 20；经 eprintln! 输出可复现基线指标。
#[test]
fn index_budget_baseline() {
    let tmp = unique_temp_dir("index_budget_baseline");

    // 构造 8 个 session：前 6 个中等大小，第 7 个含超长 user 正文，第 8 个 30 条消息
    // 触发 recent_messages 截断（当前唯一稳定的截断语义）。
    let long_body = "L".repeat(8_192);
    let mut session_ids: Vec<String> = Vec::new();

    for i in 0..6 {
        let sid = format!("budget-sess-{i:02}");
        let user = format!(
            r#"{{"type":"user","message":{{"role":"user","content":"baseline prompt {i}"}},"timestamp":"2026-07-0{}T0{}:00:00Z","cwd":"/tmp/budget"}}"#,
            (i % 9) + 1,
            i % 9
        );
        let asst = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"reply {i}"}}]}},"timestamp":"2026-07-0{}T0{}:01:00Z"}}"#,
            (i % 9) + 1,
            i % 9
        );
        let _ = write_jsonl(&tmp, &sid, &[user.as_str(), asst.as_str()]);
        session_ids.push(sid);
    }

    // 超长正文 session：当前实现会把整段写入 user_text（无 1M scalar / 行长预算）
    {
        let sid = "budget-sess-long".to_string();
        let content = format!("prefix-{long_body}-suffix");
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-07-14T12:00:00Z"}}"#,
            serde_json::Value::String(content.clone())
        );
        let path = write_jsonl(&tmp, &sid, &[line.as_str()]);
        let _ = path;
        session_ids.push(sid);
    }

    // 30 条消息 session：recent_messages 截到 20，message_count 仍为 30
    {
        let sid = "budget-sess-many".to_string();
        let mut lines: Vec<String> = Vec::new();
        for i in 1..=30 {
            let role = if i % 2 == 1 { "user" } else { "assistant" };
            let text = format!("m{i:02}");
            let ts = format!("2026-07-14T13:{:02}:00Z", i);
            let content = if role == "user" {
                format!(r#""{text}""#)
            } else {
                format!(r#"[{{"type":"text","text":"{text}"}}]"#)
            };
            lines.push(format!(
                    r#"{{"type":"{role}","message":{{"role":"{role}","content":{content}}},"timestamp":"{ts}"}}"#
                ));
        }
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let _ = write_jsonl(&tmp, &sid, &refs);
        session_ids.push(sid);
    }

    // 汇总 fixture 字节（当前实现会完整读入这些字节；后续预算化后可能截断）
    let mut total_fixture_bytes: u64 = 0;
    for entry in fs::read_dir(&tmp).unwrap().flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            total_fixture_bytes += fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        }
    }

    // Heartbeat 线程：索引期间每 2ms 自增，用于表征「调用线程被同步解析占用」时
    // 仍可观测的并发心跳次数（非生产 watcher heartbeat；仅基线证据）。
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::thread;
    let stop = Arc::new(AtomicBool::new(false));
    let ticks = Arc::new(AtomicU64::new(0));
    let stop_hb = Arc::clone(&stop);
    let ticks_hb = Arc::clone(&ticks);
    let hb = thread::spawn(move || {
        while !stop_hb.load(Ordering::Relaxed) {
            ticks_hb.fetch_add(1, Ordering::Relaxed);
            thread::sleep(Duration::from_millis(2));
        }
    });

    let wall_start = Instant::now();
    let mut sessions: HashMap<String, ClaudeSessionIndex> = HashMap::new();
    let mut indexed_bytes: u64 = 0;
    for entry in fs::read_dir(&tmp).unwrap().flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let file_len = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        if let Some(idx) = build_session_index(&p) {
            indexed_bytes += file_len;
            sessions.insert(idx.session_id.clone(), idx);
        }
    }
    let elapsed = wall_start.elapsed();
    stop.store(true, Ordering::Relaxed);
    let _ = hb.join();
    let heartbeat_ticks = ticks.load(Ordering::Relaxed);

    // --- 当前行为断言（characterization，Task 4/5 优化后会改语义） ---
    // 1) 无 file/byte budget：全部 8 个 fixture 都应入库
    assert_eq!(
        sessions.len(),
        session_ids.len(),
        "当前实现应对全部 fixture session 建索引（无截断预算）"
    );

    // 2) 超长正文全文缓存进 user_text（无 per-session scalar 截断）
    let long = sessions
        .get("budget-sess-long")
        .expect("long session should be indexed");
    assert!(
        long.user_text.contains(&long_body),
        "当前实现缓存完整 user_text，不含截断标记"
    );
    assert!(
        long.user_text.len() >= long_body.len(),
        "user_text 应保留完整长正文"
    );

    // 3) recent_messages 是当前唯一稳定截断：30 条 → 20
    let many = sessions
        .get("budget-sess-many")
        .expect("many-messages session should be indexed");
    assert_eq!(many.message_count, 30);
    assert_eq!(many.recent_messages.len(), RECENT_MESSAGES_MAX);
    assert_eq!(many.recent_messages[0].text, "m11");
    assert_eq!(many.recent_messages[19].text, "m30");

    // 4) 中等 session 的 assistant 全文也缓存
    let mid = sessions
        .get("budget-sess-00")
        .expect("mid session should be indexed");
    assert!(mid.assistant_text.contains("reply 0"));
    assert!(mid.user_text.contains("baseline prompt 0"));

    // 可重复基线输出（cargo test ... -- --nocapture）
    eprintln!(
        "[perf-baseline] claude_sessions index_budget_baseline: \
             sessions={} fixture_bytes={} indexed_bytes={} elapsed_ms={} heartbeat_ticks={} \
             truncation=recent_messages_only(max={}) full_text_cache=true file_budget=none",
        sessions.len(),
        total_fixture_bytes,
        indexed_bytes,
        elapsed.as_millis(),
        heartbeat_ticks,
        RECENT_MESSAGES_MAX,
    );

    // 基本健全性：应处理完 fixture 字节且耗时有限（避免无限挂起）
    assert_eq!(indexed_bytes, total_fixture_bytes);
    assert!(
        elapsed < Duration::from_secs(5),
        "fixture 索引应在 5s 内完成，实际 {:?}",
        elapsed
    );
    // heartbeat 线程在同步索引期间应至少跳动过（证明测量面可观测；次数随机器变化）
    assert!(
        heartbeat_ticks > 0,
        "heartbeat 线程应在索引期间至少 tick 一次"
    );
}

/// 准备临时 projects 布局：返回 (worktree, projects_dir, session_dir)。
///
/// Business Logic（为什么需要这个函数）:
///     预算扫描测试需把 jsonl 放到 `projects/<encoded-cwd>/`，不能污染真实 ~/.claude。
///
/// Code Logic（这个函数做什么）:
///     创建 worktree 与 projects/<encoded> 目录。
fn prepare_scan_fixture(test_name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = unique_temp_dir(test_name);
    let worktree = root.join("wt");
    fs::create_dir_all(&worktree).unwrap();
    let projects = root.join("projects");
    let canonical = worktree.canonicalize().unwrap_or_else(|_| worktree.clone());
    let encoded = encode_claude_project_path(&canonical.to_string_lossy());
    let session_dir = projects.join(&encoded);
    fs::create_dir_all(&session_dir).unwrap();
    (worktree, projects, session_dir)
}

/// Business Logic（为什么需要这个测试）:
///     max_files 预算必须截断候选并标记 truncated + reason=max_files。
///
/// Code Logic（这个测试做什么）:
///     5 个 jsonl，budget.max_files=2，断言 indexed=2、truncated、reasons 含 max_files。
#[test]
fn budget_max_files_truncates() {
    let (worktree, projects, session_dir) = prepare_scan_fixture("budget_max_files");
    for i in 0..5 {
        let _ = write_jsonl(
            &session_dir,
            &format!("f{i}"),
            &[&format!(
                r#"{{"type":"user","message":{{"role":"user","content":"file {i}"}},"timestamp":"2026-01-01T00:0{i}:00Z"}}"#
            )],
        );
    }
    let budget = ClaudeIndexBudget {
        max_files: 2,
        ..ClaudeIndexBudget::default()
    };
    let index = scan_worktree_sessions_at(&worktree, Some(&projects), &budget);
    assert_eq!(index.sessions.len(), 2);
    assert!(index.truncated);
    assert_eq!(index.diagnostics.status, DIAG_STATUS_TRUNCATED);
    assert!(index.diagnostics.reasons.iter().any(|r| r == "max_files"));
    assert_eq!(index.diagnostics.files_considered, 5);
    assert_eq!(index.diagnostics.files_indexed, 2);
}

/// Business Logic（为什么需要这个测试）:
///     超过 max_file_bytes 的文件必须整文件跳过。
///
/// Code Logic（这个测试做什么）:
///     写一个较大 jsonl，设 max_file_bytes 很小，断言 0 indexed + reason max_file_bytes。
#[test]
fn budget_max_file_bytes_skips_oversized() {
    let (worktree, projects, session_dir) = prepare_scan_fixture("budget_max_file_bytes");
    let big = "X".repeat(2000);
    let line = format!(
        r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-01-01T00:00:00Z"}}"#,
        serde_json::Value::String(big)
    );
    let _ = write_jsonl(&session_dir, "huge", &[line.as_str()]);
    let budget = ClaudeIndexBudget {
        max_file_bytes: 100,
        ..ClaudeIndexBudget::default()
    };
    let index = scan_worktree_sessions_at(&worktree, Some(&projects), &budget);
    assert!(index.sessions.is_empty());
    assert!(index.truncated);
    assert!(index
        .diagnostics
        .reasons
        .iter()
        .any(|r| r == "max_file_bytes"));
}

/// Business Logic（为什么需要这个测试）:
///     超长 jsonl 行不得整行分配进内存；应跳过该行并记 max_jsonl_line_bytes。
///
/// Code Logic（这个测试做什么）:
///     写一行 > budget 的 payload + 一行正常 user；断言完成、reason 命中、正常行仍可能入库。
#[test]
fn budget_max_jsonl_line_bytes_drops_long_line_without_allocating_all() {
    let (worktree, projects, session_dir) = prepare_scan_fixture("budget_max_jsonl_line");
    let long_content = "Z".repeat(500);
    // 故意构造一行远超 max_jsonl_line_bytes=80 的 JSON 行
    let long_line = format!(
        r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-01-01T00:00:00Z"}}"#,
        serde_json::Value::String(long_content)
    );
    let good = r#"{"type":"user","message":{"role":"user","content":"short ok"},"timestamp":"2026-01-01T00:01:00Z"}"#;
    let path = session_dir.join("mixed.jsonl");
    {
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "{long_line}").unwrap();
        writeln!(f, "{good}").unwrap();
    }
    // 短行约 100 字节，长行远超 120；预算取中间值
    let budget = ClaudeIndexBudget {
        max_jsonl_line_bytes: 120,
        ..ClaudeIndexBudget::default()
    };
    let index = scan_worktree_sessions_at(&worktree, Some(&projects), &budget);
    assert!(index.truncated);
    assert!(index
        .diagnostics
        .reasons
        .iter()
        .any(|r| r == "max_jsonl_line_bytes"));
    // 短行应仍被索引
    let sess = index.sessions.get("mixed").expect("session should exist");
    assert!(
        sess.user_text.contains("short ok"),
        "short line should be indexed, user_text={:?}",
        sess.user_text
    );
    assert!(!sess.user_text.contains("ZZZZ"));
}

/// Business Logic（为什么需要这个测试）:
///     max_total_bytes 必须在累计字节将超时停止后续文件。
///
/// Code Logic（这个测试做什么）:
///     多个中等文件 + 极小 max_total_bytes，断言 partial index + reason。
#[test]
fn budget_max_total_bytes_stops_early() {
    let (worktree, projects, session_dir) = prepare_scan_fixture("budget_max_total");
    for i in 0..4 {
        let body = "Y".repeat(300);
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-01-01T00:0{i}:00Z"}}"#,
            serde_json::Value::String(body)
        );
        let _ = write_jsonl(&session_dir, &format!("t{i}"), &[line.as_str()]);
    }
    let budget = ClaudeIndexBudget {
        max_total_bytes: 400,
        ..ClaudeIndexBudget::default()
    };
    let index = scan_worktree_sessions_at(&worktree, Some(&projects), &budget);
    assert!(index.sessions.len() < 4);
    assert!(index.truncated);
    assert!(index
        .diagnostics
        .reasons
        .iter()
        .any(|r| r == "max_total_bytes"));
}

/// Business Logic（为什么需要这个测试）:
///     max_session_chars 必须以 Unicode scalar 截断，且只在 char 边界切断（含中文/emoji）。
///
/// Code Logic（这个测试做什么）:
///     title/user/assistant 含中文与 emoji，小 budget 截断后 len 合法、无 panic。
#[test]
fn budget_max_session_chars_truncates_at_char_boundary() {
    let tmp = unique_temp_dir("budget_max_session_chars");
    let text = "你好世界🌍🚀测试文本额外内容";
    // 短 title 占 2 scalar，剩余预算给 user_text
    let user_line = format!(
        r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-01-01T00:00:00Z"}}"#,
        serde_json::Value::String(text.to_string())
    );
    let path = write_jsonl(
        &tmp,
        "uni",
        &[
            r#"{"type":"last-prompt","lastPrompt":"标题"}"#,
            user_line.as_str(),
        ],
    );
    let budget = ClaudeIndexBudget {
        max_session_chars: 7, // 标题 2 + user 5
        ..ClaudeIndexBudget::default()
    };
    let (idx, outcome) = build_session_index_with_budget(&path, &budget).expect("ok");
    assert!(outcome.reasons.iter().any(|r| r == "max_session_chars"));
    assert_eq!(idx.title.chars().count(), 2);
    assert_eq!(idx.user_text.chars().count(), 5);
    // 截断结果必须是合法 UTF-8 且为原文前缀
    assert!(
        text.starts_with(&idx.user_text),
        "user_text should be a prefix of original, got {:?}",
        idx.user_text
    );
    // 明确 char 边界：重新 collect 应相等
    let recomposed: String = idx.user_text.chars().collect();
    assert_eq!(recomposed, idx.user_text);
    // emoji 截断不 panic：只取前 5 个 scalar（可能含不完整语义但合法 UTF-8）
    assert!(!idx.user_text.is_empty());
}

/// Business Logic（为什么需要这个测试）:
///     候选排序必须 mtime desc 再 path asc，保证 max_files 截断确定性。
///
/// Code Logic（这个测试做什么）:
///     写 3 个文件并 sleep 拉开 mtime，max_files=2，断言保留最新两个。
#[test]
fn scan_orders_by_mtime_desc_then_path_asc() {
    let (worktree, projects, session_dir) = prepare_scan_fixture("scan_order");
    let _a = write_jsonl(
        &session_dir,
        "a-old",
        &[
            r#"{"type":"user","message":{"role":"user","content":"a"},"timestamp":"2026-01-01T00:00:00Z"}"#,
        ],
    );
    std::thread::sleep(Duration::from_millis(1100));
    let _b = write_jsonl(
        &session_dir,
        "b-mid",
        &[
            r#"{"type":"user","message":{"role":"user","content":"b"},"timestamp":"2026-01-01T00:00:00Z"}"#,
        ],
    );
    std::thread::sleep(Duration::from_millis(1100));
    let _c = write_jsonl(
        &session_dir,
        "c-new",
        &[
            r#"{"type":"user","message":{"role":"user","content":"c"},"timestamp":"2026-01-01T00:00:00Z"}"#,
        ],
    );
    let budget = ClaudeIndexBudget {
        max_files: 2,
        ..ClaudeIndexBudget::default()
    };
    let index = scan_worktree_sessions_at(&worktree, Some(&projects), &budget);
    assert_eq!(index.sessions.len(), 2);
    assert!(index.sessions.contains_key("c-new"));
    assert!(index.sessions.contains_key("b-mid"));
    assert!(!index.sessions.contains_key("a-old"));
}

/// Business Logic（为什么需要这个测试）:
///     初始扫描必须在 spawn_blocking 中运行，不阻塞 tokio 心跳。
///
/// Code Logic（这个测试做什么）:
///     构造多文件 fixture；spawn interval heartbeat + spawn_blocking 紧预算扫描；
///     断言 heartbeat≥3 且 truncated。
#[tokio::test]
async fn initial_scan_does_not_block_tokio_heartbeat() {
    let (worktree, projects, session_dir) = prepare_scan_fixture("initial_scan_heartbeat");
    // 多个中等文件，制造可观测扫描耗时
    // 多个多行 transcript + 故意在 blocking 侧做足量解析工作
    for i in 0..12 {
        let mut lines: Vec<String> = Vec::new();
        for j in 0..1500 {
            lines.push(format!(
                    r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-01-01T00:00:00Z"}}"#,
                    serde_json::Value::String(format!("hb-{i}-{j}-{}", "p".repeat(128)))
                ));
        }
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let _ = write_jsonl(&session_dir, &format!("hb{i:02}"), &refs);
    }
    let budget = ClaudeIndexBudget {
        max_files: 6,
        max_jsonl_line_bytes: 64 * 1024,
        ..ClaudeIndexBudget::default()
    };
    let worktree2 = worktree.clone();
    let projects2 = projects.clone();
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    let stop = Arc::new(AtomicBool::new(false));
    let ticks = Arc::new(AtomicU32::new(0));
    let stop_hb = Arc::clone(&stop);
    let ticks_hb = Arc::clone(&ticks);
    // 先启动 heartbeat，确保与 blocking 扫描重叠
    let hb = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(2));
        interval.tick().await; // 跳过立即完成的首 tick
        while !stop_hb.load(Ordering::Relaxed) {
            interval.tick().await;
            ticks_hb.fetch_add(1, Ordering::Relaxed);
        }
    });
    // 让 heartbeat 至少先走一拍，再启动扫描
    tokio::time::sleep(Duration::from_millis(5)).await;

    let index = tokio::task::spawn_blocking(move || {
        scan_worktree_sessions_at(&worktree2, Some(&projects2), &budget)
    })
    .await
    .expect("join");
    stop.store(true, Ordering::Relaxed);
    let _ = hb.await;
    let beats = ticks.load(Ordering::Relaxed);
    assert!(index.truncated, "紧预算应 truncated");
    assert!(beats >= 3, "扫描期间 tokio heartbeat 应 >=3，实际 {beats}");
}

/// Business Logic（为什么需要这个测试）:
///     singleflight 必须让并发 ensure 共享一次扫描（AtomicUsize 计数=1）。
///
/// Code Logic（这个测试做什么）:
///     用 watch + Mutex map 模拟 inflight；两个任务并发进入，work 只应执行一次。
#[tokio::test]
async fn singleflight_shares_one_scan() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Mutex, Notify};

    let scans = Arc::new(AtomicUsize::new(0));
    type Slot = Arc<(Notify, std::sync::Mutex<Option<Result<u32, String>>>)>;
    let map: Arc<Mutex<HashMap<String, Slot>>> = Arc::new(Mutex::new(HashMap::new()));

    async fn ensure(
        map: Arc<Mutex<HashMap<String, Slot>>>,
        scans: Arc<AtomicUsize>,
        key: &str,
    ) -> u32 {
        // fast path 省略
        let (slot, is_leader) = {
            let mut g = map.lock().await;
            if let Some(s) = g.get(key) {
                (Arc::clone(s), false)
            } else {
                let s = Arc::new((
                    Notify::new(),
                    std::sync::Mutex::new(None::<Result<u32, String>>),
                ));
                g.insert(key.to_string(), Arc::clone(&s));
                (s, true)
            }
        };
        if is_leader {
            let scans2 = Arc::clone(&scans);
            let value = tokio::task::spawn_blocking(move || {
                scans2.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(50));
                42u32
            })
            .await
            .unwrap();
            *slot.1.lock().unwrap() = Some(Ok(value));
            slot.0.notify_waiters();
            let mut g = map.lock().await;
            g.remove(key);
            value
        } else {
            loop {
                if let Some(r) = slot.1.lock().unwrap().clone() {
                    return r.unwrap();
                }
                slot.0.notified().await;
            }
        }
    }

    let m1 = Arc::clone(&map);
    let s1 = Arc::clone(&scans);
    let m2 = Arc::clone(&map);
    let s2 = Arc::clone(&scans);
    let (a, b) = tokio::join!(ensure(m1, s1, "k"), ensure(m2, s2, "k"),);
    assert_eq!(a, 42);
    assert_eq!(b, 42);
    assert_eq!(scans.load(Ordering::SeqCst), 1, "只应扫描一次");
}

/// Business Logic（为什么需要这个测试）:
///     新客户端必须能解码旧服务端返回的 `Vec<SessionSearchHit>`。
///
/// Code Logic（这个测试做什么）:
///     序列化数组 body，decode 得 truncated=false + unavailable diagnostics。
#[test]
fn decode_legacy_array_body_synthesizes_unavailable() {
    let items = vec![SessionSearchHit {
        session_id: "s1".into(),
        title: "t".into(),
        title_hit: true,
        user_hit: false,
        assistant_hit: false,
        first_activity_at: "a".into(),
        last_activity_at: "b".into(),
        message_count: 1,
        preview_snippets: vec![],
    }];
    let bytes = serde_json::to_vec(&items).unwrap();
    let result = decode_session_search_response_body(&bytes).expect("decode");
    assert_eq!(result.items.len(), 1);
    assert!(!result.truncated);
    assert_eq!(result.diagnostics.status, DIAG_STATUS_UNAVAILABLE);
    assert!(result.diagnostics.reasons.is_empty());
    assert_eq!(result.diagnostics.files_indexed, 0);
}

/// Business Logic（为什么需要这个测试）:
///     旧/新客户端都必须能解码 v2 对象 DTO。
///
/// Code Logic（这个测试做什么）:
///     序列化 SessionSearchResult 对象，decode 字段完整保留。
#[test]
fn decode_v2_object_body_preserves_diagnostics() {
    let dto = SessionSearchResult {
        items: vec![],
        truncated: true,
        diagnostics: SessionSearchDiagnostics::truncated(vec!["max_files".into()], 10, 2, 100),
    };
    let bytes = serde_json::to_vec(&dto).unwrap();
    let result = decode_session_search_response_body(&bytes).expect("decode");
    assert!(result.truncated);
    assert_eq!(result.diagnostics.status, DIAG_STATUS_TRUNCATED);
    assert_eq!(result.diagnostics.files_considered, 10);
    assert_eq!(result.diagnostics.files_indexed, 2);
    assert_eq!(result.diagnostics.bytes_read, 100);
    assert!(result.diagnostics.reasons.iter().any(|r| r == "max_files"));
}

/// 构造带两个 session 的共享索引，便于 delete/rename 断言。
///
/// Business Logic（为什么需要这个函数）:
///     watcher 生命周期测试需要可写内存索引。
///
/// Code Logic（这个函数做什么）:
///     在 tmp 下写两个 jsonl，构建 SharedWorktreeSessionIndex。
fn make_shared_index_with_two_sessions(
    label: &str,
) -> (
    PathBuf,
    PathBuf,
    SharedWorktreeSessionIndex,
    PathBuf,
    PathBuf,
) {
    let root = unique_temp_dir(label);
    let worktree = root.join("wt");
    fs::create_dir_all(&worktree).unwrap();
    let watch_dir = root.join("sessions");
    fs::create_dir_all(&watch_dir).unwrap();
    let path_a = write_jsonl(
        &watch_dir,
        "sess-a",
        &[
            r#"{"type":"user","message":{"role":"user","content":"alpha"},"timestamp":"2026-01-01T00:00:00Z"}"#,
        ],
    );
    let path_b = write_jsonl(
        &watch_dir,
        "sess-b",
        &[
            r#"{"type":"user","message":{"role":"user","content":"beta"},"timestamp":"2026-01-01T00:00:00Z"}"#,
        ],
    );
    let mut sessions = HashMap::new();
    sessions.insert("sess-a".into(), build_session_index(&path_a).expect("a"));
    sessions.insert("sess-b".into(), build_session_index(&path_b).expect("b"));
    let index = WorktreeSessionIndex {
        worktree_path: worktree.clone(),
        encoded_cwd: "enc".into(),
        sessions,
        last_scan_at: "t0".into(),
        truncated: false,
        diagnostics: SessionSearchDiagnostics::ok(2, 2, 0),
    };
    (
        worktree,
        watch_dir,
        Arc::new(RwLock::new(index)),
        path_a,
        path_b,
    )
}

/// Business Logic（为什么需要这个测试）:
///     删除 jsonl 后旧 session 必须从索引消失，否则搜索会展示幽灵会话。
///
/// Code Logic（这个测试做什么）:
///     classify Remove 事件 → apply plan → 断言 sess-a 消失、sess-b 仍在。
#[test]
fn watcher_delete_removes_session_from_index() {
    let (worktree, watch_dir, shared, path_a, _path_b) =
        make_shared_index_with_two_sessions("watcher_delete");
    // 模拟删除文件后 notify 上报 Remove
    fs::remove_file(&path_a).unwrap();
    let plan = classify_session_watch_event(
        &EventKind::Remove(RemoveKind::File),
        std::slice::from_ref(&path_a),
        &watch_dir,
    );
    assert_eq!(
        plan,
        SessionWatchPlan::Remove(vec!["sess-a".into()]),
        "delete 应映射为 Remove"
    );
    apply_session_watch_plan(&shared, &worktree, &watch_dir, plan);
    let guard = shared.read().unwrap();
    assert!(
        !guard.sessions.contains_key("sess-a"),
        "删除后 sess-a 必须消失"
    );
    assert!(
        guard.sessions.contains_key("sess-b"),
        "未删除的 sess-b 应保留"
    );
}

/// Business Logic（为什么需要这个测试）:
///     rename 必须删旧 id 并索引新文件，否则旧路径幽灵 + 新路径缺失。
///
/// Code Logic（这个测试做什么）:
///     rename sess-a → sess-c；classify Both → apply → 断言 a 消失、c 出现。
#[test]
fn watcher_rename_removes_old_and_adds_new() {
    let (worktree, watch_dir, shared, path_a, _path_b) =
        make_shared_index_with_two_sessions("watcher_rename");
    let path_c = watch_dir.join("sess-c.jsonl");
    fs::rename(&path_a, &path_c).unwrap();
    let plan = classify_session_watch_event(
        &EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &[path_a.clone(), path_c.clone()],
        &watch_dir,
    );
    match &plan {
        SessionWatchPlan::Rename {
            remove_ids,
            upsert_paths,
        } => {
            assert_eq!(remove_ids, &vec!["sess-a".to_string()]);
            assert_eq!(upsert_paths.len(), 1);
            assert_eq!(
                upsert_paths[0].file_name().and_then(|s| s.to_str()),
                Some("sess-c.jsonl")
            );
        }
        other => panic!("期望 Rename，得到 {other:?}"),
    }
    apply_session_watch_plan(&shared, &worktree, &watch_dir, plan);
    let guard = shared.read().unwrap();
    assert!(
        !guard.sessions.contains_key("sess-a"),
        "rename 后旧 id 必须移除"
    );
    assert!(
        guard.sessions.contains_key("sess-c"),
        "rename 后新 id 必须出现"
    );
    assert_eq!(
        guard.sessions.get("sess-c").map(|s| s.title.as_str()),
        Some("alpha")
    );
}

/// Business Logic（为什么需要这个测试）:
///     域外路径绝不能改写当前 worktree 索引。
///
/// Code Logic（这个测试做什么）:
///     对 watch_dir 外的 jsonl 上报 Remove，断言 Ignore 且索引不变。
#[test]
fn watcher_delete_ignores_paths_outside_root() {
    let (worktree, watch_dir, shared, _path_a, _path_b) =
        make_shared_index_with_two_sessions("watcher_outside");
    let outside_root = unique_temp_dir("watcher_outside_peer");
    let outside = write_jsonl(
        &outside_root,
        "sess-x",
        &[
            r#"{"type":"user","message":{"role":"user","content":"x"},"timestamp":"2026-01-01T00:00:00Z"}"#,
        ],
    );
    let plan =
        classify_session_watch_event(&EventKind::Remove(RemoveKind::File), &[outside], &watch_dir);
    assert_eq!(plan, SessionWatchPlan::Ignore);
    apply_session_watch_plan(&shared, &worktree, &watch_dir, plan);
    let guard = shared.read().unwrap();
    assert_eq!(guard.sessions.len(), 2);
}

/// Business Logic（为什么需要这个测试）:
///     不确定事件必须收敛为一次有界 rescan，而不是 silently drop。
///
/// Code Logic（这个测试做什么）:
///     EventKind::Any → BoundedRescan；删文件后 apply rescan 收敛索引。
#[test]
fn watcher_uncertain_event_requests_bounded_rescan() {
    let (worktree, watch_dir, shared, path_a, _path_b) =
        make_shared_index_with_two_sessions("watcher_uncertain");
    fs::remove_file(&path_a).unwrap();
    let plan =
        classify_session_watch_event(&EventKind::Any, std::slice::from_ref(&path_a), &watch_dir);
    assert_eq!(plan, SessionWatchPlan::BoundedRescan);
    apply_session_watch_plan(&shared, &worktree, &watch_dir, plan);
    let guard = shared.read().unwrap();
    assert!(
        !guard.sessions.contains_key("sess-a"),
        "rescan 后已删文件对应 session 应消失"
    );
    assert!(guard.sessions.contains_key("sess-b"));
}

/// Business Logic（为什么需要这个测试）:
///     shutdown/cancel 后不得再有 trailing 后台任务执行写回。
///
/// Code Logic（这个测试做什么）:
///     用 CancellationToken + select 模拟 trailing 任务；cancel 后断言 flag 未置位。
#[tokio::test]
async fn watcher_shutdown_cancels_pending_background_task() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let cancel = CancellationToken::new();
    let ran = Arc::new(AtomicBool::new(false));
    let ran2 = Arc::clone(&ran);
    let cancel2 = cancel.clone();
    let handle = tauri::async_runtime::spawn(async move {
        tokio::select! {
            _ = cancel2.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
        if cancel2.is_cancelled() {
            return;
        }
        ran2.store(true, Ordering::SeqCst);
    });

    // 模拟 force_dispose：cancel + abort
    cancel.cancel();
    handle.abort();
    // 给调度器一点时间确认任务不会跑完
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(cancel.is_cancelled(), "dispose 后 token 必须 cancelled");
    assert!(
        !ran.load(Ordering::SeqCst),
        "取消后 trailing 任务不得执行写回"
    );
}

/// Business Logic（为什么需要这个测试）:
///     normalize 必须拒绝 root 外路径，接受 root 内路径。
///
/// Code Logic（这个测试做什么）:
///     在 root 内外各建文件，断言 inside Some / outside None。
#[test]
fn normalize_path_inside_root_filters_outsiders() {
    let root = unique_temp_dir("normalize_root");
    let inside = root.join("a.jsonl");
    fs::write(&inside, b"{}\n").unwrap();
    let outside_root = unique_temp_dir("normalize_out");
    let outside = outside_root.join("b.jsonl");
    fs::write(&outside, b"{}\n").unwrap();
    assert!(normalize_path_inside_root(&inside, &root).is_some());
    assert!(normalize_path_inside_root(&outside, &root).is_none());
}
