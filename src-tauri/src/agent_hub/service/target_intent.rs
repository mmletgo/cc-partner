//! agent_hub/service/target_intent — target binding 意图执行
//!
//! Business Logic（为什么需要这个模块）:
//!     presence/enabled/restore/deleteEverywhere 四条命令共享同一转移表与
//!     removal-blocked 预检；禁止各自猜测 tombstone 或绕过 adapter disable 策略。
//!
//! Code Logic（这个模块做什么）:
//!     apply_target_intent 加载 binding/materialization 后经 TargetBindingTransition
//!     写库/调度；包含 removal-blocked 路径计算与 disable 策略落地。

use super::dto::AgentHubAssetSummaryDto;
use super::instruction_document::{load_asset_or_not_found, object_store};
use super::summary::build_summary;
use crate::agent_hub::models::{
    AgentTarget, AssetKind, DesiredPresence, LogicalAsset, Materialization, MaterializationStatus,
    NewMaterialization, NewRevision, NewTargetBinding, RevisionId, RevisionOperation,
    RevisionOriginKind, TargetBinding, TargetBindingIntent, TargetBindingTransition,
    TargetDisableStrategy,
};
use crate::agent_hub::object_store::sha256_hex;
use crate::error::AppError;
use crate::state::AppState;
use std::path::PathBuf;

/// 执行 target binding 意图（presence/enabled/restore/everywhere）。
///
/// Business Logic（为什么需要这个函数）:
///     四条命令共享同一转移表；禁止各自猜测 tombstone。
///
/// Code Logic（这个函数做什么）:
///     加载 binding/materialization → apply_intent → 写库/调度 → summary。
pub(super) async fn apply_target_intent(
    state: &AppState,
    asset_id: &str,
    target: AgentTarget,
    intent: TargetBindingIntent,
) -> Result<AgentHubAssetSummaryDto, AppError> {
    let asset = load_asset_or_not_found(state, asset_id).await?;
    let bindings = state
        .agent_hub_repo
        .list_target_bindings_for_asset(&asset.id)
        .await?;
    let present_count = bindings
        .iter()
        .filter(|b| b.desired_presence == DesiredPresence::Present)
        .count();
    let binding = bindings
        .iter()
        .find(|b| b.target == target)
        .cloned()
        .unwrap_or(TargetBinding {
            id: String::new(),
            asset_id: asset.id.clone(),
            target,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Absent,
            desired_enabled: false,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        });
    let mat = if binding.id.is_empty() {
        None
    } else {
        state
            .agent_hub_repo
            .get_materialization_by_binding(&binding.id)
            .await?
    };
    // Absent / DeleteEverywhere 前先计算 removal-blocked 预览；绑定尚未变更。
    let needs_removal_preflight = matches!(
        intent,
        TargetBindingIntent::SetPresence(DesiredPresence::Absent)
            | TargetBindingIntent::DeleteEverywhere
    );
    let removal_blocked = if needs_removal_preflight {
        match intent {
            TargetBindingIntent::DeleteEverywhere => {
                collect_removal_blocked_for_asset(state, &asset.id).await?
            }
            _ => compute_removal_blocked_paths(mat.as_ref()),
        }
    } else {
        Vec::new()
    };
    let transition = binding.apply_intent(
        intent,
        mat.as_ref().map(|m| m.status),
        asset.policy,
        present_count,
        &removal_blocked,
    );

    match transition {
        TargetBindingTransition::UpdateEnabled {
            desired_enabled,
            disable_strategy,
            schedule_projection,
        } => {
            let updated = state
                .agent_hub_repo
                .upsert_target_binding(NewTargetBinding {
                    asset_id: asset.id.clone(),
                    target,
                    local_scope_mapping_id: binding.local_scope_mapping_id.clone(),
                    checkout_binding_id: binding.checkout_binding_id.clone(),
                    desired_presence: binding.desired_presence,
                    desired_enabled,
                })
                .await?;
            // disable 必须落到 adapter 策略，而非仅 flip DB 位。
            if !desired_enabled {
                apply_disable_strategy(state, &asset, &updated, mat.as_ref(), disable_strategy)
                    .await?;
            } else if let Some(m) = mat.as_ref() {
                // re-enable：Pending 等待投影/激活重新应用
                state
                    .agent_hub_repo
                    .upsert_materialization(NewMaterialization {
                        asset_id: asset.id.clone(),
                        target,
                        target_binding_id: updated.id.clone(),
                        native_path: m.native_path.clone(),
                        last_projected_revision_id: m.last_projected_revision_id.clone(),
                        rendered_hash: m.rendered_hash.clone(),
                        observed_external_hash: m.observed_external_hash.clone(),
                        status: MaterializationStatus::Pending,
                        last_error: Some(format!("enable_strategy:{}", disable_strategy.as_str())),
                    })
                    .await?;
            }
            if schedule_projection {
                schedule_after_binding_change(state, &asset.id).await;
            }
        }
        TargetBindingTransition::UpdatePresence {
            desired_presence,
            schedule_projection,
            ..
        } => {
            let enabled = if desired_presence == DesiredPresence::Absent {
                false
            } else {
                binding.desired_enabled
            };
            state
                .agent_hub_repo
                .upsert_target_binding(NewTargetBinding {
                    asset_id: asset.id.clone(),
                    target,
                    local_scope_mapping_id: binding.local_scope_mapping_id.clone(),
                    checkout_binding_id: binding.checkout_binding_id.clone(),
                    desired_presence,
                    desired_enabled: enabled,
                })
                .await?;
            if schedule_projection {
                schedule_after_binding_change(state, &asset.id).await;
            }
        }
        TargetBindingTransition::RestoreDetached {
            desired_presence,
            schedule_projection,
            clear_detached_status,
        } => {
            let updated = state
                .agent_hub_repo
                .upsert_target_binding(NewTargetBinding {
                    asset_id: asset.id.clone(),
                    target,
                    local_scope_mapping_id: binding.local_scope_mapping_id.clone(),
                    checkout_binding_id: binding.checkout_binding_id.clone(),
                    desired_presence,
                    desired_enabled: true,
                })
                .await?;
            if clear_detached_status {
                // Pending 表示等待投影；禁止保持 Detached 否则 scheduler 会 no-op
                state
                    .agent_hub_repo
                    .upsert_materialization(NewMaterialization {
                        asset_id: asset.id.clone(),
                        target,
                        target_binding_id: updated.id.clone(),
                        native_path: mat.as_ref().and_then(|m| m.native_path.clone()),
                        last_projected_revision_id: mat
                            .as_ref()
                            .and_then(|m| m.last_projected_revision_id.clone()),
                        rendered_hash: mat.as_ref().and_then(|m| m.rendered_hash.clone()),
                        observed_external_hash: None,
                        status: MaterializationStatus::Pending,
                        last_error: None,
                    })
                    .await?;
            }
            if schedule_projection {
                schedule_after_binding_change(state, &asset.id).await;
            }
        }
        TargetBindingTransition::DeleteEverywhere {
            append_canonical_tombstone,
            fan_out_absent,
        } => {
            // 单 write-lease 事务：tombstone + fan-out，避免中途失败留下半状态。
            // Plugin package 必须走 ownership-aware 删除，否则 package-owned component 成孤儿。
            if append_canonical_tombstone || fan_out_absent {
                if asset.kind == AssetKind::Plugin {
                    // ownership tombstone + 全部 binding→Absent 已在同一 repo TX 完成；
                    // 此处仅 durable schedule（可恢复，非权威写）。
                    let store = object_store()?;
                    let delete_result = state
                        .agent_hub_repo
                        .delete_plugin_package_with_ownership(
                            &asset.id,
                            &store,
                            RevisionOriginKind::Ui,
                            state.device_id.as_str().to_string(),
                        )
                        .await?;
                    let mut schedule_ids = vec![asset.id.clone()];
                    for d in &delete_result.component_decisions {
                        if d.decision
                            == crate::agent_hub::plugins::ownership::ComponentDeleteDecision::TombstoneOwned
                        {
                            schedule_ids.push(d.component_asset_id.clone());
                        }
                    }
                    for aid in schedule_ids {
                        schedule_after_binding_change(state, &aid).await;
                    }
                } else {
                    let parents = asset
                        .current_revision_id
                        .clone()
                        .into_iter()
                        .collect::<Vec<_>>();
                    let expected_parent_id = asset.current_revision_id.clone();
                    let fan_out: Vec<NewTargetBinding> = {
                        let all = state
                            .agent_hub_repo
                            .list_target_bindings_for_asset(&asset.id)
                            .await?;
                        if all.is_empty() {
                            vec![NewTargetBinding {
                                asset_id: asset.id.clone(),
                                target,
                                local_scope_mapping_id: None,
                                checkout_binding_id: None,
                                desired_presence: DesiredPresence::Absent,
                                desired_enabled: false,
                            }]
                        } else {
                            all.into_iter()
                                .map(|b| NewTargetBinding {
                                    asset_id: asset.id.clone(),
                                    target: b.target,
                                    local_scope_mapping_id: b.local_scope_mapping_id,
                                    checkout_binding_id: b.checkout_binding_id,
                                    desired_presence: DesiredPresence::Absent,
                                    desired_enabled: false,
                                })
                                .collect()
                        }
                    };
                    state
                        .agent_hub_repo
                        .delete_asset_everywhere_atomic(
                            &asset.id,
                            NewRevision {
                                id: RevisionId::new_v7(),
                                asset_lineage_id: asset.id.clone(),
                                parents,
                                operation: RevisionOperation::Delete,
                                origin_kind: RevisionOriginKind::Ui,
                                origin_target: None,
                                origin_replica_id: state.device_id.as_str().to_string(),
                                payload_hash: None,
                                tree_manifest_hash: None,
                                created_at: chrono::Utc::now().to_rfc3339(),
                                expected_parent_id,
                            },
                            fan_out,
                        )
                        .await?;
                    schedule_after_binding_change(state, &asset.id).await;
                }
            }
        }
        TargetBindingTransition::RejectLastTargetOnlyRequiresEverywhere { code } => {
            return Err(AppError::validation(code));
        }
        TargetBindingTransition::RejectRemovalBlocked {
            code,
            preview_paths,
        } => {
            return Err(AppError::validation(format!(
                "{code}:{}",
                preview_paths.join(",")
            )));
        }
    }

    let asset = load_asset_or_not_found(state, asset_id).await?;
    build_summary(state, &asset).await
}

/// ensure enabled + schedule projections（best-effort）。
async fn schedule_after_binding_change(state: &AppState, asset_id: &str) {
    if let Err(e) = crate::agent_hub::projection_ops::ensure_agent_hub_enabled(state).await {
        tracing::warn!(error = %e, "agent_hub binding change ensure enabled failed");
    }
    if let Err(e) =
        crate::agent_hub::projection_ops::schedule_asset_projections(state, asset_id).await
    {
        tracing::warn!(
            asset_id = %asset_id,
            error = %e,
            "agent_hub binding change schedule projections failed"
        );
    }
}

/// 计算单个 materialization 的 removal-blocked 路径预览。
///
/// Business Logic（为什么需要这个函数）:
///     Absent 前必须返回精确 preview；外部改动/未知子项不得先把 binding 标 Absent。
///
/// Code Logic（这个函数做什么）:
///     文件：current hash 与 rendered/observed 均不一致 → 路径入 preview；
///     目录：未知子项或 managed 子路径 hash 漂移 → 路径入 preview；
///     路径不存在或 hash 命中 managed 集合 → 可删（空 preview）。
pub(crate) fn compute_removal_blocked_paths(mat: Option<&Materialization>) -> Vec<String> {
    let Some(mat) = mat else {
        return Vec::new();
    };
    let Some(path_str) = mat.native_path.as_deref().filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let path = PathBuf::from(path_str);
    if !path.exists() {
        return Vec::new();
    }
    let managed: Vec<String> = [
        mat.rendered_hash.clone(),
        mat.observed_external_hash.clone(),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .collect();
    if path.is_file() {
        return match std::fs::read(&path) {
            Ok(bytes) => {
                let current = sha256_hex(&bytes);
                if managed.iter().any(|h| h == &current) {
                    Vec::new()
                } else {
                    vec![path_str.to_string()]
                }
            }
            Err(_) => vec![path_str.to_string()],
        };
    }
    if path.is_dir() {
        let mut blocked = Vec::new();
        let entries = match std::fs::read_dir(&path) {
            Ok(e) => e,
            Err(_) => return vec![path_str.to_string()],
        };
        for entry in entries.flatten() {
            let child = entry.path();
            if !child.is_file() {
                // 未知子目录/非文件一律阻塞，禁止递归删
                blocked.push(child.to_string_lossy().into_owned());
                continue;
            }
            match std::fs::read(&child) {
                Ok(bytes) => {
                    let current = sha256_hex(&bytes);
                    if !managed.iter().any(|h| h == &current) {
                        blocked.push(child.to_string_lossy().into_owned());
                    }
                }
                Err(_) => blocked.push(child.to_string_lossy().into_owned()),
            }
        }
        return blocked;
    }
    vec![path_str.to_string()]
}

/// 汇总资产全部 binding 的 removal-blocked 路径（DeleteEverywhere 预检）。
///
/// Business Logic: everywhere 任一条路径被外部改过 → 整次拒绝并返回完整 preview。
/// Code Logic: list bindings + materializations → 合并 compute_removal_blocked_paths。
async fn collect_removal_blocked_for_asset(
    state: &AppState,
    asset_id: &str,
) -> Result<Vec<String>, AppError> {
    let bindings = state
        .agent_hub_repo
        .list_target_bindings_for_asset(asset_id)
        .await?;
    let mut out = Vec::new();
    for b in bindings {
        let mat = state
            .agent_hub_repo
            .get_materialization_by_binding(&b.id)
            .await?;
        out.extend(compute_removal_blocked_paths(mat.as_ref()));
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// 执行 adapter 声明的 disable 策略。
///
/// Business Logic（为什么需要这个函数）:
///     desiredEnabled=false 不得只改 DB 位；package 资产需 remove-with-binding-retained，
///     指令资产通过 schedule Present+disabled 投影（内容可保留但 desired 已禁用）。
///
/// Code Logic（这个函数做什么）:
///     更新 materialization 为 Pending + 策略 token；package 路径若存在则 best-effort 标记
///     deactivate 作业意图（真实 CLI uninstall 由后续 activator/runtime 消费 binding）。
async fn apply_disable_strategy(
    state: &AppState,
    asset: &LogicalAsset,
    binding: &TargetBinding,
    mat: Option<&Materialization>,
    strategy: TargetDisableStrategy,
) -> Result<(), AppError> {
    let is_package = matches!(
        asset.kind,
        AssetKind::Skill
            | AssetKind::Command
            | AssetKind::Agent
            | AssetKind::Plugin
            | AssetKind::Mcp
    );
    let strategy_token = strategy.as_str();
    // 无论是否已有 materialization，都写入 Pending + 策略 token，
    // 避免仅 flip desired_enabled 而无可观测 deactivation 意图。
    let (native_path, last_rev, rendered_hash, observed) = match mat {
        Some(m) => (
            m.native_path.clone(),
            m.last_projected_revision_id.clone(),
            m.rendered_hash.clone(),
            m.observed_external_hash.clone(),
        ),
        None => (None, None, None, None),
    };
    let _ = is_package; // 策略 token 区分 package/instruction 由 strategy.as_str
    state
        .agent_hub_repo
        .upsert_materialization(NewMaterialization {
            asset_id: asset.id.clone(),
            target: binding.target,
            target_binding_id: binding.id.clone(),
            native_path,
            last_projected_revision_id: last_rev,
            rendered_hash,
            observed_external_hash: observed,
            status: MaterializationStatus::Pending,
            last_error: Some(format!("disable_strategy:{strategy_token}")),
        })
        .await?;
    // 调度 deactivation job：package 走 projection_ops package 路径（若有）；
    // 当前 instruction schedule 已覆盖 Present+disabled；package 调度 best-effort。
    if is_package {
        if let Err(e) =
            crate::agent_hub::projection_ops::schedule_package_deactivation(state, &asset.id).await
        {
            tracing::warn!(
                asset_id = %asset.id,
                target = %binding.target.as_str(),
                error = %e,
                "agent_hub disable strategy schedule deactivation failed (best-effort)"
            );
        }
    }
    Ok(())
}
