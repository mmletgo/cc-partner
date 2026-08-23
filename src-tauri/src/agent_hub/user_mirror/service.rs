//! user_mirror/service — 本机 Pull 的 preview / apply / get 门面
//!
//! Business Logic（为什么需要这个模块）:
//!     镜像必须先 preview 落库，apply 原子 claim 后再写盘；同 request 重放结果，
//!     崩溃未完成经 get 暴露 outcomeUnknown，禁止跳过 plan 直接覆盖。
//!
//! Code Logic（这个模块做什么）:
//!     preview 插入 `agent_hub_user_mirror_plans`；apply claim → 调 apply.rs → complete；
//!     get 按 client_request_id 读 result 或拼 outcomeUnknown。本地 Pull 可用两份 AppState。

use super::apply::apply_user_mirror_instructions_with_env;
use super::inventory::build_local_user_mirror_inventory_with_env;
use super::ledger::{UserMirrorClaim, UserMirrorPlanRecord};
use super::models::{
    ApplyUserMirrorRequest, PreviewUserMirrorRequest, UserMirrorAgentResultDto,
    UserMirrorDirection, UserMirrorInventoryDto, UserMirrorItemState, UserMirrorPlanDto,
    UserMirrorResultDto, USER_MIRROR_PREVIEW_REQUIRED, USER_MIRROR_STALE,
};
use super::preview::preview_from_two_inventories as build_preview_from_two_inventories;
use super::selection::UserMirrorObjectBinding;
use crate::agent_hub::targets::TargetEnvironment;
use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use std::collections::BTreeMap;

/// 用两份已构建 inventory 生成 preview 并写入 dest 端 plan ledger。
///
/// Business Logic（为什么需要这个函数）:
///     本地-本地测试与后续 LAN dest apply 都需要把 plan 绑到 dest owner 的 SQLite；
///     纯 diff 的空 token 不能当正式 apply 凭证。
///
/// Code Logic（这个函数做什么）:
///     调用 preview.rs 填 token/TTL，再 `insert_user_mirror_plan`；plan_json 为 DTO。
pub async fn preview_from_two_inventories(
    dest_state: &AppState,
    source: &UserMirrorInventoryDto,
    dest: &UserMirrorInventoryDto,
    source_device_id: &str,
    dest_device_id: &str,
    direction: UserMirrorDirection,
) -> Result<UserMirrorPlanDto, AppError> {
    let plan = build_preview_from_two_inventories(
        source,
        dest,
        source_device_id,
        dest_device_id,
        direction,
    );
    persist_plan(dest_state, &plan).await?;
    Ok(plan)
}

/// 本地 Pull：从 source/dest 两份 AppState 扫 inventory 并在 dest 落 plan。
///
/// Business Logic（为什么需要这个函数）:
///     T7 先接通本机双 AppState Pull；LAN 源 inventory 由后续路由替换扫描入口。
///
/// Code Logic（这个函数做什么）:
///     用进程环境扫两端 `build_local`；dest 为 apply 端并插入 plan 行。
pub async fn preview_user_mirror(
    dest_state: &AppState,
    source_state: &AppState,
    request: PreviewUserMirrorRequest,
) -> Result<UserMirrorPlanDto, AppError> {
    let env = TargetEnvironment::from_process();
    preview_user_mirror_with_envs(dest_state, source_state, request, &env, &env).await
}

/// 注入源/目标环境的本地 preview（DualEnv 测试与生产共用落库规则）。
///
/// Business Logic: 隔离 HOME 必须与生产走同一 inventory 白名单，plan 仍写 dest DB。
/// Code Logic: 分别扫 source/dest inventory，再 `preview_from_two_inventories` 落库。
pub(crate) async fn preview_user_mirror_with_envs(
    dest_state: &AppState,
    source_state: &AppState,
    request: PreviewUserMirrorRequest,
    source_env: &TargetEnvironment,
    dest_env: &TargetEnvironment,
) -> Result<UserMirrorPlanDto, AppError> {
    let source_device_id = match request.direction {
        UserMirrorDirection::Pull => request
            .source_device_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| AppError::validation(USER_MIRROR_PREVIEW_REQUIRED))?
            .to_string(),
        UserMirrorDirection::Push => source_state.device_id.as_str().to_string(),
    };
    let dest_device_id = dest_state.device_id.as_str();
    let source_inventory =
        build_local_user_mirror_inventory_with_env(source_state, &source_device_id, source_env)
            .await?;
    let dest_inventory =
        build_local_user_mirror_inventory_with_env(dest_state, dest_device_id, dest_env).await?;
    preview_from_two_inventories(
        dest_state,
        &source_inventory,
        &dest_inventory,
        &source_device_id,
        dest_device_id,
        request.direction,
    )
    .await
}

/// 应用已预览镜像（claim → 写盘+extras → complete）。
///
/// Business Logic（为什么需要这个函数）:
///     dest owner 必须按 preview 覆盖；同 request 重放；缺 plan 强制重新预览。
///
/// Code Logic（这个函数做什么）:
///     用进程 `TargetEnvironment` 调 `apply_user_mirror_with_env`。
pub async fn apply_user_mirror(
    dest_state: &AppState,
    request: ApplyUserMirrorRequest,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<UserMirrorResultDto, AppError> {
    apply_user_mirror_with_env(
        dest_state,
        request,
        objects,
        bindings,
        &TargetEnvironment::from_process(),
    )
    .await
}

/// 注入 dest 环境的 apply（测试隔离 HOME）。
///
/// Business Logic: DualEnv 不得扫到开发者真实配置，写盘规则与生产相同。
/// Code Logic: claim 三态；Claimed 调 apply.rs 后 complete；失败也 complete 以免假死 Pending。
pub(crate) async fn apply_user_mirror_with_env(
    dest_state: &AppState,
    request: ApplyUserMirrorRequest,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
    env: &TargetEnvironment,
) -> Result<UserMirrorResultDto, AppError> {
    if request.plan_token.trim().is_empty() || request.client_request_id.trim().is_empty() {
        return Err(AppError::validation(USER_MIRROR_PREVIEW_REQUIRED));
    }
    let claim = dest_state
        .agent_hub_repo
        .claim_user_mirror_plan(&request.plan_token, &request.client_request_id)
        .await?;
    match claim {
        UserMirrorClaim::Replay(json) => serde_json::from_str(&json).map_err(AppError::from),
        UserMirrorClaim::Pending => {
            let row = dest_state
                .agent_hub_repo
                .get_user_mirror_plan(&request.plan_token)
                .await?
                .ok_or_else(|| AppError::validation(USER_MIRROR_PREVIEW_REQUIRED))?;
            let plan = parse_plan(&row.plan_json)?;
            Ok(outcome_unknown_result(
                &request.plan_token,
                &request.client_request_id,
                &plan,
            ))
        }
        UserMirrorClaim::Claimed(record) => {
            let plan = parse_plan(&record.plan_json)?;
            if plan.expires_at.as_str() < Utc::now().to_rfc3339().as_str() {
                let fail =
                    failed_stale_result(&request.plan_token, &request.client_request_id, &plan);
                dest_state
                    .agent_hub_repo
                    .complete_user_mirror_plan(
                        &request.plan_token,
                        &request.client_request_id,
                        &serde_json::to_string(&fail)?,
                    )
                    .await?;
                return Err(AppError::conflict(USER_MIRROR_STALE));
            }
            let result = match apply_user_mirror_instructions_with_env(
                dest_state, &plan, objects, bindings, env,
            )
            .await
            {
                Ok(agents) => build_result(
                    &request.plan_token,
                    &request.client_request_id,
                    &plan,
                    agents,
                ),
                Err(error) => {
                    let fail = failed_apply_result(
                        &request.plan_token,
                        &request.client_request_id,
                        &plan,
                        &error,
                    );
                    dest_state
                        .agent_hub_repo
                        .complete_user_mirror_plan(
                            &request.plan_token,
                            &request.client_request_id,
                            &serde_json::to_string(&fail)?,
                        )
                        .await?;
                    return Ok(fail);
                }
            };
            dest_state
                .agent_hub_repo
                .complete_user_mirror_plan(
                    &request.plan_token,
                    &request.client_request_id,
                    &serde_json::to_string(&result)?,
                )
                .await?;
            Ok(result)
        }
    }
}

/// 按 clientRequestId 读取镜像结果；未完成返回 outcomeUnknown。
///
/// Business Logic（为什么需要这个函数）:
///     UI/重试在不确定窗口必须诚实未知，不得把崩溃中的 apply 标成功。
///
/// Code Logic（这个函数做什么）:
///     查 dest ledger；有 result_json 则反序列化，否则按 plan 拼 OutcomeUnknown。
pub async fn get_user_mirror(
    dest_state: &AppState,
    client_request_id: &str,
) -> Result<UserMirrorResultDto, AppError> {
    if client_request_id.trim().is_empty() {
        return Err(AppError::validation(USER_MIRROR_PREVIEW_REQUIRED));
    }
    let row = dest_state
        .agent_hub_repo
        .get_user_mirror_by_request_id(client_request_id)
        .await?
        .ok_or_else(|| AppError::validation(USER_MIRROR_PREVIEW_REQUIRED))?;
    if let Some(result_json) = row.result_json.as_deref() {
        return serde_json::from_str(result_json).map_err(AppError::from);
    }
    let plan = parse_plan(&row.plan_json)?;
    Ok(outcome_unknown_result(
        &row.plan_token,
        client_request_id,
        &plan,
    ))
}

/// 把 preview DTO 插入 dest `agent_hub_user_mirror_plans`。
async fn persist_plan(dest_state: &AppState, plan: &UserMirrorPlanDto) -> Result<(), AppError> {
    dest_state
        .agent_hub_repo
        .insert_user_mirror_plan(UserMirrorPlanRecord {
            plan_token: plan.plan_token.clone(),
            expires_at: plan.expires_at.clone(),
            plan_json: serde_json::to_string(plan)?,
            client_request_id: None,
            claimed_at: None,
            consumed_at: None,
            result_json: None,
            created_at: Utc::now().to_rfc3339(),
        })
        .await?;
    Ok(())
}

fn parse_plan(plan_json: &str) -> Result<UserMirrorPlanDto, AppError> {
    serde_json::from_str(plan_json).map_err(AppError::from)
}

fn outcome_unknown_result(
    plan_token: &str,
    client_request_id: &str,
    plan: &UserMirrorPlanDto,
) -> UserMirrorResultDto {
    UserMirrorResultDto {
        plan_token: plan_token.to_string(),
        client_request_id: client_request_id.to_string(),
        source_device_id: plan.source_device_id.clone(),
        destination_device_id: plan.destination_device_id.clone(),
        partial: true,
        agents: plan
            .agents
            .iter()
            .map(|agent| UserMirrorAgentResultDto {
                target: agent.target,
                state: UserMirrorItemState::OutcomeUnknown,
                error_code: None,
                message: None,
            })
            .collect(),
    }
}

fn build_result(
    plan_token: &str,
    client_request_id: &str,
    plan: &UserMirrorPlanDto,
    agents: Vec<UserMirrorAgentResultDto>,
) -> UserMirrorResultDto {
    let partial = agents
        .iter()
        .any(|agent| agent.state != UserMirrorItemState::Succeeded);
    UserMirrorResultDto {
        plan_token: plan_token.to_string(),
        client_request_id: client_request_id.to_string(),
        source_device_id: plan.source_device_id.clone(),
        destination_device_id: plan.destination_device_id.clone(),
        partial,
        agents,
    }
}

fn failed_stale_result(
    plan_token: &str,
    client_request_id: &str,
    plan: &UserMirrorPlanDto,
) -> UserMirrorResultDto {
    UserMirrorResultDto {
        plan_token: plan_token.to_string(),
        client_request_id: client_request_id.to_string(),
        source_device_id: plan.source_device_id.clone(),
        destination_device_id: plan.destination_device_id.clone(),
        partial: true,
        agents: plan
            .agents
            .iter()
            .map(|agent| UserMirrorAgentResultDto {
                target: agent.target,
                state: UserMirrorItemState::Failed,
                error_code: Some(USER_MIRROR_STALE.to_string()),
                message: Some(USER_MIRROR_STALE.to_string()),
            })
            .collect(),
    }
}

fn failed_apply_result(
    plan_token: &str,
    client_request_id: &str,
    plan: &UserMirrorPlanDto,
    error: &AppError,
) -> UserMirrorResultDto {
    let message = error.to_string();
    UserMirrorResultDto {
        plan_token: plan_token.to_string(),
        client_request_id: client_request_id.to_string(),
        source_device_id: plan.source_device_id.clone(),
        destination_device_id: plan.destination_device_id.clone(),
        partial: true,
        agents: plan
            .agents
            .iter()
            .map(|agent| UserMirrorAgentResultDto {
                target: agent.target,
                state: UserMirrorItemState::Failed,
                error_code: Some(message.clone()),
                message: Some(message.clone()),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_user_mirror_with_env, get_user_mirror, preview_from_two_inventories,
        preview_user_mirror_with_envs,
    };
    use crate::agent_hub::targets::TargetEnvironment;
    use crate::agent_hub::user_mirror::inventory::build_local_user_mirror_inventory_with_env;
    use crate::agent_hub::user_mirror::models::{
        ApplyUserMirrorRequest, PreviewUserMirrorRequest, UserMirrorDirection, UserMirrorItemState,
    };
    use crate::agent_hub::user_mirror::selection::freeze_user_mirror_selection_with_env;
    use crate::backend::runtime::build_app_state;
    use crate::backend::ui::RecordingBackendUi;
    use crate::config::{install_data_dir_env, install_env_var};
    use crate::state::AppState;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct DualEnv {
        _tmp: tempfile::TempDir,
        _guards: Vec<Box<dyn std::any::Any>>,
        source_state: AppState,
        dest_state: AppState,
        source_home: PathBuf,
        dest_home: PathBuf,
        source_env: TargetEnvironment,
        dest_env: TargetEnvironment,
    }

    /// Business Logic（为什么需要这个函数）:
    ///     service 测试必须隔离源/目标 HOME 与 data_dir，避免改写开发者真实配置。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先构建 source AppState 并释放其 env 锁，再安装 dest HOME/data_dir。
    async fn seed_dual_env() -> DualEnv {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source_home = tmp.path().join("source-home");
        let dest_home = tmp.path().join("dest-home");
        let source_data = tmp.path().join("source-data");
        let dest_data = tmp.path().join("dest-data");
        for path in [&source_home, &dest_home, &source_data, &dest_data] {
            fs::create_dir_all(path).expect("mkdir");
        }
        let source_env = isolated_target_env(&source_home);
        let dest_env = isolated_target_env(&dest_home);

        let source_state = {
            let _data = install_data_dir_env(Some(source_data.to_str().expect("utf8 source data")));
            let _home = install_env_var(
                "HOME",
                Some(source_home.to_str().expect("utf8 source home")),
            );
            let ui = Arc::new(RecordingBackendUi::default());
            build_app_state(ui).await.expect("source state")
        };
        let dest_data_guard =
            install_data_dir_env(Some(dest_data.to_str().expect("utf8 dest data")));
        let dest_home_guard =
            install_env_var("HOME", Some(dest_home.to_str().expect("utf8 dest home")));
        let dest_state = {
            let ui = Arc::new(RecordingBackendUi::default());
            build_app_state(ui).await.expect("dest state")
        };
        DualEnv {
            _tmp: tmp,
            _guards: vec![Box::new(dest_data_guard), Box::new(dest_home_guard)],
            source_state,
            dest_state,
            source_home,
            dest_home,
            source_env,
            dest_env,
        }
    }

    fn isolated_target_env(home: &Path) -> TargetEnvironment {
        TargetEnvironment {
            home: home.to_path_buf(),
            vars: BTreeMap::new(),
            path_entries: Vec::new(),
        }
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, text).expect("write");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本地 Pull 必须把 preview 写入 dest ledger，apply 写盘后同 request 重放且 get 一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     DualEnv 源 CLAUDE.md=FROM-SRC；preview_user_mirror 落库 → freeze → apply →
    ///     dest 文件覆盖；第二次 apply 与 get 返回同一 result。
    #[tokio::test]
    async fn local_pull_preview_apply_replays_and_get_matches() {
        let env = seed_dual_env().await;
        write(
            env.source_home.join(".claude/CLAUDE.md").as_path(),
            "FROM-SRC",
        );
        write(
            env.dest_home.join(".claude/CLAUDE.md").as_path(),
            "OLD-DEST",
        );
        let plan = preview_user_mirror_with_envs(
            &env.dest_state,
            &env.source_state,
            PreviewUserMirrorRequest {
                direction: UserMirrorDirection::Pull,
                source_device_id: Some("src-dev".into()),
                peer_device_ids: Vec::new(),
            },
            &env.source_env,
            &env.dest_env,
        )
        .await
        .expect("preview");
        let stored = env
            .dest_state
            .agent_hub_repo
            .get_user_mirror_plan(&plan.plan_token)
            .await
            .unwrap()
            .expect("plan row");
        assert!(stored.client_request_id.is_none());
        assert!(stored.result_json.is_none());

        let source_inventory = build_local_user_mirror_inventory_with_env(
            &env.source_state,
            "src-dev",
            &env.source_env,
        )
        .await
        .expect("source inventory");
        let built = freeze_user_mirror_selection_with_env(
            &env.source_state,
            &source_inventory,
            &env.source_env,
        )
        .await
        .expect("freeze");
        let request = ApplyUserMirrorRequest {
            plan_token: plan.plan_token.clone(),
            client_request_id: "req-local-1".into(),
        };
        let first = apply_user_mirror_with_env(
            &env.dest_state,
            request.clone(),
            &built.object_bytes,
            &built.item_bindings,
            &env.dest_env,
        )
        .await
        .expect("apply");
        assert!(!first.partial);
        assert_eq!(
            fs::read_to_string(env.dest_home.join(".claude/CLAUDE.md")).unwrap(),
            "FROM-SRC"
        );
        let replay = apply_user_mirror_with_env(
            &env.dest_state,
            request,
            &BTreeMap::new(),
            &[],
            &env.dest_env,
        )
        .await
        .expect("replay");
        assert_eq!(replay.plan_token, first.plan_token);
        assert_eq!(replay.client_request_id, first.client_request_id);
        assert_eq!(replay.partial, first.partial);
        let got = get_user_mirror(&env.dest_state, "req-local-1")
            .await
            .expect("get");
        assert_eq!(got.plan_token, first.plan_token);
        assert_eq!(got.client_request_id, "req-local-1");
        assert_eq!(got.partial, first.partial);
        assert!(got
            .agents
            .iter()
            .all(|agent| agent.state == UserMirrorItemState::Succeeded));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     preview_from_two_inventories 必须在 dest DB 插入可 claim 的 plan 行。
    ///
    /// Code Logic（这个测试做什么）:
    ///     两份空 inventory persist 后按 token 读到行，expires_at 非空。
    #[tokio::test]
    async fn preview_from_two_inventories_inserts_plan_row() {
        let env = seed_dual_env().await;
        let source = build_local_user_mirror_inventory_with_env(
            &env.source_state,
            "src-dev",
            &env.source_env,
        )
        .await
        .unwrap();
        let dest =
            build_local_user_mirror_inventory_with_env(&env.dest_state, "dst-dev", &env.dest_env)
                .await
                .unwrap();
        let plan = preview_from_two_inventories(
            &env.dest_state,
            &source,
            &dest,
            "src-dev",
            "dst-dev",
            UserMirrorDirection::Pull,
        )
        .await
        .unwrap();
        assert!(!plan.plan_token.is_empty());
        let row = env
            .dest_state
            .agent_hub_repo
            .get_user_mirror_plan(&plan.plan_token)
            .await
            .unwrap()
            .expect("inserted");
        assert_eq!(row.plan_token, plan.plan_token);
        assert_eq!(row.expires_at, plan.expires_at);
        assert!(row.client_request_id.is_none());
    }
}
