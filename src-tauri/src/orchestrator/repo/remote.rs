//! remote outbox/mirror
//!
//! Business Logic（为什么需要这个模块）:
//!     从 monofile 按职责拆分 OrchestratorRepo 方法，SQL 与公共签名不变。
//!
//! Code Logic（这个模块做什么）:
//!     为 `OrchestratorRepo` 提供对应 `impl` 方法块。

#![allow(dead_code)]
#![allow(unused_imports)]

use super::helpers::*;
use super::{IdempotentCreateTaskOutcome, OrchestratorRepo};
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
    ///     远端 P2P create 请求可能在 owning device 已创建任务后响应超时，客户端会用同一 clientRequestId 重试。
    ///     仓储必须把 requestId->taskId 与任务创建放在同一事务中，避免重复任务；
    ///     同时必须按 project 隔离幂等键，防止跨项目返回另一项目的任务内容。
    ///     调用方还需要区分“首次插入”与“幂等命中”，避免 Start 重放再次触发全局调度副作用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在事务内对非空 client_request_id：
    ///     1) 按 request_id 查找既有映射；
    ///     2) 映射的 project_id 与本次请求不一致 → conflict（跨项目不得复用）；
    ///     3) 同项目且 fingerprint 匹配 → 返回既有 task，`newly_created=false`；
    ///     4) 同项目但 fingerprint 为空或不同 → conflict（旧行空指纹 fail-closed）；
    ///     5) 首次登记写入 (request_id, project_id, fingerprint, task_id) 后插入任务，
    ///        返回 `newly_created=true`。
    pub async fn create_remote_task_idempotent(
        &self,
        client_request_id: Option<&str>,
        row: &OrchestratorTaskRow,
        create_action: OrchestratorCreateAction,
    ) -> Result<IdempotentCreateTaskOutcome, AppError> {
        let client_request_id = client_request_id.and_then(non_empty_trimmed);
        let row_to_insert = task_row_for_create_action(row, create_action);
        let external_labels_json = serialize_external_labels(&row_to_insert.external_labels)?;
        let request_fingerprint =
            create_request_fingerprint(&row_to_insert, create_action, &external_labels_json)?;
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
                    &row_to_insert.project_id,
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

            let now = Utc::now().to_rfc3339();
            let inserted = sqlx::query(
                "INSERT OR IGNORE INTO orchestrator_remote_task_create_requests \
                 (request_id, project_id, task_id, request_fingerprint, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(request_id)
            .bind(&row_to_insert.project_id)
            .bind(&row_to_insert.id)
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
                    &row_to_insert.project_id,
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

        sqlx::query(
            "INSERT INTO orchestrator_tasks \
             (id, project_id, title, goal, acceptance_criteria, status, priority, branch_name, \
              workflow_state, run_state, attempt_phase, source, external_id, external_identifier, \
              external_url, external_state, external_labels_json, runner_provider, claude_session_id, \
              agent_session_id, transcript_path, runtime_started_at, last_activity_at, last_runtime_event, \
              last_runtime_message, worktree_id, session_id, prepare_claim_token, blocked_reason, attempt, created_at, \
              updated_at, started_at, finished_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row_to_insert.id)
        .bind(&row_to_insert.project_id)
        .bind(&row_to_insert.title)
        .bind(&row_to_insert.goal)
        .bind(&row_to_insert.acceptance_criteria)
        .bind(row_to_insert.status.as_str())
        .bind(row_to_insert.priority)
        .bind(&row_to_insert.branch_name)
        .bind(row_to_insert.workflow_state.as_str())
        .bind(row_to_insert.run_state.as_str())
        .bind(row_to_insert.attempt_phase.map(OrchestratorAttemptPhase::as_str))
        .bind(&row_to_insert.source)
        .bind(&row_to_insert.external_id)
        .bind(&row_to_insert.external_identifier)
        .bind(&row_to_insert.external_url)
        .bind(&row_to_insert.external_state)
        .bind(&external_labels_json)
        .bind(&row_to_insert.runner_provider)
        .bind(&row_to_insert.claude_session_id)
        .bind(&row_to_insert.agent_session_id)
        .bind(&row_to_insert.transcript_path)
        .bind(&row_to_insert.runtime_started_at)
        .bind(&row_to_insert.last_activity_at)
        .bind(&row_to_insert.last_runtime_event)
        .bind(&row_to_insert.last_runtime_message)
        .bind(&row_to_insert.worktree_id)
        .bind(&row_to_insert.session_id)
        .bind(&row_to_insert.prepare_claim_token)
        .bind(&row_to_insert.blocked_reason)
        .bind(row_to_insert.attempt)
        .bind(&row_to_insert.created_at)
        .bind(&row_to_insert.updated_at)
        .bind(&row_to_insert.started_at)
        .bind(&row_to_insert.finished_at)
        .execute(&mut *tx)
        .await?;

        let task_row = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks WHERE id = ?"
        ))
        .bind(&row_to_insert.id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(IdempotentCreateTaskOutcome {
            task: row_to_task(&task_row)?,
            newly_created: true,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端 route 使用非空 clientRequestId 时，需要一个语义明确的仓储入口表达“按客户端请求幂等创建”。
    ///
    /// Code Logic（这个函数做什么）:
    ///     作为 create_remote_task_idempotent 的非空 request id 包装，复用同一个事务实现。
    pub async fn create_remote_task_for_client_request(
        &self,
        client_request_id: &str,
        row: &OrchestratorTaskRow,
        create_action: OrchestratorCreateAction,
    ) -> Result<IdempotentCreateTaskOutcome, AppError> {
        self.create_remote_task_idempotent(Some(client_request_id), row, create_action)
            .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端设备离线时，本机仍允许用户创建远端任务；创建请求必须先持久化为 pending outbox。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验目标设备、路径和请求 JSON 非空，生成 outbox id/时间戳，并插入 status=pending 行。
    pub async fn insert_remote_outbox_pending(
        &self,
        device_id: &str,
        device_name: &str,
        remote_project_path: &str,
        remote_project_id: Option<&str>,
        request_json: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let device_id = device_id.trim();
        let device_name = device_name.trim();
        let remote_project_path = remote_project_path.trim();
        let remote_project_id = remote_project_id.and_then(non_empty_trimmed);
        if device_id.is_empty() {
            return Err(AppError::generic("远端 outbox 缺少设备 ID"));
        }
        if device_name.is_empty() {
            return Err(AppError::generic("远端 outbox 缺少设备名称"));
        }
        if remote_project_path.is_empty() {
            return Err(AppError::generic("远端 outbox 缺少项目路径"));
        }
        if request_json.trim().is_empty() {
            return Err(AppError::generic("远端 outbox 请求不能为空"));
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO orchestrator_remote_outbox \
                 (id, device_id, device_name, remote_project_path, remote_project_id, request_json, \
                  status, remote_task_id, last_error, state_version, created_at, updated_at, sent_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(device_id)
            .bind(device_name)
            .bind(remote_project_path)
            .bind(remote_project_id)
            .bind(request_json)
            .bind(RemoteOutboxStatus::Pending.as_str())
            .bind(Option::<&str>::None)
            .bind(Option::<&str>::None)
            .bind(0_i64)
            .bind(&now)
            .bind(&now)
            .bind(Option::<&str>::None)
            .execute(&self.pool)
            .await
        }).await?;

        Ok(OrchestratorRemoteOutboxRow {
            id,
            device_id: device_id.to_string(),
            device_name: device_name.to_string(),
            remote_project_path: remote_project_path.to_string(),
            remote_project_id: remote_project_id.map(str::to_string),
            request_json: request_json.to_string(),
            status: RemoteOutboxStatus::Pending,
            remote_task_id: None,
            last_error: None,
            state_version: 0,
            created_at: now.clone(),
            updated_at: now,
            sent_at: None,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     命令层和 dispatcher 需要按 id 读取 outbox 当前状态，便于返回 UI 或处理并发 claim 失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询单条 outbox 行；缺失返回 None，存在时转换为强类型 Row。
    pub async fn get_remote_outbox_item(
        &self,
        item_id: &str,
    ) -> Result<Option<OrchestratorRemoteOutboxRow>, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {REMOTE_OUTBOX_COLUMNS} FROM orchestrator_remote_outbox WHERE id = ?"
        ))
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| row_to_remote_outbox(&row)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     后台 dispatcher 每个 tick 需要按创建顺序扫描尚未投递的 pending 远端任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询 status=pending 的 outbox 行，按 created_at/id 稳定排序并限制批量大小。
    pub async fn list_pending_remote_outbox_items(
        &self,
        limit: i64,
    ) -> Result<Vec<OrchestratorRemoteOutboxRow>, AppError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(&format!(
            "SELECT {REMOTE_OUTBOX_COLUMNS} FROM orchestrator_remote_outbox \
             WHERE status = ? ORDER BY created_at ASC, id ASC LIMIT ?"
        ))
        .bind(RemoteOutboxStatus::Pending.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_remote_outbox).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     应用在 outbox item 进入 sending 后崩溃时，后台 dispatcher 重启后必须释放旧 lease，避免任务永久卡在发送中。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 updated_at 早于 lease 截止时间的 sending 行恢复为 pending，写入 last_error 和 updated_at，返回恢复数量。
    pub async fn recover_stale_remote_outbox_sending_items(
        &self,
        lease: Duration,
    ) -> Result<u64, AppError> {
        let lease = chrono::Duration::from_std(lease)
            .map_err(|err| AppError::generic(format!("远端 outbox lease 无效: {err}")))?;
        let cutoff = (Utc::now() - lease).to_rfc3339();
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_remote_outbox \
                 SET status = ?, last_error = ?, updated_at = ? \
                 WHERE status = ? AND updated_at < ?",
            )
            .bind(RemoteOutboxStatus::Pending.as_str())
            .bind("sending lease expired; recovered for retry")
            .bind(now)
            .bind(RemoteOutboxStatus::Sending.as_str())
            .bind(cutoff)
            .execute(&self.pool)
            .await
        })
        .await?;
        Ok(result.rows_affected())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端项目任务列表需要合并本机尚未投递或投递失败的 outbox 项，避免用户看不到离线提交。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 device_id 和 remote_project_path 查询 pending/sending/failed 行；mirrored 行不再作为 pending 展示。
    pub async fn list_remote_outbox_items_for_project_path(
        &self,
        device_id: &str,
        remote_project_path: &str,
    ) -> Result<Vec<OrchestratorRemoteOutboxRow>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {REMOTE_OUTBOX_COLUMNS} FROM orchestrator_remote_outbox \
             WHERE device_id = ? AND remote_project_path = ? AND status IN (?, ?, ?) \
             ORDER BY created_at ASC, id ASC"
        ))
        .bind(device_id)
        .bind(remote_project_path)
        .bind(RemoteOutboxStatus::Pending.as_str())
        .bind(RemoteOutboxStatus::Sending.as_str())
        .bind(RemoteOutboxStatus::Failed.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_remote_outbox).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     dispatcher 必须先独占 claim 一条 pending item，避免多个 tick 或多个任务重复创建远端 task。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 `UPDATE ... WHERE status=pending` 原子切换到 sending；命中返回更新后 Row，未命中返回 None。
    pub async fn claim_remote_outbox_item_as_sending(
        &self,
        item_id: &str,
    ) -> Result<Option<OrchestratorRemoteOutboxRow>, AppError> {
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_remote_outbox SET status = ?, last_error = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(RemoteOutboxStatus::Sending.as_str())
            .bind(Option::<&str>::None)
            .bind(&now)
            .bind(item_id)
            .bind(RemoteOutboxStatus::Pending.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;

        if result.rows_affected() != 1 {
            return Ok(None);
        }
        self.get_remote_outbox_item(item_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")))
            .map(Some)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     网络离线、设备不在线或连接超时不代表请求无效，必须回到 pending 等待下次自动投递。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 sending item 改回 pending，写 last_error/updated_at，并返回当前 Row。
    pub async fn mark_remote_outbox_pending_after_network_failure(
        &self,
        item_id: &str,
        last_error: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_remote_outbox SET status = ?, last_error = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(RemoteOutboxStatus::Pending.as_str())
            .bind(last_error)
            .bind(now)
            .bind(item_id)
            .bind(RemoteOutboxStatus::Sending.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;
        self.get_remote_outbox_item(item_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端协议错误或业务校验错误不可自动重试，应停止 dispatcher 对该 item 的反复投递。
    ///     但旧投递路径晚到时不能覆盖已经恢复、成功镜像或人工处理过的 outbox 状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 item 仍为 sending 时标记 failed 并保存 last_error；非 sending 时不覆盖，返回当前 Row。
    pub async fn mark_remote_outbox_failed(
        &self,
        item_id: &str,
        last_error: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_remote_outbox \
                 SET status = ?, last_error = ?, state_version = state_version + 1, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(RemoteOutboxStatus::Failed.as_str())
            .bind(last_error)
            .bind(now)
            .bind(item_id)
            .bind(RemoteOutboxStatus::Sending.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;
        self.get_remote_outbox_item(item_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户在原 Automation UI 对协议/校验失败的 outbox 选择「重新发送」时，必须原子恢复为 pending，
    ///     并保留原 request payload 与 clientRequestId，避免创建重复远端任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅用 `WHERE id=? AND status='failed'` 更新 status=pending、清空 last_error、刷新 updated_at，
    ///     绝不改写 request_json；0 行更新时读取当前状态，缺失返回 not_found，其它状态返回 conflict。
    pub async fn retry_failed_remote_outbox_item(
        &self,
        item_id: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let item_id = item_id.trim();
        if item_id.is_empty() {
            return Err(AppError::validation("远端 outbox ID 不能为空"));
        }
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_remote_outbox \
                 SET status = ?, last_error = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(RemoteOutboxStatus::Pending.as_str())
            .bind(Option::<&str>::None)
            .bind(&now)
            .bind(item_id)
            .bind(RemoteOutboxStatus::Failed.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;

        if result.rows_affected() == 1 {
            return self
                .get_remote_outbox_item(item_id)
                .await?
                .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")));
        }

        match self.get_remote_outbox_item(item_id).await? {
            None => Err(AppError::not_found(format!(
                "远端 outbox 不存在: {item_id}"
            ))),
            Some(current) => Err(AppError::conflict(format!(
                "只有失败的远端 outbox 可以重新发送，当前状态为 {}",
                current.status.as_str()
            ))),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户确认放弃某条失败 outbox 后，条目应进入 discarded 终态：保留审计与 last_error，
    ///     但不再参与 dispatcher、active 列表或 Attention 投影。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅用 `WHERE id=? AND status='failed'` 更新 status=discarded 与 updated_at，保留 last_error/request_json；
    ///     0 行更新时读取当前状态，缺失返回 not_found，其它状态返回 conflict。
    pub async fn discard_failed_remote_outbox_item(
        &self,
        item_id: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let item_id = item_id.trim();
        if item_id.is_empty() {
            return Err(AppError::validation("远端 outbox ID 不能为空"));
        }
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_remote_outbox \
                 SET status = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(RemoteOutboxStatus::Discarded.as_str())
            .bind(&now)
            .bind(item_id)
            .bind(RemoteOutboxStatus::Failed.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;

        if result.rows_affected() == 1 {
            return self
                .get_remote_outbox_item(item_id)
                .await?
                .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")));
        }

        match self.get_remote_outbox_item(item_id).await? {
            None => Err(AppError::not_found(format!(
                "远端 outbox 不存在: {item_id}"
            ))),
            Some(current) => Err(AppError::conflict(format!(
                "只有失败的远端 outbox 可以放弃发送，当前状态为 {}",
                current.status.as_str()
            ))),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     dispatcher 打开远端项目后会拿到远端 local projectId，outbox 行应记录该映射便于后续排查和 UI 展示。
    ///
    /// Code Logic（这个函数做什么）:
    ///     更新 remote_project_id 与 updated_at；不改变投递状态。
    pub async fn update_remote_outbox_remote_project_id(
        &self,
        item_id: &str,
        remote_project_id: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_remote_outbox SET remote_project_id = ?, updated_at = ? \
                 WHERE id = ?",
            )
            .bind(remote_project_id)
            .bind(now)
            .bind(item_id)
            .execute(&self.pool)
            .await
        })
        .await?;
        self.get_remote_outbox_item(item_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     旧 outbox 行可能缺少 clientRequestId；dispatcher 补齐幂等键后需要持久化，确保后续重试复用同一请求体。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 item 仍为 sending 时更新 request_json 和 updated_at；并发状态变化时返回 None。
    pub async fn update_remote_outbox_request_json_if_sending(
        &self,
        item_id: &str,
        request_json: &str,
    ) -> Result<Option<OrchestratorRemoteOutboxRow>, AppError> {
        if request_json.trim().is_empty() {
            return Err(AppError::generic("远端 outbox 请求不能为空"));
        }
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_remote_outbox SET request_json = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(request_json)
            .bind(now)
            .bind(item_id)
            .bind(RemoteOutboxStatus::Sending.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;
        if result.rows_affected() != 1 {
            return Ok(None);
        }
        self.get_remote_outbox_item(item_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端任务创建成功后，本机 pending item 应转为 mirrored 并保存远端 task id，避免后续重复投递。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写 status=mirrored、remote_task_id、sent_at 和 updated_at，清空 last_error 后返回当前 Row。
    pub async fn mark_remote_outbox_mirrored(
        &self,
        item_id: &str,
        remote_task_id: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_remote_outbox \
                 SET status = ?, remote_task_id = ?, last_error = ?, updated_at = ?, sent_at = ? \
                 WHERE id = ?",
            )
            .bind(RemoteOutboxStatus::Mirrored.as_str())
            .bind(remote_task_id)
            .bind(Option::<&str>::None)
            .bind(&now)
            .bind(&now)
            .bind(item_id)
            .execute(&self.pool)
            .await
        })
        .await?;
        self.get_remote_outbox_item(item_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     成功投递后，outbox mirrored 状态、remote_project_id 和 mirror payload 必须同生共死；
    ///     否则 UI 可能看不到已创建的远端任务，或 outbox 显示已完成但 mirror 丢失。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在单个 SQLite 事务里仅对 status=sending 的 item 写 mirrored/sent_at/remote_task_id/remote_project_id，
    ///     并按 `(device_id, remote_task_id)` upsert mirror；状态不再是 sending 时返回 None 且不覆盖。
    #[allow(clippy::too_many_arguments)]
    pub async fn mark_remote_outbox_mirrored_and_upsert_mirror_if_sending(
        &self,
        item_id: &str,
        device_id: &str,
        device_name: &str,
        remote_project_id: &str,
        remote_project_path: &str,
        remote_task_id: &str,
        payload_json: &str,
    ) -> Result<Option<OrchestratorRemoteOutboxRow>, AppError> {
        let item_id = item_id.trim();
        let device_id = device_id.trim();
        let device_name = device_name.trim();
        let remote_project_id = remote_project_id.trim();
        let remote_project_path = remote_project_path.trim();
        let remote_task_id = remote_task_id.trim();
        if item_id.is_empty() {
            return Err(AppError::generic("远端 outbox 缺少 ID"));
        }
        if device_id.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少设备 ID"));
        }
        if device_name.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少设备名称"));
        }
        if remote_project_id.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少项目 ID"));
        }
        if remote_project_path.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少项目路径"));
        }
        if remote_task_id.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少任务 ID"));
        }
        if payload_json.trim().is_empty() {
            return Err(AppError::generic("远端任务镜像 payload 不能为空"));
        }

        let (_permit, mut tx) = begin_shared_write(&self.pool, &self.gate).await?;
        let now = Utc::now().to_rfc3339();
        let updated = sqlx::query(
            "UPDATE orchestrator_remote_outbox \
             SET remote_project_id = ?, status = ?, remote_task_id = ?, last_error = ?, \
                 updated_at = ?, sent_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(remote_project_id)
        .bind(RemoteOutboxStatus::Mirrored.as_str())
        .bind(remote_task_id)
        .bind(Option::<&str>::None)
        .bind(&now)
        .bind(&now)
        .bind(item_id)
        .bind(RemoteOutboxStatus::Sending.as_str())
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            tx.commit().await?;
            return Ok(None);
        }

        let mirror_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO orchestrator_remote_task_mirrors \
             (id, device_id, device_name, remote_project_id, remote_project_path, remote_task_id, \
              payload_json, last_synced_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(device_id, remote_task_id) DO UPDATE SET \
               device_name = excluded.device_name, \
               remote_project_id = excluded.remote_project_id, \
               remote_project_path = excluded.remote_project_path, \
               payload_json = excluded.payload_json, \
               last_synced_at = excluded.last_synced_at",
        )
        .bind(&mirror_id)
        .bind(device_id)
        .bind(device_name)
        .bind(remote_project_id)
        .bind(remote_project_path)
        .bind(remote_task_id)
        .bind(payload_json)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        let row = sqlx::query(&format!(
            "SELECT {REMOTE_OUTBOX_COLUMNS} FROM orchestrator_remote_outbox WHERE id = ?"
        ))
        .bind(item_id)
        .fetch_one(&mut *tx)
        .await?;
        let item = row_to_remote_outbox(&row)?;
        tx.commit().await?;
        Ok(Some(item))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     本机需要保存远端任务的最新展示快照；同一远端 task 重复同步时应覆盖旧 payload。
    ///
    /// Code Logic（这个函数做什么）:
    ///     使用 `(device_id, remote_task_id)` 唯一键 upsert mirror，更新 payload_json 和 last_synced_at 后返回 Row。
    pub async fn upsert_remote_task_mirror(
        &self,
        device_id: &str,
        device_name: &str,
        remote_project_id: &str,
        remote_project_path: &str,
        remote_task_id: &str,
        payload_json: &str,
    ) -> Result<RemoteMirrorTask, AppError> {
        let device_id = device_id.trim();
        let device_name = device_name.trim();
        let remote_project_id = remote_project_id.trim();
        let remote_project_path = remote_project_path.trim();
        let remote_task_id = remote_task_id.trim();
        if device_id.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少设备 ID"));
        }
        if device_name.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少设备名称"));
        }
        if remote_project_id.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少项目 ID"));
        }
        if remote_project_path.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少项目路径"));
        }
        if remote_task_id.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少任务 ID"));
        }
        if payload_json.trim().is_empty() {
            return Err(AppError::generic("远端任务镜像 payload 不能为空"));
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO orchestrator_remote_task_mirrors \
                 (id, device_id, device_name, remote_project_id, remote_project_path, remote_task_id, \
                  payload_json, last_synced_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(device_id, remote_task_id) DO UPDATE SET \
                   device_name = excluded.device_name, \
                   remote_project_id = excluded.remote_project_id, \
                   remote_project_path = excluded.remote_project_path, \
                   payload_json = excluded.payload_json, \
                   last_synced_at = excluded.last_synced_at",
            )
            .bind(&id)
            .bind(device_id)
            .bind(device_name)
            .bind(remote_project_id)
            .bind(remote_project_path)
            .bind(remote_task_id)
            .bind(payload_json)
            .bind(&now)
            .execute(&self.pool)
            .await
        }).await?;
        self.get_remote_task_mirror(device_id, remote_task_id)
            .await?
            .ok_or_else(|| AppError::not_found("远端任务镜像不存在"))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     mirror upsert 和远端任务详情需要按设备与远端 task id 读取唯一镜像。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询 `(device_id, remote_task_id)` 唯一行，缺失返回 None。
    pub async fn get_remote_task_mirror(
        &self,
        device_id: &str,
        remote_task_id: &str,
    ) -> Result<Option<RemoteMirrorTask>, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {REMOTE_MIRROR_COLUMNS} FROM orchestrator_remote_task_mirrors \
             WHERE device_id = ? AND remote_task_id = ?"
        ))
        .bind(device_id)
        .bind(remote_task_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| row_to_remote_mirror(&row)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端项目在线刷新后，UI 需要读取该项目所有已缓存的远端任务镜像。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 device_id + remote_project_id 查询 mirror，按 last_synced_at/id 稳定排序。
    pub async fn list_remote_task_mirrors_for_project(
        &self,
        device_id: &str,
        remote_project_id: &str,
    ) -> Result<Vec<RemoteMirrorTask>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {REMOTE_MIRROR_COLUMNS} FROM orchestrator_remote_task_mirrors \
             WHERE device_id = ? AND remote_project_id = ? ORDER BY last_synced_at DESC, id ASC"
        ))
        .bind(device_id)
        .bind(remote_project_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_remote_mirror).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端离线时可能无法拿到最新 remote_project_id，本机仍应按保存的远端路径展示最近镜像快照。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 device_id + remote_project_path 查询 mirror，按 last_synced_at/id 稳定排序。
    pub async fn list_remote_task_mirrors_for_project_path(
        &self,
        device_id: &str,
        remote_project_path: &str,
    ) -> Result<Vec<RemoteMirrorTask>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {REMOTE_MIRROR_COLUMNS} FROM orchestrator_remote_task_mirrors \
             WHERE device_id = ? AND remote_project_path = ? ORDER BY last_synced_at DESC, id ASC"
        ))
        .bind(device_id)
        .bind(remote_project_path)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_remote_mirror).collect()
    }
}
