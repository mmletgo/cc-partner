//! orchestrator/remote_protocol.rs — Orchestrator 远端 HTTP 协议 DTO
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench remote shortcut 上的 Orchestrator 操作需要发送到项目所在设备执行，client 与 server route
//!     必须共享同一套请求/响应结构，避免字段漂移。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `/api/orchestrator/...` 远端路由请求与响应 DTO，统一使用 camelCase 序列化/反序列化。

use crate::orchestrator::config::OrchestratorAutomationConfigDto;
use crate::orchestrator::models::{
    OrchestratorCreateAction, OrchestratorEvidenceDto, OrchestratorReviewDiff, OrchestratorTaskDto,
};
use serde::{Deserialize, Serialize};

/// 远端创建 Orchestrator 任务请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     本机 remote shortcut 创建任务时，实际任务必须创建在项目所属设备的本机 Orchestrator 队列中。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、标题、目标、验收标准、优先级、创建动作、幂等键和 tracker 预留字段，字段使用 camelCase。
///     clientRequestId 以 Option 解析，便于 route 返回统一业务错误；create route 会拒绝缺失或空白 key。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCreateOrchestratorTaskReq {
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
    pub priority: i64,
    #[serde(default)]
    pub create_action: OrchestratorCreateAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_labels: Option<Vec<String>>,
}

/// 远端任务 ID 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     evidence、queue、retry 和 abort 等操作只需要定位远端设备上的一个权威任务。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{taskId}`，供 client 与 axum route 共用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskReq {
    pub task_id: String,
}

/// 远端交付人工复核任务请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     deliver-reviewed 需要在 owning device 上比对用户已确认的 review digest；
///     旧 peer 无 review-diff 能力时 client 仍只发 `{taskId}`，不使用本 DTO。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{taskId, expectedReviewDigest?}`；digest 可选以便 wire 兼容反序列化。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeliverReviewedReq {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_review_digest: Option<String>,
}

/// 远端任务返工请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     用户在 remote shortcut 上请求返工时，原因必须记录在 owning device 的权威任务 evidence 中。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{taskId, reason}`，供 client 与 axum route 共用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskReworkReq {
    pub task_id: String,
    pub reason: String,
}

/// 远端任务列表请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     remote shortcut 打开项目时只应拉取该远端 local projectId 的任务列表。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{projectId}`，供任务列表 handler 按项目筛选。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteListTasksReq {
    pub project_id: String,
}

/// 远端 Orchestrator 创建任务 Prompt 完善请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     移动端 `/mobile` 和局域网入口需要通过 HTTP 把简单 Prompt 交给 owning device 生成任务表单字段。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{prompt, workingDirectory}`；工作目录可为空，表示 pure/headless 模式。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCompleteOrchestratorTaskPromptReq {
    pub project_id: Option<String>,
    pub prompt: String,
    pub working_directory: Option<String>,
}

/// 远端 Orchestrator runtime snapshot 请求体（P2P owning-device 路由）。
///
/// Business Logic（为什么需要这个结构体）:
///     其它设备需要通过 P2P HTTP 拉取 owning device 上的项目运行时快照（调度器、workflow、槽位和事件），
///     供 remote shortcut 的状态条展示。请求只携带 owning device 上的 local projectId。
///
/// Code Logic（这个结构体做什么）:
///     P2P wire 契约锁定 snake_case `{"project_id":"..."}`（与 Shared Contracts / Spec 9.2 一致）；
///     route 解析 project_id、确认是本机 local 项目后调用共享 builder。手机浏览器入口使用独立 camelCase DTO。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteRuntimeSnapshotReq {
    #[serde(rename = "project_id")]
    pub project_id: String,
}

/// 手机浏览器 Orchestrator runtime snapshot 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     `/mobile` SPA 通过同源 HTTP 拉取本机或 remote shortcut 的 runtime snapshot，前端约定 camelCase。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 反序列化 `{projectId}`，与 P2P snake_case 请求体刻意分离，避免协议漂移。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MobileRuntimeSnapshotReq {
    pub project_id: String,
}

/// 远端 Orchestrator 任务列表响应。
///
/// Business Logic（为什么需要这个结构体）:
///     客户端需要一个稳定外层字段承载任务列表，后续 Phase 5 mirror/outbox 可复用同一 payload。
///
/// Code Logic（这个结构体做什么）:
///     包装 camelCase `{tasks}`，内部任务沿用 OrchestratorTaskDto。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteOrchestratorTaskListResp {
    pub tasks: Vec<OrchestratorTaskDto>,
}

/// 远端 Orchestrator evidence 响应。
///
/// Business Logic（为什么需要这个结构体）:
///     远端任务详情需要展示 owning device 上真实写入的验证与交付 evidence。
///
/// Code Logic（这个结构体做什么）:
///     包装 camelCase `{evidence}`，内部 evidence 沿用 OrchestratorEvidenceDto。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteOrchestratorEvidenceResp {
    pub evidence: Vec<OrchestratorEvidenceDto>,
}

/// 远端 Orchestrator review diff 响应。
///
/// Business Logic（为什么需要这个结构体）:
///     remote shortcut / mobile 需要展示 owning device 生成的有界 Human Review diff。
///
/// Code Logic（这个结构体做什么）:
///     包装 camelCase `{diff}`，内部 diff 沿用 OrchestratorReviewDiff。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteOrchestratorReviewDiffResp {
    pub diff: OrchestratorReviewDiff,
}

/// Mobile/remote-aware review diff 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     手机浏览器与本机 remote-aware helper 需要同时携带 projectId 与 taskId，
///     才能在 local/remote shortcut 上定位正确任务。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{projectId, taskId}`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileReviewDiffReq {
    pub project_id: String,
    pub task_id: String,
}

/// 远端 Orchestrator 全局配置响应。
///
/// Business Logic（为什么需要这个结构体）:
///     远端诊断/兼容接口需要返回项目所在设备的全局自动化配置，而不是本机 shortcut 的配置。
///     用户可见配置入口固定在对应设备的 Settings 自动化 tab。
///
/// Code Logic（这个结构体做什么）:
///     包装 camelCase `{config}`，内部使用设备级 OrchestratorAutomationConfigDto。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteOrchestratorConfigResp {
    pub config: OrchestratorAutomationConfigDto,
}

/// 远端项目刷新响应。
///
/// Business Logic（为什么需要这个结构体）:
///     用户在 remote shortcut 上刷新项目时，需要知道 owning device 本次 best-effort 调度领取了多少任务。
///
/// Code Logic（这个结构体做什么）:
///     包装 camelCase `{projectId, dispatched}`，projectId 为远端本机项目 id。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteOrchestratorProjectRefreshResp {
    pub project_id: String,
    pub dispatched: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OrchestratorAutomationConfig;
    use crate::orchestrator::models::{
        OrchestratorEvidenceDto, OrchestratorTaskDto, OrchestratorTaskStatus,
    };

    /// Business Logic（为什么需要这个测试）:
    ///     远端 Orchestrator 创建任务协议由局域网其它设备发送，字段必须稳定为前端/HTTP 约定的 camelCase。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 RemoteCreateOrchestratorTaskReq，断言 projectId、acceptanceCriteria 等字段名没有退回 snake_case。
    #[test]
    fn create_request_serializes_as_camel_case() {
        let req = RemoteCreateOrchestratorTaskReq {
            project_id: "project-1".to_string(),
            title: "实现远端任务".to_string(),
            goal: "目标".to_string(),
            acceptance_criteria: "验收".to_string(),
            priority: 7,
            create_action: OrchestratorCreateAction::Todo,
            client_request_id: Some("request-1".to_string()),
            source: Some("linear".to_string()),
            external_id: Some("lin-123".to_string()),
            external_identifier: Some("APP-123".to_string()),
            external_url: Some("https://linear.app/team/issue/APP-123".to_string()),
            external_state: Some("In Progress".to_string()),
            external_labels: Some(vec!["frontend".to_string(), "p1".to_string()]),
        };

        let value = serde_json::to_value(req).expect("serialize request");

        assert_eq!(value["projectId"], "project-1");
        assert_eq!(value["acceptanceCriteria"], "验收");
        assert_eq!(value["priority"], 7);
        assert_eq!(value["createAction"], "todo");
        assert_eq!(value["clientRequestId"], "request-1");
        assert_eq!(value["externalState"], "In Progress");
        assert_eq!(
            value["externalLabels"],
            serde_json::json!(["frontend", "p1"])
        );
        assert!(value.get("project_id").is_none());
        assert!(value.get("queue").is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     route 需要把缺失 clientRequestId 转成统一业务错误，协议层必须先能解析为 None。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化不含 clientRequestId 的 JSON，断言 Option 字段为 None，后续由 route 做必填校验。
    #[test]
    fn create_request_deserializes_missing_client_request_id_for_route_validation() {
        let req: RemoteCreateOrchestratorTaskReq = serde_json::from_value(serde_json::json!({
            "projectId": "project-1",
            "title": "任务",
            "goal": "目标",
            "acceptanceCriteria": "验收",
            "priority": 1,
            "createAction": "backlog"
        }))
        .expect("deserialize request");

        assert!(req.client_request_id.is_none());
        assert_eq!(req.create_action, OrchestratorCreateAction::Backlog);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧移动端协议缺省不应再自动排队，避免创建弹窗未选择动作时直接启动任务。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化不含 createAction 的 JSON，断言协议默认值为 backlog。
    #[test]
    fn create_request_defaults_create_action_to_backlog() {
        let req: RemoteCreateOrchestratorTaskReq = serde_json::from_value(serde_json::json!({
            "projectId": "project-1",
            "title": "任务",
            "goal": "目标",
            "acceptanceCriteria": "验收",
            "priority": 1,
            "clientRequestId": "request-1"
        }))
        .expect("deserialize request");

        assert_eq!(req.create_action, OrchestratorCreateAction::Backlog);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     移动端 AI 完善任务字段走 HTTP 协议，字段名必须稳定为 camelCase。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 RemoteCompleteOrchestratorTaskPromptReq，断言 workingDirectory 字段没有退回 snake_case。
    #[test]
    fn prompt_completion_request_serializes_as_camel_case() {
        let req = RemoteCompleteOrchestratorTaskPromptReq {
            project_id: Some("project-1".to_string()),
            prompt: "创建弹窗".to_string(),
            working_directory: Some("/tmp/project".to_string()),
        };

        let value = serde_json::to_value(req).expect("serialize completion request");

        assert_eq!(value["projectId"], "project-1");
        assert_eq!(value["prompt"], "创建弹窗");
        assert_eq!(value["workingDirectory"], "/tmp/project");
        assert!(value.get("project_id").is_none());
        assert!(value.get("working_directory").is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     P2P owning-device runtime-snapshot 请求体必须精确使用 snake_case `{project_id}`，
    ///     与 Shared Contracts 锁定契约一致；camelCase `{projectId}` 不得被 P2P DTO 接受。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化/序列化 `{"project_id":"..."}` 成功；`{"projectId":"..."}` 失败；空白值仍可解析并由 route 拒绝。
    #[test]
    fn runtime_snapshot_request_round_trips_as_snake_case_project_id() {
        let req: RemoteRuntimeSnapshotReq = serde_json::from_value(serde_json::json!({
            "project_id": "project-1"
        }))
        .expect("deserialize P2P runtime snapshot request");

        assert_eq!(req.project_id, "project-1");

        let value = serde_json::to_value(req).expect("serialize P2P runtime snapshot request");
        assert_eq!(value["project_id"], "project-1");
        assert!(value.get("projectId").is_none());

        let camel_rejected =
            serde_json::from_value::<RemoteRuntimeSnapshotReq>(serde_json::json!({
                "projectId": "project-1"
            }));
        assert!(
            camel_rejected.is_err(),
            "P2P DTO must reject camelCase projectId body"
        );

        // 空白 project_id 由 route guard 校验，协议层仍可解析。
        let blank: RemoteRuntimeSnapshotReq = serde_json::from_value(serde_json::json!({
            "project_id": "   "
        }))
        .expect("deserialize blank project id");
        assert_eq!(blank.project_id, "   ");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     手机浏览器入口继续使用 camelCase `{projectId}`，不得与 P2P snake_case 契约混用。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化/序列化 `{"projectId":"..."}`，断言不退回 snake_case。
    #[test]
    fn mobile_runtime_snapshot_request_round_trips_as_camel_case() {
        let req: MobileRuntimeSnapshotReq = serde_json::from_value(serde_json::json!({
            "projectId": "project-1"
        }))
        .expect("deserialize mobile runtime snapshot request");

        assert_eq!(req.project_id, "project-1");

        let value = serde_json::to_value(req).expect("serialize mobile runtime snapshot request");
        assert_eq!(value["projectId"], "project-1");
        assert!(value.get("project_id").is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 requestRework 需要把返工原因写入 owning device，协议字段不能退回 snake_case 或遗漏 reason。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 RemoteTaskReworkReq，断言 taskId/reason 字段名稳定为 camelCase。
    #[test]
    fn rework_request_serializes_as_camel_case() {
        let req = RemoteTaskReworkReq {
            task_id: "task-1".to_string(),
            reason: "需要补充验证证据".to_string(),
        };

        let value = serde_json::to_value(req).expect("serialize rework request");

        assert_eq!(value["taskId"], "task-1");
        assert_eq!(value["reason"], "需要补充验证证据");
        assert!(value.get("task_id").is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     deliver-reviewed 请求体必须稳定 camelCase 携带 expectedReviewDigest，
    ///     缺 digest 时序列化只保留 taskId，兼容旧 wire 形态。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化带/不带 digest 的 RemoteDeliverReviewedReq，断言字段名与 skip_serializing 行为。
    #[test]
    fn deliver_reviewed_request_serializes_expected_review_digest_as_camel_case() {
        let with_digest = RemoteDeliverReviewedReq {
            task_id: "task-1".to_string(),
            expected_review_digest: Some("digest-a".to_string()),
        };
        let value = serde_json::to_value(with_digest).expect("serialize deliver req");
        assert_eq!(value["taskId"], "task-1");
        assert_eq!(value["expectedReviewDigest"], "digest-a");
        assert!(value.get("expected_review_digest").is_none());

        let without_digest = RemoteDeliverReviewedReq {
            task_id: "task-2".to_string(),
            expected_review_digest: None,
        };
        let value = serde_json::to_value(without_digest).expect("serialize deliver req without digest");
        assert_eq!(value["taskId"], "task-2");
        assert!(value.get("expectedReviewDigest").is_none());

        let parsed: RemoteDeliverReviewedReq = serde_json::from_value(serde_json::json!({
            "taskId": "task-legacy"
        }))
        .expect("legacy body without digest must deserialize");
        assert_eq!(parsed.task_id, "task-legacy");
        assert!(parsed.expected_review_digest.is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端任务列表是 remote shortcut 后续展示的基础 payload，列表外层字段必须与客户端约定一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 RemoteOrchestratorTaskListResp，断言外层 tasks 与内部 DTO 的 camelCase 字段。
    #[test]
    fn list_response_serializes_tasks_as_camel_case() {
        let resp = RemoteOrchestratorTaskListResp {
            tasks: vec![task_dto("task-1", OrchestratorTaskStatus::Queued)],
        };

        let value = serde_json::to_value(resp).expect("serialize list response");

        assert_eq!(value["tasks"][0]["projectId"], "project-1");
        assert_eq!(value["tasks"][0]["acceptanceCriteria"], "验收");
        assert_eq!(value["tasks"][0]["status"], "queued");
        assert!(
            value["tasks"][0].get("externalState").is_some(),
            "externalState should stay in remote task payload"
        );
        assert!(
            value["tasks"][0].get("externalLabels").is_some(),
            "externalLabels should stay in remote task payload"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Evidence 详情会被远端客户端直接展示，外层与内层字段都必须维持 camelCase 契约。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 RemoteOrchestratorEvidenceResp，断言 evidence/taskId/createdAt 字段名。
    #[test]
    fn evidence_response_serializes_as_camel_case() {
        let resp = RemoteOrchestratorEvidenceResp {
            evidence: vec![OrchestratorEvidenceDto {
                id: "evidence-1".to_string(),
                task_id: "task-1".to_string(),
                kind: "verificationOutput".to_string(),
                title: "验证命令".to_string(),
                summary: "passed".to_string(),
                content: "ok".to_string(),
                created_at: "2026-07-05T00:00:00Z".to_string(),
            }],
        };

        let value = serde_json::to_value(resp).expect("serialize evidence response");

        assert_eq!(value["evidence"][0]["taskId"], "task-1");
        assert_eq!(value["evidence"][0]["createdAt"], "2026-07-05T00:00:00Z");
        assert!(value["evidence"][0].get("task_id").is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 config 接口暴露设备级 Orchestrator 自动化策略，客户端依赖 config 外层字段和内部 camelCase。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 RemoteOrchestratorConfigResp，断言 config.maxConcurrentTasks 与 delivery flag 字段。
    #[test]
    fn config_response_serializes_as_camel_case() {
        let resp = RemoteOrchestratorConfigResp {
            config: OrchestratorAutomationConfig::default().into(),
        };

        let value = serde_json::to_value(resp).expect("serialize config response");

        assert_eq!(value["config"]["maxConcurrentTasks"], 1);
        assert_eq!(value["config"]["autoPushMain"], true);
        assert!(value["config"].get("max_concurrent_tasks").is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     refreshOrchestratorProject 的远端响应需要让本机知道 owning device 本次领取了多少任务。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 RemoteOrchestratorProjectRefreshResp，断言 projectId/dispatched 字段为 camelCase。
    #[test]
    fn refresh_response_serializes_as_camel_case() {
        let resp = RemoteOrchestratorProjectRefreshResp {
            project_id: "project-1".to_string(),
            dispatched: 2,
        };

        let value = serde_json::to_value(resp).expect("serialize refresh response");

        assert_eq!(value["projectId"], "project-1");
        assert_eq!(value["dispatched"], 2);
        assert!(value.get("project_id").is_none());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     协议测试需要一个完整任务 DTO 样本，避免每个测试重复填充所有可选字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按传入 id/status 构造稳定 OrchestratorTaskDto，其它字段使用固定测试值。
    fn task_dto(id: &str, status: OrchestratorTaskStatus) -> OrchestratorTaskDto {
        OrchestratorTaskDto {
            id: id.to_string(),
            project_id: "project-1".to_string(),
            title: "任务".to_string(),
            goal: "目标".to_string(),
            acceptance_criteria: "验收".to_string(),
            status,
            priority: 0,
            branch_name: None,
            worktree_id: None,
            session_id: None,
            blocked_reason: None,
            attempt: 0,
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
            started_at: None,
            finished_at: None,
            ..OrchestratorTaskDto::default_for_status(status)
        }
    }
}
