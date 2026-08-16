//! workbench/agent_runtime/live_usage — 进程内 live usage 缓存
//!
//! Business Logic（为什么需要这个模块）:
//!     状态卡在 working/needsInput/idle 期间也要展示输入/输出速率与上下文用量；
//!     Ledger 只在终态写入，不能作为当前会话真值。抽取结果只含 tokens/model，
//!     禁止缓存 native_session_id、路径、prompt 或 transcript。
//!
//! Code Logic（这个模块做什么）:
//!     按 agent_session_id 缓存最近一次可靠 usage；定位会话文件后用 mtime+size
//!     跳过未变更重解析；OpenCode 无文件路径则每次查询 SQLite。缓存不落盘。

use super::agent_usage::{
    extract_provider_usage, extract_provider_usage_from_path, is_usage_extractable_provider,
    locate_provider_session_file,
};
use super::models::AgentSessionRuntime;
use super::snapshot::AgentLiveUsageDto;
use crate::workbench::agent_ledger::ReliableUsageSnapshot;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 进程内最多缓存的 agent 行（超出时只保留本轮仍 active 的 id）。
const MAX_CACHED_SESSIONS: usize = 256;

/// tokens + model 指纹，用于判断是否需要向 UI 再投影。
#[derive(Debug, Clone, PartialEq, Eq)]
struct UsageFingerprint {
    model_id: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    context_length: Option<u64>,
    context_window: Option<u64>,
    active_duration_ms: Option<u64>,
}

impl UsageFingerprint {
    fn from_snapshot(snapshot: &ReliableUsageSnapshot) -> Self {
        Self {
            model_id: snapshot.model_id.clone(),
            input_tokens: snapshot.input_tokens,
            output_tokens: snapshot.output_tokens,
            cache_read_tokens: snapshot.cache_read_tokens,
            cache_write_tokens: snapshot.cache_write_tokens,
            context_length: snapshot.context_length,
            context_window: snapshot.context_window,
            active_duration_ms: snapshot.active_duration_ms,
        }
    }
}

/// 单个 agent 的 live usage 缓存行。
#[derive(Debug, Clone)]
struct CachedLiveUsage {
    fingerprint: UsageFingerprint,
    snapshot: ReliableUsageSnapshot,
    extracted_at: String,
    file_path: Option<PathBuf>,
    file_len: Option<u64>,
    file_mtime_ns: Option<u128>,
}

/// agent_session_id → 最近一次抽取结果。
fn usage_cache() -> &'static Mutex<HashMap<String, CachedLiveUsage>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedLiveUsage>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// provider\0native → 已定位的会话文件（避免重复目录遍历）。
fn path_index() -> &'static Mutex<HashMap<String, PathBuf>> {
    static INDEX: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    INDEX.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_cache() -> std::sync::MutexGuard<'static, HashMap<String, CachedLiveUsage>> {
    usage_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_paths() -> std::sync::MutexGuard<'static, HashMap<String, PathBuf>> {
    path_index()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn path_index_key(provider_id: &str, native_session_id: &str) -> String {
    format!("{provider_id}\0{native_session_id}")
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 读取普通文件的长度与 mtime（纳秒）；失败返回 None（fail-closed 重解析）。
fn read_file_stamp(path: &std::path::Path) -> Option<(u64, u128)> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .or_else(|| {
            meta.created()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        })
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some((meta.len(), mtime_ns))
}

/// 解析或复用已缓存的会话文件路径。
fn resolve_session_path(provider_id: &str, native_session_id: &str) -> Option<PathBuf> {
    let key = path_index_key(provider_id, native_session_id);
    {
        let index = lock_paths();
        if let Some(cached) = index.get(&key) {
            if cached.is_file() {
                return Some(cached.clone());
            }
        }
    }
    let found = locate_provider_session_file(provider_id, native_session_id)?;
    lock_paths().insert(key, found.clone());
    Some(found)
}

fn snapshot_to_dto(snapshot: &ReliableUsageSnapshot, extracted_at: &str) -> AgentLiveUsageDto {
    AgentLiveUsageDto {
        model_id: snapshot.model_id.clone(),
        input_tokens: snapshot.input_tokens,
        output_tokens: snapshot.output_tokens,
        cache_read_tokens: snapshot.cache_read_tokens,
        cache_write_tokens: snapshot.cache_write_tokens,
        context_length: snapshot.context_length,
        context_window: snapshot.context_window,
        active_duration_ms: snapshot.active_duration_ms,
        extracted_at: extracted_at.to_string(),
    }
}

/// 读取进程内缓存的投影 DTO（不含 native id / 路径）。
///
/// Business Logic（为什么需要这个函数）:
///     snapshot/event 投影必须在边界附上 live usage，且不得回源 CLI 文件。
///
/// Code Logic（这个函数做什么）:
///     按 agent_session_id 查缓存；未命中返回 None。
pub fn cached_usage_dto(agent_session_id: &str) -> Option<AgentLiveUsageDto> {
    let cache = lock_cache();
    cache
        .get(agent_session_id)
        .map(|entry| snapshot_to_dto(&entry.snapshot, &entry.extracted_at))
}

/// 把一次抽取结果写入缓存（终态补记与 live tick 共用）。
///
/// Business Logic（为什么需要这个函数）:
///     终态 extract 成功后应立刻出现在下一次 emit，避免状态卡继续「未提供」。
///
/// Code Logic（这个函数做什么）:
///     覆盖该 agent 的缓存行；返回 tokens/model 是否相对旧值变化。
pub fn store(agent_session_id: &str, snapshot: &ReliableUsageSnapshot) -> bool {
    if !snapshot.has_any() {
        return false;
    }
    let fingerprint = UsageFingerprint::from_snapshot(snapshot);
    let mut cache = lock_cache();
    let changed = cache
        .get(agent_session_id)
        .map(|old| old.fingerprint != fingerprint)
        .unwrap_or(true);
    cache.insert(
        agent_session_id.to_string(),
        CachedLiveUsage {
            fingerprint,
            snapshot: snapshot.clone(),
            extracted_at: now_rfc3339(),
            file_path: None,
            file_len: None,
            file_mtime_ns: None,
        },
    );
    changed
}

/// 只保留仍需要投影的 agent id，防止缓存无限增长。
///
/// Business Logic（为什么需要这个函数）:
///     终态 session 离开 list_active 后不必永久占满内存。
///
/// Code Logic（这个函数做什么）:
///     retain 传入集合；同时丢掉失效的 path index 不在这里做（按 native 生命期更长）。
pub fn retain_agent_ids(keep: &HashSet<String>) {
    let mut cache = lock_cache();
    if cache.len() <= MAX_CACHED_SESSIONS && keep.is_empty() {
        return;
    }
    if !keep.is_empty() {
        cache.retain(|id, _| keep.contains(id));
    }
    if cache.len() > MAX_CACHED_SESSIONS {
        // 无序 HashMap：超额时整表按 keep 已裁过仍超则清空最便宜；生产 active ≤256。
        cache.clear();
    }
}

/// 单行刷新结果：未抽取 / 已抽取（是否值得再投影）。
enum RefreshOutcome {
    Skip,
    Extracted { changed: bool },
}

/// 对一行 active session 抽取并更新缓存。
///
/// Business Logic（为什么需要这个函数）:
///     owner worker 每 2s 刷新 working/needsInput/idle 的 tokens，文件未变则跳过重解析。
///
/// Code Logic（这个函数做什么）:
///     无 native / 不支持的 provider → false；文件 stamp 未变 → false；
///     抽取成功且 fingerprint 变化 → true。
pub fn refresh_row(row: &AgentSessionRuntime) -> bool {
    matches!(
        refresh_row_inner(row),
        RefreshOutcome::Extracted { changed: true }
    )
}

fn refresh_row_inner(row: &AgentSessionRuntime) -> RefreshOutcome {
    if !is_usage_extractable_provider(&row.provider_id) {
        return RefreshOutcome::Skip;
    }
    let Some(native) = row
        .native_session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return RefreshOutcome::Skip;
    };
    refresh_one(&row.id, &row.provider_id, native)
}

fn refresh_one(agent_id: &str, provider_id: &str, native_session_id: &str) -> RefreshOutcome {
    let located = resolve_session_path(provider_id, native_session_id);
    if let Some(ref path) = located {
        if let Some((len, mtime_ns)) = read_file_stamp(path) {
            let cache = lock_cache();
            if let Some(existing) = cache.get(agent_id) {
                if existing.file_path.as_ref() == Some(path)
                    && existing.file_len == Some(len)
                    && existing.file_mtime_ns == Some(mtime_ns)
                {
                    return RefreshOutcome::Skip;
                }
            }
        }
    }

    let extracted = if let Some(ref path) = located {
        extract_provider_usage_from_path(provider_id, path, native_session_id)
    } else {
        extract_provider_usage(provider_id, native_session_id)
    };
    let Some(snapshot) = extracted.filter(|item| item.has_any()) else {
        return RefreshOutcome::Skip;
    };

    let stamp = located.as_ref().and_then(|path| {
        read_file_stamp(path).map(|(len, mtime_ns)| (path.clone(), len, mtime_ns))
    });
    let fingerprint = UsageFingerprint::from_snapshot(&snapshot);
    let mut cache = lock_cache();
    let changed = cache
        .get(agent_id)
        .map(|old| old.fingerprint != fingerprint)
        .unwrap_or(true);
    cache.insert(
        agent_id.to_string(),
        CachedLiveUsage {
            fingerprint,
            snapshot,
            extracted_at: now_rfc3339(),
            file_path: stamp.as_ref().map(|(path, _, _)| path.clone()),
            file_len: stamp.as_ref().map(|(_, len, _)| *len),
            file_mtime_ns: stamp.as_ref().map(|(_, _, mtime)| *mtime),
        },
    );
    RefreshOutcome::Extracted { changed }
}

/// 每 tick 最多做这么多次真实抽取（stamp 未变不计入）。
const LIVE_USAGE_EXTRACT_CAP: usize = 32;

/// 批量刷新 active 行，返回 usage 发生变化的 agent id。
///
/// Business Logic（为什么需要这个函数）:
///     worker tick 需要一份变更清单再 emit，避免无变化刷屏。
///     retain 必须覆盖本轮全部 active，不能只留即将抽取的 32 条，否则其余会话缓存被误删。
///
/// Code Logic（这个函数做什么）:
///     先 retain 传入的全部 id；stamp 未变跳过且不占抽取配额；真实抽取最多 32 次。
pub fn refresh_active_rows(rows: &[AgentSessionRuntime]) -> Vec<String> {
    let keep: HashSet<String> = rows.iter().map(|row| row.id.clone()).collect();
    retain_agent_ids(&keep);
    let mut changed = Vec::new();
    let mut extracts = 0;
    for row in rows {
        match refresh_row_inner(row) {
            RefreshOutcome::Skip => {}
            RefreshOutcome::Extracted {
                changed: did_change,
            } => {
                extracts += 1;
                if did_change {
                    changed.push(row.id.clone());
                }
                if extracts >= LIVE_USAGE_EXTRACT_CAP {
                    break;
                }
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::agent_runtime::models::AgentSessionPhase;

    fn sample_snapshot(input: u64, output: u64) -> ReliableUsageSnapshot {
        ReliableUsageSnapshot {
            model_id: Some("claude-sonnet-4".into()),
            input_tokens: Some(input),
            output_tokens: Some(output),
            cache_read_tokens: Some(3),
            cache_write_tokens: Some(1),
            cost_major: None,
            cost_currency: None,
            context_length: Some(input.saturating_add(3).saturating_add(1)),
            context_window: Some(200_000),
            active_duration_ms: Some(10_000),
        }
    }

    fn sample_row(id: &str, native: &str) -> AgentSessionRuntime {
        AgentSessionRuntime {
            id: id.into(),
            project_id: "p".into(),
            worktree_id: None,
            terminal_session_id: "t".into(),
            orchestrator_task_id: None,
            orchestrator_attempt: None,
            provider_id: "claudeCodeVisible".into(),
            native_session_id: Some(native.into()),
            phase: AgentSessionPhase::Working,
            version: 1,
            started_at: "2026-08-16T10:00:00Z".into(),
            last_activity_at: "2026-08-16T10:00:01Z".into(),
            ended_at: None,
            outcome_code: None,
            resumed_from_agent_session_id: None,
            is_active: true,
        }
    }

    /// store 投影不得带 native/path，且同 fingerprint 视为未变化。
    #[test]
    fn store_projects_metadata_only_and_dedupes() {
        let snap = sample_snapshot(10, 4);
        assert!(store("agent-live-1", &snap));
        let dto = cached_usage_dto("agent-live-1").expect("cached");
        assert_eq!(dto.input_tokens, Some(10));
        assert_eq!(dto.output_tokens, Some(4));
        assert_eq!(dto.model_id.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(dto.context_length, Some(14));
        assert_eq!(dto.context_window, Some(200_000));
        assert_eq!(dto.active_duration_ms, Some(10_000));
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("native"));
        assert!(!json.contains("path"));
        assert!(!json.contains("transcript"));
        assert!(!store("agent-live-1", &snap));
        assert!(store("agent-live-1", &sample_snapshot(20, 8)));
    }

    /// 空 native / 不支持的 provider 不得写入缓存。
    #[test]
    fn refresh_skips_unextractable_rows() {
        let mut row = sample_row("agent-skip", "native-1");
        row.provider_id = "genericTerminal".into();
        assert!(!refresh_row(&row));
        row.provider_id = "claudeCodeVisible".into();
        row.native_session_id = Some(String::new());
        assert!(!refresh_row(&row));
        assert!(cached_usage_dto("agent-skip").is_none());
    }

    /// Claude fixture：文件未变跳过；tokens 增长后投影更新。
    #[test]
    fn claude_file_fingerprint_skips_unchanged_and_picks_up_growth() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("-Users-hans-live");
        std::fs::create_dir_all(&project).unwrap();
        let native = "live-s1";
        let path = project.join(format!("{native}.jsonl"));
        let line = |input: u64, output: u64| {
            serde_json::json!({
                "sessionId": native,
                "message": {
                    "id": "msg_1",
                    "model": "claude-sonnet-4",
                    "usage": {
                        "input_tokens": input,
                        "output_tokens": output,
                        "cache_read_input_tokens": 1,
                        "cache_creation_input_tokens": 0
                    }
                }
            })
            .to_string()
        };
        std::fs::write(&path, line(11, 5)).unwrap();

        let located = crate::workbench::agent_runtime::agent_usage::extract_claude_usage(
            Some(tmp.path().to_path_buf()),
            native,
        )
        .expect("first extract");
        assert!(store("agent-live-file", &located));

        // 直接走 from_path + stamp：同一内容再次 store 不视为变化。
        let again = crate::workbench::agent_runtime::agent_usage::extract_claude_usage(
            Some(tmp.path().to_path_buf()),
            native,
        )
        .unwrap();
        assert!(!store("agent-live-file", &again));

        std::fs::write(&path, line(40, 12)).unwrap();
        let grown = crate::workbench::agent_runtime::agent_usage::extract_claude_usage(
            Some(tmp.path().to_path_buf()),
            native,
        )
        .unwrap();
        assert!(store("agent-live-file", &grown));
        assert_eq!(
            cached_usage_dto("agent-live-file").unwrap().input_tokens,
            Some(40)
        );
    }
}
