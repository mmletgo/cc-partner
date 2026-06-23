//! cc/collector.rs — Claude Code 历史 prompt 采集器
//!
//! Business Logic（为什么需要这个模块）:
//!     用户在本机用 Claude Code 时，每次"用户输入 prompt"都 append 到
//!     `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl`。这些 prompt 分散在大量 jsonl
//!     文件中，难以检索和跨设备复用。本模块定时扫描这些文件，提取真正的"用户输入"
//!     （过滤掉 slash 命令、`!` bash 命令、工具结果回显、空内容），按项目(cwd)归类入库，
//!     并通过 scan_state 记录每个文件的 (mtime, size) 实现增量去重——文件未变则跳过。
//!
//! Code Logic（这个模块做什么）:
//!     - `claude_projects_dir()`：返回 `~/.claude/projects` 路径（跨平台用 dirs::home_dir）。
//!     - jsonl 行用宽松结构反序列化（`#[serde(default)]`，content 为 Option<Value>，未知字段忽略）。
//!     - `extract_prompt`：过滤 type==user && message.role==user && content 为 String &&
//!       trim 非空 && 不以 '/' 开头 && 不以 '!' 开头 && uuid/cwd/timestamp 齐全 → 产出 Extracted。
//!     - `scan_once`：枚举 projects 子目录 → 每个目录 *.jsonl → 比对 scan_state，未变跳过；
//!       变化则 spawn_blocking 内 BufReader::lines() 流式解析，过滤后转 Row（vector_clock 恒
//!       `{device_id:1}`），bulk_ingest（INSERT OR IGNORE）入库，更新 scan_state；返回新插入总数。
//!     - `start`：spawn 后台任务，立即扫一次 → 5 分钟 interval（MissedTickBehavior::Skip）循环扫描，
//!       提供 CancellationToken 供应用退出时优雅停止。

use crate::cc::models::ClaudeHistoryRow;
use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// 扫描间隔（秒）。对照任务规格 300s。
const SCAN_INTERVAL_SECS: u64 = 300;

/// 返回 Claude Code projects 目录：`~/.claude/projects`。
///
/// Business Logic: Claude Code 把每个工作目录的 session jsonl 存到该目录下的编码子目录。
/// Code Logic: dirs::home_dir 跨平台取 home，拼接 `.claude/projects`。home 取不到返回 None。
pub fn claude_projects_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// jsonl 单行的宽松反序列化结构（未知字段忽略，缺失字段用 default）。
///
/// Business Logic: Claude Code jsonl 行字段会随版本演进变化（新增 entrypoint/userType 等），
///     采集只需关心 prompt 提取相关字段，其余一律忽略，避免字段变更导致反序列化失败。
#[derive(Debug, Default, Deserialize)]
struct JsonlLine {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    message: Option<RawMessage>,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default, rename = "gitBranch")]
    git_branch: Option<String>,
    #[serde(default, rename = "version")]
    cc_version: Option<String>,
}

/// message 字段的宽松结构（仅关心 role 与 content）。
#[derive(Debug, Default, Deserialize)]
struct RawMessage {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: Option<serde_json::Value>,
}

/// 从一行 jsonl 提取出的有效用户 prompt（已通过全部过滤条件）。
struct Extracted {
    uuid: String,
    cwd: String,
    session_id: String,
    content: String,
    git_branch: Option<String>,
    cc_version: Option<String>,
    timestamp: String,
}

/// 从一行 jsonl 解析并过滤，返回有效 prompt 的 Extracted（不符合条件返回 None）。
///
/// Business Logic: Claude Code jsonl 里 type==user 的行包含"真正用户输入"和"工具结果回显"
///     （后者 content 是 array of tool_result blocks）。只保留真正的文本用户输入，且排除
///     slash 命令（`/xxx`）和 bash 命令（`!xxx`，Claude Code 的 bash 模式）。
///
/// Code Logic: 条件全部满足才返回 Extracted：
///     1. type == "user"；
///     2. message.role == "user"；
///     3. content 是 Value::String（排除 array 形式的工具结果回显）；
///     4. trim 后非空；
///     5. 不以 '/' 开头（slash 命令）；
///     6. 不以 '!' 开头（bash 命令）；
///     7. uuid / cwd / timestamp 齐全。
///     session_id 缺失时回退用 timestamp 派生（极少数旧版本无 sessionId）。
fn extract_prompt(line: &str) -> Option<Extracted> {
    let parsed: JsonlLine = serde_json::from_str(line).ok()?;
    if parsed.r#type != "user" {
        return None;
    }
    let message = parsed.message.as_ref()?;
    if message.role != "user" {
        return None;
    }
    // content 必须是纯字符串（工具结果回显是 array，跳过）
    let content_str = match &message.content {
        Some(serde_json::Value::String(s)) => s.clone(),
        _ => return None,
    };
    let trimmed = content_str.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 排除 slash 命令与 bash 命令
    if trimmed.starts_with('/') || trimmed.starts_with('!') {
        return None;
    }
    let uuid = parsed.uuid.clone()?;
    let cwd = parsed.cwd.clone()?;
    let timestamp = parsed.timestamp.clone()?;
    // session_id 缺失时回退（避免 id 拼接失败），正常路径取 jsonl 的 sessionId
    let session_id = parsed
        .session_id
        .clone()
        .unwrap_or_else(|| format!("unknown-{}", &timestamp));
    Some(Extracted {
        uuid,
        cwd,
        session_id,
        content: trimmed.to_string(),
        git_branch: parsed.git_branch.clone(),
        cc_version: parsed.cc_version.clone(),
        timestamp,
    })
}

/// 把 Extracted 转成 ClaudeHistoryRow。
///
/// Business Logic: 采集入库的行 vector_clock 恒为 `{device_id:1}`（永不递增——递增会破坏
///     同步合并出的因果历史）；id 用 `{session_id}:{uuid}` 保证同 session 内 uuid 唯一、
///     跨 session 隔离；created_at/updated_at 用当前入库时间。
fn extracted_to_row(e: &Extracted, device_id: &str, now: &str) -> ClaudeHistoryRow {
    let mut vc = HashMap::new();
    vc.insert(device_id.to_string(), 1u64);
    ClaudeHistoryRow {
        id: format!("{}:{}", e.session_id, e.uuid),
        project_path: e.cwd.clone(),
        project_name: ClaudeHistoryRow::derive_project_name(&e.cwd),
        session_id: e.session_id.clone(),
        content: e.content.clone(),
        git_branch: e.git_branch.clone(),
        cc_version: e.cc_version.clone(),
        occurred_at: e.timestamp.clone(),
        device_id: device_id.to_string(),
        vector_clock: vc,
        created_at: now.to_string(),
        updated_at: now.to_string(),
        deleted: false,
    }
}

/// 执行一次完整扫描：枚举 projects 目录、增量解析变化文件、入库新 prompt。
///
/// Business Logic: 定时器与启动时各调一次。只处理 mtime 或 size 变化的 jsonl 文件，
///     用 INSERT OR IGNORE 入库（绝不覆盖已存在行），返回本次新插入总条数。
///
/// Code Logic:
///     1. 取 projects 目录，不存在直接返回 0；
///     2. 读取 scan_state（{file_path: (mtime_sec, size)}）；
///     3. 在 spawn_blocking 内同步枚举目录与读文件（fs IO 不阻塞 async runtime）：
///        - 枚举 projects 一级子目录（每个对应一个 cwd 项目）；
///        - 每个子目录下 *.jsonl，取 metadata 的 (mtime_sec, size)，与 scan_state 比对，未变跳过；
///        - 变化的文件 BufReader::lines() 流式解析，单行失败 tracing::warn 跳过；
///        - 过滤出 Extracted，转 Row，收集待入库；
///     4. await 前 clone device_id（Arc<String>）；
///     5. bulk_ingest 入库；逐个变化文件 update_scan_state；返回新插入总数。
pub async fn scan_once(state: &AppState) -> Result<usize, AppError> {
    let projects_dir = match claude_projects_dir() {
        Some(p) => p,
        None => {
            tracing::debug!("无法获取 home 目录，跳过 CC 历史采集");
            return Ok(0);
        }
    };
    if !projects_dir.exists() {
        tracing::debug!("Claude Code projects 目录不存在: {:?}", projects_dir);
        return Ok(0);
    }

    // await 前 clone device_id（Arc<String>），避免跨 await 持引用
    let device_id: String = state.device_id.as_ref().clone();

    // 读取 scan_state 快照（HashMap clone，spawn_blocking 内只读使用）
    let scan_states = state.cc_history_repo.get_scan_states().await?;

    // spawn_blocking 内做全部 fs IO（枚举目录、读 metadata、流式读 jsonl）
    let (rows, changed_files, scan_errors) = tokio::task::spawn_blocking(move || {
        let mut rows: Vec<ClaudeHistoryRow> = Vec::new();
        let mut changed_files: Vec<(PathBuf, i64, i64)> = Vec::new();
        let mut scan_errors: usize = 0;
        let now = Utc::now().to_rfc3339();

        // 枚举 projects 一级子目录
        let sub_dirs = match std::fs::read_dir(&projects_dir) {
            Ok(rd) => rd,
            Err(e) => {
                tracing::warn!("读取 projects 目录失败: {e}");
                return (rows, changed_files, scan_errors);
            }
        };

        for entry in sub_dirs.flatten() {
            let dir_path = match entry.file_type().ok().filter(|t| t.is_dir()) {
                Some(_) => entry.path(),
                None => continue,
            };
            // 子目录下 *.jsonl
            let file_iter = match std::fs::read_dir(&dir_path) {
                Ok(rd) => rd,
                Err(e) => {
                    tracing::warn!("读取项目目录失败 {:?}: {e}", dir_path);
                    continue;
                }
            };
            for f in file_iter.flatten() {
                let path = f.path();
                if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }
                // metadata 取 mtime_sec 与 size
                let md = match std::fs::metadata(&path) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("读取文件元数据失败 {:?}: {e}", path);
                        continue;
                    }
                };
                let size = md.len() as i64;
                let mtime_sec = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let key = path.to_string_lossy().to_string();
                // 增量比对：mtime 与 size 都未变 → 跳过
                if let Some((prev_mtime, prev_size)) = scan_states.get(&key) {
                    if *prev_mtime == mtime_sec && *prev_size == size {
                        continue;
                    }
                }
                // 流式解析该文件
                let file = match std::fs::File::open(&path) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!("打开 jsonl 失败 {:?}: {e}", path);
                        continue;
                    }
                };
                use std::io::{BufRead, BufReader};
                let reader = BufReader::new(file);
                for line_res in reader.lines() {
                    match line_res {
                        Ok(line) => {
                            if let Some(e) = extract_prompt(&line) {
                                rows.push(extracted_to_row(&e, &device_id, &now));
                            }
                        }
                        Err(e) => {
                            scan_errors += 1;
                            tracing::warn!("读取 jsonl 行失败 {:?}: {e}", path);
                        }
                    }
                }
                changed_files.push((path, mtime_sec, size));
            }
        }

        (rows, changed_files, scan_errors)
    })
    .await
    .map_err(|e| AppError::generic(format!("采集任务 join 失败: {e}")))?;

    if scan_errors > 0 {
        tracing::warn!("本次扫描共遇到 {} 个行读取错误（已跳过）", scan_errors);
    }

    // 入库（INSERT OR IGNORE，绝不覆盖已存在行）
    let inserted = state.cc_history_repo.bulk_ingest(&rows).await?;

    // 更新 scan_state（仅变化的文件）
    let scanned_at = Utc::now().to_rfc3339();
    for (path, mtime_sec, size) in &changed_files {
        let key = path.to_string_lossy().to_string();
        if let Err(e) = state
            .cc_history_repo
            .update_scan_state(&key, *mtime_sec, *size, &scanned_at)
            .await
        {
            tracing::warn!("更新 scan_state 失败 {key}: {e}");
        }
    }

    tracing::info!(
        "CC 历史扫描完成：解析出 {} 条候选，新入库 {} 条，扫描 {} 个文件",
        rows.len(),
        inserted,
        changed_files.len()
    );
    Ok(inserted)
}

/// 启动后台采集器，返回 CancellationToken 供应用退出时停止。
///
/// Business Logic: 应用启动后立即扫一次，之后每 5 分钟扫一次。错误仅记录不 panic，
///     不影响应用主功能。
///
/// Code Logic: 用 `tauri::async_runtime::spawn`（非 `tokio::spawn`）启动后台任务——
///     本函数在 lib.rs setup 闭包的同步部分（`app.manage` 之后，block_on 之外）调用，
///     主线程无 Tokio reactor 上下文，`tokio::spawn` 会 panic "there is no reactor running"；
///     `tauri::async_runtime::spawn` 走 Tauri 全局 runtime handle，不依赖当前线程上下文
///    （与 discovery.rs / commands/updater.rs 的 spawn 位置一致）。任务内：
///     1. 立即 scan_once 一次；
///     2. 建 interval(300s) + MissedTickBehavior::Skip，先 tick() 吃掉首次立即触发；
///     3. loop select! { cancel.cancelled() => break; ticker.tick() => scan_once }；
///     scan_once 错误仅 tracing::error。
pub fn start(state: AppState) -> CancellationToken {
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tauri::async_runtime::spawn(async move {
        tracing::info!("CC 历史采集器启动，立即执行首次扫描");
        // 立即扫一次
        if let Err(e) = scan_once(&state).await {
            tracing::error!("CC 历史首次扫描失败: {e}");
        }
        // 5 分钟 interval，跳过累积的错过的 tick
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(SCAN_INTERVAL_SECS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // 吃掉 interval 首次立即触发（首次扫描已手动做过）
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    tracing::info!("CC 历史采集器收到取消信号，退出循环");
                    break;
                }
                _ = ticker.tick() => {
                    if let Err(e) = scan_once(&state).await {
                        tracing::error!("CC 历史扫描失败: {e}");
                    }
                }
            }
        }
    });
    cancel
}

#[cfg(test)]
mod tests {
    //! collector 单测：验证 extract_prompt 的过滤逻辑（slash/bash 命令、工具结果 array、空内容等）。

    use super::*;

    #[test]
    fn extract_plain_user_prompt() {
        let line = r#"{"type":"user","message":{"role":"user","content":"Read test.txt and print it."},"uuid":"u1","cwd":"/tmp/proj","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","version":"2.1.1","gitBranch":"main"}"#;
        let e = extract_prompt(line).expect("应提取成功");
        assert_eq!(e.uuid, "u1");
        assert_eq!(e.cwd, "/tmp/proj");
        assert_eq!(e.session_id, "s1");
        assert_eq!(e.content, "Read test.txt and print it.");
        assert_eq!(e.git_branch.as_deref(), Some("main"));
        assert_eq!(e.cc_version.as_deref(), Some("2.1.1"));
    }

    #[test]
    fn extract_skips_assistant_and_tool_result() {
        // type != user
        let assistant = r#"{"type":"assistant","message":{"role":"assistant","content":"hi"},"uuid":"u","cwd":"/p","timestamp":"t"}"#;
        assert!(extract_prompt(assistant).is_none());

        // content 是 array（工具结果回显）→ 跳过
        let tool_result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"..."}]},"uuid":"u","cwd":"/p","timestamp":"t"}"#;
        assert!(extract_prompt(tool_result).is_none());
    }

    #[test]
    fn extract_skips_slash_and_bash_commands() {
        let slash = r#"{"type":"user","message":{"role":"user","content":"/clear"},"uuid":"u","cwd":"/p","timestamp":"t"}"#;
        assert!(extract_prompt(slash).is_none());
        let bash = r#"{"type":"user","message":{"role":"user","content":"!ls -la"},"uuid":"u","cwd":"/p","timestamp":"t"}"#;
        assert!(extract_prompt(bash).is_none());
    }

    #[test]
    fn extract_skips_empty_and_missing_fields() {
        // 空白内容
        let empty = r#"{"type":"user","message":{"role":"user","content":"   "},"uuid":"u","cwd":"/p","timestamp":"t"}"#;
        assert!(extract_prompt(empty).is_none());
        // 缺 uuid
        let no_uuid = r#"{"type":"user","message":{"role":"user","content":"hi"},"cwd":"/p","timestamp":"t"}"#;
        assert!(extract_prompt(no_uuid).is_none());
        // 缺 cwd
        let no_cwd = r#"{"type":"user","message":{"role":"user","content":"hi"},"uuid":"u","timestamp":"t"}"#;
        assert!(extract_prompt(no_cwd).is_none());
    }

    #[test]
    fn extract_trims_content() {
        // 内容带首尾空白应 trim
        let line = r#"{"type":"user","message":{"role":"user","content":"  hello world  "},"uuid":"u","cwd":"/p","timestamp":"t","sessionId":"s"}"#;
        let e = extract_prompt(line).unwrap();
        assert_eq!(e.content, "hello world");
    }

    #[test]
    fn extract_handles_missing_optional_fields() {
        // 无 gitBranch/version/sessionId 也能提取（sessionId 回退）
        let line = r#"{"type":"user","message":{"role":"user","content":"hi"},"uuid":"u","cwd":"/p","timestamp":"2026-01-01T00:00:00Z"}"#;
        let e = extract_prompt(line).unwrap();
        assert!(e.git_branch.is_none());
        assert!(e.cc_version.is_none());
        // sessionId 缺失 → 回退 timestamp 派生
        assert!(e.session_id.starts_with("unknown-"));
    }

    #[test]
    fn derived_id_combines_session_and_uuid() {
        let e = Extracted {
            uuid: "uuid-123".to_string(),
            cwd: "/p".to_string(),
            session_id: "sess-1".to_string(),
            content: "hi".to_string(),
            git_branch: None,
            cc_version: None,
            timestamp: "t".to_string(),
        };
        let row = extracted_to_row(&e, "d1", "2026-01-01T00:00:00+00:00");
        assert_eq!(row.id, "sess-1:uuid-123");
        // vector_clock 恒 {device_id:1}
        assert_eq!(row.vector_clock.get("d1"), Some(&1));
        assert_eq!(row.vector_clock.len(), 1);
        assert!(!row.deleted);
    }

    #[test]
    fn invalid_json_line_returns_none() {
        assert!(extract_prompt("not json").is_none());
        assert!(extract_prompt("").is_none());
    }

    #[test]
    fn derive_project_name_from_path() {
        assert_eq!(
            ClaudeHistoryRow::derive_project_name("/Users/hans/foo"),
            "foo"
        );
        assert_eq!(ClaudeHistoryRow::derive_project_name("/foo/bar/baz"), "baz");
        // 根路径无末段 → 回退原路径
        assert_eq!(ClaudeHistoryRow::derive_project_name("/"), "/");
    }
}
