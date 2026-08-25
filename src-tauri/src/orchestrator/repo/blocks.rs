//! 任务块仓储：建块、追加成员、重排、共享 worktree。
//!
//! Business Logic（为什么需要这个模块）:
//!     串行任务块必须与普通任务、实验组隔离：共享 worktree/branch，且中间步跳过 Human Review / merge。
//!
//! Code Logic（这个模块做什么）:
//!     为 `OrchestratorRepo` 提供 block schema、幂等 create/append/reorder 与 claim/runner helper。

#![allow(dead_code)]
#![allow(unused_imports)]

use super::helpers::*;
use super::{IdempotentCreateBlockOutcome, IdempotentCreateTaskOutcome, OrchestratorRepo};
use crate::error::AppError;
use crate::orchestrator::models::{
    OrchestratorAttemptPhase, OrchestratorCreateAction, OrchestratorRunState,
    OrchestratorTaskBlockRow, OrchestratorTaskRow, OrchestratorTaskStatus,
    OrchestratorWorkflowState, SplitTaskState,
};
use crate::storage::maintenance_gate::{begin_shared_write, with_shared_write_lease};
use chrono::Utc;
use sqlx::sqlite::SqliteRow;
use sqlx::Row;
use uuid::Uuid;

pub const MIN_BLOCK_MEMBERS: usize = 2;
pub const MAX_BLOCK_MEMBERS: usize = 8;

const BLOCK_COLUMNS: &str =
    "id, project_id, title, shared_worktree_id, shared_branch_name, created_at, updated_at";

/// 创建块成员入参（仓储层）。
///
/// Business Logic（为什么需要这个结构体）:
///     建块与追加只需要三字段，避免把 tracker/priority 泄漏进块契约。
///
/// Code Logic（这个结构体做什么）:
///     保存已 trim 的 title/goal/acceptance。
#[derive(Debug, Clone)]
pub struct BlockMemberDraft {
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
}

/// Business Logic（为什么需要这个函数）:
///     仓储读取块行时必须统一投影，避免命令层直接依赖 SQLite row。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 组装 OrchestratorTaskBlockRow。
fn row_to_block(row: &SqliteRow) -> Result<OrchestratorTaskBlockRow, AppError> {
    Ok(OrchestratorTaskBlockRow {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        title: row.try_get("title")?,
        shared_worktree_id: row.try_get("shared_worktree_id")?,
        shared_branch_name: row.try_get("shared_branch_name")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Business Logic（为什么需要这个函数）:
///     create-block 幂等键必须包含 kind、块标题、成员顺序与 createAction，不能与单任务指纹碰撞。
///
/// Code Logic（这个函数做什么）:
///     固定 key 顺序 JSON → SHA256 hex。
pub(crate) fn create_block_fingerprint(
    project_id: &str,
    title: &str,
    members: &[BlockMemberDraft],
    create_action: OrchestratorCreateAction,
) -> Result<String, AppError> {
    let action = match create_action {
        OrchestratorCreateAction::Backlog => "backlog",
        OrchestratorCreateAction::Todo => "todo",
        OrchestratorCreateAction::Start => "start",
    };
    let members_json: Vec<serde_json::Value> = members
        .iter()
        .map(|member| {
            serde_json::json!({
                "title": member.title,
                "goal": member.goal,
                "acceptance": member.acceptance_criteria,
            })
        })
        .collect();
    block_request_fingerprint(&serde_json::json!({
        "kind": "create-block",
        "project_id": project_id,
        "title": title,
        "members": members_json,
        "action": action,
    }))
}

/// Business Logic（为什么需要这个函数）:
///     append 重试必须按同一块 + 同一成员三字段去重，不能误命中其它追加。
///
/// Code Logic（这个函数做什么）:
///     固定 key 顺序 JSON → SHA256 hex。
pub(crate) fn append_block_member_fingerprint(
    project_id: &str,
    block_id: &str,
    title: &str,
    goal: &str,
    acceptance: &str,
) -> Result<String, AppError> {
    block_request_fingerprint(&serde_json::json!({
        "kind": "append-block-member",
        "project_id": project_id,
        "block_id": block_id,
        "title": title,
        "goal": goal,
        "acceptance": acceptance,
    }))
}

/// Business Logic（为什么需要这个函数）:
///     reorder 重试必须按完整成员排列去重，避免半完成重排被当成成功。
///
/// Code Logic（这个函数做什么）:
///     固定 key 顺序 JSON → SHA256 hex。
pub(crate) fn reorder_block_members_fingerprint(
    project_id: &str,
    block_id: &str,
    ordered_task_ids: &[String],
) -> Result<String, AppError> {
    block_request_fingerprint(&serde_json::json!({
        "kind": "reorder-block-members",
        "project_id": project_id,
        "block_id": block_id,
        "ordered_task_ids": ordered_task_ids,
    }))
}

/// Business Logic（为什么需要这个函数）:
///     块幂等 ledger 的 `task_id` 存 block_id；命中后要回放整组成员，不能只读一行任务。
///
/// Code Logic（这个函数做什么）:
///     校验 project/fingerprint，再按 block_id 读取块与成员。
async fn resolve_existing_block_request(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request_id: &str,
    project_id: &str,
    request_fingerprint: &str,
    existing: SqliteRow,
) -> Result<(OrchestratorTaskBlockRow, Vec<OrchestratorTaskRow>), AppError> {
    let mapped_project_id: String = existing.try_get("project_id")?;
    let block_id: String = existing.try_get("task_id")?;
    let mapped_fingerprint: String = existing
        .try_get::<String, _>("request_fingerprint")
        .unwrap_or_default();

    if mapped_project_id != project_id {
        return Err(AppError::conflict(format!(
            "clientRequestId `{request_id}` 已绑定项目 `{mapped_project_id}`，不能用于项目 `{project_id}`"
        )));
    }
    if mapped_fingerprint.trim().is_empty() {
        return Err(AppError::conflict(format!(
            "clientRequestId `{request_id}` 缺少可靠请求指纹，请使用新的 clientRequestId 重新创建"
        )));
    }
    if mapped_fingerprint != request_fingerprint {
        return Err(AppError::conflict(format!(
            "clientRequestId `{request_id}` 已用于不同创建内容，拒绝冲突重放"
        )));
    }

    let block = load_block_in_tx(tx, &block_id).await?;
    if block.project_id != project_id {
        return Err(AppError::conflict(format!(
            "clientRequestId `{request_id}` 已绑定其它项目的任务块"
        )));
    }
    let tasks = list_block_members_in_tx(tx, &block_id).await?;
    Ok((block, tasks))
}

/// Business Logic（为什么需要这个函数）:
///     事务内读取块行，供幂等回放与 append/reorder 校验共用。
///
/// Code Logic（这个函数做什么）:
///     SELECT 块表；缺失返回 not_found。
async fn load_block_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    block_id: &str,
) -> Result<OrchestratorTaskBlockRow, AppError> {
    let row = sqlx::query(&format!(
        "SELECT {BLOCK_COLUMNS} FROM orchestrator_task_blocks WHERE id = ?"
    ))
    .bind(block_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => row_to_block(&row),
        None => Err(AppError::not_found(format!("任务块不存在: {block_id}"))),
    }
}

/// Business Logic（为什么需要这个函数）:
///     claim/append/reorder/verifier 都需要按 block_index 读取全部成员。
///
/// Code Logic（这个函数做什么）:
///     SELECT 成员并按 block_index ASC, created_at ASC 排序。
async fn list_block_members_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    block_id: &str,
) -> Result<Vec<OrchestratorTaskRow>, AppError> {
    let rows = sqlx::query(&format!(
        "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
         WHERE block_id = ? ORDER BY block_index ASC, created_at ASC, id ASC"
    ))
    .bind(block_id)
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(row_to_task).collect()
}

/// Business Logic（为什么需要这个函数）:
///     看板要把块标题挂到每个成员 DTO，避免前端再发一轮查询。
///
/// Code Logic（这个函数做什么）:
///     批量读取 block_id → title，写回 `block_title`。
async fn hydrate_block_titles(
    pool: &sqlx::SqlitePool,
    tasks: &mut [OrchestratorTaskRow],
) -> Result<(), AppError> {
    let mut ids: Vec<String> = tasks
        .iter()
        .filter_map(|task| task.block_id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql =
        format!("SELECT id, title FROM orchestrator_task_blocks WHERE id IN ({placeholders})");
    let mut query = sqlx::query(&sql);
    for id in &ids {
        query = query.bind(id);
    }
    let rows = query.fetch_all(pool).await?;
    let mut titles = std::collections::HashMap::new();
    for row in rows {
        let id: String = row.try_get("id")?;
        let title: String = row.try_get("title")?;
        titles.insert(id, title);
    }
    for task in tasks.iter_mut() {
        if let Some(block_id) = task.block_id.as_deref() {
            task.block_title = titles.get(block_id).cloned();
        }
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     块 head 决定追加是否允许：只有 backlog/todo/inProgress 可在末尾加步。
///
/// Code Logic（这个函数做什么）:
///     第一个非 done/canceled 成员的 workflow_state；全部完成则 Done。
pub(crate) fn block_head_workflow_state(
    members: &[OrchestratorTaskRow],
) -> OrchestratorWorkflowState {
    members
        .iter()
        .find(|member| {
            member.workflow_state != OrchestratorWorkflowState::Done
                && member.workflow_state != OrchestratorWorkflowState::Canceled
        })
        .map(|member| member.workflow_state)
        .unwrap_or(OrchestratorWorkflowState::Done)
}

/// Business Logic（为什么需要这个函数）:
///     verifier 通过当下必须按 live `max(block_index)` 判断，不能用创建时冻结的最后一步。
///
/// Code Logic（这个函数做什么）:
///     `current_index < max_index` 视为中间成员，跳过 Human Review / merge。
pub(crate) fn is_intermediate_block_member(current_index: i64, max_index: i64) -> bool {
    current_index < max_index
}

/// Business Logic（为什么需要这个函数）:
///     追加必须拒绝已进入复核/返工/合并/交付的块，避免 live last-member 语义被打乱。
///
/// Code Logic（这个函数做什么）:
///     head 必须是 backlog|todo|inProgress；任何成员不得处于 humanReview/rework/merging 或 delivering。
pub(crate) fn ensure_block_accepts_append(members: &[OrchestratorTaskRow]) -> Result<(), AppError> {
    if members.len() >= MAX_BLOCK_MEMBERS {
        return Err(AppError::generic("任务块最多包含 8 个成员"));
    }
    let head = block_head_workflow_state(members);
    if !matches!(
        head,
        OrchestratorWorkflowState::Backlog
            | OrchestratorWorkflowState::Todo
            | OrchestratorWorkflowState::InProgress
    ) {
        return Err(AppError::generic(
            "仅 Backlog、Todo 或进行中的任务块可以在末尾追加成员",
        ));
    }
    for member in members {
        if matches!(
            member.workflow_state,
            OrchestratorWorkflowState::HumanReview
                | OrchestratorWorkflowState::Rework
                | OrchestratorWorkflowState::Merging
        ) || member.run_state == OrchestratorRunState::Delivering
        {
            return Err(AppError::generic(
                "任务块已进入复核、返工、合并或交付，不能再追加成员",
            ));
        }
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     重排只能在整块仍停在 Backlog/Todo 且空闲时进行，进行中以后顺序冻结。
///
/// Code Logic（这个函数做什么）:
///     全部成员 backlog|todo 且 idle；否则 conflict。
pub(crate) fn ensure_block_accepts_reorder(
    members: &[OrchestratorTaskRow],
) -> Result<(), AppError> {
    for member in members {
        let lane_ok = matches!(
            member.workflow_state,
            OrchestratorWorkflowState::Backlog | OrchestratorWorkflowState::Todo
        );
        if !lane_ok || member.run_state != OrchestratorRunState::Idle {
            return Err(AppError::generic(
                "仅当任务块全部成员仍在 Backlog 或 Todo 且空闲时才能调整顺序",
            ));
        }
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     事务内插入成员任务，必须带上 block_id/block_index 与可选共享 worktree。
///
/// Code Logic（这个函数做什么）:
///     复用 create_task 列清单，在已有 tx 上 INSERT。
async fn insert_task_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    row: &OrchestratorTaskRow,
) -> Result<(), AppError> {
    let external_labels_json = serialize_external_labels(&row.external_labels)?;
    sqlx::query(
        "INSERT INTO orchestrator_tasks \
         (id, project_id, title, goal, acceptance_criteria, status, priority, branch_name, \
          workflow_state, run_state, attempt_phase, source, external_id, external_identifier, \
          external_url, external_state, external_labels_json, runner_provider, runner_max_turns, \
          runner_stall_timeout_ms, claude_session_id, agent_session_id, transcript_path, \
          runtime_started_at, last_activity_at, last_runtime_event, last_runtime_message, \
          worktree_id, session_id, prepare_claim_token, blocked_reason, attempt, \
          state_version, created_at, updated_at, started_at, finished_at, \
          experiment_id, delivery_suppressed, block_id, block_index) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&row.id)
    .bind(&row.project_id)
    .bind(&row.title)
    .bind(&row.goal)
    .bind(&row.acceptance_criteria)
    .bind(row.status.as_str())
    .bind(row.priority)
    .bind(&row.branch_name)
    .bind(row.workflow_state.as_str())
    .bind(row.run_state.as_str())
    .bind(row.attempt_phase.map(OrchestratorAttemptPhase::as_str))
    .bind(&row.source)
    .bind(&row.external_id)
    .bind(&row.external_identifier)
    .bind(&row.external_url)
    .bind(&row.external_state)
    .bind(&external_labels_json)
    .bind(&row.runner_provider)
    .bind(row.runner_max_turns)
    .bind(row.runner_stall_timeout_ms)
    .bind(&row.claude_session_id)
    .bind(&row.agent_session_id)
    .bind(&row.transcript_path)
    .bind(&row.runtime_started_at)
    .bind(&row.last_activity_at)
    .bind(&row.last_runtime_event)
    .bind(&row.last_runtime_message)
    .bind(&row.worktree_id)
    .bind(&row.session_id)
    .bind(&row.prepare_claim_token)
    .bind(&row.blocked_reason)
    .bind(row.attempt)
    .bind(row.state_version)
    .bind(&row.created_at)
    .bind(&row.updated_at)
    .bind(&row.started_at)
    .bind(&row.finished_at)
    .bind(&row.experiment_id)
    .bind(if row.delivery_suppressed { 1i64 } else { 0 })
    .bind(&row.block_id)
    .bind(row.block_index)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     块成员要复用单任务 createAction 映射，但 Start 只作用于第一个成员。
///
/// Code Logic（这个函数做什么）:
///     构造 Draft row，再按 action 覆盖 split state；Start 仅 index=0。
#[allow(clippy::too_many_arguments)]
fn member_row_for_action(
    project_id: &str,
    draft: &BlockMemberDraft,
    block_id: &str,
    block_index: i64,
    create_action: OrchestratorCreateAction,
    now: &str,
    shared_worktree_id: Option<&str>,
    shared_branch_name: Option<&str>,
) -> OrchestratorTaskRow {
    let member_action = if block_index == 0 {
        create_action
    } else if create_action == OrchestratorCreateAction::Backlog {
        OrchestratorCreateAction::Backlog
    } else {
        OrchestratorCreateAction::Todo
    };
    let mut row = OrchestratorTaskRow::default_for_status(member_action.initial_status());
    let split = SplitTaskState::from_create_action(member_action);
    row.id = Uuid::new_v4().to_string();
    row.project_id = project_id.to_string();
    row.title = draft.title.clone();
    row.goal = draft.goal.clone();
    row.acceptance_criteria = draft.acceptance_criteria.clone();
    row.status = member_action.initial_status();
    row.workflow_state = split.workflow_state;
    row.run_state = split.run_state;
    row.created_at = now.to_string();
    row.updated_at = now.to_string();
    row.block_id = Some(block_id.to_string());
    row.block_index = Some(block_index);
    if let Some(worktree_id) = shared_worktree_id.filter(|value| !value.is_empty()) {
        row.worktree_id = Some(worktree_id.to_string());
    }
    if let Some(branch_name) = shared_branch_name.filter(|value| !value.is_empty()) {
        row.branch_name = Some(branch_name.to_string());
    }
    row
}

impl OrchestratorRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     启动/测试必须补齐任务块表与 task.block_id/block_index，旧库不能因缺列读失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     CREATE TABLE/INDEX IF NOT EXISTS，再 ensure_column 两列。
    pub async fn init_task_block_schema(pool: &sqlx::sqlite::SqlitePool) -> Result<(), AppError> {
        for statement in [
            ORCHESTRATOR_TASK_BLOCK_SCHEMA,
            ORCHESTRATOR_TASK_BLOCKS_PROJECT_INDEX,
        ] {
            sqlx::query(statement).execute(pool).await?;
        }
        ensure_column(pool, "orchestrator_tasks", "block_id", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "block_index", "INTEGER").await?;
        sqlx::query(ORCHESTRATOR_TASKS_BLOCK_INDEX)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     详情/claim/runner 需要按 id 读取块元数据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT 块表；缺失返回 not_found。
    pub async fn get_task_block(
        &self,
        block_id: &str,
    ) -> Result<OrchestratorTaskBlockRow, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {BLOCK_COLUMNS} FROM orchestrator_task_blocks WHERE id = ?"
        ))
        .bind(block_id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => row_to_block(&row),
            None => Err(AppError::not_found(format!("任务块不存在: {block_id}"))),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     claim/verifier/看板需要按序号读取同一块的全部成员。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT 成员并 hydrate 块标题。
    pub async fn list_block_members(
        &self,
        block_id: &str,
    ) -> Result<Vec<OrchestratorTaskRow>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
             WHERE block_id = ? ORDER BY block_index ASC, created_at ASC, id ASC"
        ))
        .bind(block_id)
        .fetch_all(&self.pool)
        .await?;
        let mut tasks: Vec<OrchestratorTaskRow> =
            rows.iter().map(row_to_task).collect::<Result<_, _>>()?;
        hydrate_block_titles(&self.pool, &mut tasks).await?;
        Ok(tasks)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     列表/详情 DTO 需要带上 blockTitle，前端才能把成员收进块卡片。
    ///
    /// Code Logic（这个函数做什么）:
    ///     批量 JOIN 标题写回传入任务切片。
    pub async fn attach_block_titles(
        &self,
        tasks: &mut [OrchestratorTaskRow],
    ) -> Result<(), AppError> {
        hydrate_block_titles(&self.pool, tasks).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户一次创建 2–8 步串行块；重试必须整组回放，不能拆成多条普通任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     同一事务写 block + N tasks + ledger（task_id=block_id）；命中则校验指纹后回放成员。
    pub async fn create_task_block_idempotent(
        &self,
        client_request_id: Option<&str>,
        project_id: &str,
        title: &str,
        members: &[BlockMemberDraft],
        create_action: OrchestratorCreateAction,
    ) -> Result<IdempotentCreateBlockOutcome, AppError> {
        let project_id = project_id.trim();
        let title = title.trim();
        if project_id.is_empty() {
            return Err(AppError::generic("项目不能为空"));
        }
        if title.is_empty() {
            return Err(AppError::generic("任务块标题不能为空"));
        }
        if members.len() < MIN_BLOCK_MEMBERS {
            return Err(AppError::generic("任务块至少需要 2 个成员"));
        }
        if members.len() > MAX_BLOCK_MEMBERS {
            return Err(AppError::generic("任务块最多包含 8 个成员"));
        }
        for (index, member) in members.iter().enumerate() {
            if member.title.trim().is_empty() {
                return Err(AppError::generic(format!(
                    "第 {} 个成员标题不能为空",
                    index + 1
                )));
            }
            if member.goal.trim().is_empty() {
                return Err(AppError::generic(format!(
                    "第 {} 个成员目标不能为空",
                    index + 1
                )));
            }
        }

        let request_fingerprint =
            create_block_fingerprint(project_id, title, members, create_action)?;
        let client_request_id = client_request_id.and_then(non_empty_trimmed);
        let (_permit, mut tx) = begin_shared_write(&self.pool, &self.gate).await?;

        if let Some(request_id) = client_request_id {
            if let Some(existing) = sqlx::query(
                "SELECT project_id, task_id, request_fingerprint \
                 FROM orchestrator_remote_task_create_requests WHERE request_id = ?",
            )
            .bind(request_id)
            .fetch_optional(&mut *tx)
            .await?
            {
                let (block, mut tasks) = resolve_existing_block_request(
                    &mut tx,
                    request_id,
                    project_id,
                    &request_fingerprint,
                    existing,
                )
                .await?;
                tx.commit().await?;
                hydrate_block_titles(&self.pool, &mut tasks).await?;
                return Ok(IdempotentCreateBlockOutcome {
                    block,
                    tasks,
                    newly_created: false,
                });
            }
        }

        let now = Utc::now().to_rfc3339();
        let block_id = Uuid::new_v4().to_string();
        if let Some(request_id) = client_request_id {
            let inserted = sqlx::query(
                "INSERT OR IGNORE INTO orchestrator_remote_task_create_requests \
                 (request_id, project_id, task_id, request_fingerprint, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(request_id)
            .bind(project_id)
            .bind(&block_id)
            .bind(&request_fingerprint)
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            if inserted.rows_affected() != 1 {
                let existing = sqlx::query(
                    "SELECT project_id, task_id, request_fingerprint \
                     FROM orchestrator_remote_task_create_requests WHERE request_id = ?",
                )
                .bind(request_id)
                .fetch_one(&mut *tx)
                .await?;
                let (block, mut tasks) = resolve_existing_block_request(
                    &mut tx,
                    request_id,
                    project_id,
                    &request_fingerprint,
                    existing,
                )
                .await?;
                tx.commit().await?;
                hydrate_block_titles(&self.pool, &mut tasks).await?;
                return Ok(IdempotentCreateBlockOutcome {
                    block,
                    tasks,
                    newly_created: false,
                });
            }
        }

        sqlx::query(
            "INSERT INTO orchestrator_task_blocks \
             (id, project_id, title, shared_worktree_id, shared_branch_name, created_at, updated_at) \
             VALUES (?, ?, ?, NULL, NULL, ?, ?)",
        )
        .bind(&block_id)
        .bind(project_id)
        .bind(title)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        let mut tasks = Vec::with_capacity(members.len());
        for (index, draft) in members.iter().enumerate() {
            let row = member_row_for_action(
                project_id,
                draft,
                &block_id,
                index as i64,
                create_action,
                &now,
                None,
                None,
            );
            insert_task_in_tx(&mut tx, &row).await?;
            tasks.push(row);
        }

        tx.commit().await?;
        for task in &mut tasks {
            task.block_title = Some(title.to_string());
        }
        Ok(IdempotentCreateBlockOutcome {
            block: OrchestratorTaskBlockRow {
                id: block_id,
                project_id: project_id.to_string(),
                title: title.to_string(),
                shared_worktree_id: None,
                shared_branch_name: None,
                created_at: now.clone(),
                updated_at: now,
            },
            tasks,
            newly_created: true,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户要在块末尾追加下一步；进行中追加后，原 last 步 verifier 必须按 live max_index 走中间路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验 head/拒绝态/上限 8；新成员 index=max+1；整块仍 Backlog 则 Draft，否则 Todo；复制共享 worktree。
    pub async fn append_task_block_member_idempotent(
        &self,
        client_request_id: Option<&str>,
        project_id: &str,
        block_id: &str,
        title: &str,
        goal: &str,
        acceptance_criteria: &str,
    ) -> Result<IdempotentCreateTaskOutcome, AppError> {
        let project_id = project_id.trim();
        let block_id = block_id.trim();
        let title = title.trim();
        let goal = goal.trim();
        let acceptance_criteria = acceptance_criteria.trim();
        if project_id.is_empty() {
            return Err(AppError::generic("项目不能为空"));
        }
        if block_id.is_empty() {
            return Err(AppError::generic("任务块不能为空"));
        }
        if title.is_empty() {
            return Err(AppError::generic("任务标题不能为空"));
        }
        if goal.is_empty() {
            return Err(AppError::generic("任务目标不能为空"));
        }

        let request_fingerprint = append_block_member_fingerprint(
            project_id,
            block_id,
            title,
            goal,
            acceptance_criteria,
        )?;
        let client_request_id = client_request_id.and_then(non_empty_trimmed);
        let (_permit, mut tx) = begin_shared_write(&self.pool, &self.gate).await?;

        if let Some(request_id) = client_request_id {
            if let Some(existing) = sqlx::query(
                "SELECT project_id, task_id, request_fingerprint \
                 FROM orchestrator_remote_task_create_requests WHERE request_id = ?",
            )
            .bind(request_id)
            .fetch_optional(&mut *tx)
            .await?
            {
                let task = resolve_existing_create_request(
                    &mut tx,
                    request_id,
                    project_id,
                    &request_fingerprint,
                    existing,
                )
                .await?;
                tx.commit().await?;
                return Ok(IdempotentCreateTaskOutcome {
                    task,
                    newly_created: false,
                });
            }
        }

        let block = load_block_in_tx(&mut tx, block_id).await?;
        if block.project_id != project_id {
            return Err(AppError::not_found(format!("任务块不存在: {block_id}")));
        }
        let members = list_block_members_in_tx(&mut tx, block_id).await?;
        ensure_block_accepts_append(&members)?;
        let next_index = members
            .iter()
            .filter_map(|member| member.block_index)
            .max()
            .unwrap_or(-1)
            + 1;
        let all_backlog = members
            .iter()
            .all(|member| member.workflow_state == OrchestratorWorkflowState::Backlog);
        let create_action = if all_backlog {
            OrchestratorCreateAction::Backlog
        } else {
            OrchestratorCreateAction::Todo
        };
        let now = Utc::now().to_rfc3339();
        let draft = BlockMemberDraft {
            title: title.to_string(),
            goal: goal.to_string(),
            acceptance_criteria: acceptance_criteria.to_string(),
        };
        let row = member_row_for_action(
            project_id,
            &draft,
            block_id,
            next_index,
            create_action,
            &now,
            block.shared_worktree_id.as_deref(),
            block.shared_branch_name.as_deref(),
        );

        if let Some(request_id) = client_request_id {
            let inserted = sqlx::query(
                "INSERT OR IGNORE INTO orchestrator_remote_task_create_requests \
                 (request_id, project_id, task_id, request_fingerprint, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(request_id)
            .bind(project_id)
            .bind(&row.id)
            .bind(&request_fingerprint)
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            if inserted.rows_affected() != 1 {
                let existing = sqlx::query(
                    "SELECT project_id, task_id, request_fingerprint \
                     FROM orchestrator_remote_task_create_requests WHERE request_id = ?",
                )
                .bind(request_id)
                .fetch_one(&mut *tx)
                .await?;
                let task = resolve_existing_create_request(
                    &mut tx,
                    request_id,
                    project_id,
                    &request_fingerprint,
                    existing,
                )
                .await?;
                tx.commit().await?;
                return Ok(IdempotentCreateTaskOutcome {
                    task,
                    newly_created: false,
                });
            }
        }

        insert_task_in_tx(&mut tx, &row).await?;
        sqlx::query("UPDATE orchestrator_task_blocks SET updated_at = ? WHERE id = ?")
            .bind(&now)
            .bind(block_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let mut task = row;
        task.block_title = Some(block.title);
        Ok(IdempotentCreateTaskOutcome {
            task,
            newly_created: true,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Backlog/Todo 空闲块允许调整成员顺序；进行中以后顺序冻结。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验排列是全部成员的置换，事务内按数组下标重写 block_index。
    pub async fn reorder_task_block_members_idempotent(
        &self,
        client_request_id: Option<&str>,
        project_id: &str,
        block_id: &str,
        ordered_task_ids: &[String],
    ) -> Result<(OrchestratorTaskBlockRow, Vec<OrchestratorTaskRow>, bool), AppError> {
        let project_id = project_id.trim();
        let block_id = block_id.trim();
        if project_id.is_empty() {
            return Err(AppError::generic("项目不能为空"));
        }
        if block_id.is_empty() {
            return Err(AppError::generic("任务块不能为空"));
        }
        if ordered_task_ids.is_empty() {
            return Err(AppError::generic("重排成员列表不能为空"));
        }

        let request_fingerprint =
            reorder_block_members_fingerprint(project_id, block_id, ordered_task_ids)?;
        let client_request_id = client_request_id.and_then(non_empty_trimmed);
        let (_permit, mut tx) = begin_shared_write(&self.pool, &self.gate).await?;

        if let Some(request_id) = client_request_id {
            if let Some(existing) = sqlx::query(
                "SELECT project_id, task_id, request_fingerprint \
                 FROM orchestrator_remote_task_create_requests WHERE request_id = ?",
            )
            .bind(request_id)
            .fetch_optional(&mut *tx)
            .await?
            {
                let (block, tasks) = resolve_existing_block_request(
                    &mut tx,
                    request_id,
                    project_id,
                    &request_fingerprint,
                    existing,
                )
                .await?;
                tx.commit().await?;
                return Ok((block, tasks, false));
            }
        }

        let block = load_block_in_tx(&mut tx, block_id).await?;
        if block.project_id != project_id {
            return Err(AppError::not_found(format!("任务块不存在: {block_id}")));
        }
        let members = list_block_members_in_tx(&mut tx, block_id).await?;
        ensure_block_accepts_reorder(&members)?;
        let mut current_ids: Vec<String> = members.iter().map(|member| member.id.clone()).collect();
        let mut requested = ordered_task_ids.to_vec();
        current_ids.sort();
        requested.sort();
        if current_ids != requested {
            return Err(AppError::generic("重排列表必须恰好覆盖该任务块的全部成员"));
        }

        let now = Utc::now().to_rfc3339();
        if let Some(request_id) = client_request_id {
            let inserted = sqlx::query(
                "INSERT OR IGNORE INTO orchestrator_remote_task_create_requests \
                 (request_id, project_id, task_id, request_fingerprint, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(request_id)
            .bind(project_id)
            .bind(block_id)
            .bind(&request_fingerprint)
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            if inserted.rows_affected() != 1 {
                let existing = sqlx::query(
                    "SELECT project_id, task_id, request_fingerprint \
                     FROM orchestrator_remote_task_create_requests WHERE request_id = ?",
                )
                .bind(request_id)
                .fetch_one(&mut *tx)
                .await?;
                let (block, tasks) = resolve_existing_block_request(
                    &mut tx,
                    request_id,
                    project_id,
                    &request_fingerprint,
                    existing,
                )
                .await?;
                tx.commit().await?;
                return Ok((block, tasks, false));
            }
        }

        for (index, task_id) in ordered_task_ids.iter().enumerate() {
            sqlx::query(
                "UPDATE orchestrator_tasks SET block_index = ?, updated_at = ? \
                 WHERE id = ? AND block_id = ?",
            )
            .bind(index as i64)
            .bind(&now)
            .bind(task_id)
            .bind(block_id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE orchestrator_task_blocks SET updated_at = ? WHERE id = ?")
            .bind(&now)
            .bind(block_id)
            .execute(&mut *tx)
            .await?;
        let tasks = list_block_members_in_tx(&mut tx, block_id).await?;
        tx.commit().await?;
        Ok((block, tasks, true))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     首个成员创建 worktree 后，后续成员 attempt=1 必须复用同一 worktree/branch。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入块 shared_*，并把空 worktree 的后续成员补上同一对 id/branch。
    pub async fn persist_block_shared_worktree(
        &self,
        block_id: &str,
        worktree_id: &str,
        branch_name: &str,
    ) -> Result<(), AppError> {
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_task_blocks \
                 SET shared_worktree_id = ?, shared_branch_name = ?, updated_at = ? \
                 WHERE id = ?",
            )
            .bind(worktree_id)
            .bind(branch_name)
            .bind(&now)
            .bind(block_id)
            .execute(&self.pool)
            .await?;
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET worktree_id = COALESCE(NULLIF(worktree_id, ''), ?), \
                     branch_name = COALESCE(NULLIF(branch_name, ''), ?), \
                     updated_at = ? \
                 WHERE block_id = ?",
            )
            .bind(worktree_id)
            .bind(branch_name)
            .bind(&now)
            .bind(block_id)
            .execute(&self.pool)
            .await?;
            Ok::<(), AppError>(())
        })
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier 中间步通过后，下一步必须立刻能复用共享 worktree 被 claim。
    ///
    /// Code Logic（这个函数做什么）:
    ///     把 shared worktree/branch 写到指定成员（若仍为空）。
    pub async fn copy_shared_worktree_to_member(
        &self,
        block_id: &str,
        task_id: &str,
    ) -> Result<(), AppError> {
        let block = self.get_task_block(block_id).await?;
        let Some(worktree_id) = block
            .shared_worktree_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(());
        };
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET worktree_id = COALESCE(NULLIF(worktree_id, ''), ?), \
                     branch_name = COALESCE(NULLIF(branch_name, ''), ?), \
                     updated_at = ? \
                 WHERE id = ? AND block_id = ?",
            )
            .bind(worktree_id)
            .bind(block.shared_branch_name.as_deref().unwrap_or(""))
            .bind(&now)
            .bind(task_id)
            .bind(block_id)
            .execute(&self.pool)
            .await?;
            Ok::<(), AppError>(())
        })
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     last-member 判定必须在 verifier pass 时 live 重读 max(block_index)，不能冻结在创建时。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT MAX(block_index)；空块返回 None。
    pub async fn max_block_index(&self, block_id: &str) -> Result<Option<i64>, AppError> {
        let value: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(block_index) FROM orchestrator_tasks WHERE block_id = ?",
        )
        .bind(block_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(value)
    }
}
