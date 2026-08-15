//! commands/workbench/agent_ledger — Agent Metadata Ledger 本机查询/清除命令
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端需分页查看 metadata-only 历史、时间窗聚合与一键清除；GUI 代理到 sidecar。
//!
//! Code Logic（这个模块做什么）:
//!     Tauri commands + control for_state helpers；不暴露 prompt/path。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::agent_ledger::aggregation::{list_entries, summarize_window};
use crate::workbench::agent_ledger::models::{
    AgentLedgerPage, AgentLedgerQuery, AgentLedgerSummary, LedgerWindow,
};
use chrono::Utc;
use serde::Deserialize;
use tauri::State;

use super::common::proxy_workbench_if_gui;

/// 列表查询请求（可选 camelCase 字段）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAgentLedgerReq {
    pub project_id: Option<String>,
    pub provider_id: Option<String>,
    pub outcome: Option<String>,
    pub ended_after: Option<String>,
    pub ended_before: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// 聚合请求。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummarizeAgentLedgerReq {
    pub window: String,
    pub project_id: Option<String>,
}

/// 分页列出本机 Agent ledger。
///
/// Business Logic（为什么需要这个命令）:
///     Workbench 二级 drawer 加载 metadata 历史。
///
/// Code Logic（这个命令做什么）:
///     GuiClient 代理 `agent_ledger.list`；owner 调 aggregation::list_entries。
#[tauri::command]
pub async fn list_agent_ledger(
    state: State<'_, AppState>,
    req: ListAgentLedgerReq,
) -> Result<AgentLedgerPage, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "agent_ledger.list",
        serde_json::to_value(&req).unwrap_or_default(),
    )
    .await?
    {
        return Ok(v);
    }
    list_agent_ledger_for_state(state.inner(), req).await
}

/// owner 本机 list。
///
/// Business Logic（为什么需要这个函数）:
///     control 与 tauri 共用。
///
/// Code Logic（这个函数做什么）:
///     解析 outcome → AgentLedgerQuery → list_entries。
pub async fn list_agent_ledger_for_state(
    state: &AppState,
    req: ListAgentLedgerReq,
) -> Result<AgentLedgerPage, AppError> {
    let outcome = match req
        .outcome
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => Some(
            crate::workbench::agent_ledger::models::AgentLedgerOutcome::parse(raw)
                .ok_or_else(|| AppError::validation(format!("非法 outcome: {raw}")))?,
        ),
        None => None,
    };
    let query = AgentLedgerQuery {
        project_id: req.project_id,
        provider_id: req.provider_id,
        outcome,
        ended_after: req.ended_after,
        ended_before: req.ended_before,
        cursor: req.cursor,
        limit: req.limit,
    };
    list_entries(&state.agent_ledger_repo, query).await
}

/// 时间窗聚合 summary。
///
/// Business Logic（为什么需要这个命令）:
///     本机 summary 与后续 Fleet 本地回落。
///
/// Code Logic（这个命令做什么）:
///     解析 window → summarize_window(now)。
#[tauri::command]
pub async fn summarize_agent_ledger(
    state: State<'_, AppState>,
    req: SummarizeAgentLedgerReq,
) -> Result<AgentLedgerSummary, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "agent_ledger.summarize",
        serde_json::to_value(&req).unwrap_or_default(),
    )
    .await?
    {
        return Ok(v);
    }
    summarize_agent_ledger_for_state(state.inner(), req).await
}

/// owner 本机 summarize。
///
/// Business Logic（为什么需要这个函数）:
///     control 与 tauri 共用。
///
/// Code Logic（这个函数做什么）:
///     parse window；调用 aggregation。
pub async fn summarize_agent_ledger_for_state(
    state: &AppState,
    req: SummarizeAgentLedgerReq,
) -> Result<AgentLedgerSummary, AppError> {
    let window = LedgerWindow::parse(&req.window).ok_or_else(|| {
        AppError::validation(format!("非法 window: {}（仅 24h|7d|30d）", req.window))
    })?;
    summarize_window(
        &state.agent_ledger_repo,
        window,
        req.project_id.as_deref(),
        Utc::now(),
    )
    .await
}

/// 清除全部 Agent metadata 历史。
///
/// Business Logic（为什么需要这个命令）:
///     设置页一键清除；幂等；不影响 runtime/task。
///
/// Code Logic（这个命令做什么）:
///     clear_history → 返回 deleted 计数。
#[tauri::command]
pub async fn clear_agent_ledger(state: State<'_, AppState>) -> Result<u64, AppError> {
    if let Some(v) =
        proxy_workbench_if_gui(state.inner(), "agent_ledger.clear", serde_json::json!({})).await?
    {
        return Ok(v);
    }
    clear_agent_ledger_for_state(state.inner()).await
}

/// owner 本机 clear。
///
/// Business Logic（为什么需要这个函数）:
///     control 与 tauri 共用；须与 reconcile 串行并写隐私水位。
///
/// Code Logic（这个函数做什么）:
///     service.clear_history（水位 + DELETE，持 clear_reconcile_lock）。
pub async fn clear_agent_ledger_for_state(state: &AppState) -> Result<u64, AppError> {
    state.agent_ledger_service.clear_history().await
}

// 为 proxy_workbench_if_gui 提供 Serialize
impl serde::Serialize for ListAgentLedgerReq {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut st = serializer.serialize_struct("ListAgentLedgerReq", 7)?;
        st.serialize_field("projectId", &self.project_id)?;
        st.serialize_field("providerId", &self.provider_id)?;
        st.serialize_field("outcome", &self.outcome)?;
        st.serialize_field("endedAfter", &self.ended_after)?;
        st.serialize_field("endedBefore", &self.ended_before)?;
        st.serialize_field("cursor", &self.cursor)?;
        st.serialize_field("limit", &self.limit)?;
        st.end()
    }
}

impl serde::Serialize for SummarizeAgentLedgerReq {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut st = serializer.serialize_struct("SummarizeAgentLedgerReq", 2)?;
        st.serialize_field("window", &self.window)?;
        st.serialize_field("projectId", &self.project_id)?;
        st.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::agent_ledger_repo::AgentLedgerRepo;
    use crate::workbench::agent_ledger::models::{
        AgentLedgerFinalizeInput, AgentLedgerOutcome, LedgerUsageCoverage,
    };
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::str::FromStr;

    async fn memory_repo() -> AgentLedgerRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentLedgerRepo::ensure_schema(&pool).await.unwrap();
        AgentLedgerRepo::new(pool)
    }

    /// Business Logic: 命令层 query 映射 outcome。
    #[tokio::test]
    async fn list_via_repo_helpers() {
        let repo = memory_repo().await;
        repo.finalize(AgentLedgerFinalizeInput {
            agent_session_id: "cmd1".into(),
            project_id: "p".into(),
            worktree_id: None,
            provider_id: "claudeCodeVisible".into(),
            model_id: None,
            started_at: "2026-07-01T00:00:00Z".into(),
            ended_at: "2026-07-01T00:01:00Z".into(),
            outcome: AgentLedgerOutcome::Completed,
            usage: None,
            terminal_title: None,
        })
        .await
        .unwrap();
        let page = list_entries(
            &repo,
            AgentLedgerQuery {
                limit: Some(10),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), 1);
        let summary = summarize_window(
            &repo,
            LedgerWindow::Days30,
            None,
            chrono::DateTime::parse_from_rfc3339("2026-07-10T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        )
        .await
        .unwrap();
        assert_eq!(summary.sessions, 1);
        assert_eq!(summary.usage_coverage, LedgerUsageCoverage::Unavailable);
        let _ = summary;
    }
}
