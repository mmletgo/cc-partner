//! workbench/agent_runtime/snapshot — 有界 Agent runtime 投影快照
//!
//! Business Logic（为什么需要这个模块）:
//!     Gap/owner change 后 desktop/remote/mobile 需要同一份 active session baseline，
//!     且不得包含 native_session_id、正文或路径。
//!
//! Code Logic（这个模块做什么）:
//!     定义 sanitized DTO、snapshot 结构；`get_agent_runtime_snapshot_for_state` 读 repo
//!     并捕获 event_bus cursor（变化则重试一次）。

use super::models::{AgentSessionPhase, AgentSessionRuntime};
use crate::error::AppError;
use crate::state::AppState;
use serde::{Deserialize, Serialize};

/// 单 snapshot 最多返回的 active sessions。
pub const AGENT_RUNTIME_SNAPSHOT_LIMIT: i64 = 1_000;

/// 投影用 Agent session DTO（禁止 native_session_id）。
///
/// Business Logic（为什么需要这个类型）:
///     UI/CLI/P2P 只需稳定 cc-partner ID 与 phase；provider-native id 仅 owner-local。
///
/// Code Logic（这个类型做什么）:
///     camelCase serde；字段集刻意不含 nativeSessionId。
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
}

impl AgentSessionRuntimeDto {
    /// 从内部 runtime 行映射投影 DTO（剔除 native_session_id）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     repo 行含 owner-local 字段；投影边界必须统一剥离。
    ///
    /// Code Logic（这个函数做什么）:
    ///     逐字段拷贝，忽略 native_session_id。
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
///
/// Code Logic（这个函数做什么）:
///     读 event_bus cursor → list_active(limit+1) → 截断 → 若 cursor 变化重试一次。
pub async fn get_agent_runtime_snapshot_for_state(
    state: &AppState,
    project_id: Option<String>,
) -> Result<AgentRuntimeSnapshot, AppError> {
    let owner_instance_id = state.config_runtime.owner_instance_id().to_string();
    let mut attempt = 0;
    loop {
        let cursor_before = state.event_bus.latest_sequence();
        let limit = AGENT_RUNTIME_SNAPSHOT_LIMIT;
        let mut rows = state
            .workbench_agent_session_repo
            .list_active(project_id.as_deref(), limit + 1)
            .await?;
        let truncated = rows.len() as i64 > limit;
        if truncated {
            rows.truncate(limit as usize);
        }
        let cursor_after = state.event_bus.latest_sequence();
        if cursor_before != cursor_after && attempt == 0 {
            attempt += 1;
            continue;
        }
        let sessions = rows
            .iter()
            .map(AgentSessionRuntimeDto::from_runtime)
            .collect();
        return Ok(AgentRuntimeSnapshot {
            owner_instance_id,
            as_of_sequence: cursor_after,
            project_id,
            sessions,
            truncated,
        });
    }
}

/// 发出 workbench:agent-runtime 事件（mutation 已 durable 后调用）。
///
/// Business Logic（为什么需要这个函数）:
///     前端与 control relay 需要在提交后立刻看到 phase 变化。
///
/// Code Logic（这个函数做什么）:
///     映射 DTO 后 `state.emit_event`；不携带 native_session_id。
pub fn emit_agent_runtime_changed(state: &AppState, row: &AgentSessionRuntime) {
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
    // A2：仅 needsInput/failed 进入运营通知；working/idle/completed 默认无 OS 噪音。
    emit_agent_operational_notification_if_needed(state, row);
}

/// Business Logic（为什么需要这个函数）:
///     用户不必盯住 terminal 也能知道 Agent 等待输入或失败；completed 默认不发。
///
/// Code Logic（这个函数做什么）:
///     phase=NeedsInput/Failed 时 emit operational:notification，opaque=agentSessionId，
///     state_version=version；无 title/project/path。
fn emit_agent_operational_notification_if_needed(state: &AppState, row: &AgentSessionRuntime) {
    use crate::orchestrator::models::{OperationalNotificationEvent, OperationalNotificationKind};
    use crate::orchestrator::notifications::emit_operational_notification;

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
        assert!(json.contains("agent") || json.contains("\"id\":\"a1\""));
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
}
