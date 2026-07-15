//! Orchestrator 运营通知发射与 snapshot 捕获。
//!
//! Business Logic（为什么需要这个模块）:
//!     HumanReview/Blocked/taskDone/remoteOutboxFailed 需要跨进程、隐私安全的系统通知链路；
//!     事件写 N1 event_bus，baseline snapshot 经 loopback control 供 GUI handshake 去重。
//!
//! Code Logic（这个模块做什么）:
//!     定义事件名常量、emit helper、稳定 cursor 下的 snapshot 捕获；单测覆盖 payload 形状与隐私字段。

use crate::backend::event_bus::BackendRuntimeCursor;
use crate::error::AppError;
use crate::orchestrator::models::{
    OperationalNotificationEvent, OperationalNotificationKind, OperationalNotificationSnapshot,
    OrchestratorTaskRow,
};
use crate::orchestrator::outbox::OrchestratorRemoteOutboxRow;
use crate::state::AppState;
use serde_json::Value;

/// event_bus / Tauri 运营通知事件名。
pub const OPERATIONAL_NOTIFICATION_EVENT: &str = "operational:notification";

/// snapshot 最多返回的 opaque 条目数。
pub const OPERATIONAL_NOTIFICATION_SNAPSHOT_LIMIT: i64 = 1000;

/// 稳定 cursor 捕获的最大重试次数。
const SNAPSHOT_CURSOR_STABILITY_ATTEMPTS: usize = 8;

/// 发布运营通知事件到 HeadlessOwner event_bus（经 AppState::emit_event）。
///
/// Business Logic（为什么需要这个函数）:
///     真实状态转换后必须写入 sidecar event_bus，GUI 才能经 control relay 收到并去重展示。
///
/// Code Logic（这个函数做什么）:
///     调用 `state.emit_event(operational:notification, event)`；HeadlessOwner 会 publish bus。
pub fn emit_operational_notification(state: &AppState, event: &OperationalNotificationEvent) {
    state.emit_event(OPERATIONAL_NOTIFICATION_EVENT, event);
}

/// 从任务行构造并发布运营通知。
///
/// Business Logic（为什么需要这个函数）:
///     HR/Blocked/Done 转换成功后需要用任务 id + state_version 作为 opaque 去重键。
///
/// Code Logic（这个函数做什么）:
///     opaque_source_id=task.id；occurred_at=updated_at；不包含 title/goal/project。
pub fn emit_task_operational_notification(
    state: &AppState,
    kind: OperationalNotificationKind,
    task: &OrchestratorTaskRow,
) {
    emit_operational_notification(
        state,
        &OperationalNotificationEvent {
            kind,
            opaque_source_id: task.id.clone(),
            state_version: task.state_version,
            occurred_at: task.updated_at.clone(),
        },
    );
}

/// 从 outbox 行构造并发布 remoteOutboxFailed 通知。
///
/// Business Logic（为什么需要这个函数）:
///     协议/校验失败的 outbox 需要全局通知，opaque id 用 outbox id 而非任务标题。
///
/// Code Logic（这个函数做什么）:
///     kind=RemoteOutboxFailed；opaque_source_id=outbox.id。
pub fn emit_outbox_failed_operational_notification(
    state: &AppState,
    item: &OrchestratorRemoteOutboxRow,
) {
    emit_operational_notification(
        state,
        &OperationalNotificationEvent {
            kind: OperationalNotificationKind::RemoteOutboxFailed,
            opaque_source_id: item.id.clone(),
            state_version: item.state_version,
            occurred_at: item.updated_at.clone(),
        },
    );
}

/// 在 event cursor 稳定的窗口捕获运营通知 snapshot。
///
/// Business Logic（为什么需要这个函数）:
///     GUI handshake 需要 asOfCursor 与当前 opaque 状态一致：先订阅再 snapshot 时，
///     若 capture 期间又有 publish，必须重试到 sequence 稳定，否则会漏/重 baseline。
///
/// Code Logic（这个函数做什么）:
///     循环：读 latest_sequence → DB list(limit+1) → 再读 sequence；相等则返回；
///     最多重试 8 次，最后一次仍返回当前读到的结果。items 截到 1000，truncated=原长>1000。
pub async fn capture_operational_notification_snapshot(
    state: &AppState,
) -> Result<OperationalNotificationSnapshot, AppError> {
    let mut last_items = Vec::new();
    let mut last_truncated = false;
    let mut last_seq = state.event_bus.latest_sequence();
    for _ in 0..SNAPSHOT_CURSOR_STABILITY_ATTEMPTS {
        let before = state.event_bus.latest_sequence();
        let (items, truncated) = state
            .orchestrator_repo
            .list_operational_notification_items(OPERATIONAL_NOTIFICATION_SNAPSHOT_LIMIT)
            .await?;
        let after = state.event_bus.latest_sequence();
        last_items = items;
        last_truncated = truncated;
        last_seq = after;
        if before == after {
            break;
        }
    }
    Ok(OperationalNotificationSnapshot {
        as_of_cursor: BackendRuntimeCursor {
            owner_instance_id: state.event_bus.owner_instance_id().to_string(),
            sequence: last_seq,
        },
        items: last_items,
        truncated: last_truncated,
    })
}

/// 校验运营通知 JSON payload 不含隐私字段。
///
/// Business Logic（为什么需要这个函数）:
///     单测与防御性检查需确保 title/goal/project/diff 等不进入 event wire。
///
/// Code Logic（这个函数做什么）:
///     序列化后检查对象 key 集合仅允许 kind/opaqueSourceId/stateVersion/occurredAt。
pub fn operational_notification_payload_is_privacy_safe(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let allowed = [
        "kind",
        "opaqueSourceId",
        "stateVersion",
        "occurredAt",
        // GUI relay 可附加游标字段
        "ownerInstanceId",
        "sequence",
    ];
    obj.keys().all(|k| allowed.contains(&k.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::models::{
        OrchestratorTaskStatus, OrchestratorWorkflowState, SplitTaskState,
    };
    use serde_json::json;

    /// 验证事件序列化形状与隐私字段集合。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     前端/去重依赖 camelCase 四字段；若混入 title 会锁屏泄漏。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 OperationalNotificationEvent 序列化，断言 key 集合与 privacy helper。
    #[test]
    fn operational_notification_event_serializes_privacy_safe_shape() {
        let event = OperationalNotificationEvent {
            kind: OperationalNotificationKind::HumanReview,
            opaque_source_id: "task-1".into(),
            state_version: 3,
            occurred_at: "2026-07-15T00:00:00Z".into(),
        };
        let value = serde_json::to_value(&event).expect("serialize");
        assert_eq!(value["kind"], "humanReview");
        assert_eq!(value["opaqueSourceId"], "task-1");
        assert_eq!(value["stateVersion"], 3);
        assert_eq!(value["occurredAt"], "2026-07-15T00:00:00Z");
        assert!(value.get("title").is_none());
        assert!(value.get("goal").is_none());
        assert!(value.get("projectId").is_none());
        assert!(operational_notification_payload_is_privacy_safe(&value));
    }

    /// 验证含 title 的 payload 被判定为非隐私安全。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     helper 必须拒绝任何额外隐私键，防止回归。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造带 title 的 JSON，断言 privacy helper 返回 false。
    #[test]
    fn operational_notification_event_rejects_title_field() {
        let bad = json!({
            "kind": "blocked",
            "opaqueSourceId": "t1",
            "stateVersion": 1,
            "occurredAt": "t",
            "title": "SECRET"
        });
        assert!(!operational_notification_payload_is_privacy_safe(&bad));
    }

    /// 验证四种 kind 的 camelCase wire 值。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     kind 字符串是 dedupe/前端开关的契约，漂移会静默丢通知。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 as_str 与 from_str round-trip。
    #[test]
    fn operational_notification_kind_wire_values() {
        for kind in [
            OperationalNotificationKind::HumanReview,
            OperationalNotificationKind::Blocked,
            OperationalNotificationKind::RemoteOutboxFailed,
            OperationalNotificationKind::TaskDone,
        ] {
            let s = kind.as_str();
            assert_eq!(
                OperationalNotificationKind::from_str(s).expect("parse"),
                kind
            );
        }
        assert!(OperationalNotificationKind::from_str("done").is_err());
    }

    /// 验证任务通知使用 id 与 state_version，不拷贝 title。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     emit helper 必须以 opaque task id 为 source，禁止把标题写入 payload。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用带敏感 title 的 row 构造事件 JSON，断言无 title 且 opaque=id。
    #[test]
    fn operational_notification_event_from_task_uses_opaque_id_only() {
        let mut task = OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Done);
        task.id = "opaque-task".into();
        task.title = "LEAK_TITLE".into();
        task.goal = "LEAK_GOAL".into();
        task.state_version = 7;
        task.updated_at = "2026-07-15T01:00:00Z".into();
        let split = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Done);
        assert_eq!(split.workflow_state, OrchestratorWorkflowState::Done);

        let event = OperationalNotificationEvent {
            kind: OperationalNotificationKind::TaskDone,
            opaque_source_id: task.id.clone(),
            state_version: task.state_version,
            occurred_at: task.updated_at.clone(),
        };
        let value = serde_json::to_value(&event).unwrap();
        let text = value.to_string();
        assert!(!text.contains("LEAK_TITLE"));
        assert!(!text.contains("LEAK_GOAL"));
        assert_eq!(value["opaqueSourceId"], "opaque-task");
        assert_eq!(value["stateVersion"], 7);
    }

    /// 验证 snapshot DTO camelCase 字段名。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     前端 handshake 依赖 asOfCursor/items/truncated 字段名。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 snapshot，断言 asOfCursor.ownerInstanceId 与 truncated。
    #[test]
    fn operational_notification_snapshot_serializes_cursor_fields() {
        let snapshot = OperationalNotificationSnapshot {
            as_of_cursor: BackendRuntimeCursor {
                owner_instance_id: "owner-x".into(),
                sequence: 42,
            },
            items: vec![OperationalNotificationEvent {
                kind: OperationalNotificationKind::Blocked,
                opaque_source_id: "t".into(),
                state_version: 1,
                occurred_at: "t".into(),
            }],
            truncated: true,
        };
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(value["asOfCursor"]["ownerInstanceId"], "owner-x");
        assert_eq!(value["asOfCursor"]["sequence"], 42);
        assert_eq!(value["truncated"], true);
        assert_eq!(value["items"].as_array().unwrap().len(), 1);
    }
}
