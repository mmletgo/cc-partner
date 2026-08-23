//! user_mirror/ledger — 用户级镜像 preview plan 的 SQLite 幂等记录
//!
//! Business Logic（为什么需要这个模块）:
//!     apply 必须绑定短期 preview；同 `clientRequestId` 重放精确结果，崩溃后未完成不得标成功。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `agent_hub_user_mirror_plans` 行与 claim 三态；具体 SQL 在 `AgentHubRepo`。

use serde::{Deserialize, Serialize};

/// 用户级镜像 preview plan 持久化行。
///
/// Business Logic: owner 重启后仍可按 `clientRequestId` 对账；plan 有 15 分钟 TTL。
/// Code Logic: SQLite `agent_hub_user_mirror_plans` 行；变更明细在 `plan_json`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorPlanRecord {
    /// 不可猜短期 token
    pub plan_token: String,
    /// 过期时间 RFC3339
    pub expires_at: String,
    /// 完整 `UserMirrorPlanDto` JSON
    pub plan_json: String,
    /// 首次 apply 幂等键
    pub client_request_id: Option<String>,
    /// 原子 claim 时间
    pub claimed_at: Option<String>,
    /// 已消费时间
    pub consumed_at: Option<String>,
    /// 幂等返回结果 JSON
    pub result_json: Option<String>,
    /// 创建时间
    pub created_at: String,
}

/// 用户级镜像 plan 原子 claim 结果。
///
/// Business Logic: 同 token+request 且已 complete → Replay；已 claim 未 complete → Pending。
/// Code Logic: 与 `PortablePullClaim` 三态对齐。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserMirrorClaim {
    /// 本请求获得执行权
    Claimed(Box<UserMirrorPlanRecord>),
    /// 同 id 请求正在执行
    Pending,
    /// 同 id 已完成（精确 result JSON）
    Replay(String),
}

#[cfg(test)]
mod tests {
    use super::{UserMirrorClaim, UserMirrorPlanRecord};
    use crate::agent_hub::user_mirror::models::{
        ApplyUserMirrorRequest, UserMirrorDirection, UserMirrorInventoryDto, UserMirrorItemState,
        USER_MIRROR_PREVIEW_REQUIRED, USER_MIRROR_STALE,
    };
    use crate::agent_hub::user_mirror::preview::preview_from_two_inventories;
    use crate::agent_hub::user_mirror::service::{
        apply_user_mirror, get_user_mirror, preview_from_two_inventories as persist_preview,
    };
    use crate::backend::runtime::build_app_state;
    use crate::backend::ui::RecordingBackendUi;
    use crate::config::install_data_dir_env;
    use crate::error::AppError;
    use crate::state::AppState;
    use chrono::{Duration, Utc};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    async fn isolated_state() -> (tempfile::TempDir, AppState, crate::config::DataDirEnvGuard) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).expect("mkdir");
        let guard = install_data_dir_env(Some(data.to_str().expect("utf8")));
        let ui = Arc::new(RecordingBackendUi::default());
        let state = build_app_state(ui).await.expect("state");
        (tmp, state, guard)
    }

    fn empty_inventory(device: &str) -> UserMirrorInventoryDto {
        UserMirrorInventoryDto {
            source_device_id: device.to_string(),
            inventory_snapshot_hash: format!("snap-{device}"),
            refreshed_at: "2026-08-23T00:00:00Z".into(),
            agents: Vec::new(),
            credential_bearing_count: 0,
        }
    }

    fn empty_result_json(plan_token: &str, request_id: &str) -> String {
        serde_json::json!({
            "planToken": plan_token,
            "clientRequestId": request_id,
            "sourceDeviceId": "src",
            "destinationDeviceId": "dst",
            "partial": false,
            "agents": []
        })
        .to_string()
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同 plan+request 且已有 result 必须精确重放，禁止二次写盘。
    ///
    /// Code Logic（这个测试做什么）:
    ///     insert → claim Claimed → complete → 再 claim 得到 Replay，JSON 与写入一致。
    #[tokio::test]
    async fn claim_replays_completed_result_for_same_plan_and_request() {
        let (_tmp, state, _guard) = isolated_state().await;
        let plan = preview_from_two_inventories(
            &empty_inventory("src"),
            &empty_inventory("dst"),
            "src",
            "dst",
            UserMirrorDirection::Pull,
        );
        state
            .agent_hub_repo
            .insert_user_mirror_plan(UserMirrorPlanRecord {
                plan_token: plan.plan_token.clone(),
                expires_at: plan.expires_at.clone(),
                plan_json: serde_json::to_string(&plan).unwrap(),
                client_request_id: None,
                claimed_at: None,
                consumed_at: None,
                result_json: None,
                created_at: Utc::now().to_rfc3339(),
            })
            .await
            .unwrap();
        let first = state
            .agent_hub_repo
            .claim_user_mirror_plan(&plan.plan_token, "req-1")
            .await
            .unwrap();
        assert!(matches!(first, UserMirrorClaim::Claimed(_)));
        let result_json = empty_result_json(&plan.plan_token, "req-1");
        state
            .agent_hub_repo
            .complete_user_mirror_plan(&plan.plan_token, "req-1", &result_json)
            .await
            .unwrap();
        let replay = state
            .agent_hub_repo
            .claim_user_mirror_plan(&plan.plan_token, "req-1")
            .await
            .unwrap();
        match replay {
            UserMirrorClaim::Replay(json) => assert_eq!(json, result_json),
            other => panic!("expected replay, got {other:?}"),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     已 claim 未 complete 表示写盘可能仍在进行；不得标成功，get 必须 outcomeUnknown。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim 后不 complete；第二次 claim 为 Pending；get(request) 各 Agent 为 OutcomeUnknown。
    #[tokio::test]
    async fn pending_claim_makes_get_return_outcome_unknown() {
        let (_tmp, state, _guard) = isolated_state().await;
        let plan = persist_preview(
            &state,
            &empty_inventory("src"),
            &empty_inventory("dst"),
            "src",
            "dst",
            UserMirrorDirection::Pull,
        )
        .await
        .unwrap();
        let claimed = state
            .agent_hub_repo
            .claim_user_mirror_plan(&plan.plan_token, "req-pending")
            .await
            .unwrap();
        assert!(matches!(claimed, UserMirrorClaim::Claimed(_)));
        let pending = state
            .agent_hub_repo
            .claim_user_mirror_plan(&plan.plan_token, "req-pending")
            .await
            .unwrap();
        assert!(matches!(pending, UserMirrorClaim::Pending));
        let got = get_user_mirror(&state, "req-pending").await.unwrap();
        assert_eq!(got.plan_token, plan.plan_token);
        assert_eq!(got.client_request_id, "req-pending");
        assert!(got.partial);
        assert!(got
            .agents
            .iter()
            .all(|agent| agent.state == UserMirrorItemState::OutcomeUnknown));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     一个 clientRequestId 只能绑定一份 preview；换 plan 重放会覆盖错误目标。
    ///
    /// Code Logic（这个测试做什么）:
    ///     plan-a 已 claim；plan-b 用同一 request → conflict。
    #[tokio::test]
    async fn different_plan_same_request_conflicts() {
        let (_tmp, state, _guard) = isolated_state().await;
        let plan_a = persist_preview(
            &state,
            &empty_inventory("src"),
            &empty_inventory("dst"),
            "src",
            "dst",
            UserMirrorDirection::Pull,
        )
        .await
        .unwrap();
        state
            .agent_hub_repo
            .claim_user_mirror_plan(&plan_a.plan_token, "req-1")
            .await
            .unwrap();
        let plan_b = persist_preview(
            &state,
            &empty_inventory("src2"),
            &empty_inventory("dst2"),
            "src2",
            "dst2",
            UserMirrorDirection::Pull,
        )
        .await
        .unwrap();
        let conflict = state
            .agent_hub_repo
            .claim_user_mirror_plan(&plan_b.plan_token, "req-1")
            .await;
        match conflict {
            Err(AppError::Conflict(message)) => {
                assert!(
                    message.contains("USER_MIRROR_REQUEST_BOUND_TO_OTHER_PLAN"),
                    "{message}"
                );
            }
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     过期预览不得覆盖原生文件；调用方必须重新 preview。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 expires_at 已过的 plan，apply → USER_MIRROR_STALE。
    #[tokio::test]
    async fn expired_plan_apply_is_stale() {
        let (_tmp, state, _guard) = isolated_state().await;
        let mut plan = preview_from_two_inventories(
            &empty_inventory("src"),
            &empty_inventory("dst"),
            "src",
            "dst",
            UserMirrorDirection::Pull,
        );
        plan.expires_at = (Utc::now() - Duration::minutes(1)).to_rfc3339();
        state
            .agent_hub_repo
            .insert_user_mirror_plan(UserMirrorPlanRecord {
                plan_token: plan.plan_token.clone(),
                expires_at: plan.expires_at.clone(),
                plan_json: serde_json::to_string(&plan).unwrap(),
                client_request_id: None,
                claimed_at: None,
                consumed_at: None,
                result_json: None,
                created_at: Utc::now().to_rfc3339(),
            })
            .await
            .unwrap();
        let err = apply_user_mirror(
            &state,
            ApplyUserMirrorRequest {
                plan_token: plan.plan_token,
                client_request_id: "req-stale".into(),
            },
            &BTreeMap::new(),
            &[],
        )
        .await
        .expect_err("stale");
        assert!(err.to_string().contains(USER_MIRROR_STALE), "{}", err);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     未预览不得直接 apply，强制走 preview + 破坏性确认。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对不存在的 plan_token 调 apply → USER_MIRROR_PREVIEW_REQUIRED。
    #[tokio::test]
    async fn missing_plan_apply_requires_preview() {
        let (_tmp, state, _guard) = isolated_state().await;
        let err = apply_user_mirror(
            &state,
            ApplyUserMirrorRequest {
                plan_token: "no-such-plan".into(),
                client_request_id: "req-missing".into(),
            },
            &BTreeMap::new(),
            &[],
        )
        .await
        .expect_err("preview required");
        assert!(
            err.to_string().contains(USER_MIRROR_PREVIEW_REQUIRED),
            "{}",
            err
        );
    }
}
