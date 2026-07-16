//! 组级远端 experiment outbox。
//!
//! Business Logic（为什么需要这个模块）:
//!     owner 离线时必须写一条 experiment outbox，而非 N 条 task outbox；
//!     重试与 lost ACK 必须原子复用同一 remote experiment。
//!
//! Code Logic（这个模块做什么）:
//!     enqueue / claim sending / mark mirrored|failed / list pending。

use crate::error::AppError;
use crate::orchestrator::experiments::models::CreateExperimentRequest;
use crate::orchestrator::repo::OrchestratorRepo;
use crate::storage::maintenance_gate::with_shared_write_lease;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

/// outbox 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExperimentOutboxStatus {
    Pending,
    Sending,
    Mirrored,
    Failed,
}

impl ExperimentOutboxStatus {
    /// Business Logic（为什么需要这个函数）:
    ///     持久化状态字符串。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回小写字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sending => "sending",
            Self::Mirrored => "mirrored",
            Self::Failed => "failed",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     读取时 fail-closed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析状态。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Result<Self, AppError> {
        match value {
            "pending" => Ok(Self::Pending),
            "sending" => Ok(Self::Sending),
            "mirrored" => Ok(Self::Mirrored),
            "failed" => Ok(Self::Failed),
            other => Err(AppError::generic(format!(
                "未知 experiment outbox 状态: {other}"
            ))),
        }
    }
}

/// outbox 行。
#[derive(Debug, Clone)]
pub struct ExperimentOutboxRow {
    pub id: String,
    pub device_id: String,
    pub device_name: String,
    pub remote_project_path: String,
    pub remote_project_id: Option<String>,
    pub request_json: String,
    pub status: ExperimentOutboxStatus,
    pub remote_experiment_id: Option<String>,
    pub last_error: Option<String>,
    pub state_version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub sent_at: Option<String>,
}

impl OrchestratorRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     离线创建写一条组级 outbox。
    ///
    /// Code Logic（这个函数做什么）:
    ///     INSERT pending 行，request_json 为 CreateExperimentRequest。
    pub async fn enqueue_experiment_outbox(
        &self,
        device_id: &str,
        device_name: &str,
        remote_project_path: &str,
        remote_project_id: Option<&str>,
        request: &CreateExperimentRequest,
    ) -> Result<ExperimentOutboxRow, AppError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let request_json = serde_json::to_string(request)?;
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO orchestrator_remote_experiment_outbox \
                 (id, device_id, device_name, remote_project_path, remote_project_id, request_json, \
                  status, remote_experiment_id, last_error, state_version, created_at, updated_at, sent_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, 0, ?, ?, NULL)",
            )
            .bind(&id)
            .bind(device_id)
            .bind(device_name)
            .bind(remote_project_path)
            .bind(remote_project_id)
            .bind(&request_json)
            .bind(ExperimentOutboxStatus::Pending.as_str())
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await
        })
        .await?;
        self.get_experiment_outbox(&id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     dispatcher 读取 outbox 行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT by id。
    pub async fn get_experiment_outbox(
        &self,
        id: &str,
    ) -> Result<ExperimentOutboxRow, AppError> {
        let row = sqlx::query(
            "SELECT id, device_id, device_name, remote_project_path, remote_project_id, request_json, \
             status, remote_experiment_id, last_error, state_version, created_at, updated_at, sent_at \
             FROM orchestrator_remote_experiment_outbox WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::not_found(format!("experiment outbox 不存在: {id}")))?;
        row_to_outbox(&row)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     dispatcher 列出 pending。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT status=pending ORDER BY created_at。
    pub async fn list_pending_experiment_outbox(
        &self,
    ) -> Result<Vec<ExperimentOutboxRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, device_id, device_name, remote_project_path, remote_project_id, request_json, \
             status, remote_experiment_id, last_error, state_version, created_at, updated_at, sent_at \
             FROM orchestrator_remote_experiment_outbox WHERE status = ? \
             ORDER BY created_at ASC, id ASC",
        )
        .bind(ExperimentOutboxStatus::Pending.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_outbox).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     成功后标记 mirrored 并记录 remote experiment id。
    ///
    /// Code Logic（这个函数做什么）:
    ///     UPDATE status=mirrored + remote_experiment_id。
    pub async fn mark_experiment_outbox_mirrored(
        &self,
        id: &str,
        remote_experiment_id: &str,
    ) -> Result<(), AppError> {
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_remote_experiment_outbox \
                 SET status = ?, remote_experiment_id = ?, last_error = NULL, \
                     updated_at = ?, sent_at = ?, state_version = state_version + 1 \
                 WHERE id = ?",
            )
            .bind(ExperimentOutboxStatus::Mirrored.as_str())
            .bind(remote_experiment_id)
            .bind(&now)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await
        })
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     协议失败标记 failed；网络失败回 pending。
    ///
    /// Code Logic（这个函数做什么）:
    ///     UPDATE status + last_error。
    pub async fn mark_experiment_outbox_status(
        &self,
        id: &str,
        status: ExperimentOutboxStatus,
        last_error: Option<&str>,
    ) -> Result<(), AppError> {
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_remote_experiment_outbox \
                 SET status = ?, last_error = ?, updated_at = ?, state_version = state_version + 1 \
                 WHERE id = ?",
            )
            .bind(status.as_str())
            .bind(last_error)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await
        })
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     成功响应后 upsert 本机 mirror。
    ///
    /// Code Logic（这个函数做什么）:
    ///     INSERT OR REPLACE mirror payload。
    pub async fn upsert_experiment_mirror(
        &self,
        device_id: &str,
        device_name: &str,
        remote_project_id: &str,
        remote_project_path: &str,
        remote_experiment_id: &str,
        payload_json: &str,
    ) -> Result<(), AppError> {
        let id = format!("{device_id}:{remote_experiment_id}");
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO orchestrator_remote_experiment_mirrors \
                 (id, device_id, device_name, remote_project_id, remote_project_path, \
                  remote_experiment_id, payload_json, last_synced_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(device_id, remote_experiment_id) DO UPDATE SET \
                   payload_json = excluded.payload_json, \
                   last_synced_at = excluded.last_synced_at, \
                   device_name = excluded.device_name, \
                   remote_project_id = excluded.remote_project_id, \
                   remote_project_path = excluded.remote_project_path",
            )
            .bind(&id)
            .bind(device_id)
            .bind(device_name)
            .bind(remote_project_id)
            .bind(remote_project_path)
            .bind(remote_experiment_id)
            .bind(payload_json)
            .bind(&now)
            .execute(&self.pool)
            .await
        })
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     dispatcher 需要条件领取 pending experiment outbox，避免双投递。
    ///
    /// Code Logic（这个函数做什么）:
    ///     UPDATE pending → sending WHERE id；命中则返回行。
    pub async fn claim_experiment_outbox_as_sending(
        &self,
        id: &str,
    ) -> Result<Option<ExperimentOutboxRow>, AppError> {
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_remote_experiment_outbox \
                 SET status = ?, updated_at = ?, state_version = state_version + 1 \
                 WHERE id = ? AND status = ?",
            )
            .bind(ExperimentOutboxStatus::Sending.as_str())
            .bind(&now)
            .bind(id)
            .bind(ExperimentOutboxStatus::Pending.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;
        if result.rows_affected() != 1 {
            return Ok(None);
        }
        Ok(Some(self.get_experiment_outbox(id).await?))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     owner 离线时桌面仍需展示最近同步的实验组。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 device_id + remote_project_path 读 mirrors 并反序列化 DTO。
    pub async fn list_experiment_mirrors_for_project_path(
        &self,
        device_id: &str,
        remote_project_path: &str,
    ) -> Result<Vec<crate::orchestrator::experiments::models::OrchestratorExperimentDto>, AppError>
    {
        let rows = sqlx::query(
            "SELECT payload_json FROM orchestrator_remote_experiment_mirrors \
             WHERE device_id = ? AND remote_project_path = ? \
             ORDER BY last_synced_at DESC, remote_experiment_id ASC",
        )
        .bind(device_id)
        .bind(remote_project_path)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let payload: String = row.try_get("payload_json")?;
            match serde_json::from_str(&payload) {
                Ok(dto) => out.push(dto),
                Err(err) => {
                    tracing::warn!("experiment mirror payload 解析失败，跳过: {err}");
                }
            }
        }
        Ok(out)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     本机无权威 experiment 行时，详情/批准需从 mirror 回读 DTO。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 remote_experiment_id 取一条 payload。
    pub async fn get_experiment_mirror_by_remote_id(
        &self,
        remote_experiment_id: &str,
    ) -> Result<
        Option<crate::orchestrator::experiments::models::OrchestratorExperimentDto>,
        AppError,
    > {
        let row = sqlx::query(
            "SELECT payload_json FROM orchestrator_remote_experiment_mirrors \
             WHERE remote_experiment_id = ? ORDER BY last_synced_at DESC LIMIT 1",
        )
        .bind(remote_experiment_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let payload: String = row.try_get("payload_json")?;
        Ok(Some(serde_json::from_str(&payload)?))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     remote 批准/取消需要 mirror 上的 device 与路径信息。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 device_id/name 与 remote project 字段。
    pub async fn get_experiment_mirror_meta(
        &self,
        remote_experiment_id: &str,
    ) -> Result<Option<ExperimentMirrorMeta>, AppError> {
        let row = sqlx::query(
            "SELECT device_id, device_name, remote_project_id, remote_project_path \
             FROM orchestrator_remote_experiment_mirrors \
             WHERE remote_experiment_id = ? ORDER BY last_synced_at DESC LIMIT 1",
        )
        .bind(remote_experiment_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(ExperimentMirrorMeta {
            device_id: row.try_get("device_id")?,
            device_name: row.try_get("device_name")?,
            remote_project_id: row.try_get("remote_project_id")?,
            remote_project_path: row.try_get("remote_project_path")?,
        }))
    }
}

/// mirror 元数据（不含 payload）。
///
/// Business Logic（为什么需要这个结构体）:
///     remote 动作需要定位 owning device，不必反序列化完整 experiment。
///
/// Code Logic（这个结构体做什么）:
///     保存 device/project 路由字段。
#[derive(Debug, Clone)]
pub struct ExperimentMirrorMeta {
    pub device_id: String,
    pub device_name: String,
    pub remote_project_id: String,
    pub remote_project_path: String,
}

/// Business Logic（为什么需要这个函数）:
///     后台必须把 pending experiment outbox 原子投递到 owning device，与 task outbox 同 tick。
///
/// Code Logic（这个函数做什么）:
///     list pending → claim sending → open remote project → create_remote_experiment →
///     mark mirrored + upsert mirror；网络错误回 pending，协议错误标 failed。
pub async fn dispatch_experiment_outbox_once(
    state: &crate::state::AppState,
) -> Result<usize, AppError> {
    use crate::orchestrator::experiments::remote_client::create_remote_experiment;
    use crate::orchestrator::outbox::{
        is_remote_network_error, open_remote_project_for_shortcut,
    };
    use crate::workbench::models::WorkbenchProjectRow;

    let pending = state
        .orchestrator_repo
        .list_pending_experiment_outbox()
        .await?;
    let mut dispatched = 0usize;
    let http = reqwest::Client::new();

    for item in pending {
        let Some(claimed) = state
            .orchestrator_repo
            .claim_experiment_outbox_as_sending(&item.id)
            .await?
        else {
            continue;
        };

        let request: CreateExperimentRequest = match serde_json::from_str(&claimed.request_json) {
            Ok(req) => req,
            Err(err) => {
                let _ = state
                    .orchestrator_repo
                    .mark_experiment_outbox_status(
                        &claimed.id,
                        ExperimentOutboxStatus::Failed,
                        Some(&format!("experiment outbox 请求解析失败: {err}")),
                    )
                    .await;
                continue;
            }
        };

        let shortcut = WorkbenchProjectRow {
            id: String::new(),
            name: claimed.remote_project_path.clone(),
            kind: "remote".to_string(),
            device_id: claimed.device_id.clone(),
            device_name: claimed.device_name.clone(),
            path: claimed.remote_project_path.clone(),
            last_opened_at: claimed.updated_at.clone(),
            created_at: claimed.created_at.clone(),
            updated_at: claimed.updated_at.clone(),
        };

        let context = match open_remote_project_for_shortcut(state, &shortcut, None).await {
            Ok(ctx) => ctx,
            Err(err) if is_remote_network_error(&err) => {
                let _ = state
                    .orchestrator_repo
                    .mark_experiment_outbox_status(
                        &claimed.id,
                        ExperimentOutboxStatus::Pending,
                        Some(&err.to_string()),
                    )
                    .await;
                continue;
            }
            Err(err) => {
                let _ = state
                    .orchestrator_repo
                    .mark_experiment_outbox_status(
                        &claimed.id,
                        ExperimentOutboxStatus::Failed,
                        Some(&err.to_string()),
                    )
                    .await;
                continue;
            }
        };

        let mut request = request;
        request.project_id = context.remote_project_id.clone();

        match create_remote_experiment(
            state.peer_client.as_ref(),
            &http,
            &context.base_url,
            &request,
        )
        .await
        {
            Ok(resp) => {
                if let Ok(payload) = serde_json::to_string(&resp.experiment) {
                    let _ = state
                        .orchestrator_repo
                        .upsert_experiment_mirror(
                            &claimed.device_id,
                            &claimed.device_name,
                            &context.remote_project_id,
                            &context.remote_project_path,
                            &resp.experiment.id,
                            &payload,
                        )
                        .await;
                }
                state
                    .orchestrator_repo
                    .mark_experiment_outbox_mirrored(&claimed.id, &resp.experiment.id)
                    .await?;
                dispatched = dispatched.saturating_add(1);
            }
            Err(err) if is_remote_network_error(&err) => {
                let _ = state
                    .orchestrator_repo
                    .mark_experiment_outbox_status(
                        &claimed.id,
                        ExperimentOutboxStatus::Pending,
                        Some(&err.to_string()),
                    )
                    .await;
            }
            Err(err) => {
                let _ = state
                    .orchestrator_repo
                    .mark_experiment_outbox_status(
                        &claimed.id,
                        ExperimentOutboxStatus::Failed,
                        Some(&err.to_string()),
                    )
                    .await;
            }
        }
    }

    Ok(dispatched)
}

/// Business Logic（为什么需要这个函数）:
///     统一 outbox 行映射。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 组装 ExperimentOutboxRow。
fn row_to_outbox(row: &sqlx::sqlite::SqliteRow) -> Result<ExperimentOutboxRow, AppError> {
    let status_text: String = row.try_get("status")?;
    Ok(ExperimentOutboxRow {
        id: row.try_get("id")?,
        device_id: row.try_get("device_id")?,
        device_name: row.try_get("device_name")?,
        remote_project_path: row.try_get("remote_project_path")?,
        remote_project_id: row.try_get("remote_project_id")?,
        request_json: row.try_get("request_json")?,
        status: ExperimentOutboxStatus::from_str(&status_text)?,
        remote_experiment_id: row.try_get("remote_experiment_id")?,
        last_error: row.try_get("last_error")?,
        state_version: row.try_get("state_version").unwrap_or(0),
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        sent_at: row.try_get("sent_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::experiments::create::request_with_candidates;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup() -> OrchestratorRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        OrchestratorRepo::init_schema(&pool).await.unwrap();
        OrchestratorRepo::new(pool)
    }

    /// Business Logic（为什么需要这个测试）:
    ///     离线 enqueue + mirrored 路径必须保留同一 remote id。
    #[tokio::test]
    async fn retry_after_lost_response_reuses_same_remote_experiment() {
        let repo = setup().await;
        let req = request_with_candidates(3);
        let item = repo
            .enqueue_experiment_outbox("dev", "Device", "/proj", Some("rp1"), &req)
            .await
            .unwrap();
        assert_eq!(item.status, ExperimentOutboxStatus::Pending);
        // 模拟成功：两次 mark mirrored 同一 id
        repo.mark_experiment_outbox_mirrored(&item.id, "remote-exp-1")
            .await
            .unwrap();
        repo.mark_experiment_outbox_mirrored(&item.id, "remote-exp-1")
            .await
            .unwrap();
        let again = repo.get_experiment_outbox(&item.id).await.unwrap();
        assert_eq!(again.status, ExperimentOutboxStatus::Mirrored);
        assert_eq!(again.remote_experiment_id.as_deref(), Some("remote-exp-1"));
        repo.upsert_experiment_mirror(
            "dev",
            "Device",
            "rp1",
            "/proj",
            "remote-exp-1",
            r#"{"id":"remote-exp-1","status":"queued"}"#,
        )
        .await
        .unwrap();
        // 仍只有一条 outbox
        let pending = repo.list_pending_experiment_outbox().await.unwrap();
        assert!(pending.is_empty());
    }
}
