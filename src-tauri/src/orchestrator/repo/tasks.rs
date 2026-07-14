//! 任务 CRUD/claim/状态
//!
//! Business Logic（为什么需要这个模块）:
//!     从 monofile 按职责拆分 OrchestratorRepo 方法，SQL 与公共签名不变。
//!
//! Code Logic（这个模块做什么）:
//!     为 `OrchestratorRepo` 提供对应 `impl` 方法块。

#![allow(dead_code)]
#![allow(unused_imports)]

use super::helpers::*;
use super::OrchestratorRepo;
use crate::error::AppError;
use crate::orchestrator::claim::{
    preflight_claim_candidates, ClaimCandidate, ClaimCasOutcome, ClaimScanCursor,
    CLAIM_CANDIDATE_LIMIT,
};
use crate::orchestrator::claude_runtime::ClaudeRuntimeSummary;
use crate::orchestrator::models::{
    OrchestratorAttemptPhase, OrchestratorCreateAction, OrchestratorEvidenceDto,
    OrchestratorProjectConfigDto, OrchestratorRunState, OrchestratorTaskAttemptRow,
    OrchestratorTaskRow, OrchestratorTaskStatus, OrchestratorWorkflowState, SplitTaskState,
    EVIDENCE_KIND_REPAIR_PROMPT,
};
use crate::orchestrator::outbox::{
    OrchestratorRemoteOutboxRow, RemoteMirrorTask, RemoteOutboxStatus,
};
use crate::storage::maintenance_gate::{begin_shared_write, with_shared_write_lease};
use chrono::Utc;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

impl OrchestratorRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     命令层会先完成校验、生成 id/时间戳并构造 Row，仓储只负责按 Row 持久化。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将调用方传入的 OrchestratorTaskRow 全字段插入 orchestrator_tasks，不改写业务字段。
    pub async fn create_task(&self, row: &OrchestratorTaskRow) -> Result<(), AppError> {
        let external_labels_json = serialize_external_labels(&row.external_labels)?;
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO orchestrator_tasks \
                 (id, project_id, title, goal, acceptance_criteria, status, priority, branch_name, \
                  workflow_state, run_state, attempt_phase, source, external_id, external_identifier, \
                  external_url, external_state, external_labels_json, runner_provider, claude_session_id, \
                  transcript_path, runtime_started_at, last_activity_at, last_runtime_event, \
                  last_runtime_message, worktree_id, session_id, prepare_claim_token, blocked_reason, attempt, created_at, \
                  updated_at, started_at, finished_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
            .bind(&row.claude_session_id)
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
            .bind(&row.created_at)
            .bind(&row.updated_at)
            .bind(&row.started_at)
            .bind(&row.finished_at)
            .execute(&self.pool)
            .await
        }).await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Workbench 启动摘要需要全局最近“值得关注”的编排任务，且禁止按项目 N+1 查询。
    ///
    /// Code Logic（这个函数做什么）:
    ///     单 SQL：status ∈ running/preparing/verifying/delivering/blocked 或
    ///     workflow_state=humanReview，按 updated_at DESC LIMIT；limit 裁剪到 0..=5。
    pub async fn list_launch_tasks(&self, limit: i64) -> Result<Vec<OrchestratorTaskRow>, AppError> {
        let limit = limit.clamp(0, 5);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
             WHERE status IN (?, ?, ?, ?, ?) OR workflow_state = ? \
             ORDER BY updated_at DESC, id ASC LIMIT ?"
        ))
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(OrchestratorTaskStatus::Preparing.as_str())
        .bind(OrchestratorTaskStatus::Verifying.as_str())
        .bind(OrchestratorTaskStatus::Delivering.as_str())
        .bind(OrchestratorTaskStatus::Blocked.as_str())
        .bind(OrchestratorWorkflowState::HumanReview.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_task).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     任务列表需要支持按项目筛选或返回全局任务，并按调度优先级排序。
    ///
    /// Code Logic（这个函数做什么）:
    ///     根据可选 project_id 选择 SQL，结果统一按 priority DESC, created_at ASC 排序。
    pub async fn list_tasks(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<OrchestratorTaskRow>, AppError> {
        let rows = match project_id {
            Some(project_id) => {
                sqlx::query(&format!(
                    "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
                     WHERE project_id = ? ORDER BY priority DESC, created_at ASC"
                ))
                .bind(project_id)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
                     ORDER BY priority DESC, created_at ASC"
                ))
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter().map(row_to_task).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调度器和详情页需要按任务 id 读取完整任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 id 查询任务表，缺失时转换为 AppError::not_found。
    pub async fn get_task(&self, task_id: &str) -> Result<OrchestratorTaskRow, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks WHERE id = ?"
        ))
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => row_to_task(&row),
            None => Err(AppError::not_found(format!(
                "Orchestrator 任务不存在: {task_id}"
            ))),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     legacy 项目配置表仍需支持旧数据兼容和调试读取；新项目缺失记录时保持可创建默认行。
    ///     Settings 自动化 tab 已成为唯一用户配置入口，scheduler、验证和 delivery 运行时不再读取该表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对缺失 project_id 执行 INSERT OR IGNORE 写入 full-auto-but-disabled 默认值，再读取并解析 DTO。
    pub async fn get_or_create_project_config(
        &self,
        project_id: &str,
    ) -> Result<OrchestratorProjectConfigDto, AppError> {
        let project_id = project_id.trim();
        if project_id.is_empty() {
            return Err(AppError::generic("项目不能为空"));
        }

        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT OR IGNORE INTO orchestrator_project_config \
                 (project_id, enabled, max_concurrent_tasks, branch_prefix, verification_commands_json, \
                  auto_commit, auto_push_task_branch, auto_merge_to_main, auto_push_main, retry_limit, \
                  retain_worktree_on_done, retain_worktree_on_blocked, created_at, updated_at) \
                 VALUES (?, 0, 1, 'agent', '[]', 1, 1, 1, 1, 0, 0, 1, ?, ?)",
            )
            .bind(project_id)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await
        }).await?;

        let row = sqlx::query(&format!(
            "SELECT {PROJECT_CONFIG_COLUMNS} FROM orchestrator_project_config WHERE project_id = ?"
        ))
        .bind(project_id)
        .fetch_one(&self.pool)
        .await?;

        row_to_project_config(&row)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     legacy 项目配置列表仅用于兼容/调试旧数据；运行时调度已改读 AppConfig.orchestrator。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询 enabled=1 的 legacy 配置，按 project_id 稳定排序后转换为 OrchestratorProjectConfigDto。
    pub async fn list_enabled_project_configs(
        &self,
    ) -> Result<Vec<OrchestratorProjectConfigDto>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {PROJECT_CONFIG_COLUMNS} FROM orchestrator_project_config \
             WHERE enabled = 1 ORDER BY project_id ASC"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_project_config).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     项目级并发上限只应计算已经被调度器接管或正在后续阶段处理的任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     统计 Preparing/Running/Verifying/Delivering 四个 active run_state 数量，返回 SQLite COUNT 结果。
    pub async fn count_active_tasks(&self, project_id: &str) -> Result<i64, AppError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS count FROM orchestrator_tasks \
             WHERE project_id = ? AND run_state IN (?, ?, ?, ?)",
        )
        .bind(project_id)
        .bind(OrchestratorRunState::Preparing.as_str())
        .bind(OrchestratorRunState::Running.as_str())
        .bind(OrchestratorRunState::Verifying.as_str())
        .bind(OrchestratorRunState::Delivering.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("count")?)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runtime snapshot 需要展示当前项目已占用的本机自动化槽位，避免 UI 重新拼 SQL 或误用 legacy status。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 project_id 统计 Preparing/Running/Verifying/Delivering 四个 active run_state 数量。
    pub async fn count_active_run_states_for_project(
        &self,
        project_id: &str,
    ) -> Result<i64, AppError> {
        self.count_active_tasks(project_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runtime snapshot 应尽量暴露最近阻塞原因，帮助用户理解自动化为什么没有继续推进。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询同项目最近一个 run_state=blocked 且 blocked_reason 非空的任务，按 updated_at/id 倒序取一条。
    pub async fn latest_blocked_reason_for_project(
        &self,
        project_id: &str,
    ) -> Result<Option<String>, AppError> {
        let row = sqlx::query(
            "SELECT blocked_reason FROM orchestrator_tasks \
             WHERE project_id = ? AND run_state = ? \
               AND blocked_reason IS NOT NULL AND TRIM(blocked_reason) <> '' \
             ORDER BY updated_at DESC, id DESC LIMIT 1",
        )
        .bind(project_id)
        .bind(OrchestratorRunState::Blocked.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|item| item.try_get("blocked_reason")).transpose()?)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runtime snapshot 需要展示当前仍占用执行槽位的任务摘要，避免用户只能看到槽位数字。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 project_id 查询 Preparing/Running/Verifying/Delivering run_state 的任务，按更新时间倒序限制数量。
    pub async fn list_active_runtime_tasks_for_project(
        &self,
        project_id: &str,
        limit: i64,
    ) -> Result<Vec<OrchestratorTaskRow>, AppError> {
        let limit = limit.clamp(0, 50);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
             WHERE project_id = ? AND run_state IN (?, ?, ?, ?) \
             ORDER BY updated_at DESC, priority DESC, created_at ASC LIMIT ?"
        ))
        .bind(project_id)
        .bind(OrchestratorRunState::Preparing.as_str())
        .bind(OrchestratorRunState::Running.as_str())
        .bind(OrchestratorRunState::Verifying.as_str())
        .bind(OrchestratorRunState::Delivering.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_task).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runtime snapshot 需要突出需要重试或返工的任务，让用户能从状态条直接看到阻塞队列。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询 workflow_state=Rework 或 run_state=Retrying/Blocked 的任务，按更新时间倒序限制数量。
    pub async fn list_retrying_runtime_tasks_for_project(
        &self,
        project_id: &str,
        limit: i64,
    ) -> Result<Vec<OrchestratorTaskRow>, AppError> {
        let limit = limit.clamp(0, 50);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
             WHERE project_id = ? \
               AND (workflow_state = ? OR run_state IN (?, ?)) \
             ORDER BY updated_at DESC, priority DESC, created_at ASC LIMIT ?"
        ))
        .bind(project_id)
        .bind(OrchestratorWorkflowState::Rework.as_str())
        .bind(OrchestratorRunState::Retrying.as_str())
        .bind(OrchestratorRunState::Blocked.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_task).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runtime snapshot 状态条需要展示当前项目最近的 scheduler/runner 事件，而不是让前端遍历全量任务事件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     join task_events 与 tasks 过滤 project_id，按事件 created_at/id 倒序取最近 limit 条。
    pub async fn list_recent_events_for_project(
        &self,
        project_id: &str,
        limit: i64,
    ) -> Result<Vec<OrchestratorRecentEventRow>, AppError> {
        let limit = limit.clamp(0, 50);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT event.id, event.task_id, task.title AS task_title, \
                    event.kind, event.message, event.created_at \
             FROM orchestrator_task_events event \
             INNER JOIN orchestrator_tasks task ON task.id = event.task_id \
             WHERE task.project_id = ? \
             ORDER BY event.created_at DESC, event.id DESC LIMIT ?",
        )
        .bind(project_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(OrchestratorRecentEventRow {
                    id: row.try_get("id")?,
                    task_id: row.try_get("task_id")?,
                    task_title: row.try_get("task_title")?,
                    kind: row.try_get("kind")?,
                    message: row.try_get("message")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Phase 3 的调度容量属于本设备全局配置，必须统计所有本机 local Workbench 项目的执行中任务。
    ///     远端项目只是快捷方式，不能占用或触发本机 Runner 容量。
    ///
    /// Code Logic（这个函数做什么）:
    ///     通过 workbench_projects join 过滤 kind='local'，统计 Preparing/Running/Verifying/Delivering 四个 active run_state。
    pub async fn count_active_local_tasks(&self) -> Result<i64, AppError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS count FROM orchestrator_tasks task \
             INNER JOIN workbench_projects project ON project.id = task.project_id \
             WHERE project.kind = 'local' AND task.run_state IN (?, ?, ?, ?)",
        )
        .bind(OrchestratorRunState::Preparing.as_str())
        .bind(OrchestratorRunState::Running.as_str())
        .bind(OrchestratorRunState::Verifying.as_str())
        .bind(OrchestratorRunState::Delivering.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("count")?)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调度器领取任务必须在同一个仓储原子边界内校验项目并发容量，避免后台 tick 与手动 dispatch 同时看到空闲槽位后各自领取任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在单个事务中处理 max<=0、active 状态计数、Queued 任务选择、带 status 条件的 Preparing 更新和更新后 Row 读取。
    ///     active 达到上限、无 Queued 任务或并发竞争导致 UPDATE 未命中时返回 None。
    pub async fn claim_next_queued_task_with_capacity(
        &self,
        project_id: &str,
        max_concurrent_tasks: i64,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        let (_permit, mut tx) = begin_shared_write(&self.pool, &self.gate).await?;
        if max_concurrent_tasks <= 0 {
            tx.commit().await?;
            return Ok(None);
        }

        let active = sqlx::query(
            "SELECT COUNT(*) AS count FROM orchestrator_tasks \
             WHERE project_id = ? AND status IN (?, ?, ?, ?)",
        )
        .bind(project_id)
        .bind(OrchestratorTaskStatus::Preparing.as_str())
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(OrchestratorTaskStatus::Verifying.as_str())
        .bind(OrchestratorTaskStatus::Delivering.as_str())
        .fetch_one(&mut *tx)
        .await?
        .try_get::<i64, _>("count")?;
        if active >= max_concurrent_tasks {
            tx.commit().await?;
            return Ok(None);
        }

        let selected = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
             WHERE project_id = ? AND status = ? \
             ORDER BY priority DESC, created_at ASC LIMIT 1"
        ))
        .bind(project_id)
        .bind(OrchestratorTaskStatus::Queued.as_str())
        .fetch_optional(&mut *tx)
        .await?;

        let Some(row) = selected else {
            tx.commit().await?;
            return Ok(None);
        };
        let task_id: String = row.try_get("id")?;
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Preparing);
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Preparing.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind(Option::<&str>::None)
        .bind(now)
        .bind(&task_id)
        .bind(OrchestratorTaskStatus::Queued.as_str())
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(None);
        }

        let row = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks WHERE id = ?"
        ))
        .bind(&task_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(row_to_task(&row)?))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     全局 scheduler / 手动 dispatch 需要按设备级剩余容量领取本机 local 且 workflow 允许的任务。
    ///     兼容入口保持旧签名；内部改为三阶段：有界候选 → 事务外 preflight → 短 CAS 写事务，
    ///     不再在事务内做文件 IO 或无界 SELECT。
    ///
    /// Code Logic（这个函数做什么）:
    ///     limit<=0 直接返回空。否则无 cursor 读最多 256 候选 → `preflight_claim_candidates` →
    ///     `claim_preflighted_candidates_with_global_capacity`，返回 CAS 成功的 Preparing 行。
    ///     不维护扫描游标（游标由 scheduler 生命周期拥有）。
    pub async fn claim_next_local_queued_tasks_with_global_capacity(
        &self,
        limit: i64,
    ) -> Result<Vec<OrchestratorTaskRow>, AppError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let candidates = self
            .list_local_queued_claim_candidates(None, CLAIM_CANDIDATE_LIMIT)
            .await?;
        let preflight = preflight_claim_candidates(candidates).await?;
        let outcome = self
            .claim_preflighted_candidates_with_global_capacity(limit, &preflight.eligible)
            .await?;
        Ok(outcome.claimed)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     阶段 C 必须在极短写事务内按全局容量 CAS 领取已 preflight 的候选，
    ///     保证并发 dispatch 不会重复 claim，且事务内零文件/YAML/路径 IO。
    ///
    /// Code Logic（这个函数做什么）:
    ///     begin 后重算 active local 槽位与 remaining；按 eligible 原序逐条
    ///     `UPDATE ... WHERE status/workflow_state/run_state` 且
    ///     `EXISTS(workbench_projects kind='local')`；命中签发新 UUID `prepare_claim_token` 并读回行。
    ///     begin 之后禁止 std::fs / Path::exists / YAML / project path SELECT。
    pub async fn claim_preflighted_candidates_with_global_capacity(
        &self,
        limit: i64,
        eligible: &[ClaimCandidate],
    ) -> Result<ClaimCasOutcome, AppError> {
        self.claim_preflighted_candidates_with_global_capacity_metrics(limit, eligible, None)
            .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     生产 scheduler 需要在 claim 短事务路径上记录 `db.acquire_wait_ms` /
    ///     `db.transaction_ms`，供扩池门槛与本地诊断复用同一指标面。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `begin_shared_write`（shared lease + begin）计时计入 acquire；commit 后记录
    ///     transaction 耗时；`metrics=None` 时行为与无埋点路径一致。
    pub async fn claim_preflighted_candidates_with_global_capacity_metrics(
        &self,
        limit: i64,
        eligible: &[ClaimCandidate],
        metrics: Option<&crate::backend::runtime_metrics::RuntimeMetrics>,
    ) -> Result<ClaimCasOutcome, AppError> {
        // shared lease + begin；acquire 等待与 begin 合并计入 db.acquire_wait_ms
        // （max_connections(1) 下不可先持 conn 再 pool.begin，会自锁）
        let acq_start = std::time::Instant::now();
        let (_permit, mut tx) = begin_shared_write(&self.pool, &self.gate).await?;
        if let Some(m) = metrics {
            m.measure_db_acquire(acq_start.elapsed());
        }
        let tx_start = std::time::Instant::now();
        if limit <= 0 || eligible.is_empty() {
            tx.commit().await?;
            if let Some(m) = metrics {
                m.measure_db_transaction(tx_start.elapsed());
            }
            return Ok(ClaimCasOutcome {
                claimed: Vec::new(),
                cas_miss: 0,
            });
        }

        let active = sqlx::query(
            "SELECT COUNT(*) AS count FROM orchestrator_tasks task \
             INNER JOIN workbench_projects project ON project.id = task.project_id \
             WHERE project.kind = 'local' AND task.run_state IN (?, ?, ?, ?)",
        )
        .bind(OrchestratorRunState::Preparing.as_str())
        .bind(OrchestratorRunState::Running.as_str())
        .bind(OrchestratorRunState::Verifying.as_str())
        .bind(OrchestratorRunState::Delivering.as_str())
        .fetch_one(&mut *tx)
        .await?
        .try_get::<i64, _>("count")?;
        let remaining = limit - active;
        if remaining <= 0 {
            tx.commit().await?;
            if let Some(m) = metrics {
                m.measure_db_transaction(tx_start.elapsed());
            }
            return Ok(ClaimCasOutcome {
                claimed: Vec::new(),
                cas_miss: 0,
            });
        }

        let mut claimed = Vec::new();
        let mut cas_miss = 0u64;
        for candidate in eligible {
            if claimed.len() >= remaining as usize {
                break;
            }
            let task_id = candidate.task.id.as_str();
            let now = Utc::now().to_rfc3339();
            // 每次 claim 签发新 token，使旧 runner 的 touch/mark_running CAS 全部失效。
            let claim_token = Uuid::new_v4().to_string();
            let result = sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                     blocked_reason = ?, prepare_claim_token = ?, updated_at = ? \
                 WHERE id = ? \
                   AND status = ? \
                   AND workflow_state = ? \
                   AND run_state = ? \
                   AND EXISTS (\
                     SELECT 1 FROM workbench_projects project \
                     WHERE project.id = orchestrator_tasks.project_id \
                       AND project.kind = 'local'\
                   )",
            )
            .bind(OrchestratorTaskStatus::Preparing.as_str())
            .bind(OrchestratorWorkflowState::InProgress.as_str())
            .bind(OrchestratorRunState::Preparing.as_str())
            .bind(OrchestratorAttemptPhase::PreparingWorkspace.as_str())
            .bind(Option::<&str>::None)
            .bind(&claim_token)
            .bind(now)
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Queued.as_str())
            .bind(candidate.task.workflow_state.as_str())
            .bind(OrchestratorRunState::Idle.as_str())
            .execute(&mut *tx)
            .await?;

            if result.rows_affected() != 1 {
                cas_miss = cas_miss.saturating_add(1);
                continue;
            }

            let updated = sqlx::query(&format!(
                "SELECT {TASK_COLUMNS} FROM orchestrator_tasks WHERE id = ?"
            ))
            .bind(task_id)
            .fetch_one(&mut *tx)
            .await?;
            claimed.push(row_to_task(&updated)?);
        }

        tx.commit().await?;
        if let Some(m) = metrics {
            m.measure_db_transaction(tx_start.elapsed());
        }
        Ok(ClaimCasOutcome { claimed, cas_miss })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     三阶段 claim 的阶段 A 需要在短 DB 读取中拿到有界 Queued/Idle local 候选快照，
    ///     且每行一次 JOIN 带出 project path，避免事务内逐候选查项目或无界 SELECT。
    ///
    /// Code Logic（这个函数做什么）:
    ///     不开启事务、不调用 workflow resolver。JOIN `workbench_projects` 过滤 `kind='local'`，
    ///     且 `status=queued`、`run_state=idle`。排序固定 `priority DESC, created_at ASC, id ASC`。
    ///     `limit` 绑定为 `min(requested, CLAIM_CANDIDATE_LIMIT)`；`cursor` 非空时使用 keyset 谓词继续翻页。
    ///     返回的每行 `ClaimCandidate` 已携带 JOIN 得到的 `project_path`。
    pub async fn list_local_queued_claim_candidates(
        &self,
        cursor: Option<&ClaimScanCursor>,
        limit: u32,
    ) -> Result<Vec<ClaimCandidate>, AppError> {
        let page_limit = limit.min(CLAIM_CANDIDATE_LIMIT);
        if page_limit == 0 {
            return Ok(Vec::new());
        }

        let task_columns = task_columns_for_alias("task");
        let base_sql = format!(
            "SELECT {task_columns}, project.path AS project_path \
             FROM orchestrator_tasks task \
             INNER JOIN workbench_projects project ON project.id = task.project_id \
             WHERE project.kind = 'local' \
               AND task.status = ? \
               AND task.run_state = ?"
        );

        let rows = if let Some(cursor) = cursor {
            // keyset: after (priority DESC, created_at ASC, id ASC)
            let sql = format!(
                "{base_sql} \
                 AND ( \
                   task.priority < ? \
                   OR (task.priority = ? AND task.created_at > ?) \
                   OR (task.priority = ? AND task.created_at = ? AND task.id > ?) \
                 ) \
                 ORDER BY task.priority DESC, task.created_at ASC, task.id ASC \
                 LIMIT ?"
            );
            sqlx::query(&sql)
                .bind(OrchestratorTaskStatus::Queued.as_str())
                .bind(OrchestratorRunState::Idle.as_str())
                .bind(cursor.priority)
                .bind(cursor.priority)
                .bind(&cursor.created_at)
                .bind(cursor.priority)
                .bind(&cursor.created_at)
                .bind(&cursor.id)
                .bind(page_limit as i64)
                .fetch_all(&self.pool)
                .await?
        } else {
            let sql = format!(
                "{base_sql} \
                 ORDER BY task.priority DESC, task.created_at ASC, task.id ASC \
                 LIMIT ?"
            );
            sqlx::query(&sql)
                .bind(OrchestratorTaskStatus::Queued.as_str())
                .bind(OrchestratorRunState::Idle.as_str())
                .bind(page_limit as i64)
                .fetch_all(&self.pool)
                .await?
        };

        let mut candidates = Vec::with_capacity(rows.len());
        for row in rows {
            let task = row_to_task(&row)?;
            let project_path: String = row.try_get("project_path")?;
            candidates.push(ClaimCandidate {
                task,
                project_path: PathBuf::from(project_path),
            });
        }
        Ok(candidates)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     claim 把任务批量挂到 Preparing 后若进程崩溃或 runner 启动前失败补偿中断，Preparing 会永久占用全局容量；
    ///     启动与每次 scheduler tick 必须回收过期 Preparing，否则槽位可被卡死。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 local 项目中 status=Preparing 且 run_state=Preparing、updated_at 早于 lease 截止的任务原子恢复为
    ///     legacy Blocked + workflow Rework + run Blocked（与 from_legacy_status(Blocked) 一致），
    ///     写入固定中文 blocked_reason 与 updated_at；返回恢复行数。
    ///     用户可通过 retry 回到 Queued/Idle 后再被 claim。
    ///     仍在 prepare 的 runner 必须周期 touch_preparing_lease 刷新 updated_at，否则长 git 步骤会被误回收。
    pub async fn recover_stale_local_preparing_tasks(
        &self,
        lease: Duration,
    ) -> Result<u64, AppError> {
        let lease = chrono::Duration::from_std(lease)
            .map_err(|err| AppError::generic(format!("Preparing lease 无效: {err}")))?;
        let cutoff = (Utc::now() - lease).to_rfc3339();
        let now = Utc::now().to_rfc3339();
        let reason = "Preparing 中断（进程崩溃或启动未完成），请重试";
        let blocked = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Blocked);
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, blocked_reason = ?, updated_at = ? \
                 WHERE status = ? AND run_state = ? AND updated_at < ? \
                   AND project_id IN (SELECT id FROM workbench_projects WHERE kind = 'local')",
            )
            .bind(OrchestratorTaskStatus::Blocked.as_str())
            .bind(blocked.workflow_state.as_str())
            .bind(blocked.run_state.as_str())
            .bind(OrchestratorAttemptPhase::Failed.as_str())
            .bind(reason)
            .bind(now)
            .bind(OrchestratorTaskStatus::Preparing.as_str())
            .bind(OrchestratorRunState::Preparing.as_str())
            .bind(cutoff)
            .execute(&self.pool)
            .await
        }).await?;
        Ok(result.rows_affected())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     git worktree / session 创建可能超过调度 lease；若不在 Preparing 阶段续租 updated_at，
    ///     并发 dispatch 会把仍在合法 prepare 的任务回收为 Blocked，原 runner 继续创建后 Running CAS 失败并留下孤儿现场。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 status=Preparing、run_state=Preparing 且 prepare_claim_token 匹配本轮 claim 时刷新 updated_at；
    ///     命中返回 true；状态已变或 token 不匹配（旧 runner）返回 false。
    pub async fn touch_preparing_lease(
        &self,
        task_id: &str,
        prepare_claim_token: &str,
    ) -> Result<bool, AppError> {
        let token = prepare_claim_token.trim();
        if token.is_empty() {
            return Err(AppError::generic("Preparing claim token 不能为空"));
        }
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks SET updated_at = ? \
                 WHERE id = ? AND status = ? AND run_state = ? AND prepare_claim_token = ?",
            )
            .bind(now)
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Preparing.as_str())
            .bind(OrchestratorRunState::Preparing.as_str())
            .bind(token)
            .execute(&self.pool)
            .await
        })
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     队列动作和状态机推进后需要只更新任务状态和阻塞原因，保持任务身份字段不变。
    ///
    /// Code Logic（这个函数做什么）:
    ///     只更新 status、blocked_reason 和 updated_at，再读取完整任务返回。
    pub async fn set_task_status(
        &self,
        task_id: &str,
        status: OrchestratorTaskStatus,
        blocked_reason: Option<&str>,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(status);
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
                 WHERE id = ?",
            )
            .bind(status.as_str())
            .bind(split_state.workflow_state.as_str())
            .bind(split_state.run_state.as_str())
            .bind(blocked_reason)
            .bind(now)
            .bind(task_id)
            .execute(&self.pool)
            .await
        }).await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     自动交付完整成功后，任务需要进入 Done；但用户可能在交付期间终止任务，Done 写入不能覆盖 Aborted。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当当前状态仍为 Delivering 时将 status 置为 done、清空 blocked_reason 并写 finished_at；
    ///     条件未命中时读取并返回当前任务，不改写终止或其它状态。
    pub async fn finish_task_done(&self, task_id: &str) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Done);
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ?, finished_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(OrchestratorTaskStatus::Done.as_str())
            .bind(split_state.workflow_state.as_str())
            .bind(split_state.run_state.as_str())
            .bind(Option::<&str>::None)
            .bind(&now)
            .bind(&now)
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Delivering.as_str())
            .execute(&self.pool)
            .await
        }).await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     自动交付失败后任务应进入 Blocked；但用户终止状态优先，失败兜底不得覆盖 Aborted。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当当前状态仍为 Delivering 时写入 Blocked 和 blocked_reason；未命中时返回当前任务。
    pub async fn block_task_if_delivering(
        &self,
        task_id: &str,
        reason: &str,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Blocked);
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(OrchestratorTaskStatus::Blocked.as_str())
            .bind(split_state.workflow_state.as_str())
            .bind(split_state.run_state.as_str())
            .bind(reason)
            .bind(now)
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Delivering.as_str())
            .execute(&self.pool)
            .await
        }).await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证阶段失败应进入 Blocked；但用户可能在验证运行期间终止任务，失败处理不能覆盖 Aborted。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当当前状态仍为 Verifying 时写入 Blocked 和 blocked_reason；未命中时返回当前任务。
    pub async fn block_task_if_verifying(
        &self,
        task_id: &str,
        reason: &str,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Blocked);
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(OrchestratorTaskStatus::Blocked.as_str())
            .bind(split_state.workflow_state.as_str())
            .bind(split_state.run_state.as_str())
            .bind(reason)
            .bind(now)
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Verifying.as_str())
            .execute(&self.pool)
            .await
        }).await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证成功后推进 Delivering 可能与用户 Abort 并发发生；调用方需要知道是否成功取得下一阶段执行权。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行 `UPDATE ... WHERE id=? AND status=?` 条件更新；命中返回 Some(更新后 Row)，
    ///     未命中时确认任务仍存在并返回 None，不把当前状态当作业务错误。
    pub async fn try_transition_task_status(
        &self,
        task_id: &str,
        expected_status: OrchestratorTaskStatus,
        next_status: OrchestratorTaskStatus,
        blocked_reason: Option<&str>,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(next_status);
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(next_status.as_str())
            .bind(split_state.workflow_state.as_str())
            .bind(split_state.run_state.as_str())
            .bind(blocked_reason)
            .bind(now)
            .bind(task_id)
            .bind(expected_status.as_str())
            .execute(&self.pool)
            .await
        }).await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await.map(Some);
        }

        self.get_task(task_id).await?;
        Ok(None)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier pass/fail 需要在保留 legacy status 原子守卫的同时写入新的 workflow/run/attempt split state。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行 `UPDATE ... WHERE id=? AND status=?`，命中时按调用方传入的 split state 与 attempt phase 更新；
    ///     未命中时确认任务仍存在并返回 None，避免迟到 verifier 覆盖 Abort 或其它并发状态。
    #[allow(clippy::too_many_arguments)]
    pub async fn try_transition_task_split_state(
        &self,
        task_id: &str,
        expected_status: OrchestratorTaskStatus,
        next_status: OrchestratorTaskStatus,
        workflow_state: OrchestratorWorkflowState,
        run_state: OrchestratorRunState,
        attempt_phase: Option<OrchestratorAttemptPhase>,
        blocked_reason: Option<&str>,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                     blocked_reason = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(next_status.as_str())
            .bind(workflow_state.as_str())
            .bind(run_state.as_str())
            .bind(attempt_phase.map(|phase| phase.as_str()))
            .bind(blocked_reason)
            .bind(now)
            .bind(task_id)
            .bind(expected_status.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await.map(Some);
        }

        self.get_task(task_id).await?;
        Ok(None)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier failed 自动修复轮必须原子地 Verifying→Preparing 并签发新 prepare_claim_token，
    ///     否则 prepare_runner_attempt 因空 token 直接失败并被误标 Blocked。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 status=Verifying 时写入 Preparing/Rework/Preparing/Failed phase，并签发 UUID claim token；
    ///     命中返回 Some(row)；未命中确认任务存在后返回 None。
    pub async fn try_transition_verifying_to_preparing_with_claim(
        &self,
        task_id: &str,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        let now = Utc::now().to_rfc3339();
        let claim_token = Uuid::new_v4().to_string();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                     blocked_reason = ?, prepare_claim_token = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(OrchestratorTaskStatus::Preparing.as_str())
            .bind(OrchestratorWorkflowState::Rework.as_str())
            .bind(OrchestratorRunState::Preparing.as_str())
            .bind(OrchestratorAttemptPhase::Failed.as_str())
            .bind(Option::<&str>::None)
            .bind(&claim_token)
            .bind(now)
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Verifying.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await.map(Some);
        }

        self.get_task(task_id).await?;
        Ok(None)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     terminal sentinel 来自某个具体 session/attempt，旧 session 的迟到哨兵不能推进当前 active runner。
    ///
    /// Code Logic（这个函数做什么）:
    ///     原子校验任务仍为 Running 且 active attempt/session 匹配后切到 Verifying；未命中确认任务存在并返回 None。
    pub async fn try_transition_running_attempt_to_verifying(
        &self,
        task_id: &str,
        attempt: i64,
        session_id: &str,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        if attempt <= 0 {
            return Err(AppError::generic("任务尝试轮次必须大于 0"));
        }
        if session_id.trim().is_empty() {
            return Err(AppError::generic("任务尝试缺少 session"));
        }

        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Verifying);
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
                 WHERE id = ? AND status = ? AND attempt = ? AND session_id = ?",
            )
            .bind(OrchestratorTaskStatus::Verifying.as_str())
            .bind(split_state.workflow_state.as_str())
            .bind(split_state.run_state.as_str())
            .bind(Option::<&str>::None)
            .bind(now)
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Running.as_str())
            .bind(attempt)
            .bind(session_id.trim())
            .execute(&self.pool)
            .await
        }).await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await.map(Some);
        }

        self.get_task(task_id).await?;
        Ok(None)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     完成验证、重试等用户动作可能与 scheduler 并发发生，状态推进必须在数据库内一次性校验当前状态，避免旧点击覆盖新状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行 `UPDATE ... WHERE id=? AND status=?` 条件更新；命中则返回更新后 Row，未命中时读取当前任务并返回中文业务错误，缺失任务沿用 not_found。
    pub async fn transition_task_status(
        &self,
        task_id: &str,
        expected_status: OrchestratorTaskStatus,
        next_status: OrchestratorTaskStatus,
        blocked_reason: Option<&str>,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(next_status);
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(next_status.as_str())
            .bind(split_state.workflow_state.as_str())
            .bind(split_state.run_state.as_str())
            .bind(blocked_reason)
            .bind(now)
            .bind(task_id)
            .bind(expected_status.as_str())
            .execute(&self.pool)
            .await
        }).await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await;
        }

        let current = self.get_task(task_id).await?;
        Err(AppError::generic(format!(
            "任务状态已变化，无法从 {} 切换到 {}，当前状态为 {}",
            expected_status.as_str(),
            next_status.as_str(),
            current.status.as_str()
        )))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户手动入队只应作用于草稿任务，避免运行中、已完成、阻塞或已终止任务被回退到队列。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用带 status=draft 条件的原子 UPDATE 切换到 queued；未更新时读取当前任务并返回中文业务错误。
    pub async fn queue_task(&self, task_id: &str) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Queued);
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(OrchestratorTaskStatus::Queued.as_str())
            .bind(split_state.workflow_state.as_str())
            .bind(split_state.run_state.as_str())
            .bind(Option::<&str>::None)
            .bind(now)
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Draft.as_str())
            .execute(&self.pool)
            .await
        }).await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await;
        }

        let current = self.get_task(task_id).await?;
        Err(AppError::generic(format!(
            "只有草稿任务可以入队，当前状态为 {}",
            current.status.as_str()
        )))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     显式 startTask 动作只负责把 Backlog/Draft 或 Todo/Idle 任务放入 scheduler 可领取路径，
    ///     后续是否立即启动 Runner 交给调度器按全局开关和容量 best-effort 决定。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先读取当前任务并校验允许的 split state；通过后用 status/workflow/run 三字段 CAS 写为 Queued + Todo/Idle，
    ///     清空 blocked_reason 和 attempt_phase，保留 worktree/session/evidence。
    pub async fn start_task(&self, task_id: &str) -> Result<OrchestratorTaskRow, AppError> {
        let current = self.get_task(task_id).await?;
        let can_start_from_backlog = current.status == OrchestratorTaskStatus::Draft
            && current.workflow_state == OrchestratorWorkflowState::Backlog
            && current.run_state == OrchestratorRunState::Idle;
        let can_start_from_todo = current.workflow_state == OrchestratorWorkflowState::Todo
            && current.run_state == OrchestratorRunState::Idle;
        if !can_start_from_backlog && !can_start_from_todo {
            return Err(AppError::generic(format!(
                "只有待整理草稿或待办空闲任务可以开始，当前 workflow={}, run={}, status={}",
                current.workflow_state.as_str(),
                current.run_state.as_str(),
                current.status.as_str()
            )));
        }

        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                     blocked_reason = ?, updated_at = ? \
                 WHERE id = ? AND status = ? AND workflow_state = ? AND run_state = ?",
            )
            .bind(OrchestratorTaskStatus::Queued.as_str())
            .bind(OrchestratorWorkflowState::Todo.as_str())
            .bind(OrchestratorRunState::Idle.as_str())
            .bind(Option::<&str>::None)
            .bind(Option::<&str>::None)
            .bind(now)
            .bind(&current.id)
            .bind(current.status.as_str())
            .bind(current.workflow_state.as_str())
            .bind(current.run_state.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(&current.id).await;
        }

        self.get_task(&current.id).await?;
        Err(AppError::generic("任务状态已变化，请刷新后重试"))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     人工复核未通过时，用户需要显式 requestRework 并留下原因，后续 scheduler 才能在同一现场继续领取任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅允许 legacy Done + workflow HumanReview + run Idle 的任务进入 Queued + Rework/Idle；
    ///     写入 repairPrompt evidence 和 rework event，保留 worktree/session/runtime 字段。
    pub async fn request_task_rework(
        &self,
        task_id: &str,
        reason: &str,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(AppError::generic("返工原因不能为空"));
        }
        let current = self.get_task(task_id).await?;
        if current.status != OrchestratorTaskStatus::Done
            || current.workflow_state != OrchestratorWorkflowState::HumanReview
            || current.run_state != OrchestratorRunState::Idle
        {
            return Err(AppError::generic(format!(
                "只有人工复核中的任务可以请求返工，当前 workflow={}, run={}, status={}",
                current.workflow_state.as_str(),
                current.run_state.as_str(),
                current.status.as_str()
            )));
        }

        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                     blocked_reason = ?, updated_at = ? \
                 WHERE id = ? AND status = ? AND workflow_state = ? AND run_state = ?",
            )
            .bind(OrchestratorTaskStatus::Queued.as_str())
            .bind(OrchestratorWorkflowState::Rework.as_str())
            .bind(OrchestratorRunState::Idle.as_str())
            .bind(OrchestratorAttemptPhase::Failed.as_str())
            .bind(Option::<&str>::None)
            .bind(now)
            .bind(&current.id)
            .bind(OrchestratorTaskStatus::Done.as_str())
            .bind(OrchestratorWorkflowState::HumanReview.as_str())
            .bind(OrchestratorRunState::Idle.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;

        if result.rows_affected() != 1 {
            self.get_task(&current.id).await?;
            return Err(AppError::generic("任务状态已变化，请刷新后重试"));
        }

        self.add_evidence(
            &current.id,
            EVIDENCE_KIND_REPAIR_PROMPT,
            "人工返工原因",
            "failed",
            reason,
        )
        .await?;
        self.add_event(&current.id, "rework", reason, None).await?;
        self.get_task(&current.id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     显式 deliverReviewedTask 只能从人工复核泳道取得交付执行权，避免普通完成态被误送入 Git side effect pipeline。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅允许 legacy Done + workflow HumanReview + run Idle 原子切到 Delivering + Merging/Delivering；
    ///     不清理 worktree/session，后续由 delivery pipeline 在 per-task lock 内处理 Git 阶段。
    pub async fn start_delivery_from_human_review(
        &self,
        task_id: &str,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let current = self.get_task(task_id).await?;
        if current.status != OrchestratorTaskStatus::Done
            || current.workflow_state != OrchestratorWorkflowState::HumanReview
            || current.run_state != OrchestratorRunState::Idle
        {
            return Err(AppError::generic(format!(
                "只有人工复核中的任务可以交付，当前 workflow={}, run={}, status={}",
                current.workflow_state.as_str(),
                current.run_state.as_str(),
                current.status.as_str()
            )));
        }

        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                     blocked_reason = ?, updated_at = ? \
                 WHERE id = ? AND status = ? AND workflow_state = ? AND run_state = ?",
            )
            .bind(OrchestratorTaskStatus::Delivering.as_str())
            .bind(OrchestratorWorkflowState::Merging.as_str())
            .bind(OrchestratorRunState::Delivering.as_str())
            .bind(Option::<&str>::None)
            .bind(Option::<&str>::None)
            .bind(now)
            .bind(&current.id)
            .bind(OrchestratorTaskStatus::Done.as_str())
            .bind(OrchestratorWorkflowState::HumanReview.as_str())
            .bind(OrchestratorRunState::Idle.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(&current.id).await;
        }

        self.get_task(&current.id).await?;
        Err(AppError::generic("任务状态已变化，请刷新后重试"))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     显式 cancelTask 表示用户不再希望任务继续被 scheduler 或 delivery 接管，但仍要保留现场和证据供人工审计。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 legacy status 写为 Aborted、split state 写为 Canceled/Idle，清空 blocked_reason 和 attempt_phase；
    ///     不修改 branch/worktree/session/runtime/evidence 字段。
    pub async fn cancel_task(&self, task_id: &str) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                     blocked_reason = ?, updated_at = ? \
                 WHERE id = ?",
            )
            .bind(OrchestratorTaskStatus::Aborted.as_str())
            .bind(OrchestratorWorkflowState::Canceled.as_str())
            .bind(OrchestratorRunState::Idle.as_str())
            .bind(Option::<&str>::None)
            .bind(Option::<&str>::None)
            .bind(now)
            .bind(task_id)
            .execute(&self.pool)
            .await
        })
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await;
        }

        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     看板拖拽是用户手动调整任务工作流阶段的轻量动作，必须避免隐式启动 Runner 或改变交付设置。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取当前任务，拒绝 active run_state，只允许移动到固定顺序中的相邻泳道；通过后仅更新 workflow_state 和 updated_at。
    pub async fn move_task_workflow_state(
        &self,
        task_id: &str,
        target: OrchestratorWorkflowState,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let current = self.get_task(task_id).await?;
        self.move_task_workflow_state_from_snapshot(&current, target)
            .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     拖拽移动需要基于用户看到的任务快照做条件更新，防止 scheduler 或其它动作在读写之间改变任务后被覆盖。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验快照 run_state 和相邻泳道后，用 workflow_state/run_state 作为 CAS 条件更新；未命中时读取最新任务返回中文业务错误。
    pub(crate) async fn move_task_workflow_state_from_snapshot(
        &self,
        current: &OrchestratorTaskRow,
        target: OrchestratorWorkflowState,
    ) -> Result<OrchestratorTaskRow, AppError> {
        if is_active_run_state(current.run_state) {
            return Err(AppError::generic("运行中的任务不能通过拖拽移动"));
        }

        let current_index = workflow_lane_index(current.workflow_state)?;
        let target_index = workflow_lane_index(target)?;
        if current_index.abs_diff(target_index) != 1 {
            return Err(AppError::generic("只能移动到相邻泳道"));
        }

        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET workflow_state = ?, updated_at = ? \
                 WHERE id = ? AND workflow_state = ? AND run_state = ?",
            )
            .bind(target.as_str())
            .bind(now)
            .bind(&current.id)
            .bind(current.workflow_state.as_str())
            .bind(current.run_state.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;

        if result.rows_affected() != 1 {
            let latest = self.get_task(&current.id).await?;
            if is_active_run_state(latest.run_state) {
                return Err(AppError::generic("运行中的任务不能通过拖拽移动"));
            }
            return Err(AppError::generic("任务状态已变化，请刷新后重试"));
        }

        self.get_task(&current.id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     早期调用点仍使用 update_task_status；保留兼容可避免后续计划任务重构被一次性阻塞。
    ///
    /// Code Logic（这个函数做什么）:
    ///     直接转发到 set_task_status，使旧 API 与新命名共享同一实现。
    pub async fn update_task_status(
        &self,
        task_id: &str,
        status: OrchestratorTaskStatus,
        blocked_reason: Option<&str>,
    ) -> Result<OrchestratorTaskRow, AppError> {
        self.set_task_status(task_id, status, blocked_reason).await
    }
}
