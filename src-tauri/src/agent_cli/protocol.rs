//! agent_cli/protocol.rs — Agent control 查询/变更 wire 类型与 replay 策略。
//!
//! Business Logic（为什么需要这个模块）:
//!     CLI 与 loopback control agent endpoints 共享闭集 query/mutate 合同。
//!
//! Code Logic（这个模块做什么）:
//!     定义 AgentControlQuery/Mutation 枚举、结果信封与 MutationReplayPolicy。

use crate::agent_cli::selectors::{ProjectSelector, WorktreeSelector};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// mutation 重放策略。
///
/// Business Logic（为什么需要这个枚举）:
///     non-replayable 操作连接丢失不得盲重放；可对账的则先查再定。
///
/// Code Logic（这个枚举做什么）:
///     - `NeverReplay`：CLI 只发一次 mutation；dispatch 后 transport/timeout → `outcomeUnknown`。
///     - `ReconcileByRequestId`：服务端 ledger/领域幂等使 agent 同 requestId 重入安全；
///       CLI 在 transport uncertainty 后可单次 query 对账，**绝不**自动重放 mutation。
///     - `NaturallyIdempotent`：同 mutation 重入安全（如已取消再 cancel）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationReplayPolicy {
    ReconcileByRequestId,
    NaturallyIdempotent,
    NeverReplay,
}

/// 本机 control agent 查询变体（闭集）。
///
/// Business Logic（为什么需要这个枚举）:
///     query 必须 side-effect-free，且不得隐式 spawn/restore terminal。
///
/// Code Logic（这个枚举做什么）:
///     tag=`kind` camelCase 序列化供 HTTP body。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AgentControlQuery {
    ProjectList,
    ProjectInspect {
        #[serde(with = "project_selector_serde")]
        selector: ProjectSelector,
    },
    WorktreeList {
        #[serde(with = "project_selector_serde")]
        project: ProjectSelector,
    },
    SessionList {
        #[serde(with = "project_selector_serde")]
        project: ProjectSelector,
        #[serde(default, with = "option_worktree_selector_serde")]
        worktree: Option<WorktreeSelector>,
    },
    SessionRead {
        session_id: String,
        after_sequence: Option<u64>,
    },
    AgentList {
        #[serde(with = "project_selector_serde")]
        project: ProjectSelector,
    },
    AgentInspect {
        agent_session_id: String,
    },
    AgentWait {
        agent_session_id: String,
        phase: String,
        timeout_ms: u64,
    },
    TaskList {
        #[serde(with = "project_selector_serde")]
        project: ProjectSelector,
    },
    ExperimentInspect {
        experiment_id: String,
    },
    AttentionList,
    FleetSnapshot,
    BrowserDiscover {
        #[serde(with = "project_selector_serde")]
        project: ProjectSelector,
    },
    BrowserInspect {
        run_id: String,
    },
    /// 从 owner mDNS 设备表解析远端 base URL（control-only，禁止 LAN business API 绕过）。
    DeviceResolve {
        device_id: String,
    },
    /// 按 clientRequestId 查询 create ledger 命中的任务（ReconcileByRequestId 对账）。
    TaskByClientRequestId {
        client_request_id: String,
    },
}

/// 本机 control agent 变更变体（闭集）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AgentControlMutation {
    WorktreeCreate {
        #[serde(with = "project_selector_serde")]
        project: ProjectSelector,
        /// 已解析的 create 字段（不含敏感日志）
        payload: Value,
    },
    SessionSend {
        session_id: String,
        /// terminal 输入；序列化进 control body，从不写 tracing
        data: String,
    },
    TaskCreate {
        #[serde(with = "project_selector_serde")]
        project: ProjectSelector,
        payload: Value,
        /// 非空幂等键；owner/ledger 按此 dedupe，禁止静默丢弃。
        client_request_id: String,
    },
    TaskCancel {
        task_id: String,
        client_request_id: String,
    },
    TaskRetry {
        task_id: String,
        client_request_id: String,
    },
    ExperimentCreate {
        #[serde(with = "project_selector_serde")]
        project: ProjectSelector,
        payload: Value,
        client_request_id: Option<String>,
    },
    ExperimentApprove {
        experiment_id: String,
        winner_task_id: String,
        reason: Option<String>,
    },
    ExperimentCancel {
        experiment_id: String,
    },
    BrowserVerify {
        #[serde(with = "project_selector_serde")]
        project: ProjectSelector,
        payload: Value,
    },
}

impl AgentControlMutation {
    /// Business Logic（为什么需要这个函数）:
    ///     每种 mutation 必须有 total match 的 replay policy。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 NeverReplay / ReconcileByRequestId / NaturallyIdempotent。
    pub fn replay_policy(&self) -> MutationReplayPolicy {
        match self {
            Self::SessionSend { .. } | Self::WorktreeCreate { .. } | Self::BrowserVerify { .. } => {
                MutationReplayPolicy::NeverReplay
            }
            Self::TaskCreate { .. }
            | Self::TaskCancel { .. }
            | Self::TaskRetry { .. }
            | Self::ExperimentCreate { .. } => MutationReplayPolicy::ReconcileByRequestId,
            Self::ExperimentApprove { .. } | Self::ExperimentCancel { .. } => {
                MutationReplayPolicy::NaturallyIdempotent
            }
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     ReconcileByRequestId 在 mutation 响应丢失后需用领域 ledger 对账，而不是盲重放。
    ///
    /// Code Logic（这个函数做什么）:
    ///     TaskCreate → `TaskByClientRequestId`；其它 reconcile 变体暂无独立 ledger query 则 None
    ///     （agent 可安全重入，依赖服务端幂等）。
    pub fn reconcile_query(&self) -> Option<AgentControlQuery> {
        match self {
            Self::TaskCreate {
                client_request_id, ..
            } => Some(AgentControlQuery::TaskByClientRequestId {
                client_request_id: client_request_id.clone(),
            }),
            _ => None,
        }
    }
}

/// control agent HTTP 请求（token + op）。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentControlRequest<T> {
    pub control_token: String,
    pub op: T,
}

/// control agent 成功响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentControlResponse {
    pub owner_instance_id: String,
    pub data: Value,
}

/// ProjectSelector 自定义 serde（wire 用字符串 `id:`/`path:`）。
mod project_selector_serde {
    use super::ProjectSelector;
    use crate::agent_cli::selectors::parse_project_selector;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &ProjectSelector, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = match value {
            ProjectSelector::Id(id) => format!("id:{id}"),
            ProjectSelector::Path(path) => format!("path:{path}"),
        };
        serializer.serialize_str(&s)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<ProjectSelector, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_project_selector(&s).map_err(serde::de::Error::custom)
    }
}

mod option_worktree_selector_serde {
    use super::WorktreeSelector;
    use crate::agent_cli::selectors::parse_worktree_selector;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Option<WorktreeSelector>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            None => serializer.serialize_none(),
            Some(WorktreeSelector::Id(id)) => serializer.serialize_some(&format!("id:{id}")),
            Some(WorktreeSelector::Branch(b)) => serializer.serialize_some(&format!("branch:{b}")),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<WorktreeSelector>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt = Option::<String>::deserialize(deserializer)?;
        match opt {
            None => Ok(None),
            Some(s) => parse_worktree_selector(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_send_is_never_replay() {
        let m = AgentControlMutation::SessionSend {
            session_id: "s1".into(),
            data: "pwd\n".into(),
        };
        assert_eq!(m.replay_policy(), MutationReplayPolicy::NeverReplay);
    }

    #[test]
    fn task_cancel_reconciles_by_request_id() {
        let m = AgentControlMutation::TaskCancel {
            task_id: "t1".into(),
            client_request_id: "r1".into(),
        };
        assert_eq!(
            m.replay_policy(),
            MutationReplayPolicy::ReconcileByRequestId
        );
    }

    #[test]
    fn experiment_cancel_is_naturally_idempotent() {
        let m = AgentControlMutation::ExperimentCancel {
            experiment_id: "e1".into(),
        };
        assert_eq!(m.replay_policy(), MutationReplayPolicy::NaturallyIdempotent);
    }

    #[test]
    fn experiment_approve_is_naturally_idempotent() {
        let m = AgentControlMutation::ExperimentApprove {
            experiment_id: "e1".into(),
            winner_task_id: "t1".into(),
            reason: None,
        };
        assert_eq!(m.replay_policy(), MutationReplayPolicy::NaturallyIdempotent);
    }

    #[test]
    fn query_roundtrip_json() {
        let q = AgentControlQuery::SessionRead {
            session_id: "s1".into(),
            after_sequence: Some(0),
        };
        let v = serde_json::to_value(&q).unwrap();
        assert_eq!(v["kind"], "sessionRead");
        let back: AgentControlQuery = serde_json::from_value(v).unwrap();
        assert_eq!(back, q);
    }
}
