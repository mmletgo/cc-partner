//! Agent usage 提取单测模块（由 `agent_usage.rs` 以 `#[path]` 挂载）。
//!
//! Business Logic:
//!     将大体量 `#[cfg(test)]` 从生产源文件拆出，避免 module-boundary 软上限与测试膨胀互相绑死，
//!     同时保留子模块对 `agent_usage` 私有 helper 的可见性。
//!
//! Code Logic:
//!     文件本体即为 `mod tests` 的模块体；仅在 `cfg(test)` 下由父模块 `#[path]` 引入。

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
    assert!(is_usage_extractable_provider("cursorCliVisible"));
    assert!(is_usage_extractable_provider("cursor"));
    assert!(is_usage_extractable_provider("piVisible"));
    assert!(is_usage_extractable_provider("pi"));
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

/// Grok 真实 signals.json 只有 context/TTFT/model，没有 billed input/output。
#[test]
fn grok_real_signals_extracts_context_without_billed_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let session = tmp.path().join("sessions/encoded-cwd/sess-real");
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(
        session.join("signals.json"),
        serde_json::json!({
            "contextTokensUsed": 324271,
            "contextWindowTokens": 500000,
            "contextWindowUsage": 0.648,
            "primaryModelId": "grok-4.6",
            "modelsUsed": ["grok-4.6"],
            "avgTimeToFirstTokenMs": 1629
        })
        .to_string(),
    )
    .unwrap();

    let snap = extract_grok_usage(Some(tmp.path().to_path_buf()), "sess-real").unwrap();
    assert!(snap.input_tokens.is_none());
    assert!(snap.output_tokens.is_none());
    assert!(snap.cache_read_tokens.is_none());
    assert_eq!(snap.model_id.as_deref(), Some("grok-4.6"));
    assert_eq!(snap.context_length, Some(324271));
    assert_eq!(snap.context_window, Some(500000));
    assert_eq!(snap.first_token_avg_ms, Some(1629));
}

/// Cursor：只有 meta.json 无 token → None；jsonl usage 可抽取。
#[test]
fn cursor_meta_only_returns_none_jsonl_extracts() {
    let tmp = tempfile::tempdir().unwrap();
    let chat = tmp.path().join("chats/hash/chat-1");
    std::fs::create_dir_all(&chat).unwrap();
    std::fs::write(
        chat.join("meta.json"),
        serde_json::json!({"cwd": "/tmp/p", "hasConversation": true}).to_string(),
    )
    .unwrap();
    assert!(extract_cursor_usage(Some(tmp.path().to_path_buf()), "chat-1").is_none());

    std::fs::write(
        chat.join("events.jsonl"),
        serde_json::json!({
            "usage": {
                "input_tokens": 4,
                "output_tokens": 7,
                "cacheRead": 2
            }
        })
        .to_string(),
    )
    .unwrap();
    let snap = extract_cursor_usage(Some(tmp.path().to_path_buf()), "chat-1").unwrap();
    assert_eq!(snap.input_tokens, Some(4));
    assert_eq!(snap.output_tokens, Some(7));
    assert_eq!(snap.cache_read_tokens, Some(2));
    assert!(extract_cursor_usage(Some(tmp.path().to_path_buf()), "../x").is_none());
}

/// Pi：累加 assistant usage；user 行忽略；totalTokens 作 occupancy。
#[test]
fn pi_jsonl_sums_assistant_usage() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions/--tmp-p--");
    std::fs::create_dir_all(&sessions).unwrap();
    let id = "11111111-2222-3333-4444-555555555555";
    let path = sessions.join(format!("2026-08-26T00-00-00_{id}.jsonl"));
    let lines = [
        serde_json::json!({
            "type": "session",
            "id": id,
            "cwd": "/tmp/p"
        })
        .to_string(),
        serde_json::json!({
            "type": "message",
            "id": "u1",
            "message": {"role": "user", "content": "hi"}
        })
        .to_string(),
        serde_json::json!({
            "type": "message",
            "id": "a1",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-5",
                "usage": {
                    "input": 10,
                    "output": 2,
                    "cacheRead": 5,
                    "cacheWrite": 1,
                    "totalTokens": 17,
                    "cost": {"total": 0.05}
                }
            }
        })
        .to_string(),
        serde_json::json!({
            "type": "message",
            "id": "a2",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-5",
                "usage": {
                    "input": 20,
                    "output": 3,
                    "cacheRead": 8,
                    "cacheWrite": 0,
                    "totalTokens": 31,
                    "cost": {"total": 0.07}
                }
            }
        })
        .to_string(),
    ];
    std::fs::write(&path, lines.join("\n")).unwrap();

    let snap = extract_pi_usage(Some(tmp.path().to_path_buf()), id).unwrap();
    assert_eq!(snap.input_tokens, Some(30));
    assert_eq!(snap.output_tokens, Some(5));
    assert_eq!(snap.cache_read_tokens, Some(13));
    assert_eq!(snap.cache_write_tokens, Some(1));
    assert_eq!(snap.context_length, Some(31));
    assert_eq!(snap.model_id.as_deref(), Some("claude-opus-4-5"));
    assert_eq!(snap.cost_major.as_deref(), Some("0.12"));
    assert!(extract_pi_usage(Some(tmp.path().to_path_buf()), "../x").is_none());
}
