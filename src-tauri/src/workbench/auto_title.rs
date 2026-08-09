//! workbench/auto_title.rs — Agent 对话自动标题 → Workbench 终端 window 名
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude Code 等 agent 会为对话生成短标题；工作台 tab/tmux window 名应可跟随该标题，
//!     方便多终端识别任务。仅 first pane（MVP：单 pane window）且未手改的会话可自动覆盖。
//!
//! Code Logic（这个模块做什么）:
//!     纯函数：清洗标题、判断是否允许 auto rename、在 live sessions 中按 cwd/native id 绑定目标；
//!     副作用路径：`try_auto_rename_from_claude_index` 调 registry rename + 可选 persist 回调钩子。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::claude_sessions::ClaudeSessionIndex;
use crate::workbench::models::WorkbenchSessionRow;
use crate::workbench::sessions::{emit_session_updated, SessionNameSource};
use std::path::{Component, Path};

/// 自动标题最大字符数（Unicode scalar），超出截断并加省略号。
pub const AUTO_TITLE_MAX_CHARS: usize = 48;

/// Business Logic（为什么需要这个函数）:
///     窗口名不能含换行/控制符，也不能过长，否则 tab 与 tmux 展示会崩。
///
/// Code Logic（这个函数做什么）:
///     trim → 空白折叠为单空格 → 去掉控制字符 → 截断到 max_chars；空则 None。
pub fn sanitize_auto_title(raw: &str) -> Option<String> {
    let collapsed: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let char_count = collapsed.chars().count();
    if char_count <= AUTO_TITLE_MAX_CHARS {
        return Some(collapsed);
    }
    let truncated: String = collapsed
        .chars()
        .take(AUTO_TITLE_MAX_CHARS.saturating_sub(1))
        .collect();
    Some(format!("{truncated}…"))
}

/// Business Logic（为什么需要这个函数）:
///     用户手改后不得被 agent 标题刷掉；同名无意义 rename 会打事件/磁盘。
///
/// Code Logic（这个函数做什么）:
///     manual → 拒绝；标题空 → 拒绝；与当前名相同 → 拒绝；default/auto 允许。
pub fn should_apply_auto_title(
    current_name: &str,
    name_source: SessionNameSource,
    candidate: &str,
) -> bool {
    if matches!(name_source, SessionNameSource::Manual) {
        return false;
    }
    let Some(next) = sanitize_auto_title(candidate) else {
        return false;
    };
    next != current_name.trim()
}

/// Business Logic（为什么需要这个函数）:
///     历史 MVP：多 pane 时无法可靠知道 agent 在哪个 pane，仅单 pane 自动改名。
///     现网已改用 title-owner pane_id 交接；本函数保留作纯判定/测试辅助。
///
/// Code Logic（这个函数做什么）:
///     pane_count <= 1 视为可自动改名（first pane only 的保守近似）。
#[allow(dead_code)]
pub fn window_allows_auto_title(pane_count: usize) -> bool {
    pane_count <= 1
}

/// 规范化路径用于 cwd 比较（失败则返回原字符串 trim）。
fn normalize_path_key(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let p = Path::new(trimmed);
    if let Ok(canon) = p.canonicalize() {
        return canon.to_string_lossy().to_string();
    }
    // 尽力去掉 . / .. 组件，不访问磁盘
    let mut out = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str().to_string_lossy().into_owned()),
        }
    }
    if out.is_empty() {
        trimmed.to_string()
    } else {
        // 保留 Unix 绝对前缀
        if p.is_absolute() && !trimmed.starts_with('/') && cfg!(windows) {
            out.join("\\")
        } else if trimmed.starts_with('/') {
            format!("/{}", out.join("/").trim_start_matches('/'))
        } else {
            out.join("/")
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     Claude sessionId 可能尚未写入 agent runtime；cwd 匹配是绑定 live terminal 的兜底。
///
/// Code Logic（这个函数做什么）:
///     在候选 live rows 中：优先 exact id 命中（由调用方预先过滤），否则 cwd 规范化后相等；
///     多命中时取 started_at 最新一条；0 命中返回 None。
pub fn pick_terminal_for_claude_session(
    candidates: &[WorkbenchSessionRow],
    claude_cwd: Option<&str>,
) -> Option<String> {
    let Some(cwd) = claude_cwd.map(str::trim).filter(|s| !s.is_empty()) else {
        return None;
    };
    let key = normalize_path_key(cwd);
    if key.is_empty() {
        return None;
    }
    let mut matched: Vec<&WorkbenchSessionRow> = candidates
        .iter()
        .filter(|row| {
            if row.cwd.trim().is_empty() {
                return false;
            }
            normalize_path_key(&row.cwd) == key
        })
        .collect();
    if matched.is_empty() {
        return None;
    }
    matched.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Some(matched[0].id.clone())
}

/// Business Logic（为什么需要这个函数）:
///     Claude / Codex / OpenCode 共用：绑定 terminal 后做 first-pane 门禁与 auto rename。
///
/// Code Logic（这个函数做什么）:
///     清洗 title → 解析 terminal（native 优先，cwd 兜底）→ pane 门禁 → try_auto_rename + 异步 upsert。
///     返回是否实际改名；失败只 debug。
pub fn try_auto_rename_bound_title(
    state: &AppState,
    title_raw: &str,
    native_session_id: Option<&str>,
    cwd: Option<&str>,
    source_label: &str,
) -> bool {
    let Some(title) = sanitize_auto_title(title_raw) else {
        return false;
    };
    let registry = &state.workbench_sessions;
    let live = registry.list_live_session_rows();
    if live.is_empty() {
        return false;
    }

    let terminal_id = native_session_id
        .and_then(|n| find_terminal_by_native_session(state, n))
        .or_else(|| pick_terminal_for_claude_session(&live, cwd));

    let Some(terminal_id) = terminal_id else {
        return false;
    };

    // seed / 校验 title-owner pane（first pane；关闭后交接）
    let _owner = registry.ensure_title_owner_pane(&terminal_id);

    match registry.try_auto_rename(&terminal_id, &title) {
        Ok(Some(row)) => {
            let repo = state.workbench_session_repo.clone();
            let row_clone = row.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(err) = repo.upsert(&row_clone).await {
                    tracing::debug!("自动标题持久化失败: {err}");
                }
            });
            emit_session_updated(state, &row);
            tracing::debug!(
                terminal_id = %terminal_id,
                title = %title,
                source = source_label,
                "已按 agent 自动标题重命名 window"
            );
            true
        }
        Ok(None) => false,
        Err(err) => {
            tracing::debug!(
                terminal_id = %terminal_id,
                source = source_label,
                "自动标题 rename 失败: {err}"
            );
            false
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     Claude 索引刷新后应 best-effort 把 ai-title 写到绑定终端 window 名。
///
/// Code Logic（这个函数做什么）:
///     委托 `try_auto_rename_bound_title`（native_session_id + cwd）。
pub fn try_auto_rename_from_claude_index(state: &AppState, index: &ClaudeSessionIndex) {
    let _ = try_auto_rename_bound_title(
        state,
        &index.title,
        Some(index.session_id.as_str()),
        index.cwd.as_deref(),
        "claude.ai-title",
    );
}

/// Business Logic（为什么需要这个函数）:
///     Codex/OpenCode 只靠 native session id 绑定时复用同一入口。
///
/// Code Logic（这个函数做什么）:
///     `try_auto_rename_bound_title(..., Some(native), cwd, label)`。
pub fn try_auto_rename_by_native_session(
    state: &AppState,
    native_session_id: &str,
    title: &str,
    cwd: Option<&str>,
    source_label: &str,
) -> bool {
    try_auto_rename_bound_title(state, title, Some(native_session_id), cwd, source_label)
}

/// 在 agent session 表中按 native_session_id 查 terminal_session_id。
///
/// Business Logic: 绑定优先走 runtime 真值，避免 cwd 多终端歧义。
/// Code Logic: 有 runtime handle 时 block_on list_active；否则返回 None。
pub fn find_terminal_by_native_session(
    state: &AppState,
    native_session_id: &str,
) -> Option<String> {
    let native = native_session_id.trim();
    if native.is_empty() {
        return None;
    }
    let repo = state.workbench_agent_session_repo.clone();
    let native_owned = native.to_string();
    let rt = tokio::runtime::Handle::try_current().ok();
    let rows = if let Some(handle) = rt {
        handle.block_on(async move { repo.list_active(None, 500).await.ok() })
    } else {
        None
    }?;
    rows.into_iter()
        .find(|row| row.native_session_id.as_deref() == Some(native_owned.as_str()))
        .map(|row| row.terminal_session_id)
}

/// Business Logic（为什么需要这个函数）:
///     Codex/OpenCode 轮询任务需要在 headless owner 生命周期内运行并可 cancel。
///
/// Code Logic（这个函数做什么）:
///     spawn 两个子任务：codex session_index 轮询 + opencode sqlite 轮询；监听 cancel。
pub fn start_provider_title_pollers(
    state: AppState,
    cancel: tokio_util::sync::CancellationToken,
) {
    let codex_state = state.clone();
    let codex_cancel = cancel.child_token();
    tauri::async_runtime::spawn(async move {
        crate::workbench::auto_title_codex::run_codex_title_poller(codex_state, codex_cancel).await;
    });
    let oc_state = state;
    let oc_cancel = cancel.child_token();
    tauri::async_runtime::spawn(async move {
        crate::workbench::auto_title_opencode::run_opencode_title_poller(oc_state, oc_cancel).await;
    });
}

/// 供测试与调用方构造 rename 错误时使用。
#[allow(dead_code)]
pub(crate) fn rename_error_not_found() -> AppError {
    AppError::not_found("工作台会话不存在")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::models::WorkbenchSessionRow;

    fn row(id: &str, cwd: &str, started: &str) -> WorkbenchSessionRow {
        WorkbenchSessionRow {
            id: id.into(),
            project_id: "p".into(),
            worktree_id: None,
            name: "Terminal".into(),
            name_source: "default".to_string(),
            command: "/bin/zsh".into(),
            cwd: cwd.into(),
            status: "running".into(),
            cols: 80,
            rows: 24,
            started_at: started.into(),
            exited_at: None,
            exit_code: None,
            backend: "tmux".into(),
            backend_id: Some("s".into()),
            backend_window_id: Some("@1".into()),
            created_at: started.into(),
            updated_at: started.into(),
        }
    }

    #[test]
    fn sanitize_collapses_whitespace_and_controls() {
        assert_eq!(
            sanitize_auto_title("  fix\twindow\nname  "),
            Some("fix window name".into())
        );
    }

    #[test]
    fn sanitize_truncates_long_titles() {
        let long = "字".repeat(60);
        let out = sanitize_auto_title(&long).expect("title");
        assert!(out.chars().count() <= AUTO_TITLE_MAX_CHARS);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn sanitize_rejects_blank() {
        assert_eq!(sanitize_auto_title("  \n\t  "), None);
    }

    #[test]
    fn should_apply_respects_manual_and_same_name() {
        assert!(!should_apply_auto_title("a", SessionNameSource::Manual, "b"));
        assert!(!should_apply_auto_title(
            "same",
            SessionNameSource::Default,
            "same"
        ));
        assert!(should_apply_auto_title(
            "old",
            SessionNameSource::Default,
            "new title"
        ));
        assert!(should_apply_auto_title(
            "old",
            SessionNameSource::Auto,
            "new title"
        ));
    }

    #[test]
    fn multi_pane_window_blocks_auto() {
        assert!(window_allows_auto_title(0));
        assert!(window_allows_auto_title(1));
        assert!(!window_allows_auto_title(2));
    }

    #[test]
    fn pick_terminal_prefers_newest_matching_cwd() {
        let candidates = vec![
            row("old", "/tmp/proj", "2026-01-01T00:00:00Z"),
            row("new", "/tmp/proj", "2026-06-01T00:00:00Z"),
            row("other", "/tmp/other", "2026-07-01T00:00:00Z"),
        ];
        assert_eq!(
            pick_terminal_for_claude_session(&candidates, Some("/tmp/proj")).as_deref(),
            Some("new")
        );
        assert_eq!(
            pick_terminal_for_claude_session(&candidates, Some("/tmp/missing")),
            None
        );
    }
}
