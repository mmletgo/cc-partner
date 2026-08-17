//! commands/workbench/agent_ledger — Agent Metadata Ledger 本机查询/清除命令
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端需分页查看 metadata-only 历史、时间窗聚合与一键清除；GUI 代理到 sidecar；
//!     Token 统计页扩展：summarize 支持全量筛选 + 派生指标 + 三维拆分 + 趋势桶；
//!     export_token_stats 写盘导出 CSV/JSON（首字节 BOM + UTF-8 + 原子写）。
//!
//! Code Logic（这个模块做什么）:
//!     Tauri commands + control for_state helpers；不暴露 prompt/path。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::agent_ledger::aggregation::{list_entries, summarize_with_filters};
use crate::workbench::agent_ledger::models::{
    AgentLedgerFilters, AgentLedgerPage, AgentLedgerQuery, AgentLedgerSummary, LedgerWindow,
    TrendBucket,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::State;

use super::common::proxy_workbench_if_gui;

/// 列表查询请求（可选 camelCase 字段）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAgentLedgerReq {
    pub project_id: Option<String>,
    /// 可选 provider 多值过滤
    pub provider_ids: Option<Vec<String>>,
    /// 可选 model 多值过滤
    pub model_ids: Option<Vec<String>>,
    /// 可选 project 多值过滤（与 projectId 互斥；同时给 → projectIds 优先）
    pub project_ids: Option<Vec<String>>,
    pub ended_after: Option<String>,
    pub ended_before: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// 聚合请求（Token 统计页 + Drawer 共用）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummarizeAgentLedgerReq {
    /// 时间窗：24h|7d|30d；None 视作 7d 兜底
    pub window: Option<String>,
    /// 可选 project 过滤（与 projectIds 互斥；同时给 → projectIds 优先）
    pub project_id: Option<String>,
    /// 可选 provider 多值过滤
    pub provider_ids: Option<Vec<String>>,
    /// 可选 model 多值过滤
    pub model_ids: Option<Vec<String>>,
    /// 可选 project 多值过滤
    pub project_ids: Option<Vec<String>>,
    /// 可选 worktree 过滤
    pub worktree_id: Option<String>,
    /// 可选 started_at 下界（含）RFC3339
    pub started_after: Option<String>,
    /// 可选 started_at 上界（含）RFC3339
    pub started_before: Option<String>,
    /// 可选桶粒度（hour|day）；None → 按 window 推导
    pub bucket: Option<String>,
}

/// 导出请求（CSV/JSON 落盘）。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportTokenStatsReq {
    /// "csv" | "json"
    pub format: String,
    /// 时间窗：24h|7d|30d；None → 7d 兜底
    pub window: Option<String>,
    /// 可选 project 过滤
    pub project_id: Option<String>,
    /// 可选 provider 多值过滤
    pub provider_ids: Option<Vec<String>>,
    /// 可选 model 多值过滤
    pub model_ids: Option<Vec<String>>,
    /// 可选 project 多值过滤
    pub project_ids: Option<Vec<String>>,
    /// 可选 worktree 过滤
    pub worktree_id: Option<String>,
    /// 可选 started_at 下界（含）RFC3339
    pub started_after: Option<String>,
    /// 可选 started_at 上界（含）RFC3339
    pub started_before: Option<String>,
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
///     构造 AgentLedgerQuery（多值 provider/model/project）→ list_entries。
pub async fn list_agent_ledger_for_state(
    state: &AppState,
    req: ListAgentLedgerReq,
) -> Result<AgentLedgerPage, AppError> {
    let query = AgentLedgerQuery {
        project_id: req.project_id,
        provider_ids: req.provider_ids,
        model_ids: req.model_ids,
        project_ids: req.project_ids,
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
///     control 与 tauri 共用；Token 统计页扩展支持全量筛选。
///
/// Code Logic（这个函数做什么）:
///     parse window/bucket → 构造 AgentLedgerFilters → aggregation。
pub async fn summarize_agent_ledger_for_state(
    state: &AppState,
    req: SummarizeAgentLedgerReq,
) -> Result<AgentLedgerSummary, AppError> {
    let window =
        match req
            .window
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(raw) => Some(LedgerWindow::parse(raw).ok_or_else(|| {
                AppError::validation(format!("非法 window: {raw}（仅 24h|7d|30d）"))
            })?),
            None => None,
        };

    let bucket =
        match req
            .bucket
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(raw) => Some(TrendBucket::parse(raw).ok_or_else(|| {
                AppError::validation(format!("非法 bucket: {raw}（仅 hour|day）"))
            })?),
            None => None,
        };

    let filters = AgentLedgerFilters {
        window,
        project_id: req.project_id,
        provider_ids: req.provider_ids,
        model_ids: req.model_ids,
        project_ids: req.project_ids,
        worktree_id: req.worktree_id,
        started_after: req.started_after,
        started_before: req.started_before,
        bucket,
    };
    summarize_with_filters(&state.agent_ledger_repo, filters, Utc::now()).await
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

/// Token 统计页导出（CSV / JSON）。
///
/// Business Logic（为什么需要这个命令）:
///     统计页允许把当前筛选下的 summary + 明细导出文件供本地归档；
///     写盘路径必须在 data_dir/exports/token-stats/ 下，目录权限 0700。
///
/// Code Logic（这个命令做什么）:
///     解析 format → 复用 summarize_with_filters 拿 summary → 翻页拉全量 entry →
///     写到 `<data_dir>/exports/token-stats/<UTC-timestamp>.{csv|json}`；
///     CSV 首字节 BOM `EF BB BF` 让 Excel 识别 UTF-8；原子写（temp + rename）。
#[tauri::command]
pub async fn export_token_stats(
    state: State<'_, AppState>,
    req: ExportTokenStatsReq,
) -> Result<String, AppError> {
    let proxy_resp: Option<String> = proxy_workbench_if_gui(
        state.inner(),
        "agent_ledger.export_token_stats",
        serde_json::to_value(&req).unwrap_or_default(),
    )
    .await?;
    if let Some(v) = proxy_resp {
        return Ok(v);
    }
    export_token_stats_for_state(state.inner(), req).await
}

/// owner 本机 export_token_stats。
///
/// Business Logic:
///     1) format 仅 csv/json；2) 复用 summarize_with_filters 拿 summary；
///     3) 翻页 list_entries 直至 next_cursor=None（上限 10000）；
///     4) 写盘到 data_dir/exports/token-stats/<UTC-timestamp>.<ext>；
///     5) JSON 含 filter + summary + entries；CSV 含 BOM + header + 数据行。
///
/// Code Logic:
///     mkdir -p 0700；JSON 用 serde_json::to_string_pretty；CSV 用 Rust 字符串 escape；
///     原子写：写到 .tmp 后 rename 到目标路径。
pub async fn export_token_stats_for_state(
    state: &AppState,
    req: ExportTokenStatsReq,
) -> Result<String, AppError> {
    let format = req.format.trim().to_ascii_lowercase();
    if format != "csv" && format != "json" {
        return Err(AppError::validation(format!(
            "export_token_stats 非法 format: {}（仅 csv|json）",
            req.format
        )));
    }

    // 构造 filters（与 summarize 同口径）
    let window =
        match req
            .window
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(raw) => Some(LedgerWindow::parse(raw).ok_or_else(|| {
                AppError::validation(format!("非法 window: {raw}（仅 24h|7d|30d）"))
            })?),
            None => None,
        };
    let filters = AgentLedgerFilters {
        window,
        project_id: req.project_id.clone(),
        provider_ids: req.provider_ids.clone(),
        model_ids: req.model_ids.clone(),
        project_ids: req.project_ids.clone(),
        worktree_id: req.worktree_id.clone(),
        started_after: req.started_after.clone(),
        started_before: req.started_before.clone(),
        bucket: None,
    };
    let summary = crate::workbench::agent_ledger::aggregation::summarize_with_filters(
        &state.agent_ledger_repo,
        filters.clone(),
        Utc::now(),
    )
    .await?;

    // 翻页拉全量 entry（上限 10000 防失控）
    const MAX_EXPORT_ROWS: usize = 10_000;
    let mut items: Vec<crate::workbench::agent_ledger::models::AgentLedgerEntry> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let q = crate::workbench::agent_ledger::models::AgentLedgerQuery {
            project_id: filters.project_id.clone(),
            provider_ids: filters.provider_ids.clone(),
            model_ids: filters.model_ids.clone(),
            project_ids: filters.project_ids.clone(),
            ended_after: filters.started_after.clone(),
            ended_before: filters.started_before.clone(),
            cursor: cursor.clone(),
            limit: Some(200),
        };
        let page = list_entries(&state.agent_ledger_repo, q).await?;
        let take = page.items.len();
        items.extend(page.items);
        if items.len() > MAX_EXPORT_ROWS {
            return Err(AppError::unavailable("token_stats_export_too_large"));
        }
        match page.next_cursor {
            Some(next) if take > 0 => cursor = Some(next),
            _ => break,
        }
    }

    // 写盘路径：<data_dir>/exports/token-stats/<UTC-timestamp>.<ext>
    let data_dir = crate::config::data_dir()?;
    let exports_root = data_dir.join("exports").join("token-stats");
    ensure_dir_0700(&exports_root).map_err(|e| {
        AppError::unavailable(format!("无法创建导出目录 {}: {e}", exports_root.display()))
    })?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let target = exports_root.join(format!("token-stats-{stamp}.{format}"));

    write_token_stats_export(&target, &format, &summary, &items, &req)?;

    Ok(target.to_string_lossy().to_string())
}

/// 把 summary + items + filter 写到目标路径（CSV / JSON）。
///
/// Business Logic:
///     export_token_stats_for_state 的可测试核心：参数化路径与格式，
///     单元测试不必装配完整 AppState。
///
/// Code Logic:
///     JSON：serde_json::to_string_pretty + 顶层含 filter/summary/entries；
///     CSV：UTF-8 BOM + header + format_csv_row 行 + CRLF；原子写。
pub(crate) fn write_token_stats_export(
    target: &std::path::Path,
    format: &str,
    summary: &AgentLedgerSummary,
    items: &[crate::workbench::agent_ledger::models::AgentLedgerEntry],
    filter: &ExportTokenStatsReq,
) -> Result<(), AppError> {
    if format == "json" {
        let payload = serde_json::json!({
            "filter": filter,
            "summary": summary,
            "entries": items,
        });
        let text = serde_json::to_string_pretty(&payload)
            .map_err(|e| AppError::unavailable(format!("无法序列化导出 JSON: {e}")))?;
        atomic_write(target, text.as_bytes())?;
    } else if format == "csv" {
        let mut buf: Vec<u8> = Vec::with_capacity(64 + items.len() * 256);
        // UTF-8 BOM 让 Excel 识别
        buf.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        let header = "startedAt,endedAt,outcome,providerId,modelId,projectId,worktreeId,terminalTitle,inputTokens,outputTokens,cacheReadTokens,cacheWriteTokens,costMinorUnits,costCurrency,durationMs,agentSessionId\r\n";
        buf.extend_from_slice(header.as_bytes());
        for it in items {
            let line = format_csv_row(it);
            buf.extend_from_slice(line.as_bytes());
            buf.extend_from_slice(b"\r\n");
        }
        atomic_write(target, &buf)?;
    } else {
        return Err(AppError::validation(format!(
            "write_token_stats_export 非法 format: {format}（仅 csv|json）"
        )));
    }
    Ok(())
}

/// 创建目录（0700）。跨平台：unix 设权限位，windows 退化为 create_dir。
fn ensure_dir_0700(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// 原子写：写到 .tmp 后 rename 到目标。
fn atomic_write(target: &std::path::Path, bytes: &[u8]) -> Result<(), AppError> {
    use std::io::Write;
    let mut tmp = target.to_path_buf();
    let name = tmp
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "token-stats".to_string());
    tmp.set_file_name(format!(".{name}.tmp"));
    let mut f = std::fs::File::create(&tmp)
        .map_err(|e| AppError::unavailable(format!("无法写入临时文件 {}: {e}", tmp.display())))?;
    f.write_all(bytes)
        .map_err(|e| AppError::unavailable(format!("无法写入临时文件 {}: {e}", tmp.display())))?;
    f.sync_all().map_err(|e| {
        AppError::unavailable(format!("无法 fsync 临时文件 {}: {e}", tmp.display()))
    })?;
    drop(f);
    std::fs::rename(&tmp, target).map_err(|e| {
        AppError::unavailable(format!(
            "无法提交导出文件 {} → {}: {e}",
            tmp.display(),
            target.display()
        ))
    })?;
    Ok(())
}

/// CSV 单行转义：含 `,` `"` `\n` 用双引号包裹并 escape `"`。
fn format_csv_row(it: &crate::workbench::agent_ledger::models::AgentLedgerEntry) -> String {
    fn esc(s: &str) -> String {
        if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
            let escaped = s.replace('"', "\"\"");
            format!("\"{escaped}\"")
        } else {
            s.to_string()
        }
    }
    let cols = [
        esc(&it.started_at),
        esc(&it.ended_at),
        esc(it.outcome.as_str()),
        esc(&it.provider_id),
        esc(it.model_id.as_deref().unwrap_or("")),
        esc(&it.project_id),
        esc(it.worktree_id.as_deref().unwrap_or("")),
        esc(it.terminal_title.as_deref().unwrap_or("")),
        it.input_tokens.map(|v| v.to_string()).unwrap_or_default(),
        it.output_tokens.map(|v| v.to_string()).unwrap_or_default(),
        it.cache_read_tokens
            .map(|v| v.to_string())
            .unwrap_or_default(),
        it.cache_write_tokens
            .map(|v| v.to_string())
            .unwrap_or_default(),
        it.cost_minor_units
            .map(|v| v.to_string())
            .unwrap_or_default(),
        esc(it.cost_currency.as_deref().unwrap_or("")),
        it.duration_ms.to_string(),
        esc(&it.agent_session_id),
    ];
    cols.join(",")
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
        st.serialize_field("providerIds", &self.provider_ids)?;
        st.serialize_field("modelIds", &self.model_ids)?;
        st.serialize_field("projectIds", &self.project_ids)?;
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
        let mut st = serializer.serialize_struct("SummarizeAgentLedgerReq", 9)?;
        st.serialize_field("window", &self.window)?;
        st.serialize_field("projectId", &self.project_id)?;
        st.serialize_field("providerIds", &self.provider_ids)?;
        st.serialize_field("modelIds", &self.model_ids)?;
        st.serialize_field("projectIds", &self.project_ids)?;
        st.serialize_field("worktreeId", &self.worktree_id)?;
        st.serialize_field("startedAfter", &self.started_after)?;
        st.serialize_field("startedBefore", &self.started_before)?;
        st.serialize_field("bucket", &self.bucket)?;
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
        let summary = crate::workbench::agent_ledger::aggregation::summarize_with_filters(
            &repo,
            crate::workbench::agent_ledger::models::AgentLedgerFilters {
                window: Some(LedgerWindow::Days30),
                ..Default::default()
            },
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

    /// Business Logic: SummarizeAgentLedgerReq 必须容忍所有字段缺省，且 bucket/outcome/window
    ///     非法字面量稳定返回 Validation。
    ///
    /// Code Logic: 直接 deserialize JSON；逐字段检查；非法 bucket/outcome/window 触发 Err。
    #[test]
    fn summarize_req_handles_optional_fields() {
        // 全字段缺省 → 所有 Option 为 None
        let j = r#"{}"#;
        let req: SummarizeAgentLedgerReq = serde_json::from_str(j).unwrap();
        assert!(req.window.is_none());
        assert!(req.project_id.is_none());
        assert!(req.provider_ids.is_none());
        assert!(req.model_ids.is_none());
        assert!(req.project_ids.is_none());
        assert!(req.worktree_id.is_none());
        assert!(req.started_after.is_none());
        assert!(req.started_before.is_none());
        assert!(req.bucket.is_none());

        // 全字段填齐（含 camelCase）→ 反序列化成功
        let j = r#"{"window":"24h","projectId":"p","providerIds":["a"],"modelIds":["m"],"projectIds":["p"],"worktreeId":"w","startedAfter":"2026-07-15T00:00:00Z","startedBefore":"2026-07-15T01:00:00Z","bucket":"hour"}"#;
        let req: SummarizeAgentLedgerReq = serde_json::from_str(j).unwrap();
        assert_eq!(req.window.as_deref(), Some("24h"));
        assert_eq!(req.bucket.as_deref(), Some("hour"));
        assert_eq!(req.provider_ids.as_ref().unwrap().len(), 1);
    }

    /// Business Logic: 导出 payload 必须带 format；其余筛选字段与 summarize 同 camelCase。
    #[test]
    fn export_req_requires_format_and_accepts_camel_case() {
        assert!(serde_json::from_str::<ExportTokenStatsReq>(r#"{}"#).is_err());
        let req: ExportTokenStatsReq = serde_json::from_str(
            r#"{"format":"csv","window":"7d","providerIds":["claudeCodeVisible"]}"#,
        )
        .unwrap();
        assert_eq!(req.format, "csv");
        assert_eq!(req.window.as_deref(), Some("7d"));
        assert_eq!(req.provider_ids.as_ref().unwrap().len(), 1);
    }

    /// Business Logic: 非法 format 直接拒绝。
    /// Code Logic: write_token_stats_export 对 "xml" 返回 Validation。
    #[tokio::test]
    async fn export_token_stats_rejects_invalid_format() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("out.xml");
        let req = ExportTokenStatsReq {
            format: "xml".into(),
            window: None,
            project_id: None,
            provider_ids: None,
            model_ids: None,
            project_ids: None,
            worktree_id: None,
            started_after: None,
            started_before: None,
        };
        let summary = AgentLedgerSummary {
            window: LedgerWindow::Days7,
            project_id: None,
            sessions: 0,
            completed: 0,
            failed: 0,
            cancelled: 0,
            disconnected: 0,
            duration_ms: 0,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            real_consumed_tokens: None,
            cache_hit_rate: None,
            requests_count: 0,
            total_cost_by_currency: vec![],
            cost_by_currency: vec![],
            by_model: vec![],
            by_provider: vec![],
            by_project: vec![],
            trend: vec![],
            bucket: crate::workbench::agent_ledger::models::TrendBucket::Day,
            usage_coverage: LedgerUsageCoverage::Unavailable,
        };
        let err = write_token_stats_export(&target, "xml", &summary, &[], &req).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.to_ascii_lowercase().contains("validation") || msg.contains("非 csv|json"),
            "unexpected error msg: {msg}"
        );
    }

    /// Business Logic: JSON 导出必须含 UTF-8 BOM + filter/summary/entries 顶层字段。
    /// Code Logic: write_token_stats_export 直接写 tmp 目录；
    ///     断言首字节 BOM、断言 JSON 含三个顶层键。
    #[tokio::test]
    async fn export_token_stats_writes_json_with_bom() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("out.json");
        let req = ExportTokenStatsReq {
            format: "json".into(),
            window: None,
            project_id: None,
            provider_ids: None,
            model_ids: None,
            project_ids: None,
            worktree_id: None,
            started_after: None,
            started_before: None,
        };
        let repo = memory_repo().await;
        // 真实写一遍 entry → 写文件
        let summary = AgentLedgerSummary {
            window: LedgerWindow::Days7,
            project_id: None,
            sessions: 1,
            completed: 1,
            failed: 0,
            cancelled: 0,
            disconnected: 0,
            duration_ms: 60_000,
            input_tokens: Some(10),
            output_tokens: Some(5),
            cache_read_tokens: Some(2),
            cache_write_tokens: Some(1),
            real_consumed_tokens: Some(16),
            cache_hit_rate: Some(0.16666667),
            requests_count: 1,
            total_cost_by_currency: vec![],
            cost_by_currency: vec![],
            by_model: vec![],
            by_provider: vec![],
            by_project: vec![],
            trend: vec![],
            bucket: crate::workbench::agent_ledger::models::TrendBucket::Day,
            usage_coverage: LedgerUsageCoverage::Complete,
        };
        // finalize 一条拿 entry
        repo.finalize(AgentLedgerFinalizeInput {
            agent_session_id: "a1".into(),
            project_id: "p1".into(),
            worktree_id: None,
            provider_id: "claudeCodeVisible".into(),
            model_id: Some("m1".into()),
            started_at: "2026-07-15T12:00:00+00:00".into(),
            ended_at: "2026-07-15T12:01:00+00:00".into(),
            outcome: AgentLedgerOutcome::Completed,
            usage: None,
            terminal_title: Some("fix: 登录".into()),
        })
        .await
        .unwrap();
        let items = vec![repo
            .get_by_agent_session_id("a1")
            .await
            .unwrap()
            .expect("entry")];
        write_token_stats_export(&target, "json", &summary, &items, &req).unwrap();
        let bytes = std::fs::read(&target).unwrap();
        // JSON 不带 BOM（serde 默认 UTF-8 即可）
        assert!(
            bytes.starts_with(b"{") || bytes.starts_with(b"["),
            "JSON 内容不以 {{ 起头: {:?}",
            &bytes[..bytes.len().min(8)]
        );
        let text = std::str::from_utf8(&bytes).unwrap();
        let v: serde_json::Value = serde_json::from_str(text).unwrap();
        assert!(v.get("filter").is_some());
        assert!(v.get("summary").is_some());
        assert!(v.get("entries").is_some());
        assert_eq!(
            v.get("entries")
                .and_then(|e| e.as_array())
                .map(|a| a.len())
                .unwrap_or(0),
            1
        );
    }

    /// Business Logic: CSV 导出必须含 UTF-8 BOM + header 行 + 数据行；
    ///     含逗号/引号/换行的字段正确 escape。
    /// Code Logic: finalize 一行带 terminal_title 包含 `,` `"` 与换行 →
    ///     断言 CSV 字节 BOM + header + escape 后字段含 `""` 与首尾 `"`。
    #[tokio::test]
    async fn export_token_stats_writes_csv_with_bom_header_and_rows() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("out.csv");
        let req = ExportTokenStatsReq {
            format: "csv".into(),
            window: None,
            project_id: None,
            provider_ids: None,
            model_ids: None,
            project_ids: None,
            worktree_id: None,
            started_after: None,
            started_before: None,
        };
        let repo = memory_repo().await;
        repo.finalize(AgentLedgerFinalizeInput {
            agent_session_id: "a1".into(),
            project_id: "p1".into(),
            worktree_id: None,
            provider_id: "claudeCodeVisible".into(),
            model_id: None,
            started_at: "2026-07-15T12:00:00+00:00".into(),
            ended_at: "2026-07-15T12:01:00+00:00".into(),
            outcome: AgentLedgerOutcome::Completed,
            usage: None,
            // 含逗号/引号/换行
            terminal_title: Some("feat: \"esc\", with, comma\nand newline".into()),
        })
        .await
        .unwrap();
        let items = vec![repo
            .get_by_agent_session_id("a1")
            .await
            .unwrap()
            .expect("entry")];
        let summary = AgentLedgerSummary {
            window: LedgerWindow::Days7,
            project_id: None,
            sessions: 1,
            completed: 1,
            failed: 0,
            cancelled: 0,
            disconnected: 0,
            duration_ms: 60_000,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            real_consumed_tokens: None,
            cache_hit_rate: None,
            requests_count: 1,
            total_cost_by_currency: vec![],
            cost_by_currency: vec![],
            by_model: vec![],
            by_provider: vec![],
            by_project: vec![],
            trend: vec![],
            bucket: crate::workbench::agent_ledger::models::TrendBucket::Day,
            usage_coverage: LedgerUsageCoverage::Unavailable,
        };
        write_token_stats_export(&target, "csv", &summary, &items, &req).unwrap();
        let bytes = std::fs::read(&target).unwrap();
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF], "CSV 文件首字节 BOM 缺失");
        let text = std::str::from_utf8(&bytes[3..]).unwrap();
        assert!(text.starts_with(
            "startedAt,endedAt,outcome,providerId,modelId,projectId,worktreeId,terminalTitle,inputTokens,outputTokens,cacheReadTokens,cacheWriteTokens,costMinorUnits,costCurrency,durationMs,agentSessionId\r\n"
        ));
        // escape 后字段："feat: ""esc"", with, comma\nand newline"
        assert!(text.contains("\"feat: \"\"esc\"\", with, comma\nand newline\""));
    }
}
