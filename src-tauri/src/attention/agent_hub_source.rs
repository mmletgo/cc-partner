//! attention/agent_hub_source.rs — Agent Hub conflict / blocked projection 的 Attention 投影。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户需要在 Inbox 看到未解决的 Agent Hub conflict 与被阻塞的投影，并只导航到资产权威界面；
//!     解决/解除阻塞后条目自动消失；不新增 Attention 持久表。
//!
//! Code Logic（这个模块做什么）:
//!     从 AgentHubRepo.list_unresolved_conflicts / list_blocked_materializations 实时派生；
//!     稳定 ID `agent-hub:conflict:<id>` / `agent-hub:blocked:<materializationId|asset:target>`；
//!     纯投影 helper 供单测与 collect 共用。

use crate::agent_hub::models::{AgentHubConflict, Materialization, MaterializationStatus};
use crate::agent_hub::replication::sender::{
    list_failed_source_push_targets, SourcePushTargetRow, SOURCE_PUSH_KIND_USER_MIRROR,
};
use crate::attention::models::{
    AttentionCategory, AttentionDeviceRef, AttentionFreshness, AttentionItemDto,
    AttentionSourceKind, AttentionTargetDto,
};
use crate::attention::source::AttentionSource;
use crate::error::AppError;
use crate::state::AppState;
use futures_util::future::BoxFuture;

/// Agent Hub Attention 投影源（v1 + v2 均注册）。
///
/// Business Logic（为什么需要这个结构体）:
///     桌面 Inbox v1 也要展示 Hub conflict/blocked，聚合器通过统一 AttentionSource 收集。
///
/// Code Logic（这个结构体做什么）:
///     无状态；collect 读 repo 未解决 conflict 与 blocked materialization。
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentHubAttentionSource;

impl AttentionSource for AgentHubAttentionSource {
    /// Business Logic（为什么需要这个函数）:
    ///     Inbox 需要当前未解决 conflict 与阻塞投影，解决后自动从列表消失。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 collect_agent_hub_attention_items。
    fn collect<'a>(
        &'a self,
        state: &'a AppState,
    ) -> BoxFuture<'a, Result<Vec<AttentionItemDto>, AppError>> {
        Box::pin(async move { collect_agent_hub_attention_items(state).await })
    }
}

/// Business Logic（为什么需要这个函数）:
///     Tauri/Mobile v1 与 v2 共用同一 Agent Hub 投影入口。
///
/// Code Logic（这个函数做什么）:
///     list unresolved conflicts + blocked materializations + failed source push → 纯投影；
///     **任一 list 失败整源失败**（fail-closed，禁止 partial Live 快照）。
pub async fn collect_agent_hub_attention_items(
    state: &AppState,
) -> Result<Vec<AttentionItemDto>, AppError> {
    let conflicts = state.agent_hub_repo.list_unresolved_conflicts().await?;
    let blocked = state.agent_hub_repo.list_blocked_materializations().await?;
    // 源侧 multi-target push 失败：peer label + counts + error code，永不 payload。
    // 与 conflicts/blocked 一致：错误必须上抛，禁止 unwrap_or_default 吞掉。
    let push_failed = list_failed_source_push_targets(state).await?;
    collect_agent_hub_attention_from_parts(Ok(conflicts), Ok(blocked), Ok(push_failed))
}

/// Business Logic（为什么需要这个函数）:
///     Attention 源契约：任一子列表失败必须失败整源，不能静默丢 push-failed 行。
///
/// Code Logic（这个函数做什么）:
///     三路 Result 任一 Err 立即返回；成功则投影 conflicts/blocked/push-failed。
///     单测可直接注入 list_failed Err 证明 fail-closed（无完整 AppState）。
pub fn collect_agent_hub_attention_from_parts(
    conflicts: Result<Vec<AgentHubConflict>, AppError>,
    blocked: Result<Vec<Materialization>, AppError>,
    push_failed: Result<Vec<SourcePushTargetRow>, AppError>,
) -> Result<Vec<AttentionItemDto>, AppError> {
    let conflicts = conflicts?;
    let blocked = blocked?;
    let push_failed = push_failed?;
    let mut items = project_agent_hub_rows(&conflicts, &blocked);
    items.extend(project_source_push_failures(&push_failed));
    Ok(items)
}

/// Business Logic（为什么需要这个函数）:
///     纯函数便于单测稳定 ID 与 category，不依赖完整 AppState。
///
/// Code Logic（这个函数做什么）:
///     conflict 全部投影；materialization 仅 status=Blocked。
pub fn project_agent_hub_rows(
    conflicts: &[AgentHubConflict],
    materializations: &[Materialization],
) -> Vec<AttentionItemDto> {
    let mut items = Vec::with_capacity(conflicts.len() + materializations.len());
    for conflict in conflicts {
        if conflict.resolved {
            continue;
        }
        items.push(project_conflict_item(conflict));
    }
    for mat in materializations {
        if mat.status != MaterializationStatus::Blocked {
            continue;
        }
        items.push(project_blocked_item(mat));
    }
    items
}

/// Business Logic（为什么需要这个函数）:
///     源侧 push / 用户级镜像失败需进 Inbox，只展示 peer 标签与错误码，禁止 payload。
///
/// Code Logic（这个函数做什么）:
///     `kind=user_mirror` → `agent-hub:mirror-failed:<requestId>:<peerId>`；
///     否则 `agent-hub:push-failed:<requestId>:<peerId>`。
pub fn project_source_push_failures(rows: &[SourcePushTargetRow]) -> Vec<AttentionItemDto> {
    rows.iter().map(project_source_push_failure_item).collect()
}

/// Business Logic（为什么需要这个函数）:
///     单条失败 target 映射为 blocked 条目。
///
/// Code Logic（这个函数做什么）:
///     镜像 summary 仅 peer label + error_code；旧 push 另含 missing/transferred 计数。
pub fn project_source_push_failure_item(row: &SourcePushTargetRow) -> AttentionItemDto {
    let code = row
        .error_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");
    let is_mirror = row.kind.trim() == SOURCE_PUSH_KIND_USER_MIRROR;
    // 永不包含 payload / envelope / object bytes
    let (id, title, summary) = if is_mirror {
        (
            format!(
                "agent-hub:mirror-failed:{}:{}",
                row.request_id, row.peer_device_id
            ),
            "Agent Hub 用户级镜像失败".to_string(),
            format!("镜像到 {} 失败（code={}）", row.peer_label, code),
        )
    } else {
        (
            format!(
                "agent-hub:push-failed:{}:{}",
                row.request_id, row.peer_device_id
            ),
            "Agent Hub 局域网推送失败".to_string(),
            format!(
                "推送到 {} 失败（missing={} transferred={} code={}）",
                row.peer_label, row.missing_object_count, row.transferred_object_count, code
            ),
        )
    };
    AttentionItemDto {
        id,
        category: AttentionCategory::Blocked,
        // 复用 projection blocked kind（v1+v2 均展示）；导航到 Agent Hub 资产首页。
        source_kind: AttentionSourceKind::AgentHubProjectionBlocked,
        title,
        summary,
        updated_at: row.updated_at.clone(),
        freshness: AttentionFreshness::Live,
        cached_at: None,
        project: None,
        device: Some(AttentionDeviceRef {
            id: row.peer_device_id.clone(),
            name: row.peer_label.clone(),
        }),
        target: AttentionTargetDto::AgentHubAsset {
            asset_id: String::new(),
            conflict_id: None,
        },
        read_at: None,
    }
}

/// Business Logic（为什么需要这个函数）:
///     单条未解决 conflict 映射为 decision 条目，导航到资产。
///
/// Code Logic（这个函数做什么）:
///     稳定 ID `agent-hub:conflict:<id>`；target 带 conflictId。
pub fn project_conflict_item(conflict: &AgentHubConflict) -> AttentionItemDto {
    AttentionItemDto {
        id: format!("agent-hub:conflict:{}", conflict.id),
        category: AttentionCategory::Decision,
        source_kind: AttentionSourceKind::AgentHubConflict,
        title: "Agent Hub 冲突待解决".to_string(),
        summary: "有指令或资产与外部编辑冲突，需要你决策".to_string(),
        updated_at: conflict
            .resolved_at
            .clone()
            .unwrap_or_else(|| conflict.created_at.clone()),
        freshness: AttentionFreshness::Live,
        cached_at: None,
        project: None,
        device: None,
        target: AttentionTargetDto::AgentHubAsset {
            asset_id: conflict.asset_id.clone(),
            conflict_id: Some(conflict.id.clone()),
        },
        read_at: None,
    }
}

/// Business Logic（为什么需要这个函数）:
///     被阻塞的 materialization 映射为 blocked 条目，导航到资产。
///
/// Code Logic（这个函数做什么）:
///     优先 materialization id；缺失时用 assetId:target；conflictId=None。
pub fn project_blocked_item(mat: &Materialization) -> AttentionItemDto {
    let id_suffix = if mat.id.trim().is_empty() {
        format!("{}:{}", mat.asset_id, mat.target.as_str())
    } else {
        mat.id.clone()
    };
    let summary = match mat
        .last_error
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(err) => format!("投影被阻塞：{err}"),
        None => "Agent Hub 投影被阻塞，请到资产页查看".to_string(),
    };
    AttentionItemDto {
        id: format!("agent-hub:blocked:{id_suffix}"),
        category: AttentionCategory::Blocked,
        source_kind: AttentionSourceKind::AgentHubProjectionBlocked,
        title: "Agent Hub 投影被阻塞".to_string(),
        summary,
        updated_at: mat.updated_at.clone(),
        freshness: AttentionFreshness::Live,
        cached_at: None,
        project: None,
        device: None,
        target: AttentionTargetDto::AgentHubAsset {
            asset_id: mat.asset_id.clone(),
            conflict_id: None,
        },
        read_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{
        AgentTarget, AssetKind, AssetPolicy, MaterializationStatus, NewLogicalAsset,
        NewMaterialization, NewScopeNode, ScopeKind,
    };
    use crate::storage::AgentHubRepo;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// Business Logic: 构造未解决 conflict 样例。
    /// Code Logic: resolved=false。
    fn sample_conflict(id: &str, asset_id: &str) -> AgentHubConflict {
        AgentHubConflict {
            id: id.to_string(),
            asset_id: asset_id.to_string(),
            target: None,
            base_revision_id: None,
            hub_revision_id: None,
            external_revision_id: None,
            detail_json: r#"{"kind":"canonical"}"#.to_string(),
            resolved: false,
            created_at: "2026-07-29T00:00:00Z".to_string(),
            resolved_at: None,
        }
    }

    /// Business Logic: 构造 blocked materialization 样例。
    /// Code Logic: status=Blocked。
    fn sample_mat(id: &str, asset_id: &str, status: MaterializationStatus) -> Materialization {
        Materialization {
            id: id.to_string(),
            asset_id: asset_id.to_string(),
            target: AgentTarget::Claude,
            target_binding_id: "bind-1".to_string(),
            native_path: Some("/tmp/CLAUDE.md".to_string()),
            last_projected_revision_id: None,
            rendered_hash: None,
            observed_external_hash: None,
            status,
            last_error: Some("前置条件未满足".to_string()),
            created_at: "2026-07-29T00:00:00Z".to_string(),
            updated_at: "2026-07-29T01:00:00Z".to_string(),
        }
    }

    #[test]
    fn project_conflict_uses_stable_id_and_decision_category() {
        let item = project_conflict_item(&sample_conflict("c1", "asset-1"));
        assert_eq!(item.id, "agent-hub:conflict:c1");
        assert_eq!(item.category, AttentionCategory::Decision);
        assert_eq!(item.source_kind, AttentionSourceKind::AgentHubConflict);
        assert!(!item.source_kind.is_v2_only());
        assert_eq!(item.freshness, AttentionFreshness::Live);
        assert!(matches!(
            item.target,
            AttentionTargetDto::AgentHubAsset {
                ref asset_id,
                ref conflict_id,
            } if asset_id == "asset-1" && conflict_id.as_deref() == Some("c1")
        ));
    }

    #[test]
    fn project_blocked_uses_stable_id_and_blocked_category() {
        let item =
            project_blocked_item(&sample_mat("m1", "asset-2", MaterializationStatus::Blocked));
        assert_eq!(item.id, "agent-hub:blocked:m1");
        assert_eq!(item.category, AttentionCategory::Blocked);
        assert_eq!(
            item.source_kind,
            AttentionSourceKind::AgentHubProjectionBlocked
        );
        assert!(!item.source_kind.is_v2_only());
        assert!(item.summary.contains("前置条件未满足"));
        assert!(matches!(
            item.target,
            AttentionTargetDto::AgentHubAsset {
                ref asset_id,
                ref conflict_id,
            } if asset_id == "asset-2" && conflict_id.is_none()
        ));
    }

    #[test]
    fn project_rows_skips_resolved_and_non_blocked() {
        let mut resolved = sample_conflict("c-resolved", "a");
        resolved.resolved = true;
        let items = project_agent_hub_rows(
            &[sample_conflict("c-open", "a"), resolved],
            &[
                sample_mat("m-blocked", "a", MaterializationStatus::Blocked),
                sample_mat("m-synced", "a", MaterializationStatus::Synced),
            ],
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "agent-hub:conflict:c-open");
        assert_eq!(items[1].id, "agent-hub:blocked:m-blocked");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     真实 repo 插入 unresolved conflict + blocked materialization 后投影必须命中稳定 ID。
    ///
    /// Code Logic（这个测试做什么）:
    ///     ensure_schema → scope/asset → insert_conflict + upsert_materialization blocked →
    ///     list + project_agent_hub_rows。
    #[tokio::test]
    async fn repo_conflict_and_blocked_project_to_attention_ids() {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        let repo = AgentHubRepo::new(pool);

        let scope = repo
            .insert_scope(NewScopeNode {
                id: Some("scope-user".into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "attention-test".into(),
                display_name: "attention test".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();

        let conflict_id = repo
            .insert_conflict(&asset.id, None, r#"{"kind":"canonical"}"#)
            .await
            .unwrap();
        let mat = repo
            .upsert_materialization(NewMaterialization {
                asset_id: asset.id.clone(),
                target: AgentTarget::Claude,
                target_binding_id: "bind-attention".into(),
                native_path: Some("/tmp/attention-CLAUDE.md".into()),
                last_projected_revision_id: None,
                rendered_hash: None,
                observed_external_hash: None,
                status: MaterializationStatus::Blocked,
                last_error: Some("blocked for test".into()),
            })
            .await
            .unwrap();

        let conflicts = repo.list_unresolved_conflicts().await.unwrap();
        let blocked = repo.list_blocked_materializations().await.unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(blocked.len(), 1);

        let items = project_agent_hub_rows(&conflicts, &blocked);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, format!("agent-hub:conflict:{conflict_id}"));
        assert_eq!(items[1].id, format!("agent-hub:blocked:{}", mat.id));
        assert_eq!(items[0].source_kind, AttentionSourceKind::AgentHubConflict);
        assert_eq!(
            items[1].source_kind,
            AttentionSourceKind::AgentHubProjectionBlocked
        );
    }

    /// Business Logic: push 失败 Attention 含 peer label/counts/code，永不 payload。
    /// Code Logic: project_source_push_failure_item 稳定 ID 与 summary。
    #[test]
    fn project_source_push_failure_uses_label_counts_and_code_never_payload() {
        use crate::agent_hub::replication::sender::{SourcePushTargetRow, TargetPushStatus};
        let row = SourcePushTargetRow {
            request_id: "req-1".into(),
            peer_device_id: "peer-x".into(),
            peer_label: "Mac Mini".into(),
            client_request_id: "req-1:peer-x".into(),
            status: TargetPushStatus::Failed,
            retryable: true,
            error_code: Some("transport_network".into()),
            transfer_id: None,
            missing_object_count: 3,
            transferred_object_count: 1,
            kind: "push".into(),
            created_at: "2026-07-29T00:00:00Z".into(),
            updated_at: "2026-07-29T01:00:00Z".into(),
        };
        let item = project_source_push_failure_item(&row);
        assert_eq!(item.id, "agent-hub:push-failed:req-1:peer-x");
        assert_eq!(item.category, AttentionCategory::Blocked);
        assert!(item.summary.contains("Mac Mini"));
        assert!(item.summary.contains("missing=3"));
        assert!(item.summary.contains("transferred=1"));
        assert!(item.summary.contains("transport_network"));
        // 永不出现 payload/envelope/凭据
        assert!(!item.summary.to_lowercase().contains("payload"));
        assert!(!item.summary.to_lowercase().contains("envelope"));
        assert!(!item.summary.contains("token="));
        assert!(item.device.as_ref().unwrap().name == "Mac Mini");
    }

    /// Business Logic: 用户级镜像失败 Attention 仅 peer label + error code，无 payload。
    /// Code Logic: 稳定 ID `agent-hub:mirror-failed:<requestId>:<peerId>`。
    #[test]
    fn project_user_mirror_failure_uses_label_and_code_never_payload() {
        use crate::agent_hub::replication::sender::{
            SourcePushTargetRow, TargetPushStatus, SOURCE_PUSH_KIND_USER_MIRROR,
        };
        let row = SourcePushTargetRow {
            request_id: "req-m".into(),
            peer_device_id: "peer-y".into(),
            peer_label: "Laptop".into(),
            client_request_id: "req-m:peer-y".into(),
            status: TargetPushStatus::Failed,
            retryable: false,
            error_code: Some("USER_MIRROR_CAPABILITY_UNSUPPORTED".into()),
            transfer_id: None,
            missing_object_count: 9,
            transferred_object_count: 4,
            kind: SOURCE_PUSH_KIND_USER_MIRROR.into(),
            created_at: "2026-08-23T00:00:00Z".into(),
            updated_at: "2026-08-23T01:00:00Z".into(),
        };
        let item = project_source_push_failure_item(&row);
        assert_eq!(item.id, "agent-hub:mirror-failed:req-m:peer-y");
        assert_eq!(item.category, AttentionCategory::Blocked);
        assert!(item.summary.contains("Laptop"));
        assert!(item.summary.contains("USER_MIRROR_CAPABILITY_UNSUPPORTED"));
        assert!(!item.summary.contains("missing="));
        assert!(!item.summary.contains("transferred="));
        assert!(!item.summary.to_lowercase().contains("payload"));
        assert!(!item.summary.to_lowercase().contains("envelope"));
        assert!(!item.summary.contains("token="));
        assert_eq!(item.device.as_ref().unwrap().name, "Laptop");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     list_failed_source_push_targets 失败不得 silent empty；整源必须 fail-closed，
    ///     否则 Inbox 会漏掉阻塞性 push 失败并伪装 Live 完整。
    ///
    /// Code Logic（这个测试做什么）:
    ///     conflicts/blocked 成功而 push_failed=Err 时 compose 返回 Err；
    ///     不得产出 partial items。
    #[test]
    fn list_failed_source_push_error_fails_whole_agent_hub_source() {
        let err = collect_agent_hub_attention_from_parts(
            Ok(vec![sample_conflict("c-open", "asset-1")]),
            Ok(vec![]),
            Err(AppError::generic("list_failed boom".to_string())),
        )
        .expect_err("push-failed list error must fail whole source");
        assert!(err.to_string().contains("list_failed boom"), "err={err}");
        // 对照：三路皆 Ok 才产出投影
        let ok = collect_agent_hub_attention_from_parts(
            Ok(vec![sample_conflict("c-open", "asset-1")]),
            Ok(vec![]),
            Ok(vec![]),
        )
        .expect("all ok");
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].id, "agent-hub:conflict:c-open");
    }
}
