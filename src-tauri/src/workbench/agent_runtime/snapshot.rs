//! workbench/agent_runtime/snapshot — 有界 Agent runtime 投影快照
//!
//! Business Logic（为什么需要这个模块）:
//!     Gap/owner change 后 desktop/remote/mobile 需要同一份 active session baseline，
//!     且不得包含 native_session_id、正文或路径。
//!
//! Code Logic（这个模块做什么）:
//!     定义 sanitized DTO、snapshot 结构；`get_agent_runtime_snapshot_for_state` 读 repo
//!     并捕获 event_bus cursor（多轮重试至 sequence 稳定，与运营通知 capture 对齐）。

use super::models::{AgentSessionPhase, AgentSessionRuntime};
use crate::error::AppError;
use crate::state::AppState;
use serde::{Deserialize, Serialize};

/// 单 snapshot 最多返回的 active sessions。
pub const AGENT_RUNTIME_SNAPSHOT_LIMIT: i64 = 1_000;

/// 稳定 cursor 捕获的最大重试次数（对齐 operational notification snapshot）。
const SNAPSHOT_CURSOR_STABILITY_ATTEMPTS: usize = 8;

/// 投影用 live usage（仅 tokens + model + 抽取时间）。
///
/// Business Logic（为什么需要这个类型）:
///     状态卡在非终态也要展示速率/上下文；不得携带 native id、路径或正文。
///
/// Code Logic（这个类型做什么）:
///     camelCase；全字段可选 tokens；extracted_at 为 RFC3339。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLiveUsageDto {
    pub model_id: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    /// 当前上下文占用（末轮 occupancy）；缺省表示尚未抽取或旧后端。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    /// Provider 上报的模型最大上下文；缺省由前端按 modelId 查表。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// 有效生成时长（用户→助手区间合并，毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_duration_ms: Option<u64>,
    pub extracted_at: String,
}

/// 投影用 Agent session DTO（禁止 native_session_id）。
///
/// Business Logic（为什么需要这个类型）:
///     UI/CLI/P2P 只需稳定 cc-partner ID 与 phase；provider-native id 仅 owner-local。
///
/// Code Logic（这个类型做什么）:
///     camelCase serde；字段集刻意不含 nativeSessionId；usage 可选且缺省省略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRuntimeDto {
    pub id: String,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub terminal_session_id: String,
    pub orchestrator_task_id: Option<String>,
    pub orchestrator_attempt: Option<u32>,
    pub provider_id: String,
    pub phase: AgentSessionPhase,
    pub version: u64,
    pub started_at: String,
    pub last_activity_at: String,
    pub ended_at: Option<String>,
    pub outcome_code: Option<String>,
    pub resumed_from_agent_session_id: Option<String>,
    pub is_active: bool,
    /// 进程内 live/终态最近一次可靠 usage；缺省不序列化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AgentLiveUsageDto>,
}

impl AgentSessionRuntimeDto {
    /// 从内部 runtime 行映射投影 DTO（剔除 native_session_id）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     repo 行含 owner-local 字段；投影边界必须统一剥离。
    ///
    /// Code Logic（这个函数做什么）:
    ///     逐字段拷贝，忽略 native_session_id；附上进程内 live usage 缓存。
    pub fn from_runtime(row: &AgentSessionRuntime) -> Self {
        Self {
            id: row.id.clone(),
            project_id: row.project_id.clone(),
            worktree_id: row.worktree_id.clone(),
            terminal_session_id: row.terminal_session_id.clone(),
            orchestrator_task_id: row.orchestrator_task_id.clone(),
            orchestrator_attempt: row.orchestrator_attempt,
            provider_id: row.provider_id.clone(),
            phase: row.phase,
            version: row.version,
            started_at: row.started_at.clone(),
            last_activity_at: row.last_activity_at.clone(),
            ended_at: row.ended_at.clone(),
            outcome_code: row.outcome_code.clone(),
            resumed_from_agent_session_id: row.resumed_from_agent_session_id.clone(),
            is_active: row.is_active,
            usage: super::live_usage::cached_usage_dto(&row.id),
        }
    }
}

/// Agent runtime 有界快照。
///
/// Business Logic（为什么需要这个类型）:
///     Gap 恢复需要 ownerInstanceId + asOfSequence + sessions baseline。
///
/// Code Logic（这个类型做什么）:
///     sessions 最多 1000；truncated 表示是否还有更多 active。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeSnapshot {
    pub owner_instance_id: String,
    pub as_of_sequence: u64,
    pub project_id: Option<String>,
    pub sessions: Vec<AgentSessionRuntimeDto>,
    pub truncated: bool,
}

/// 变更事件载荷。
///
/// Business Logic（为什么需要这个类型）:
///     durable mutation 后前端/远端需要增量投影，不得带 native id。
///
/// Code Logic（这个类型做什么）:
///     单字段 agent_session: AgentSessionRuntimeDto。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeChangedEvent {
    pub agent_session: AgentSessionRuntimeDto,
}

/// 为 AppState 构造 Agent runtime snapshot。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri/control/P2P/mobile 共用同一 owner helper，保证 Gap baseline 一致。
///     capture 期间若 event_bus sequence 前进，必须重试，否则 asOf 高于 listed sessions
///     会让客户端丢掉尚未纳入 snapshot 的 durable phase。
///
/// Code Logic（这个函数做什么）:
///     委托 `get_agent_runtime_snapshot_for_state_with_include(..., None)`。
pub async fn get_agent_runtime_snapshot_for_state(
    state: &AppState,
    project_id: Option<String>,
) -> Result<AgentRuntimeSnapshot, AppError> {
    get_agent_runtime_snapshot_for_state_with_include(state, project_id, None).await
}

/// 为 AppState 构造 Agent runtime snapshot，并可强制纳入指定 session（含终态）。
///
/// Business Logic（为什么需要这个函数）:
///     默认 snapshot 只含 `is_active=1`；远端 `agent wait --phase completed` /
///     `agent inspect` 必须能读到 completed/failed/disconnected 终态，否则永远超时。
///
/// Code Logic（这个函数做什么）:
///     与 active snapshot 相同的 cursor 稳定循环；若 `include_agent_session_id`
///     不在 active 列表内则 `repo.get` 追加脱敏 DTO（仍无 nativeSessionId）。
pub async fn get_agent_runtime_snapshot_for_state_with_include(
    state: &AppState,
    project_id: Option<String>,
    include_agent_session_id: Option<&str>,
) -> Result<AgentRuntimeSnapshot, AppError> {
    let owner_instance_id = state.config_runtime.owner_instance_id().to_string();
    let limit = AGENT_RUNTIME_SNAPSHOT_LIMIT;
    let mut last_rows = Vec::new();
    let mut last_truncated = false;
    let mut last_seq = state.event_bus.latest_sequence();

    for _ in 0..SNAPSHOT_CURSOR_STABILITY_ATTEMPTS {
        let cursor_before = state.event_bus.latest_sequence();
        let mut rows = state
            .workbench_agent_session_repo
            .list_active(project_id.as_deref(), limit + 1)
            .await?;
        let truncated = rows.len() as i64 > limit;
        if truncated {
            rows.truncate(limit as usize);
        }
        let cursor_after = state.event_bus.latest_sequence();
        last_rows = rows;
        last_truncated = truncated;
        last_seq = cursor_after;
        if cursor_before == cursor_after {
            break;
        }
    }

    let include_id = include_agent_session_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(ref id) = include_id {
        let already = last_rows.iter().any(|r| r.id == *id);
        if !already {
            if let Some(row) = state.workbench_agent_session_repo.get(id).await? {
                // project 过滤：指定 project 时拒绝异项目 id，避免泄漏
                let project_ok = project_id
                    .as_deref()
                    .map(|pid| row.project_id == pid)
                    .unwrap_or(true);
                if project_ok {
                    last_rows.push(row);
                }
            }
        }
    }

    let sessions = last_rows
        .iter()
        .map(AgentSessionRuntimeDto::from_runtime)
        .collect();
    Ok(AgentRuntimeSnapshot {
        owner_instance_id,
        as_of_sequence: last_seq,
        project_id,
        sessions,
        truncated: last_truncated,
    })
}

/// 发出 workbench:agent-runtime 事件（mutation 已 durable 后调用）。
///
/// Business Logic（为什么需要这个函数）:
///     前端与 control relay 需要在提交后立刻看到 phase 变化。
///
/// Code Logic（这个函数做什么）:
///     映射 DTO 后 `state.emit_event`；不携带 native_session_id；
///     运营通知仅在 phase **首次进入** needsInput/failed 时发射（同 phase 升版不刷屏）。
pub fn emit_agent_runtime_changed(
    state: &AppState,
    row: &AgentSessionRuntime,
    previous_phase: Option<AgentSessionPhase>,
) {
    let dto = AgentSessionRuntimeDto::from_runtime(row);
    let event = AgentRuntimeChangedEvent {
        agent_session: dto.clone(),
    };
    state.emit_event("workbench:agent-runtime", event);
    crate::workbench::remote_events::publish_workbench_remote_event_from_state(
        state,
        crate::workbench::remote_events::WorkbenchRemoteEvent::AgentRuntime(
            crate::workbench::remote_events::WorkbenchAgentRuntimePayload { agent_session: dto },
        ),
    );
    emit_agent_operational_notification_if_needed(state, row, previous_phase);
}

/// Business Logic（为什么需要这个函数）:
///     Spec：needsInput/failed 通知只在 phase **首次进入** 时发一次；同 phase version  bump
///     （activity/coalesce）不得刷屏。
///
/// Code Logic（这个函数做什么）:
///     当前 phase 为 NeedsInput/Failed 且 previous 不同（或无 previous）时 emit；
///     opaque=agentSessionId，state_version=version；无 title/project/path。
fn emit_agent_operational_notification_if_needed(
    state: &AppState,
    row: &AgentSessionRuntime,
    previous_phase: Option<AgentSessionPhase>,
) {
    use crate::orchestrator::models::{OperationalNotificationEvent, OperationalNotificationKind};
    use crate::orchestrator::notifications::emit_operational_notification;

    if !should_emit_agent_exception_notification(previous_phase, row.phase) {
        return;
    }
    let kind = match row.phase {
        AgentSessionPhase::NeedsInput => OperationalNotificationKind::AgentNeedsInput,
        AgentSessionPhase::Failed => OperationalNotificationKind::AgentFailed,
        _ => return,
    };
    emit_operational_notification(
        state,
        &OperationalNotificationEvent {
            kind,
            opaque_source_id: row.id.clone(),
            state_version: row.version as i64,
            occurred_at: row.last_activity_at.clone(),
        },
    );
}

/// Business Logic（为什么需要这个函数）:
///     同 phase 升版不得重复 OS 通知；phase 边沿（含 Working→NeedsInput、NeedsInput→Failed）须通知。
///
/// Code Logic（这个函数做什么）:
///     仅当 current ∈ {NeedsInput,Failed} 且 previous ≠ current（含 previous=None）时返回 true。
pub fn should_emit_agent_exception_notification(
    previous_phase: Option<AgentSessionPhase>,
    current_phase: AgentSessionPhase,
) -> bool {
    match current_phase {
        AgentSessionPhase::NeedsInput | AgentSessionPhase::Failed => previous_phase
            .map(|prev| prev != current_phase)
            .unwrap_or(true),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::WorkbenchAgentSessionRepo;
    use crate::workbench::agent_runtime::{AgentSessionPhase, CreateActiveAgentSession};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// Business Logic（为什么需要这个测试）:
    ///     投影 JSON 绝不能泄漏 nativeSessionId。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化含 native 的 runtime 映射 DTO，断言 JSON 无该键。
    #[test]
    fn dto_serialization_omits_native_session_id() {
        let row = AgentSessionRuntime {
            id: "a1".into(),
            project_id: "p".into(),
            worktree_id: None,
            terminal_session_id: "t".into(),
            orchestrator_task_id: None,
            orchestrator_attempt: None,
            provider_id: "claudeCodeVisible".into(),
            native_session_id: Some("native-secret".into()),
            phase: AgentSessionPhase::Working,
            version: 2,
            started_at: "2026-07-15T00:00:00Z".into(),
            last_activity_at: "2026-07-15T00:00:01Z".into(),
            ended_at: None,
            outcome_code: None,
            resumed_from_agent_session_id: None,
            is_active: true,
        };
        let dto = AgentSessionRuntimeDto::from_runtime(&row);
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("nativeSessionId"));
        assert!(!json.contains("native_session_id"));
        assert!(!json.contains("native-secret"));
        assert!(!json.contains("\"usage\""));
        assert!(json.contains("agent") || json.contains("\"id\":\"a1\""));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     live usage 可进投影，但绝不能带回 native id 或路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     store usage 后 from_runtime，JSON 含 tokens 且不含 native-secret。
    #[test]
    fn dto_attaches_live_usage_without_native_session_id() {
        let row = AgentSessionRuntime {
            id: "a-usage".into(),
            project_id: "p".into(),
            worktree_id: None,
            terminal_session_id: "t".into(),
            orchestrator_task_id: None,
            orchestrator_attempt: None,
            provider_id: "claudeCodeVisible".into(),
            native_session_id: Some("native-secret".into()),
            phase: AgentSessionPhase::Working,
            version: 2,
            started_at: "2026-07-15T00:00:00Z".into(),
            last_activity_at: "2026-07-15T00:00:01Z".into(),
            ended_at: None,
            outcome_code: None,
            resumed_from_agent_session_id: None,
            is_active: true,
        };
        crate::workbench::agent_runtime::live_usage::store(
            "a-usage",
            &crate::workbench::agent_ledger::ReliableUsageSnapshot {
                model_id: Some("claude-sonnet-4".into()),
                input_tokens: Some(12),
                output_tokens: Some(3),
                cache_read_tokens: Some(4),
                cache_write_tokens: Some(1),
                cost_major: None,
                cost_currency: None,
                ..Default::default()
            },
        );
        let dto = AgentSessionRuntimeDto::from_runtime(&row);
        let usage = dto.usage.as_ref().expect("usage attached");
        assert_eq!(usage.input_tokens, Some(12));
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("inputTokens"));
        assert!(!json.contains("nativeSessionId"));
        assert!(!json.contains("native-secret"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     snapshot 必须稳定排序且有 1000 上界。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 1002 条 active，list_active 截断逻辑与排序断言。
    #[tokio::test]
    async fn agent_runtime_snapshot_list_is_stably_sorted_and_bounded() {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        WorkbenchAgentSessionRepo::ensure_schema(&pool)
            .await
            .unwrap();
        let repo = WorkbenchAgentSessionRepo::new(pool);
        for i in 0..1_002 {
            // 不同 terminal 才能同时 active
            let at = format!("2026-07-15T00:{:02}:{:02}Z", i / 60, i % 60);
            repo.create_active(CreateActiveAgentSession {
                id: Some(format!("agent-{i:04}")),
                project_id: "p".into(),
                worktree_id: None,
                terminal_session_id: format!("term-{i}"),
                orchestrator_task_id: None,
                orchestrator_attempt: None,
                provider_id: "p".into(),
                native_session_id: Some(format!("native-{i}")),
                phase: AgentSessionPhase::Launching,
                started_at: at,
                resumed_from_agent_session_id: None,
            })
            .await
            .unwrap();
        }
        let limit = AGENT_RUNTIME_SNAPSHOT_LIMIT;
        let mut rows = repo.list_active(None, limit + 1).await.unwrap();
        let truncated = rows.len() as i64 > limit;
        if truncated {
            rows.truncate(limit as usize);
        }
        assert_eq!(rows.len(), 1_000);
        assert!(truncated);
        assert!(rows
            .windows(2)
            .all(|w| w[0].last_activity_at >= w[1].last_activity_at
                || (w[0].last_activity_at == w[1].last_activity_at && w[0].id <= w[1].id)));
        let sessions: Vec<_> = rows
            .iter()
            .map(AgentSessionRuntimeDto::from_runtime)
            .collect();
        let json = serde_json::to_string(&sessions).unwrap();
        assert!(!json.contains("nativeSessionId"));
        assert!(!json.contains("native-"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同 phase version bump 不得再发 OS 通知；首次进入与跨异常 phase 必须发。
    ///
    /// Code Logic（这个测试做什么）:
    ///     覆盖 None→NeedsInput、NeedsInput→NeedsInput、Working→Failed、NeedsInput→Failed。
    #[test]
    fn agent_exception_notification_only_on_phase_enter() {
        use AgentSessionPhase::*;
        assert!(should_emit_agent_exception_notification(None, NeedsInput));
        assert!(should_emit_agent_exception_notification(
            Some(Working),
            NeedsInput
        ));
        assert!(!should_emit_agent_exception_notification(
            Some(NeedsInput),
            NeedsInput
        ));
        assert!(should_emit_agent_exception_notification(
            Some(Working),
            Failed
        ));
        assert!(should_emit_agent_exception_notification(
            Some(NeedsInput),
            Failed
        ));
        assert!(!should_emit_agent_exception_notification(
            Some(Failed),
            Failed
        ));
        assert!(!should_emit_agent_exception_notification(
            Some(Working),
            Idle
        ));
        assert!(!should_emit_agent_exception_notification(
            Some(NeedsInput),
            Working
        ));
    }
}
