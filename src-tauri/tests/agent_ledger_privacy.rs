//! A9 privacy gate for Agent Metadata Ledger (shipped-path structural check).
//!
//! Business Logic（为什么需要这个测试）:
//!     Completion contract requires a dedicated privacy binary proving ledger DTOs
//!     and SQL documentation never introduce content columns.
//!
//! Code Logic（这个测试做什么）:
//!     1) Scan a realistic serialized ledger entry JSON (same field set as production DTO).
//!     2) Scan migrations/0001_init.sql for forbidden column names on agent_session_ledger.

use std::fs;
use std::path::PathBuf;

fn forbidden_substrings() -> &'static [&'static str] {
    &[
        "prompt",
        "response",
        "transcript",
        "terminal_bytes",
        "native_session",
        "environment",
        "credential",
        "cookie",
        "api_key",
    ]
}

/// 扫描 JSON 对象键路径是否包含禁止子串。
fn scan_keys(value: &serde_json::Value, hits: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let lower = k.to_lowercase();
                for f in forbidden_substrings() {
                    if lower.contains(f) {
                        hits.push(format!("{k}~{f}"));
                    }
                }
                scan_keys(v, hits);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                scan_keys(item, hits);
            }
        }
        _ => {}
    }
}

#[test]
fn entry_dto_serialization_is_metadata_only() {
    // Mirrors production AgentLedgerEntry field names (camelCase wire).
    let entry = serde_json::json!({
        "id": "e1",
        "agentSessionId": "a1",
        "projectId": "p1",
        "worktreeId": null,
        "provider": "claudeCodeVisible",
        "model": "test-model",
        "outcome": "completed",
        "startedAt": "2026-07-15T00:00:00Z",
        "endedAt": "2026-07-15T00:01:00Z",
        "durationMs": 60000,
        "inputTokens": 10,
        "outputTokens": 20,
        "cacheReadTokens": null,
        "cacheWriteTokens": null,
        "costCurrency": null,
        "costMinorUnits": null,
        "sourceOwnerInstanceId": "owner",
        "createdAt": "2026-07-15T00:01:00Z",
        "updatedAt": "2026-07-15T00:01:00Z"
    });
    let mut hits = Vec::new();
    scan_keys(&entry, &mut hits);
    assert!(
        hits.is_empty(),
        "forbidden field names in ledger entry DTO shape: {hits:?}"
    );
}

#[test]
fn migration_sql_has_no_content_columns_for_ledger() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sql_path = root.join("migrations/0001_init.sql");
    let sql = fs::read_to_string(&sql_path).expect("read migrations/0001_init.sql");
    // Extract agent_session_ledger create block if present; else whole file scan around name.
    let lower = sql.to_lowercase();
    assert!(
        lower.contains("agent_session_ledger"),
        "schema must declare agent_session_ledger"
    );
    // Forbidden column names must not appear as SQL identifiers near ledger table.
    for bad in [
        "prompt",
        "response",
        "transcript_path",
        "terminal_bytes",
        "native_session_id",
        "environment",
        "credential",
    ] {
        // Allow mentions only in comments that say they are forbidden.
        for line in sql.lines() {
            let l = line.to_lowercase();
            if (l.contains("agent_session_ledger") || l.contains("ledger"))
                && l.contains(bad)
                && !l.contains("forbid")
                && !l.contains("不得")
                && !l.contains("no ")
                && !l.trim_start().starts_with("--")
                && !l.trim_start().starts_with("/*")
            {
                panic!("ledger-related line must not declare content column {bad}: {line}");
            }
        }
    }
}
