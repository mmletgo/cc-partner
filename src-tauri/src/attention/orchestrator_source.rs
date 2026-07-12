//! attention/orchestrator_source.rs — Orchestrator 任务/mirror/outbox 的 Attention 投影。
//!
//! Business Logic（为什么需要这个模块）:
//!     全局 Inbox 需要把本机 Human Review/Blocked 任务、远端 mirror 的同类任务，以及失败的
//!     remote outbox 投影为可导航 Attention 条目，且不能消费会丢失 freshness 的 task-view DTO。
//!
//! Code Logic（这个模块做什么）:
//!     纯函数投影本地任务与 failed outbox；远端 shortcut 复用 online-sync / network-fallback
//!     控制流刷新 mirror，并用 `buffer_unordered(4)` 限制并发；稳定 ID 使用 Shared Contracts。

use crate::attention::models::{
    AttentionCategory, AttentionDeviceRef, AttentionFreshness, AttentionItemDto,
    AttentionProjectKind, AttentionProjectRef, AttentionSourceKind, AttentionTargetDto,
};
use crate::attention::source::AttentionSource;
use crate::error::AppError;
use crate::orchestrator::models::{
    OrchestratorRunState, OrchestratorTaskDto, OrchestratorTaskRow, OrchestratorTaskStatus,
    OrchestratorWorkflowState, SplitTaskState,
};
use crate::orchestrator::outbox::{
    is_remote_network_error, sync_remote_task_mirror_for_project, OrchestratorRemoteOutboxRow,
    RemoteMirrorTask, RemoteOutboxStatus,
};
use crate::state::AppState;
use crate::workbench::models::WorkbenchProjectRow;
use crate::workbench::remote_ids::remote_entity_id;
use futures_util::future::BoxFuture;
use futures_util::{stream, StreamExt};
use std::future::Future;

/// 远端 mirror 刷新并发上限。
const REMOTE_REFRESH_CONCURRENCY: usize = 4;

/// Orchestrator Attention 投影源。
///
/// Business Logic（为什么需要这个结构体）:
///     聚合器通过统一 AttentionSource 接口收集 Orchestrator 相关待办，不在页面散落业务判断。
///
/// Code Logic（这个结构体做什么）:
///     无状态 source；collect 读取 AppState 中的 workbench 项目、任务、mirror 与 outbox。
#[derive(Debug, Default, Clone, Copy)]
pub struct OrchestratorAttentionSource;

impl AttentionSource for OrchestratorAttentionSource {
    /// Business Logic（为什么需要这个函数）:
    ///     聚合器需要一次性拿到本机任务、远端 mirror 与 failed outbox 的 Attention 投影。
    ///
    /// Code Logic（这个函数做什么）:
    ///     投影全部本机任务；对每个 remote shortcut 在线刷新 mirror（网络失败回退缓存）；
    ///     再投影 active remote shortcut 上的 failed outbox；任一非网络仓库/解析错误使整次 source 失败。
    fn collect<'a>(
        &'a self,
        state: &'a AppState,
    ) -> BoxFuture<'a, Result<Vec<AttentionItemDto>, AppError>> {
        Box::pin(async move { collect_orchestrator_attention_items(state).await })
    }
}

/// Business Logic（为什么需要这个函数）:
///     桌面与 Mobile 共用同一 Orchestrator 投影入口，避免 command/route 各自拼装。
///
/// Code Logic（这个函数做什么）:
///     收集 local 任务、remote mirror（限并发 4）、failed outbox，并合并返回。
pub async fn collect_orchestrator_attention_items(
    state: &AppState,
) -> Result<Vec<AttentionItemDto>, AppError> {
    let projects = state.workbench_project_repo.list().await?;
    let mut items = Vec::new();

    // 本机任务必须与当前有效 Workbench 本机项目 ID 求交；
    // 零本机项目时绝不回退全局历史任务，避免投影不可导航的孤儿条目。
    let local_projects: Vec<&WorkbenchProjectRow> = projects
        .iter()
        .filter(|project| project.kind != "remote")
        .collect();
    for project in local_projects {
        let rows = state
            .orchestrator_repo
            .list_tasks(Some(&project.id))
            .await?;
        let project_ref = AttentionProjectRef {
            id: project.id.clone(),
            name: project.name.clone(),
            kind: AttentionProjectKind::Local,
        };
        for row in rows {
            if let Some(item) = project_local_task_row(&row, Some(project_ref.clone())) {
                items.push(item);
            }
        }
    }

    let remote_projects: Vec<WorkbenchProjectRow> = projects
        .iter()
        .filter(|project| project.kind == "remote")
        .cloned()
        .collect();

    let remote_items =
        collect_remote_project_attention_items(state, &remote_projects, REMOTE_REFRESH_CONCURRENCY)
            .await?;
    items.extend(remote_items);

    let outbox_items = collect_failed_outbox_attention_items(state, &remote_projects).await?;
    items.extend(outbox_items);

    Ok(items)
}

/// Business Logic（为什么需要这个函数）:
///     多个 remote shortcut 不能无限并发扇出 HTTP 刷新，否则会压垮局域网设备。
///
/// Code Logic（这个函数做什么）:
///     用 `buffer_unordered(limit)` 并发刷新各 remote project 的 mirror 并投影；网络失败读缓存。
async fn collect_remote_project_attention_items(
    state: &AppState,
    remote_projects: &[WorkbenchProjectRow],
    concurrency: usize,
) -> Result<Vec<AttentionItemDto>, AppError> {
    if remote_projects.is_empty() {
        return Ok(Vec::new());
    }
    let limit = concurrency.max(1);
    let results: Vec<Result<Vec<AttentionItemDto>, AppError>> =
        stream::iter(remote_projects.iter().cloned())
            .map(|project| async move {
                collect_one_remote_project_attention_items(state, &project).await
            })
            .buffer_unordered(limit)
            .collect()
            .await;

    let mut items = Vec::new();
    for result in results {
        items.extend(result?);
    }
    Ok(items)
}

/// Business Logic（为什么需要这个函数）:
///     单个 remote shortcut 需要 online 刷新 mirror；离线时用最近缓存并标记 freshness=cached。
///
/// Code Logic（这个函数做什么）:
///     复用 `sync_remote_task_mirror_for_project`；仅网络类错误回退 path mirror，其它错误上抛。
async fn collect_one_remote_project_attention_items(
    state: &AppState,
    project: &WorkbenchProjectRow,
) -> Result<Vec<AttentionItemDto>, AppError> {
    let (mirrors, freshness, cached_at) =
        match sync_remote_task_mirror_for_project(state, project, None).await {
            Ok(mirrors) => (mirrors, AttentionFreshness::Live, None),
            Err(err) if is_remote_network_error(&err) => {
                let mirrors = state
                    .orchestrator_repo
                    .list_remote_task_mirrors_for_project_path(&project.device_id, &project.path)
                    .await?;
                let cached_at = mirrors
                    .iter()
                    .map(|mirror| mirror.last_synced_at.as_str())
                    .max()
                    .map(str::to_string);
                (mirrors, AttentionFreshness::Cached, cached_at)
            }
            Err(err) => return Err(err),
        };

    project_remote_mirrors(mirrors, project, freshness, cached_at)
}

/// Business Logic（为什么需要这个函数）:
///     failed outbox 只有在对应 remote shortcut 仍活跃时才进入 Inbox，orphan 行不制造待办。
///
/// Code Logic（这个函数做什么）:
///     读取每个 remote shortcut 的 active outbox，仅投影 status=failed 的行。
async fn collect_failed_outbox_attention_items(
    state: &AppState,
    remote_projects: &[WorkbenchProjectRow],
) -> Result<Vec<AttentionItemDto>, AppError> {
    let mut items = Vec::new();
    for project in remote_projects {
        let rows = state
            .orchestrator_repo
            .list_remote_outbox_items_for_project_path(&project.device_id, &project.path)
            .await?;
        for row in rows {
            if let Some(item) = project_failed_outbox_row(&row, project) {
                items.push(item);
            }
        }
    }
    Ok(items)
}

/// Business Logic（为什么需要这个函数）:
///     本机任务投影需要稳定 ID、权威 updated_at，以及 Human Review / Blocked 分类。
///
/// Code Logic（这个函数做什么）:
///     先应用 legacy blocked 映射，再按 HumanReview→decision、Blocked→blocked 投影；其余状态排除。
pub(crate) fn project_local_task_row(
    task: &OrchestratorTaskRow,
    project: Option<AttentionProjectRef>,
) -> Option<AttentionItemDto> {
    let (category, source_kind, id_prefix) =
        attention_kind_for_task(task.status, task.workflow_state, task.run_state)?;
    let task_id = task.id.clone();
    Some(AttentionItemDto {
        id: format!("{id_prefix}:{task_id}"),
        category,
        source_kind,
        title: task.title.clone(),
        summary: task_summary(task.blocked_reason.as_deref(), &task.goal, category),
        updated_at: task.updated_at.clone(),
        freshness: AttentionFreshness::Live,
        cached_at: None,
        project,
        device: None,
        target: AttentionTargetDto::OrchestratorTask {
            project_id: task.project_id.clone(),
            task_id,
        },
    })
}

/// Business Logic（为什么需要这个函数）:
///     远端 mirror payload 需要与本机任务相同的进入条件，但 taskId 必须使用 remote 包装 ID。
///
/// Code Logic（这个函数做什么）:
///     解析 mirror payload 为 DTO，应用 legacy mapping 后投影；失败返回 Err 使整 source 失败。
pub(crate) fn project_remote_mirrors(
    mirrors: Vec<RemoteMirrorTask>,
    remote_shortcut: &WorkbenchProjectRow,
    freshness: AttentionFreshness,
    cached_at: Option<String>,
) -> Result<Vec<AttentionItemDto>, AppError> {
    let mut items = Vec::new();
    for mirror in mirrors {
        let task = serde_json::from_str::<OrchestratorTaskDto>(&mirror.payload_json)
            .map_err(|err| AppError::generic(format!("远端任务镜像解析失败: {err}")))?;
        if let Some(item) =
            project_remote_task_dto(&task, remote_shortcut, freshness, cached_at.clone())
        {
            items.push(item);
        }
    }
    Ok(items)
}

/// Business Logic（为什么需要这个函数）:
///     远端任务条目必须可导航到本机 shortcut 与 remote 包装 taskId。
///
/// Code Logic（这个函数做什么）:
///     用 `remote:<deviceId>:<inner>` 包装 task id，project 使用 shortcut，device 使用 owning device。
pub(crate) fn project_remote_task_dto(
    task: &OrchestratorTaskDto,
    remote_shortcut: &WorkbenchProjectRow,
    freshness: AttentionFreshness,
    cached_at: Option<String>,
) -> Option<AttentionItemDto> {
    let (category, source_kind, id_prefix) =
        attention_kind_for_task(task.status, task.workflow_state, task.run_state)?;
    let wrapped_task_id = remote_entity_id(&remote_shortcut.device_id, &task.id);
    let item_cached_at = match freshness {
        AttentionFreshness::Live => None,
        AttentionFreshness::Cached => cached_at,
    };
    Some(AttentionItemDto {
        id: format!("{id_prefix}:{wrapped_task_id}"),
        category,
        source_kind,
        title: task.title.clone(),
        summary: task_summary(task.blocked_reason.as_deref(), &task.goal, category),
        updated_at: task.updated_at.clone(),
        freshness,
        cached_at: item_cached_at,
        project: Some(AttentionProjectRef {
            id: remote_shortcut.id.clone(),
            name: remote_shortcut.name.clone(),
            kind: AttentionProjectKind::Remote,
        }),
        device: Some(AttentionDeviceRef {
            id: remote_shortcut.device_id.clone(),
            name: remote_shortcut.device_name.clone(),
        }),
        target: AttentionTargetDto::OrchestratorTask {
            project_id: remote_shortcut.id.clone(),
            task_id: wrapped_task_id,
        },
    })
}

/// Business Logic（为什么需要这个函数）:
///     只有 failed outbox 且绑定 active remote shortcut 才进入 Inbox；pending/sending 等不制造待办。
///
/// Code Logic（这个函数做什么）:
///     过滤非 failed；target.projectId 使用本机 shortcut id；summary 用 last_error 或固定失败文案。
pub(crate) fn project_failed_outbox_row(
    row: &OrchestratorRemoteOutboxRow,
    remote_shortcut: &WorkbenchProjectRow,
) -> Option<AttentionItemDto> {
    if row.status != RemoteOutboxStatus::Failed {
        return None;
    }
    // orphan 防御：device/path 必须与 shortcut 对齐。
    if row.device_id != remote_shortcut.device_id || row.remote_project_path != remote_shortcut.path
    {
        return None;
    }
    let summary = row
        .last_error
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or("远端任务发送失败，等待重新发送或放弃")
        .to_string();
    Some(AttentionItemDto {
        id: format!("orchestrator:outbox-failed:{}", row.id),
        category: AttentionCategory::Blocked,
        source_kind: AttentionSourceKind::RemoteOutboxFailed,
        title: "远端任务发送失败".to_string(),
        summary,
        updated_at: row.updated_at.clone(),
        freshness: AttentionFreshness::Live,
        cached_at: None,
        project: Some(AttentionProjectRef {
            id: remote_shortcut.id.clone(),
            name: remote_shortcut.name.clone(),
            kind: AttentionProjectKind::Remote,
        }),
        device: Some(AttentionDeviceRef {
            id: row.device_id.clone(),
            name: row.device_name.clone(),
        }),
        target: AttentionTargetDto::RemoteOutbox {
            project_id: remote_shortcut.id.clone(),
            outbox_id: row.id.clone(),
        },
    })
}

/// Business Logic（为什么需要这个函数）:
///     测试与生产都需要验证“最多 4 路 remote 刷新”不会被突破。
///
/// Code Logic（这个函数做什么）:
///     对每个 project 调用 refresh_one，经 `buffer_unordered(4)` 并发，返回各 future 结果列表。
pub(crate) async fn run_remote_refreshes_with_limit<F, Fut, T, E>(
    projects: Vec<String>,
    concurrency: usize,
    refresh_one: F,
) -> Vec<Result<T, E>>
where
    F: Fn(String) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send,
{
    let limit = concurrency.max(1);
    stream::iter(projects)
        .map(refresh_one)
        .buffer_unordered(limit)
        .collect()
        .await
}

/// Business Logic（为什么需要这个函数）:
///     legacy 任务可能只可靠地保留 status=blocked；投影前必须映射到 run_state=blocked。
///
/// Code Logic（这个函数做什么）:
///     HumanReview 优先产出 decision；否则若 legacy 或 split 显示 Blocked 则 blocked；其余排除。
fn attention_kind_for_task(
    status: OrchestratorTaskStatus,
    workflow_state: OrchestratorWorkflowState,
    run_state: OrchestratorRunState,
) -> Option<(AttentionCategory, AttentionSourceKind, &'static str)> {
    let effective = effective_split_state(status, workflow_state, run_state);
    if effective.workflow_state == OrchestratorWorkflowState::HumanReview {
        return Some((
            AttentionCategory::Decision,
            AttentionSourceKind::OrchestratorHumanReview,
            "orchestrator:human-review",
        ));
    }
    if effective.run_state == OrchestratorRunState::Blocked {
        return Some((
            AttentionCategory::Blocked,
            AttentionSourceKind::OrchestratorBlocked,
            "orchestrator:blocked",
        ));
    }
    None
}

/// Business Logic（为什么需要这个函数）:
///     旧库/混合写入可能出现 status=blocked 但 split 字段未同步；投影必须以 legacy 映射兜底。
///
/// Code Logic（这个函数做什么）:
///     status=Blocked 时强制使用 `SplitTaskState::from_legacy_status`；否则保留现有 split。
fn effective_split_state(
    status: OrchestratorTaskStatus,
    workflow_state: OrchestratorWorkflowState,
    run_state: OrchestratorRunState,
) -> SplitTaskState {
    if status == OrchestratorTaskStatus::Blocked {
        return SplitTaskState::from_legacy_status(status);
    }
    SplitTaskState {
        workflow_state,
        run_state,
    }
}

/// Business Logic（为什么需要这个函数）:
///     Attention 列表需要可读摘要：blocked 优先展示原因，否则回退 goal。
///
/// Code Logic（这个函数做什么）:
///     blocked_reason 非空则用它；否则用 goal；再空则给分类默认文案。
fn task_summary(blocked_reason: Option<&str>, goal: &str, category: AttentionCategory) -> String {
    if let Some(reason) = blocked_reason
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return reason.to_string();
    }
    let goal = goal.trim();
    if !goal.is_empty() {
        return goal.to_string();
    }
    match category {
        AttentionCategory::Decision => "等待人工复核".to_string(),
        AttentionCategory::Blocked => "任务运行受阻".to_string(),
        AttentionCategory::Environment => "环境依赖受阻".to_string(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     投影本机任务时只能引用当前仍存在的 Workbench 项目，删除后不得合成假引用。
///
/// Code Logic（这个函数做什么）:
///     在 workbench 项目列表中查找 task.project_id；找不到返回 None（不合成）。
#[cfg(test)]
fn project_ref_for_local_task(
    task: &OrchestratorTaskRow,
    projects: &[WorkbenchProjectRow],
) -> Option<AttentionProjectRef> {
    projects
        .iter()
        .find(|p| p.id == task.project_id)
        .map(|project| AttentionProjectRef {
            id: project.id.clone(),
            name: project.name.clone(),
            kind: if project.kind == "remote" {
                AttentionProjectKind::Remote
            } else {
                AttentionProjectKind::Local
            },
        })
}

/// Business Logic（为什么需要这个函数）:
///     删除最后一个本机项目后，历史 blocked/human-review 任务不得继续进入 Inbox。
///
/// Code Logic（这个函数做什么）:
///     仅按当前有效本机项目 ID 与任务 project_id 求交后投影；零本机项目恒返回空。
#[cfg(test)]
fn project_local_tasks_for_active_projects(
    local_projects: &[&WorkbenchProjectRow],
    tasks: &[OrchestratorTaskRow],
) -> Vec<AttentionItemDto> {
    let mut items = Vec::new();
    for project in local_projects {
        let project_ref = AttentionProjectRef {
            id: project.id.clone(),
            name: project.name.clone(),
            kind: AttentionProjectKind::Local,
        };
        for row in tasks.iter().filter(|row| row.project_id == project.id) {
            if let Some(item) = project_local_task_row(row, Some(project_ref.clone())) {
                items.push(item);
            }
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::models::OrchestratorTaskStatus;
    use crate::orchestrator::outbox::RemoteOutboxStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Business Logic: 构造本机任务 row，覆盖投影所需 status/split/updated_at。
    /// Code Logic: 用 default_for_status 填默认 split，再覆盖业务字段。
    fn local_task(
        id: &str,
        project_id: &str,
        status: OrchestratorTaskStatus,
        workflow_state: OrchestratorWorkflowState,
        run_state: OrchestratorRunState,
        updated_at: &str,
    ) -> OrchestratorTaskRow {
        OrchestratorTaskRow {
            id: id.to_string(),
            project_id: project_id.to_string(),
            title: format!("任务 {id}"),
            goal: format!("目标 {id}"),
            acceptance_criteria: "验收".to_string(),
            status,
            workflow_state,
            run_state,
            blocked_reason: None,
            created_at: "2026-07-11T08:00:00Z".to_string(),
            updated_at: updated_at.to_string(),
            ..OrchestratorTaskRow::default_for_status(status)
        }
    }

    /// Business Logic: 构造 remote shortcut，供 outbox/mirror 投影测试复用。
    /// Code Logic: 固定 device/path/id 字段。
    fn remote_shortcut() -> WorkbenchProjectRow {
        WorkbenchProjectRow {
            id: "shortcut-project-1".to_string(),
            name: "Remote Demo".to_string(),
            kind: "remote".to_string(),
            device_id: "device-a".to_string(),
            device_name: "Mac Mini".to_string(),
            path: "/Users/hans/remote-demo".to_string(),
            last_opened_at: "2026-07-11T08:00:00Z".to_string(),
            created_at: "2026-07-11T08:00:00Z".to_string(),
            updated_at: "2026-07-11T08:00:00Z".to_string(),
        }
    }

    /// Business Logic: 构造 outbox row，覆盖 failed/pending 等状态投影。
    /// Code Logic: 返回完整 OrchestratorRemoteOutboxRow。
    fn outbox_row(
        id: &str,
        status: RemoteOutboxStatus,
        device_id: &str,
        path: &str,
        updated_at: &str,
        last_error: Option<&str>,
    ) -> OrchestratorRemoteOutboxRow {
        OrchestratorRemoteOutboxRow {
            id: id.to_string(),
            device_id: device_id.to_string(),
            device_name: "Mac Mini".to_string(),
            remote_project_path: path.to_string(),
            remote_project_id: Some("remote-local-proj".to_string()),
            request_json:
                r#"{"projectId":"x","title":"t","goal":"g","acceptanceCriteria":"a","priority":0}"#
                    .to_string(),
            status,
            remote_task_id: None,
            last_error: last_error.map(str::to_string),
            created_at: "2026-07-11T08:00:00Z".to_string(),
            updated_at: updated_at.to_string(),
            sent_at: None,
        }
    }

    #[test]
    fn local_human_review_projects_to_decision_with_stable_ids() {
        let task = local_task(
            "task-hr-1",
            "proj-local-1",
            OrchestratorTaskStatus::Done,
            OrchestratorWorkflowState::HumanReview,
            OrchestratorRunState::Idle,
            "2026-07-11T10:00:00Z",
        );
        let project = AttentionProjectRef {
            id: "proj-local-1".to_string(),
            name: "Local Demo".to_string(),
            kind: AttentionProjectKind::Local,
        };
        let item = project_local_task_row(&task, Some(project.clone())).expect("human review item");
        assert_eq!(item.id, "orchestrator:human-review:task-hr-1");
        assert_eq!(item.category, AttentionCategory::Decision);
        assert_eq!(
            item.source_kind,
            AttentionSourceKind::OrchestratorHumanReview
        );
        assert_eq!(item.updated_at, "2026-07-11T10:00:00Z");
        assert_eq!(item.freshness, AttentionFreshness::Live);
        assert_eq!(item.cached_at, None);
        assert_eq!(item.project, Some(project));
        assert_eq!(
            item.target,
            AttentionTargetDto::OrchestratorTask {
                project_id: "proj-local-1".to_string(),
                task_id: "task-hr-1".to_string(),
            }
        );
    }

    #[test]
    fn local_blocked_projects_to_blocked_with_stable_ids() {
        let mut task = local_task(
            "task-blocked-1",
            "proj-local-1",
            OrchestratorTaskStatus::Blocked,
            OrchestratorWorkflowState::Rework,
            OrchestratorRunState::Blocked,
            "2026-07-11T11:00:00Z",
        );
        task.blocked_reason = Some("验证失败".to_string());
        let item = project_local_task_row(
            &task,
            Some(AttentionProjectRef {
                id: "proj-local-1".to_string(),
                name: "Local Demo".to_string(),
                kind: AttentionProjectKind::Local,
            }),
        )
        .expect("blocked item");
        assert_eq!(item.id, "orchestrator:blocked:task-blocked-1");
        assert_eq!(item.category, AttentionCategory::Blocked);
        assert_eq!(item.source_kind, AttentionSourceKind::OrchestratorBlocked);
        assert_eq!(item.summary, "验证失败");
        assert_eq!(item.updated_at, "2026-07-11T11:00:00Z");
        assert_eq!(
            item.target,
            AttentionTargetDto::OrchestratorTask {
                project_id: "proj-local-1".to_string(),
                task_id: "task-blocked-1".to_string(),
            }
        );
    }

    #[test]
    fn legacy_blocked_status_maps_before_projection_even_if_split_fields_stale() {
        // 模拟 legacy 只可靠写了 status=blocked，split 字段仍是 idle/todo 的脏数据。
        let task = local_task(
            "task-legacy-blocked",
            "proj-local-1",
            OrchestratorTaskStatus::Blocked,
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            "2026-07-11T11:30:00Z",
        );
        let item = project_local_task_row(&task, None).expect("legacy blocked must project");
        assert_eq!(item.id, "orchestrator:blocked:task-legacy-blocked");
        assert_eq!(item.category, AttentionCategory::Blocked);
        assert_eq!(item.source_kind, AttentionSourceKind::OrchestratorBlocked);
    }

    #[test]
    fn non_attention_local_states_are_excluded() {
        let cases = [
            (
                OrchestratorTaskStatus::Done,
                OrchestratorWorkflowState::Done,
                OrchestratorRunState::Idle,
            ),
            (
                OrchestratorTaskStatus::Running,
                OrchestratorWorkflowState::InProgress,
                OrchestratorRunState::Running,
            ),
            (
                OrchestratorTaskStatus::Queued,
                OrchestratorWorkflowState::Todo,
                OrchestratorRunState::Queued,
            ),
            (
                OrchestratorTaskStatus::Preparing,
                OrchestratorWorkflowState::Rework,
                OrchestratorRunState::Retrying,
            ),
            (
                OrchestratorTaskStatus::Queued,
                OrchestratorWorkflowState::Todo,
                OrchestratorRunState::Idle,
            ),
        ];
        for (status, workflow, run) in cases {
            let task = local_task(
                "task-skip",
                "proj-local-1",
                status,
                workflow,
                run,
                "2026-07-11T09:00:00Z",
            );
            assert!(
                project_local_task_row(&task, None).is_none(),
                "状态 {status:?}/{workflow:?}/{run:?} 不应进入 attention"
            );
        }
    }

    #[test]
    fn failed_outbox_for_active_shortcut_projects_with_local_project_target() {
        let shortcut = remote_shortcut();
        let row = outbox_row(
            "outbox-failed-1",
            RemoteOutboxStatus::Failed,
            &shortcut.device_id,
            &shortcut.path,
            "2026-07-11T12:00:00Z",
            Some("连接超时"),
        );
        let item = project_failed_outbox_row(&row, &shortcut).expect("failed outbox item");
        assert_eq!(item.id, "orchestrator:outbox-failed:outbox-failed-1");
        assert_eq!(item.category, AttentionCategory::Blocked);
        assert_eq!(item.source_kind, AttentionSourceKind::RemoteOutboxFailed);
        assert_eq!(item.updated_at, "2026-07-11T12:00:00Z");
        assert_eq!(item.summary, "连接超时");
        assert_eq!(
            item.target,
            AttentionTargetDto::RemoteOutbox {
                project_id: "shortcut-project-1".to_string(),
                outbox_id: "outbox-failed-1".to_string(),
            }
        );
        assert_eq!(
            item.project.as_ref().map(|p| p.id.as_str()),
            Some("shortcut-project-1")
        );
    }

    #[test]
    fn non_failed_and_orphan_outbox_rows_are_excluded() {
        let shortcut = remote_shortcut();
        for status in [
            RemoteOutboxStatus::Pending,
            RemoteOutboxStatus::Sending,
            RemoteOutboxStatus::Mirrored,
            RemoteOutboxStatus::Discarded,
        ] {
            let row = outbox_row(
                "outbox-skip",
                status,
                &shortcut.device_id,
                &shortcut.path,
                "2026-07-11T12:00:00Z",
                Some("err"),
            );
            assert!(
                project_failed_outbox_row(&row, &shortcut).is_none(),
                "{status:?} 不应进入 attention"
            );
        }

        // orphan：device/path 对不上 active shortcut
        let orphan = outbox_row(
            "outbox-orphan",
            RemoteOutboxStatus::Failed,
            "other-device",
            "/tmp/other",
            "2026-07-11T12:00:00Z",
            Some("err"),
        );
        assert!(project_failed_outbox_row(&orphan, &shortcut).is_none());
    }

    #[test]
    fn remote_live_and_cached_projection_uses_wrapped_task_id_and_cached_at() {
        let shortcut = remote_shortcut();
        let mut task = OrchestratorTaskDto::default_for_status(OrchestratorTaskStatus::Done);
        task.id = "inner-task-1".to_string();
        task.project_id = "remote-local-proj".to_string();
        task.title = "远端复核".to_string();
        task.goal = "完成交付".to_string();
        task.workflow_state = OrchestratorWorkflowState::HumanReview;
        task.run_state = OrchestratorRunState::Idle;
        task.updated_at = "2026-07-11T13:00:00Z".to_string();

        let live = project_remote_task_dto(
            &task,
            &shortcut,
            AttentionFreshness::Live,
            Some("should-ignore".to_string()),
        )
        .expect("live remote item");
        assert_eq!(
            live.id,
            "orchestrator:human-review:remote:device-a:inner-task-1"
        );
        assert_eq!(live.freshness, AttentionFreshness::Live);
        assert_eq!(live.cached_at, None);
        assert_eq!(
            live.target,
            AttentionTargetDto::OrchestratorTask {
                project_id: "shortcut-project-1".to_string(),
                task_id: "remote:device-a:inner-task-1".to_string(),
            }
        );

        let cached = project_remote_task_dto(
            &task,
            &shortcut,
            AttentionFreshness::Cached,
            Some("2026-07-11T12:55:00Z".to_string()),
        )
        .expect("cached remote item");
        assert_eq!(cached.freshness, AttentionFreshness::Cached);
        assert_eq!(
            cached.cached_at.as_deref(),
            Some("2026-07-11T12:55:00Z"),
            "cachedAt 必须使用 mirror last_synced_at，不能伪装成查询时间"
        );
        assert_eq!(cached.updated_at, "2026-07-11T13:00:00Z");
    }

    #[test]
    fn corrupt_mirror_json_fails_whole_source_projection() {
        let shortcut = remote_shortcut();
        let mirrors = vec![RemoteMirrorTask {
            id: "mirror-1".to_string(),
            device_id: shortcut.device_id.clone(),
            device_name: shortcut.device_name.clone(),
            remote_project_id: "remote-local-proj".to_string(),
            remote_project_path: shortcut.path.clone(),
            remote_task_id: "inner-1".to_string(),
            payload_json: "{not-json".to_string(),
            last_synced_at: "2026-07-11T12:00:00Z".to_string(),
        }];
        let err = project_remote_mirrors(
            mirrors,
            &shortcut,
            AttentionFreshness::Cached,
            Some("2026-07-11T12:00:00Z".to_string()),
        )
        .expect_err("corrupt mirror must fail source");
        assert!(
            err.to_string().contains("镜像解析失败") || err.to_string().contains("JSON"),
            "错误应标识镜像损坏: {err}"
        );
    }

    #[tokio::test]
    async fn remote_refresh_concurrency_never_exceeds_four() {
        let projects: Vec<String> = (0..12).map(|i| format!("project-{i}")).collect();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));

        let in_flight_clone = in_flight.clone();
        let max_in_flight_clone = max_in_flight.clone();
        let results = run_remote_refreshes_with_limit(
            projects,
            REMOTE_REFRESH_CONCURRENCY,
            move |_project_id| {
                let in_flight = in_flight_clone.clone();
                let max_in_flight = max_in_flight_clone.clone();
                async move {
                    let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    // 记录峰值：CAS 循环保证并发更新时不丢更高值。
                    loop {
                        let seen = max_in_flight.load(Ordering::SeqCst);
                        if current <= seen {
                            break;
                        }
                        if max_in_flight
                            .compare_exchange(seen, current, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                        {
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    Ok::<(), AppError>(())
                }
            },
        )
        .await;

        assert_eq!(results.len(), 12);
        assert!(results.iter().all(|r| r.is_ok()));
        let peak = max_in_flight.load(Ordering::SeqCst);
        assert!(
            peak <= REMOTE_REFRESH_CONCURRENCY,
            "并发峰值 {peak} 不得超过 {}",
            REMOTE_REFRESH_CONCURRENCY
        );
        assert!(
            peak >= 2,
            "测试应观察到真实并发，峰值={peak}（若恒为 1 说明未并行）"
        );
    }

    /// Business Logic: 删除最后一个含阻塞任务的本机项目后，Inbox 不得投影孤儿任务。
    /// Code Logic: 零本机项目 + 历史 blocked 任务 → 投影结果为空。
    #[test]
    fn zero_local_projects_never_projects_orphan_local_tasks() {
        let blocked = local_task(
            "orphan-blocked",
            "deleted-proj",
            OrchestratorTaskStatus::Blocked,
            OrchestratorWorkflowState::Rework,
            OrchestratorRunState::Blocked,
            "2026-07-11T14:00:00Z",
        );
        let human_review = local_task(
            "orphan-hr",
            "deleted-proj",
            OrchestratorTaskStatus::Done,
            OrchestratorWorkflowState::HumanReview,
            OrchestratorRunState::Idle,
            "2026-07-11T14:01:00Z",
        );
        let local_projects: Vec<&WorkbenchProjectRow> = Vec::new();
        let items =
            project_local_tasks_for_active_projects(&local_projects, &[blocked, human_review]);
        assert!(
            items.is_empty(),
            "删除最后一个本机项目后不得投影历史任务: {items:?}"
        );
    }

    /// Business Logic: 已删除项目的任务不能合成假 project 引用进入 Inbox。
    /// Code Logic: project_ref_for_local_task 在项目列表无匹配时返回 None。
    #[test]
    fn removed_project_does_not_synthesize_project_ref() {
        let task = local_task(
            "orphan-task",
            "gone-proj",
            OrchestratorTaskStatus::Blocked,
            OrchestratorWorkflowState::Rework,
            OrchestratorRunState::Blocked,
            "2026-07-11T15:00:00Z",
        );
        let projects: Vec<WorkbenchProjectRow> = Vec::new();
        assert!(
            project_ref_for_local_task(&task, &projects).is_none(),
            "已删除项目不得合成 project ref"
        );
    }

    /// Business Logic: 仅当前有效本机项目上的阻塞任务才进入 Inbox。
    /// Code Logic: 有匹配项目时投影 blocked；无关项目任务被过滤。
    #[test]
    fn only_tasks_for_active_local_projects_are_projected() {
        let active = WorkbenchProjectRow {
            id: "proj-active".to_string(),
            name: "Active".to_string(),
            kind: "local".to_string(),
            device_id: String::new(),
            device_name: String::new(),
            path: "/tmp/active".to_string(),
            last_opened_at: "2026-07-11T08:00:00Z".to_string(),
            created_at: "2026-07-11T08:00:00Z".to_string(),
            updated_at: "2026-07-11T08:00:00Z".to_string(),
        };
        let active_task = local_task(
            "active-blocked",
            "proj-active",
            OrchestratorTaskStatus::Blocked,
            OrchestratorWorkflowState::Rework,
            OrchestratorRunState::Blocked,
            "2026-07-11T16:00:00Z",
        );
        let orphan_task = local_task(
            "orphan-blocked",
            "proj-gone",
            OrchestratorTaskStatus::Blocked,
            OrchestratorWorkflowState::Rework,
            OrchestratorRunState::Blocked,
            "2026-07-11T16:01:00Z",
        );
        let local_projects = vec![&active];
        let items =
            project_local_tasks_for_active_projects(&local_projects, &[active_task, orphan_task]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "orchestrator:blocked:active-blocked");
        assert_eq!(
            items[0].project.as_ref().map(|p| p.id.as_str()),
            Some("proj-active")
        );
    }
}
