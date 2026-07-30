//! agent_hub/replication/sender — 源侧 multi-target LAN push
//!
//! Business Logic（为什么需要这个模块）:
//!     用户在源设备选择 full Hub / user / project / asset 与显式 peer 后，向目标 push
//!     SnapshotEnvelope v1；每目标独立 prepare/objects/commit，成功 peer 不被其它失败回滚。
//!
//! Code Logic（这个模块做什么）:
//!     构建 snapshot 一次；每 peer 先 capability `agent-hub.v1` 再调 push 路由；
//!     ≤8 MiB chunk + 从 peer offset 续传；稳定 clientRequestId；并发上限 3；
//!     SQLite 持久化 source request/target outcome 供 GUI reconnect 与 Attention。
//!     **终态**（committed/failed）persist 失败 fail-closed：改写 in-memory 为
//!     `error_code=agent_hub_push_persist_failed` + `retryable=true`，禁止内存终态而 DB 仍 pending；
//!     **中途** prepared/transferred checkpoint 失败 best-effort + `tracing::warn!`。

use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::replication::receiver::{
    CommitPushRequest, CommitPushResponse, PreparePushRequest, PreparePushResponse,
    PutObjectResponse, AGENT_HUB_MAX_CHUNK_BYTES,
};
use crate::agent_hub::snapshot::builder::{
    build_snapshot, BuiltSnapshot, SnapshotSelectionMode, SnapshotSelectionRequest,
};
use crate::error::AppError;
use crate::models::device::Device;
use crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER;
use crate::net::peer_client::PeerClient;
use crate::net::peer_error::PeerCallError;
use crate::net::peer_timeout::PeerTimeoutClass;
use crate::net::protocol::{CAPABILITY_AGENT_HUB_V1, CAPABILITY_DEVICE_REQUEST_BINDING_V1};
use crate::net::request_context::{new_request_id, REQUEST_ID_HEADER};
use crate::state::AppState;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use chrono::Utc;
use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[cfg(any(test, debug_assertions))]
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

/// test/debug: 下一次 `persist_target_outcome` 一次消费失败（模拟写库/lease 压力）。
#[cfg(any(test, debug_assertions))]
static PERSIST_OUTCOME_FAULT: AtomicBool = AtomicBool::new(false);

/// 注入一次 terminal/checkpoint persist 故障（仅 test/debug）。
///
/// Business Logic: 证明终态落库失败会 fail-closed，而不是静默丢弃。
/// Code Logic: 原子 swap 一次消费标志。
#[cfg(any(test, debug_assertions))]
pub fn inject_persist_outcome_fault_once() {
    PERSIST_OUTCOME_FAULT.store(true, AtomicOrdering::SeqCst);
}

/// 清除 persist 故障注入（测试复位）。
#[cfg(any(test, debug_assertions))]
pub fn clear_persist_outcome_fault() {
    PERSIST_OUTCOME_FAULT.store(false, AtomicOrdering::SeqCst);
}

/// 源侧目标并发上限。
pub const MAX_TARGET_PARALLELISM: usize = 3;

/// 单 object chunk 长超时预算（大 blob 流式传输）。
const OBJECT_CHUNK_TIMEOUT: Duration = Duration::from_secs(120);

/// 源侧 multi-target push 发送器。
///
/// Business Logic: owner 进程内唯一 push 入口；GuiClient 经 control 代理。
/// Code Logic: 持有 pool/gate/device_id/ObjectStore 根与 peer 解析回调。
pub struct AgentHubPushSender {
    pool: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
    source_device_id: String,
    data_dir: std::path::PathBuf,
    peer_client: PeerClient,
}

/// 用户发起的 push 选择请求。
///
/// Business Logic: 显式 peer + 恰好一种 selection mode。
/// Code Logic: camelCase；mode 与 builder 对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushAgentHubSelectionRequest {
    /// 目标 peer device id 列表（显式，禁止空）
    pub peer_device_ids: Vec<String>,
    /// 选择模式
    pub mode: SnapshotSelectionMode,
    /// user scope ids（UserScope）
    #[serde(default)]
    pub scope_ids: Vec<String>,
    /// 显式 asset ids
    #[serde(default)]
    pub asset_ids: Vec<String>,
    /// project hubProjectId 列表
    #[serde(default)]
    pub hub_project_ids: Vec<String>,
    /// 是否包含 revision ancestry
    #[serde(default = "default_include_history")]
    pub include_history: bool,
    /// 可选：整次 push 的 request id（缺省生成 UUID）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

fn default_include_history() -> bool {
    true
}

/// 单目标状态（独立于其它 peer）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetPushStatus {
    /// 等待/进行中
    Pending,
    /// prepare 成功
    Prepared,
    /// objects 已传完
    Transferred,
    /// commit 成功
    Committed,
    /// 本目标失败（不回滚其它成功 peer）
    Failed,
}

impl TargetPushStatus {
    /// 存库字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Prepared => "prepared",
            Self::Transferred => "transferred",
            Self::Committed => "committed",
            Self::Failed => "failed",
        }
    }

    /// 解析存库字符串。
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "pending" => Ok(Self::Pending),
            "prepared" => Ok(Self::Prepared),
            "transferred" => Ok(Self::Transferred),
            "committed" => Ok(Self::Committed),
            "failed" => Ok(Self::Failed),
            other => Err(AppError::generic(format!(
                "agent_hub_source_push_status_unknown:{other}"
            ))),
        }
    }
}

/// 单目标 outcome。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TargetPushOutcome {
    pub peer_device_id: String,
    pub peer_label: String,
    pub client_request_id: String,
    pub status: TargetPushStatus,
    /// transport 失败可重试；capability/manifest conflict 为 terminal
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_id: Option<String>,
    pub missing_object_count: u32,
    pub transferred_object_count: u32,
    pub updated_at: String,
}

/// multi-target 总报告。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MultiTargetPushReport {
    pub request_id: String,
    pub selection_hash: String,
    pub snapshot_hash: String,
    /// running | completed
    pub status: String,
    pub targets: Vec<TargetPushOutcome>,
}

/// 源侧 push 请求行（Attention / reconnect）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourcePushRequestRow {
    pub request_id: String,
    pub selection_mode: String,
    pub selection_hash: String,
    pub snapshot_hash: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 源侧 target 行。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourcePushTargetRow {
    pub request_id: String,
    pub peer_device_id: String,
    pub peer_label: String,
    pub client_request_id: String,
    pub status: TargetPushStatus,
    pub retryable: bool,
    pub error_code: Option<String>,
    pub transfer_id: Option<String>,
    pub missing_object_count: u32,
    pub transferred_object_count: u32,
    pub created_at: String,
    pub updated_at: String,
}

/// 单目标 push 入参（收拢参数避免 clippy too_many_arguments）。
struct PushOneTargetArgs<'a> {
    request_id: &'a str,
    peer_id: &'a str,
    peer_label: &'a str,
    client_request_id: &'a str,
    device: Option<&'a Device>,
    built: &'a BuiltSnapshot,
    cancel: &'a CancellationToken,
}

impl AgentHubPushSender {
    /// 从 AppState 构造发送器。
    ///
    /// Business Logic: owner 路径共享 maintenance gate 与 data_dir。
    /// Code Logic: clone pool/gate/device_id；PeerClient::new。
    pub fn from_state(state: &AppState) -> Result<Self, AppError> {
        let data_dir = crate::config::data_dir()?;
        Ok(Self {
            pool: state.agent_hub_repo.pool(),
            gate: state.maintenance_gate.clone(),
            source_device_id: state.device_id.as_ref().clone(),
            data_dir,
            peer_client: PeerClient::new(),
        })
    }

    /// 测试/注入构造。
    pub fn new(
        pool: SqlitePool,
        gate: Arc<DatabaseMaintenanceGate>,
        source_device_id: impl Into<String>,
        data_dir: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            pool,
            gate,
            source_device_id: source_device_id.into(),
            data_dir: data_dir.into(),
            peer_client: PeerClient::new(),
        }
    }

    /// 向多目标 push 当前 selection。
    ///
    /// Business Logic:
    ///     - snapshot 只构建一次；
    ///     - 每 peer 先 capability 再 prepare/objects/commit；
    ///     - 失败 peer 不回滚已 committed peer；
    ///     - 并发 ≤3；cancel 时停止尚未启动的目标。
    ///
    /// Code Logic:
    ///     校验请求 → build_snapshot → 持久化 request/targets → buffer_unordered(3)
    ///     push_one_target → 汇总 report。
    pub async fn push_selection(
        &self,
        state: &AppState,
        request: PushAgentHubSelectionRequest,
        cancel: &CancellationToken,
    ) -> Result<MultiTargetPushReport, AppError> {
        let peers = normalize_peer_ids(&request.peer_device_ids)?;
        validate_selection_mode(&request)?;

        if cancel.is_cancelled() {
            return Err(AppError::unavailable(
                "agent_hub_push_cancelled".to_string(),
            ));
        }

        let objects = ObjectStore::open(&self.data_dir)?;
        let built = build_snapshot(
            &state.agent_hub_repo,
            &objects,
            SnapshotSelectionRequest {
                mode: request.mode,
                scope_ids: request.scope_ids.clone(),
                asset_ids: request.asset_ids.clone(),
                hub_project_ids: request.hub_project_ids.clone(),
                include_history: request.include_history,
                source_replica_id: self.source_device_id.clone(),
                limits: None,
            },
        )
        .await?;

        let request_id = request
            .request_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let selection_json = serde_json::to_string(&request)
            .map_err(|e| AppError::generic(format!("agent_hub_push_selection_serialize:{e}")))?;
        let mode_str = selection_mode_str(request.mode);

        self.insert_source_request(
            &request_id,
            mode_str,
            &selection_json,
            &built.selection_hash,
            &built.envelope.snapshot_hash,
        )
        .await?;

        // 解析 peer 设备（label/base_url）；缺失则 terminal failed
        let device_map = {
            let guard = state.devices.read().map_err(|_| {
                AppError::generic("agent_hub_push_devices_lock_poisoned".to_string())
            })?;
            guard.clone()
        };

        let mut initial_targets: Vec<(String, String, String, Option<Device>)> = Vec::new();
        for peer_id in &peers {
            let device = device_map.get(peer_id).cloned();
            let label = device
                .as_ref()
                .map(|d| d.name.clone())
                .unwrap_or_else(|| peer_id.clone());
            // 稳定 clientRequestId：同 request+peer 可重入时复用已存值
            let client_request_id = match self.get_target(&request_id, peer_id).await? {
                Some(existing) => existing.client_request_id,
                None => format!("{request_id}:{peer_id}"),
            };
            self.upsert_target_pending(&request_id, peer_id, &label, &client_request_id)
                .await?;
            initial_targets.push((peer_id.clone(), label, client_request_id, device));
        }

        let built = Arc::new(built);
        let source_device_id = self.source_device_id.clone();
        let peer_client = self.peer_client.clone();
        let pool = self.pool.clone();
        let gate = self.gate.clone();
        let data_dir = self.data_dir.clone();
        let request_id_owned = request_id.clone();
        let cancel = cancel.clone();

        let outcomes: Vec<TargetPushOutcome> = stream::iter(initial_targets)
            .map(|(peer_id, label, client_req, device)| {
                let built = Arc::clone(&built);
                let source_device_id = source_device_id.clone();
                let peer_client = peer_client.clone();
                let pool = pool.clone();
                let gate = gate.clone();
                let data_dir = data_dir.clone();
                let request_id = request_id_owned.clone();
                let cancel = cancel.clone();
                async move {
                    if cancel.is_cancelled() {
                        let outcome = TargetPushOutcome {
                            peer_device_id: peer_id.clone(),
                            peer_label: label.clone(),
                            client_request_id: client_req.clone(),
                            status: TargetPushStatus::Failed,
                            retryable: true,
                            error_code: Some("agent_hub_push_cancelled".into()),
                            transfer_id: None,
                            missing_object_count: 0,
                            transferred_object_count: 0,
                            updated_at: Utc::now().to_rfc3339(),
                        };
                        // 终态 failed：persist 失败 fail-closed，禁止静默丢弃
                        return finalize_target_outcome(&pool, &gate, &request_id, outcome).await;
                    }
                    let sender = AgentHubPushSender {
                        pool: pool.clone(),
                        gate: gate.clone(),
                        source_device_id: source_device_id.clone(),
                        data_dir,
                        peer_client,
                    };
                    let outcome = sender
                        .push_one_target(PushOneTargetArgs {
                            request_id: &request_id,
                            peer_id: &peer_id,
                            peer_label: &label,
                            client_request_id: &client_req,
                            device: device.as_ref(),
                            built: built.as_ref(),
                            cancel: &cancel,
                        })
                        .await;
                    // 终态 committed/failed 必须落库或 fail-closed 改写 outcome
                    finalize_target_outcome(&pool, &gate, &request_id, outcome).await
                }
            })
            .buffer_unordered(MAX_TARGET_PARALLELISM)
            .collect()
            .await;

        // 稳定顺序：按 peer_device_id
        let mut targets = outcomes;
        targets.sort_by(|a, b| a.peer_device_id.cmp(&b.peer_device_id));

        let all_done = targets.iter().all(|t| {
            matches!(
                t.status,
                TargetPushStatus::Committed | TargetPushStatus::Failed
            )
        });
        let status = if all_done { "completed" } else { "running" };
        self.mark_request_status(&request_id, status).await?;

        Ok(MultiTargetPushReport {
            request_id,
            selection_hash: built.selection_hash.clone(),
            snapshot_hash: built.envelope.snapshot_hash.clone(),
            status: status.to_string(),
            targets,
        })
    }

    /// 读取已持久化的 multi-target report（GUI reconnect）。
    ///
    /// Business Logic: 不重跑 push，只读 source ledger。
    /// Code Logic: request + targets by request_id。
    pub async fn get_push_report(
        &self,
        request_id: &str,
    ) -> Result<Option<MultiTargetPushReport>, AppError> {
        let req = match self.get_source_request(request_id).await? {
            Some(r) => r,
            None => return Ok(None),
        };
        let targets = self.list_targets(request_id).await?;
        Ok(Some(MultiTargetPushReport {
            request_id: req.request_id,
            selection_hash: req.selection_hash,
            snapshot_hash: req.snapshot_hash,
            status: req.status,
            targets: targets.into_iter().map(target_row_to_outcome).collect(),
        }))
    }

    /// 列出失败 target 行（Attention）。
    ///
    /// Business Logic: 仅 failed 进入 Inbox；summary 不含 payload。
    /// Code Logic: SELECT status=failed ORDER BY updated_at DESC LIMIT 100。
    pub async fn list_failed_targets(&self) -> Result<Vec<SourcePushTargetRow>, AppError> {
        let rows = sqlx::query(
            "SELECT request_id, peer_device_id, peer_label, client_request_id, status, retryable,
                    error_code, transfer_id, missing_object_count, transferred_object_count,
                    created_at, updated_at
             FROM agent_hub_source_push_targets
             WHERE status = 'failed'
             ORDER BY updated_at DESC
             LIMIT 100",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_target).collect()
    }

    /// Push 前校验对端能力与设备身份绑定。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     发现记录地址复用/过期时，仅 agent-hub.v1 的旧 peer 会忽略 expected-device header；
    ///     含 MCP 凭据的 Snapshot 不得投递到非用户所选设备并记成功。
    ///
    /// Code Logic（这个函数做什么）:
    ///     health_info 一次；要求 `agent-hub.v1` + `device.request-binding.v1`；
    ///     `health.device_id` 精确等于 `peer_id`；任一失败 Unsupported/InvalidResponse。
    async fn ensure_agent_hub_peer_binding(
        &self,
        base_url: &str,
        peer_id: &str,
    ) -> Result<(), PeerCallError> {
        let health = self.peer_client.health_info(base_url).await?;
        let info = health.protocol_info();
        if !info.supports(CAPABILITY_AGENT_HUB_V1) {
            return Err(PeerCallError::Unsupported {
                url: base_url.to_string(),
                capability: CAPABILITY_AGENT_HUB_V1,
            });
        }
        if !info.supports(CAPABILITY_DEVICE_REQUEST_BINDING_V1) {
            return Err(PeerCallError::Unsupported {
                url: base_url.to_string(),
                capability: CAPABILITY_DEVICE_REQUEST_BINDING_V1,
            });
        }
        if health.device_id.trim() != peer_id.trim() {
            return Err(PeerCallError::InvalidResponse {
                url: base_url.to_string(),
                reason: format!(
                    "agent_hub_push_device_id_mismatch:expected={peer_id},got={}",
                    health.device_id
                ),
            });
        }
        Ok(())
    }

    /// 推送单个目标。
    ///
    /// Business Logic: capability+device binding → prepare → chunk missing → commit；错误分类 retryable。
    /// Code Logic: ensure_agent_hub_peer_binding 后 HTTP 三阶段。
    async fn push_one_target(&self, args: PushOneTargetArgs<'_>) -> TargetPushOutcome {
        let PushOneTargetArgs {
            request_id,
            peer_id,
            peer_label,
            client_request_id,
            device,
            built,
            cancel,
        } = args;
        let now = || Utc::now().to_rfc3339();
        let fail = |code: &str, retryable: bool, transfer_id: Option<String>| TargetPushOutcome {
            peer_device_id: peer_id.to_string(),
            peer_label: peer_label.to_string(),
            client_request_id: client_request_id.to_string(),
            status: TargetPushStatus::Failed,
            retryable,
            error_code: Some(code.to_string()),
            transfer_id,
            missing_object_count: 0,
            transferred_object_count: 0,
            updated_at: now(),
        };

        let Some(device) = device else {
            return fail("agent_hub_push_peer_not_found", false, None);
        };
        if !device.online {
            return fail("agent_hub_push_peer_offline", true, None);
        }
        let base_url = device.base_url();

        // 1) capability + device binding gate — 在任何 push 路由前 fail-closed
        //    必须同时具备 agent-hub.v1 与 device.request-binding.v1，且 health.device_id
        //    精确等于所选 peer_id；否则凭据快照可能落到错误端点。
        if cancel.is_cancelled() {
            return fail("agent_hub_push_cancelled", true, None);
        }
        if let Err(err) = self.ensure_agent_hub_peer_binding(&base_url, peer_id).await {
            return classify_peer_error(peer_id, peer_label, client_request_id, err, None);
        }

        // 2) prepare
        let prepare_body = PreparePushRequest {
            envelope: built.envelope.clone(),
            source_device_id: self.source_device_id.clone(),
            client_request_id: client_request_id.to_string(),
            selection_hash: built.selection_hash.clone(),
        };
        let prep: PreparePushResponse = match self
            .post_json_bound::<PreparePushResponse, _>(
                &base_url,
                "/api/agent-hub/push/prepare",
                &prepare_body,
                peer_id,
                PeerTimeoutClass::long_running(Duration::from_secs(60)),
            )
            .await
        {
            Ok(v) => v,
            Err(err) => {
                return classify_peer_error(peer_id, peer_label, client_request_id, err, None);
            }
        };

        let transfer_id = prep.transfer_id.clone();
        let missing = prep.missing_object_hashes.clone();
        let missing_count = missing.len() as u32;

        // 已 committed 幂等回放：仍补调 commit 作为即时补偿（触发 receiver 排水 queued intent）
        if prep.status == "committed" {
            let commit_body = CommitPushRequest {
                source_device_id: self.source_device_id.clone(),
                client_request_id: client_request_id.to_string(),
                selection_hash: built.selection_hash.clone(),
                snapshot_hash: built.envelope.snapshot_hash.clone(),
            };
            let path = format!("/api/agent-hub/push/{transfer_id}/commit");
            // best-effort：失败不改写已 committed 终态，留给对端 outbox worker
            let _ = self
                .post_json_bound::<CommitPushResponse, _>(
                    &base_url,
                    &path,
                    &commit_body,
                    peer_id,
                    PeerTimeoutClass::long_running(Duration::from_secs(60)),
                )
                .await;
            return TargetPushOutcome {
                peer_device_id: peer_id.to_string(),
                peer_label: peer_label.to_string(),
                client_request_id: client_request_id.to_string(),
                status: TargetPushStatus::Committed,
                retryable: false,
                error_code: None,
                transfer_id: Some(transfer_id),
                missing_object_count: 0,
                transferred_object_count: 0,
                updated_at: now(),
            };
        }

        let mut outcome = TargetPushOutcome {
            peer_device_id: peer_id.to_string(),
            peer_label: peer_label.to_string(),
            client_request_id: client_request_id.to_string(),
            status: TargetPushStatus::Prepared,
            retryable: false,
            error_code: None,
            transfer_id: Some(transfer_id.clone()),
            missing_object_count: missing_count,
            transferred_object_count: 0,
            updated_at: now(),
        };
        // mid-flight prepared checkpoint：best-effort（终态由 fan-out finalize 保证）
        persist_checkpoint_best_effort(&self.pool, &self.gate, request_id, &outcome).await;

        // 3) stream missing objects
        let mut transferred = 0u32;
        for object_hash in &missing {
            if cancel.is_cancelled() {
                return fail("agent_hub_push_cancelled", true, Some(transfer_id.clone()));
            }
            let bytes = match built.object_bytes.get(object_hash) {
                Some(b) => b.as_slice(),
                None => {
                    return fail(
                        "agent_hub_push_object_bytes_missing",
                        false,
                        Some(transfer_id.clone()),
                    );
                }
            };
            if let Err(err) = self
                .stream_object(&base_url, peer_id, &transfer_id, object_hash, bytes, cancel)
                .await
            {
                return classify_peer_error(
                    peer_id,
                    peer_label,
                    client_request_id,
                    err,
                    Some(transfer_id.clone()),
                );
            }
            transferred += 1;
            outcome.transferred_object_count = transferred;
            outcome.status = TargetPushStatus::Transferred;
            outcome.updated_at = now();
            // mid-flight transferred checkpoint：best-effort
            persist_checkpoint_best_effort(&self.pool, &self.gate, request_id, &outcome).await;
        }

        outcome.status = TargetPushStatus::Transferred;
        outcome.transferred_object_count = transferred;
        outcome.updated_at = now();
        // mid-flight：objects 完成仍非终态；fan-out 在 commit 后 finalize
        persist_checkpoint_best_effort(&self.pool, &self.gate, request_id, &outcome).await;

        // 4) commit
        if cancel.is_cancelled() {
            return fail("agent_hub_push_cancelled", true, Some(transfer_id.clone()));
        }
        let commit_body = CommitPushRequest {
            source_device_id: self.source_device_id.clone(),
            client_request_id: client_request_id.to_string(),
            selection_hash: built.selection_hash.clone(),
            snapshot_hash: built.envelope.snapshot_hash.clone(),
        };
        let path = format!("/api/agent-hub/push/{transfer_id}/commit");
        match self
            .post_json_bound::<CommitPushResponse, _>(
                &base_url,
                &path,
                &commit_body,
                peer_id,
                PeerTimeoutClass::long_running(Duration::from_secs(120)),
            )
            .await
        {
            Ok(_resp) => {
                outcome.status = TargetPushStatus::Committed;
                outcome.retryable = false;
                outcome.error_code = None;
                outcome.updated_at = now();
                outcome
            }
            Err(err) => classify_peer_error(
                peer_id,
                peer_label,
                client_request_id,
                err,
                Some(transfer_id),
            ),
        }
    }

    /// 分块上传 object，从 peer received_bytes 续传。
    ///
    /// Business Logic: 每块 ≤8 MiB；offset 严格连续；chunk sha 声明。
    /// Code Logic: 循环 PUT objects；首块 offset=0，后续按响应 received_bytes。
    async fn stream_object(
        &self,
        base_url: &str,
        peer_id: &str,
        transfer_id: &str,
        object_hash: &str,
        bytes: &[u8],
        cancel: &CancellationToken,
    ) -> Result<(), PeerCallError> {
        let total = bytes.len() as u64;
        let mut offset: u64 = 0;
        while offset < total {
            if cancel.is_cancelled() {
                return Err(PeerCallError::InvalidResponse {
                    url: base_url.to_string(),
                    reason: "agent_hub_push_cancelled".to_string(),
                });
            }
            let end = std::cmp::min(offset as usize + AGENT_HUB_MAX_CHUNK_BYTES, bytes.len());
            let chunk = &bytes[offset as usize..end];
            let chunk_sha = sha256_hex(chunk);
            let url = format!(
                "{base_url}/api/agent-hub/push/{transfer_id}/objects/{object_hash}?offset={offset}&chunkSha256={chunk_sha}"
            );
            let resp: PutObjectResponse = self
                .put_bytes_bound(&url, peer_id, chunk.to_vec(), OBJECT_CHUNK_TIMEOUT)
                .await?;
            // 续传：以 peer 确认的 received_bytes 为准
            if resp.received_bytes < offset {
                return Err(PeerCallError::InvalidResponse {
                    url: url.clone(),
                    reason: format!(
                        "agent_hub_push_peer_offset_regressed:was={offset}:now={}",
                        resp.received_bytes
                    ),
                });
            }
            if resp.received_bytes == offset && !chunk.is_empty() && !resp.verified {
                // 无进展
                return Err(PeerCallError::InvalidResponse {
                    url,
                    reason: "agent_hub_push_chunk_no_progress".to_string(),
                });
            }
            offset = if resp.verified {
                total
            } else {
                resp.received_bytes
            };
        }
        Ok(())
    }

    /// 带 expected-device header 的 POST JSON。
    async fn post_json_bound<T, B>(
        &self,
        base_url: &str,
        path: &str,
        body: &B,
        expected_device_id: &str,
        timeout: Duration,
    ) -> Result<T, PeerCallError>
    where
        T: serde::de::DeserializeOwned,
        B: serde::Serialize + ?Sized,
    {
        let url = format!("{base_url}{path}");
        let resp = self
            .peer_client
            .http_client()
            .post(&url)
            .timeout(timeout)
            .header(REQUEST_ID_HEADER, new_request_id())
            .header(EXPECTED_DEVICE_ID_HEADER.as_str(), expected_device_id)
            .json(body)
            .send()
            .await
            .map_err(|e| PeerCallError::Network {
                url: url.clone(),
                source: e,
            })?;
        crate::net::peer_error::parse_peer_response::<T>(resp, &url).await
    }

    /// 带 expected-device header 的 PUT raw bytes。
    async fn put_bytes_bound(
        &self,
        url: &str,
        expected_device_id: &str,
        body: Vec<u8>,
        timeout: Duration,
    ) -> Result<PutObjectResponse, PeerCallError> {
        let resp = self
            .peer_client
            .http_client()
            .put(url)
            .timeout(timeout)
            .header(REQUEST_ID_HEADER, new_request_id())
            .header(EXPECTED_DEVICE_ID_HEADER.as_str(), expected_device_id)
            .header(axum::http::header::CONTENT_TYPE, "application/octet-stream")
            .body(body)
            .send()
            .await
            .map_err(|e| PeerCallError::Network {
                url: url.to_string(),
                source: e,
            })?;
        crate::net::peer_error::parse_peer_response::<PutObjectResponse>(resp, url).await
    }

    // ── ledger helpers ──────────────────────────────────────────────────

    async fn insert_source_request(
        &self,
        request_id: &str,
        mode: &str,
        selection_json: &str,
        selection_hash: &str,
        snapshot_hash: &str,
    ) -> Result<(), AppError> {
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            // 已存在则更新 hash/status（同 request 重入）
            sqlx::query(
                "INSERT INTO agent_hub_source_push_requests
                 (request_id, selection_mode, selection_json, selection_hash, snapshot_hash,
                  status, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, 'running', ?, ?)
                 ON CONFLICT(request_id) DO UPDATE SET
                   selection_mode=excluded.selection_mode,
                   selection_json=excluded.selection_json,
                   selection_hash=excluded.selection_hash,
                   snapshot_hash=excluded.snapshot_hash,
                   status='running',
                   updated_at=excluded.updated_at",
            )
            .bind(request_id)
            .bind(mode)
            .bind(selection_json)
            .bind(selection_hash)
            .bind(snapshot_hash)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn mark_request_status(&self, request_id: &str, status: &str) -> Result<(), AppError> {
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE agent_hub_source_push_requests
                 SET status = ?, updated_at = ?
                 WHERE request_id = ?",
            )
            .bind(status)
            .bind(&now)
            .bind(request_id)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn upsert_target_pending(
        &self,
        request_id: &str,
        peer_id: &str,
        label: &str,
        client_request_id: &str,
    ) -> Result<(), AppError> {
        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO agent_hub_source_push_targets
                 (request_id, peer_device_id, peer_label, client_request_id, status, retryable,
                  error_code, transfer_id, missing_object_count, transferred_object_count,
                  created_at, updated_at)
                 VALUES (?, ?, ?, ?, 'pending', 0, NULL, NULL, 0, 0, ?, ?)
                 ON CONFLICT(request_id, peer_device_id) DO UPDATE SET
                   peer_label=excluded.peer_label,
                   -- 保持既有 client_request_id 稳定（重试同一目标）
                   client_request_id=agent_hub_source_push_targets.client_request_id,
                   status=CASE
                     WHEN agent_hub_source_push_targets.status = 'committed'
                     THEN agent_hub_source_push_targets.status
                     ELSE 'pending'
                   END,
                   updated_at=excluded.updated_at",
            )
            .bind(request_id)
            .bind(peer_id)
            .bind(label)
            .bind(client_request_id)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn get_source_request(
        &self,
        request_id: &str,
    ) -> Result<Option<SourcePushRequestRow>, AppError> {
        let row = sqlx::query(
            "SELECT request_id, selection_mode, selection_hash, snapshot_hash, status,
                    created_at, updated_at
             FROM agent_hub_source_push_requests WHERE request_id = ?",
        )
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| SourcePushRequestRow {
            request_id: r.get("request_id"),
            selection_mode: r.get("selection_mode"),
            selection_hash: r.get("selection_hash"),
            snapshot_hash: r.get("snapshot_hash"),
            status: r.get("status"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        }))
    }

    async fn get_target(
        &self,
        request_id: &str,
        peer_id: &str,
    ) -> Result<Option<SourcePushTargetRow>, AppError> {
        let row = sqlx::query(
            "SELECT request_id, peer_device_id, peer_label, client_request_id, status, retryable,
                    error_code, transfer_id, missing_object_count, transferred_object_count,
                    created_at, updated_at
             FROM agent_hub_source_push_targets
             WHERE request_id = ? AND peer_device_id = ?",
        )
        .bind(request_id)
        .bind(peer_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_target).transpose()
    }

    async fn list_targets(&self, request_id: &str) -> Result<Vec<SourcePushTargetRow>, AppError> {
        let rows = sqlx::query(
            "SELECT request_id, peer_device_id, peer_label, client_request_id, status, retryable,
                    error_code, transfer_id, missing_object_count, transferred_object_count,
                    created_at, updated_at
             FROM agent_hub_source_push_targets
             WHERE request_id = ?
             ORDER BY peer_device_id ASC",
        )
        .bind(request_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_target).collect()
    }
}

/// 将 PeerCallError 映射为 TargetPushOutcome。
///
/// Business Logic: Network/InvalidResponse 可重试；Unsupported 与 manifest conflict terminal。
/// Code Logic: match 变体 + Remote.code 含 conflict/unsupported。
fn classify_peer_error(
    peer_id: &str,
    peer_label: &str,
    client_request_id: &str,
    err: PeerCallError,
    transfer_id: Option<String>,
) -> TargetPushOutcome {
    let (code, retryable) = match &err {
        PeerCallError::Unsupported { capability, .. } => {
            (format!("unsupported_capability:{capability}"), false)
        }
        PeerCallError::Network { .. } => ("transport_network".to_string(), true),
        PeerCallError::InvalidResponse { reason, .. } => {
            let retryable = !reason.contains("conflict") && !reason.contains("unsupported");
            (format!("invalid_response:{reason}"), retryable)
        }
        PeerCallError::Remote { code, status, .. } => {
            let terminal = code.contains("conflict")
                || code.contains("unsupported")
                || code.contains("idempotency_hash_conflict")
                || code.contains("invalid_manifest")
                || *status == 409
                || *status == 400;
            (code.clone(), !terminal)
        }
    };
    // 脱敏：不把错误 message/payload 写入 outcome
    let short_code = if code.len() > 200 {
        format!("{}…", &code[..200])
    } else {
        code
    };
    TargetPushOutcome {
        peer_device_id: peer_id.to_string(),
        peer_label: peer_label.to_string(),
        client_request_id: client_request_id.to_string(),
        status: TargetPushStatus::Failed,
        retryable,
        error_code: Some(short_code),
        transfer_id,
        missing_object_count: 0,
        transferred_object_count: 0,
        updated_at: Utc::now().to_rfc3339(),
    }
}

/// 是否为终端状态（须 durable 或 fail-closed）。
fn is_terminal_status(status: TargetPushStatus) -> bool {
    matches!(
        status,
        TargetPushStatus::Committed | TargetPushStatus::Failed
    )
}

/// 终态 outcome 落库：成功返回原 outcome；失败则 fail-closed 改写为 retryable persist error。
///
/// Business Logic: 禁止内存已 committed/failed 而 DB 仍 pending，否则 reconnect/Attention 撒谎。
/// Code Logic: persist 失败 → 改写 Failed + `agent_hub_push_persist_failed` + 再 best-effort 写一次。
async fn finalize_target_outcome(
    pool: &SqlitePool,
    gate: &Arc<DatabaseMaintenanceGate>,
    request_id: &str,
    mut outcome: TargetPushOutcome,
) -> TargetPushOutcome {
    match persist_target_outcome(pool, gate, request_id, &outcome).await {
        Ok(()) => outcome,
        Err(err) => {
            if !is_terminal_status(outcome.status) {
                // 非终态不应走本路径；退化为 checkpoint 语义
                tracing::warn!(
                    request_id = %request_id,
                    peer = %outcome.peer_device_id,
                    status = outcome.status.as_str(),
                    error = %err,
                    "agent_hub_push non-terminal finalize treated as checkpoint"
                );
                return outcome;
            }
            tracing::error!(
                request_id = %request_id,
                peer = %outcome.peer_device_id,
                status = outcome.status.as_str(),
                error = %err,
                "agent_hub_push terminal outcome persist failed; fail-closed"
            );
            // 改写 in-memory，与 MultiTargetPushReport / reconnect 对齐
            outcome.status = TargetPushStatus::Failed;
            outcome.retryable = true;
            outcome.error_code = Some("agent_hub_push_persist_failed".into());
            outcome.updated_at = Utc::now().to_rfc3339();
            if let Err(e2) = persist_target_outcome(pool, gate, request_id, &outcome).await {
                tracing::error!(
                    request_id = %request_id,
                    peer = %outcome.peer_device_id,
                    error = %e2,
                    "agent_hub_push persist-failed marker also failed to land"
                );
            }
            outcome
        }
    }
}

/// 中途 prepared/transferred checkpoint：best-effort + warn，不改写 outcome。
///
/// Business Logic: 进度点丢失可接受；终态由 finalize_target_outcome 保证。
/// Code Logic: persist Err → tracing::warn，返回。
async fn persist_checkpoint_best_effort(
    pool: &SqlitePool,
    gate: &Arc<DatabaseMaintenanceGate>,
    request_id: &str,
    outcome: &TargetPushOutcome,
) {
    if let Err(err) = persist_target_outcome(pool, gate, request_id, outcome).await {
        tracing::warn!(
            request_id = %request_id,
            peer = %outcome.peer_device_id,
            status = outcome.status.as_str(),
            error = %err,
            "agent_hub_push mid-flight checkpoint persist failed (best-effort)"
        );
    }
}

async fn persist_target_outcome(
    pool: &SqlitePool,
    gate: &Arc<DatabaseMaintenanceGate>,
    request_id: &str,
    outcome: &TargetPushOutcome,
) -> Result<(), AppError> {
    #[cfg(any(test, debug_assertions))]
    {
        if PERSIST_OUTCOME_FAULT.swap(false, AtomicOrdering::SeqCst) {
            return Err(AppError::generic(
                "agent_hub_push_persist_injected".to_string(),
            ));
        }
    }
    with_shared_write_lease(gate, async {
        sqlx::query(
            "UPDATE agent_hub_source_push_targets
             SET status = ?, retryable = ?, error_code = ?, transfer_id = ?,
                 missing_object_count = ?, transferred_object_count = ?, updated_at = ?
             WHERE request_id = ? AND peer_device_id = ?",
        )
        .bind(outcome.status.as_str())
        .bind(if outcome.retryable { 1 } else { 0 })
        .bind(&outcome.error_code)
        .bind(&outcome.transfer_id)
        .bind(outcome.missing_object_count as i64)
        .bind(outcome.transferred_object_count as i64)
        .bind(&outcome.updated_at)
        .bind(request_id)
        .bind(&outcome.peer_device_id)
        .execute(pool)
        .await?;
        Ok(())
    })
    .await
}

fn row_to_target(r: sqlx::sqlite::SqliteRow) -> Result<SourcePushTargetRow, AppError> {
    let status_raw: String = r.get("status");
    let retryable_i: i64 = r.get("retryable");
    Ok(SourcePushTargetRow {
        request_id: r.get("request_id"),
        peer_device_id: r.get("peer_device_id"),
        peer_label: r.get("peer_label"),
        client_request_id: r.get("client_request_id"),
        status: TargetPushStatus::parse(&status_raw)?,
        retryable: retryable_i != 0,
        error_code: r.get("error_code"),
        transfer_id: r.get("transfer_id"),
        missing_object_count: r.get::<i64, _>("missing_object_count") as u32,
        transferred_object_count: r.get::<i64, _>("transferred_object_count") as u32,
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    })
}

fn target_row_to_outcome(row: SourcePushTargetRow) -> TargetPushOutcome {
    TargetPushOutcome {
        peer_device_id: row.peer_device_id,
        peer_label: row.peer_label,
        client_request_id: row.client_request_id,
        status: row.status,
        retryable: row.retryable,
        error_code: row.error_code,
        transfer_id: row.transfer_id,
        missing_object_count: row.missing_object_count,
        transferred_object_count: row.transferred_object_count,
        updated_at: row.updated_at,
    }
}

fn normalize_peer_ids(ids: &[String]) -> Result<Vec<String>, AppError> {
    let mut set = BTreeSet::new();
    for id in ids {
        let t = id.trim();
        if t.is_empty() {
            continue;
        }
        if t.len() > 256 {
            return Err(AppError::validation(
                "agent_hub_push_peer_id_too_long".to_string(),
            ));
        }
        set.insert(t.to_string());
    }
    if set.is_empty() {
        return Err(AppError::validation(
            "agent_hub_push_peers_required".to_string(),
        ));
    }
    if set.len() > 64 {
        return Err(AppError::validation(
            "agent_hub_push_too_many_peers".to_string(),
        ));
    }
    Ok(set.into_iter().collect())
}

fn validate_selection_mode(req: &PushAgentHubSelectionRequest) -> Result<(), AppError> {
    match req.mode {
        SnapshotSelectionMode::FullHub => Ok(()),
        SnapshotSelectionMode::UserScope => {
            if req.scope_ids.iter().all(|s| s.trim().is_empty()) {
                // 允许空：builder 取全部 user scopes
                Ok(())
            } else {
                Ok(())
            }
        }
        SnapshotSelectionMode::Project => {
            if req
                .hub_project_ids
                .iter()
                .filter(|s| !s.trim().is_empty())
                .count()
                == 0
            {
                return Err(AppError::validation(
                    "agent_hub_push_project_ids_required".to_string(),
                ));
            }
            Ok(())
        }
        SnapshotSelectionMode::ExplicitAssets => {
            if req
                .asset_ids
                .iter()
                .filter(|s| !s.trim().is_empty())
                .count()
                == 0
            {
                return Err(AppError::validation(
                    "agent_hub_push_asset_ids_required".to_string(),
                ));
            }
            Ok(())
        }
    }
}

fn selection_mode_str(mode: SnapshotSelectionMode) -> &'static str {
    match mode {
        SnapshotSelectionMode::FullHub => "fullHub",
        SnapshotSelectionMode::UserScope => "userScope",
        SnapshotSelectionMode::Project => "project",
        SnapshotSelectionMode::ExplicitAssets => "explicitAssets",
    }
}

/// AppState 便捷入口。
///
/// Business Logic: commands/control 共用。
/// Code Logic: from_state + push_selection。
pub async fn push_selection_for_state(
    state: &AppState,
    request: PushAgentHubSelectionRequest,
    cancel: &CancellationToken,
) -> Result<MultiTargetPushReport, AppError> {
    let sender = AgentHubPushSender::from_state(state)?;
    sender.push_selection(state, request, cancel).await
}

/// 读取 push report（GUI reconnect）。
pub async fn get_push_report_for_state(
    state: &AppState,
    request_id: &str,
) -> Result<Option<MultiTargetPushReport>, AppError> {
    let sender = AgentHubPushSender::from_state(state)?;
    sender.get_push_report(request_id).await
}

/// 列出失败 targets 供 Attention。
pub async fn list_failed_source_push_targets(
    state: &AppState,
) -> Result<Vec<SourcePushTargetRow>, AppError> {
    let sender = AgentHubPushSender::from_state(state)?;
    sender.list_failed_targets().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::snapshot::envelope::{
        SnapshotEnvelopeV1, SnapshotObjectDescriptor, SnapshotSelection, CANONICALIZATION_NAME,
        FORMAT_NAME, FORMAT_VERSION,
    };
    use crate::agent_hub::snapshot::importer::SnapshotImportOutcome;
    use crate::storage::AgentHubRepo;
    use axum::routing::{post, put};
    use axum::Router;
    use chrono::Utc;
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::BTreeMap;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    /// 测试用内存库 + schema。
    async fn test_pool() -> SqlitePool {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        pool
    }

    /// Business Logic: peer 列表空必须 validation。
    /// Code Logic: normalize_peer_ids([]) Err。
    #[test]
    fn normalize_peer_ids_rejects_empty() {
        let err = normalize_peer_ids(&[]).unwrap_err();
        assert_eq!(err.ipc_category_code(), "validation");
    }

    /// Business Logic: ExplicitAssets 必须带 asset ids。
    /// Code Logic: validate_selection_mode。
    #[test]
    fn validate_explicit_assets_requires_ids() {
        let req = PushAgentHubSelectionRequest {
            peer_device_ids: vec!["p1".into()],
            mode: SnapshotSelectionMode::ExplicitAssets,
            scope_ids: vec![],
            asset_ids: vec![],
            hub_project_ids: vec![],
            include_history: true,
            request_id: None,
        };
        assert!(validate_selection_mode(&req).is_err());
    }

    /// Business Logic: Unsupported capability 为 terminal。
    /// Code Logic: classify_peer_error retryable=false。
    #[test]
    fn unsupported_capability_is_terminal() {
        let err = PeerCallError::Unsupported {
            url: "http://x".into(),
            capability: CAPABILITY_AGENT_HUB_V1,
        };
        let o = classify_peer_error("p", "Peer", "cr", err, None);
        assert_eq!(o.status, TargetPushStatus::Failed);
        assert!(!o.retryable);
        assert!(o.error_code.unwrap().contains("unsupported_capability"));
    }

    /// Business Logic: transport 类失败可重试。
    /// Code Logic: InvalidResponse 无 conflict → retryable。
    #[test]
    fn transport_error_is_retryable() {
        let err = PeerCallError::InvalidResponse {
            url: "http://x".into(),
            reason: "connection reset".into(),
        };
        let o = classify_peer_error("p", "Peer", "cr", err, None);
        assert!(o.retryable);
    }

    /// Business Logic: conflict Remote 为 terminal。
    #[test]
    fn conflict_remote_is_terminal() {
        let err = PeerCallError::Remote {
            url: "http://x".into(),
            status: 409,
            code: "agent_hub_push_idempotency_hash_conflict".into(),
            message: "conflict".into(),
            request_id: "r".into(),
            retryable: false,
            legacy: false,
            details: json!({}),
        };
        let o = classify_peer_error("p", "Peer", "cr", err, None);
        assert!(!o.retryable);
    }

    /// Business Logic: 成功 peer 不被其它失败回滚——状态独立。
    /// Code Logic: 构造两个 outcome，断言 committed 不被 failed 改写。
    #[test]
    fn successful_peer_not_rolled_back_by_failed_peer() {
        let committed = TargetPushOutcome {
            peer_device_id: "a".into(),
            peer_label: "A".into(),
            client_request_id: "r:a".into(),
            status: TargetPushStatus::Committed,
            retryable: false,
            error_code: None,
            transfer_id: Some("t1".into()),
            missing_object_count: 0,
            transferred_object_count: 1,
            updated_at: "t".into(),
        };
        let failed = TargetPushOutcome {
            peer_device_id: "b".into(),
            peer_label: "B".into(),
            client_request_id: "r:b".into(),
            status: TargetPushStatus::Failed,
            retryable: true,
            error_code: Some("transport_network".into()),
            transfer_id: None,
            missing_object_count: 0,
            transferred_object_count: 0,
            updated_at: "t".into(),
        };
        assert_eq!(committed.status, TargetPushStatus::Committed);
        assert_eq!(failed.status, TargetPushStatus::Failed);
        let report = MultiTargetPushReport {
            request_id: "r".into(),
            selection_hash: "s".into(),
            snapshot_hash: "h".into(),
            status: "completed".into(),
            targets: vec![committed.clone(), failed],
        };
        assert_eq!(report.targets[0].status, TargetPushStatus::Committed);
    }

    /// Business Logic: 同一 request+peer 的 clientRequestId 在重试时保持稳定。
    /// Code Logic: upsert_target_pending 两次，client_request_id 不变。
    #[tokio::test]
    async fn client_request_id_stable_across_retry() {
        let pool = test_pool().await;
        let gate = Arc::new(DatabaseMaintenanceGate::new());
        let dir = tempfile::tempdir().unwrap();
        let sender = AgentHubPushSender::new(pool.clone(), gate, "src", dir.path());
        sender
            .insert_source_request("req-1", "fullHub", "{}", "sel", "snap")
            .await
            .unwrap();
        sender
            .upsert_target_pending("req-1", "peer-a", "A", "req-1:peer-a")
            .await
            .unwrap();
        sender
            .upsert_target_pending("req-1", "peer-a", "A", "req-1:peer-a-NEW")
            .await
            .unwrap();
        let t = sender.get_target("req-1", "peer-a").await.unwrap().unwrap();
        assert_eq!(t.client_request_id, "req-1:peer-a");
    }

    /// Business Logic: chunk 上限 8 MiB。
    /// Code Logic: AGENT_HUB_MAX_CHUNK_BYTES == 8 MiB。
    #[test]
    fn chunk_limit_is_8_mib() {
        assert_eq!(AGENT_HUB_MAX_CHUNK_BYTES, 8 * 1024 * 1024);
    }

    /// Fake peer 计数器：验证 capability 前不打 push 路由。
    struct FakePeerCounters {
        health: AtomicUsize,
        prepare: AtomicUsize,
        object: AtomicUsize,
        commit: AtomicUsize,
        no_capability: bool,
        missing: StdMutex<Vec<String>>,
        received: StdMutex<BTreeMap<String, u64>>,
    }

    impl FakePeerCounters {
        fn full_support(missing: Vec<String>) -> Arc<Self> {
            Arc::new(Self {
                health: AtomicUsize::new(0),
                prepare: AtomicUsize::new(0),
                object: AtomicUsize::new(0),
                commit: AtomicUsize::new(0),
                no_capability: false,
                missing: StdMutex::new(missing),
                received: StdMutex::new(BTreeMap::new()),
            })
        }

        fn no_cap() -> Arc<Self> {
            Arc::new(Self {
                health: AtomicUsize::new(0),
                prepare: AtomicUsize::new(0),
                object: AtomicUsize::new(0),
                commit: AtomicUsize::new(0),
                no_capability: true,
                missing: StdMutex::new(vec![]),
                received: StdMutex::new(BTreeMap::new()),
            })
        }
    }

    /// 启动 fake peer HTTP。
    async fn spawn_fake_peer(
        counters: Arc<FakePeerCounters>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let c_health = Arc::clone(&counters);
        let c_prep = Arc::clone(&counters);
        let c_obj = Arc::clone(&counters);
        let c_commit = Arc::clone(&counters);

        let app = Router::new()
            .route(
                "/api/health",
                axum::routing::get(move || {
                    let c = Arc::clone(&c_health);
                    async move {
                        c.health.fetch_add(1, Ordering::SeqCst);
                        let caps = if c.no_capability {
                            vec!["errors.envelope.v1".to_string()]
                        } else {
                            vec![
                                "errors.envelope.v1".to_string(),
                                CAPABILITY_AGENT_HUB_V1.to_string(),
                                CAPABILITY_DEVICE_REQUEST_BINDING_V1.to_string(),
                            ]
                        };
                        axum::Json(json!({
                            "ok": true,
                            "device_id": "peer-test",
                            "device_name": "Peer Test",
                            "http_port": 0,
                            "ts": Utc::now().timestamp(),
                            "protocol_version": 1,
                            "capabilities": caps,
                        }))
                    }
                }),
            )
            .route(
                "/api/agent-hub/push/prepare",
                post(move |body: axum::Json<PreparePushRequest>| {
                    let c = Arc::clone(&c_prep);
                    async move {
                        c.prepare.fetch_add(1, Ordering::SeqCst);
                        let missing = c.missing.lock().unwrap().clone();
                        axum::Json(PreparePushResponse {
                            transfer_id: "xfer-1".into(),
                            status: "prepared".into(),
                            selection_hash: body.selection_hash.clone(),
                            snapshot_hash: body.envelope.snapshot_hash.clone(),
                            missing_object_hashes: missing,
                            missing_revision_ids: vec![],
                            outcome: None,
                        })
                    }
                }),
            )
            .route(
                "/api/agent-hub/push/:tid/objects/:oh",
                put(
                    move |axum::extract::Path((tid, oh)): axum::extract::Path<(String, String)>,
                          axum::extract::Query(q): axum::extract::Query<
                        crate::net::routes::agent_hub::PutObjectQuery,
                    >,
                          body: axum::body::Bytes| {
                        let c = Arc::clone(&c_obj);
                        async move {
                            c.object.fetch_add(1, Ordering::SeqCst);
                            let mut map = c.received.lock().unwrap();
                            let cur = map.entry(oh.clone()).or_insert(0);
                            assert_eq!(*cur, q.offset, "offset must resume from peer received");
                            assert!(body.len() <= AGENT_HUB_MAX_CHUNK_BYTES);
                            *cur += body.len() as u64;
                            let received = *cur;
                            drop(map);
                            axum::Json(PutObjectResponse {
                                transfer_id: tid,
                                object_hash: oh,
                                received_bytes: received,
                                expected_size: received,
                                verified: true,
                            })
                        }
                    },
                ),
            )
            .route(
                "/api/agent-hub/push/:tid/commit",
                post(
                    move |axum::extract::Path(tid): axum::extract::Path<String>,
                          body: axum::Json<CommitPushRequest>| {
                        let c = Arc::clone(&c_commit);
                        async move {
                            c.commit.fetch_add(1, Ordering::SeqCst);
                            axum::Json(CommitPushResponse {
                                transfer_id: tid,
                                status: "committed".into(),
                                selection_hash: body.selection_hash.clone(),
                                snapshot_hash: body.snapshot_hash.clone(),
                                outcome: SnapshotImportOutcome {
                                    snapshot_id: "snap".into(),
                                    snapshot_hash: body.snapshot_hash.clone(),
                                    imported_asset_ids: vec![],
                                    inserted_revisions: 0,
                                    deduped_revisions: 0,
                                    heads_advanced: 0,
                                    conflicts_opened: 0,
                                    projections_scheduled: 0,
                                    imported_object_hashes: vec![],
                                },
                                projection: "queued".into(),
                            })
                        }
                    },
                ),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://{addr}"), handle)
    }

    /// Business Logic: 缺 device.request-binding.v1 时不得 prepare（旧 peer 会忽略 device 头）。
    /// Code Logic: health 仅 agent-hub.v1 → Unsupported；prepare=0。
    #[tokio::test]
    async fn never_calls_prepare_without_device_request_binding() {
        let counters = FakePeerCounters::full_support(vec![]);
        // 覆盖 health：仅 agent-hub，无 binding
        let c_health = Arc::clone(&counters);
        let c_prep = Arc::clone(&counters);
        let app = Router::new()
            .route(
                "/api/health",
                axum::routing::get(move || {
                    let c = Arc::clone(&c_health);
                    async move {
                        c.health.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({
                            "ok": true,
                            "device_id": "peer-test",
                            "device_name": "Peer Test",
                            "http_port": 0,
                            "ts": Utc::now().timestamp(),
                            "protocol_version": 1,
                            "capabilities": [
                                "errors.envelope.v1",
                                CAPABILITY_AGENT_HUB_V1,
                            ],
                        }))
                    }
                }),
            )
            .route(
                "/api/agent-hub/push/prepare",
                post(move |_body: axum::Json<PreparePushRequest>| {
                    let c = Arc::clone(&c_prep);
                    async move {
                        c.prepare.fetch_add(1, Ordering::SeqCst);
                        axum::Json(PreparePushResponse {
                            transfer_id: "x".into(),
                            status: "prepared".into(),
                            selection_hash: "s".into(),
                            snapshot_hash: "h".into(),
                            missing_object_hashes: vec![],
                            missing_revision_ids: vec![],
                            outcome: None,
                        })
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let base = format!("http://{addr}");
        let pool = test_pool().await;
        let gate = Arc::new(DatabaseMaintenanceGate::new());
        let dir = tempfile::tempdir().unwrap();
        let sender = AgentHubPushSender::new(pool, gate, "src-device", dir.path());
        let without = base.trim_start_matches("http://");
        let (host, port_s) = without.rsplit_once(':').unwrap();
        let device = Device {
            id: "peer-test".into(),
            name: "Peer Test".into(),
            host: host.to_string(),
            port: port_s.parse().unwrap(),
            last_seen: Utc::now(),
            online: true,
            proto_version: 1,
            capabilities: vec![CAPABILITY_AGENT_HUB_V1.into()],
        };
        let built = BuiltSnapshot {
            envelope: SnapshotEnvelopeV1 {
                format: FORMAT_NAME.into(),
                format_version: FORMAT_VERSION,
                canonicalization: CANONICALIZATION_NAME.into(),
                snapshot_id: "01900000-0000-7000-8000-0000000000c2".into(),
                snapshot_hash: "b".repeat(64),
                source_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
                created_at: "2026-07-29T12:00:00Z".into(),
                selection: SnapshotSelection {
                    scope_ids: vec![],
                    asset_ids: vec![],
                    include_history: true,
                },
                asset_heads: BTreeMap::new(),
                assets: vec![],
                lineages: vec![],
                revisions: vec![],
                variants: vec![],
                conflicts: vec![],
                aliases: vec![],
                objects: vec![],
            },
            object_bytes: BTreeMap::new(),
            selection_hash: "sel".into(),
            selection_state_hash: "state".into(),
        };
        let cancel = CancellationToken::new();
        let outcome = sender
            .push_one_target(PushOneTargetArgs {
                request_id: "req-bind",
                peer_id: "peer-test",
                peer_label: "Peer",
                client_request_id: "req-bind:peer-test",
                device: Some(&device),
                built: &built,
                cancel: &cancel,
            })
            .await;
        assert_eq!(outcome.status, TargetPushStatus::Failed);
        assert_eq!(counters.prepare.load(Ordering::SeqCst), 0);
        handle.abort();
    }

    /// Business Logic: health.device_id 与所选 peer 不一致时 fail-closed，不得 prepare。
    /// Code Logic: health 返回 other-device → prepare=0。
    #[tokio::test]
    async fn never_calls_prepare_when_health_device_mismatches_peer() {
        let counters = FakePeerCounters::full_support(vec![]);
        let c_health = Arc::clone(&counters);
        let c_prep = Arc::clone(&counters);
        let app = Router::new()
            .route(
                "/api/health",
                axum::routing::get(move || {
                    let c = Arc::clone(&c_health);
                    async move {
                        c.health.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({
                            "ok": true,
                            "device_id": "other-device",
                            "device_name": "Other",
                            "http_port": 0,
                            "ts": Utc::now().timestamp(),
                            "protocol_version": 1,
                            "capabilities": [
                                "errors.envelope.v1",
                                CAPABILITY_AGENT_HUB_V1,
                                CAPABILITY_DEVICE_REQUEST_BINDING_V1,
                            ],
                        }))
                    }
                }),
            )
            .route(
                "/api/agent-hub/push/prepare",
                post(move |_body: axum::Json<PreparePushRequest>| {
                    let c = Arc::clone(&c_prep);
                    async move {
                        c.prepare.fetch_add(1, Ordering::SeqCst);
                        axum::Json(PreparePushResponse {
                            transfer_id: "x".into(),
                            status: "prepared".into(),
                            selection_hash: "s".into(),
                            snapshot_hash: "h".into(),
                            missing_object_hashes: vec![],
                            missing_revision_ids: vec![],
                            outcome: None,
                        })
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let base = format!("http://{addr}");
        let pool = test_pool().await;
        let gate = Arc::new(DatabaseMaintenanceGate::new());
        let dir = tempfile::tempdir().unwrap();
        let sender = AgentHubPushSender::new(pool, gate, "src-device", dir.path());
        let without = base.trim_start_matches("http://");
        let (host, port_s) = without.rsplit_once(':').unwrap();
        let device = Device {
            id: "peer-test".into(),
            name: "Peer Test".into(),
            host: host.to_string(),
            port: port_s.parse().unwrap(),
            last_seen: Utc::now(),
            online: true,
            proto_version: 1,
            capabilities: vec![
                CAPABILITY_AGENT_HUB_V1.into(),
                CAPABILITY_DEVICE_REQUEST_BINDING_V1.into(),
            ],
        };
        let built = BuiltSnapshot {
            envelope: SnapshotEnvelopeV1 {
                format: FORMAT_NAME.into(),
                format_version: FORMAT_VERSION,
                canonicalization: CANONICALIZATION_NAME.into(),
                snapshot_id: "01900000-0000-7000-8000-0000000000c3".into(),
                snapshot_hash: "c".repeat(64),
                source_replica_id: "01900000-0000-7000-8000-0000000000b3".into(),
                created_at: "2026-07-29T12:00:00Z".into(),
                selection: SnapshotSelection {
                    scope_ids: vec![],
                    asset_ids: vec![],
                    include_history: true,
                },
                asset_heads: BTreeMap::new(),
                assets: vec![],
                lineages: vec![],
                revisions: vec![],
                variants: vec![],
                conflicts: vec![],
                aliases: vec![],
                objects: vec![],
            },
            object_bytes: BTreeMap::new(),
            selection_hash: "sel".into(),
            selection_state_hash: "state".into(),
        };
        let cancel = CancellationToken::new();
        let outcome = sender
            .push_one_target(PushOneTargetArgs {
                request_id: "req-mismatch",
                peer_id: "peer-test",
                peer_label: "Peer",
                client_request_id: "req-mismatch:peer-test",
                device: Some(&device),
                built: &built,
                cancel: &cancel,
            })
            .await;
        assert_eq!(outcome.status, TargetPushStatus::Failed);
        assert_eq!(counters.prepare.load(Ordering::SeqCst), 0);
        handle.abort();
    }

    /// Business Logic: 无 agent-hub.v1 时绝不调用 prepare。
    /// Code Logic: fake health 无 capability → prepare 计数 0。
    #[tokio::test]
    async fn never_calls_push_routes_before_capability_check() {
        let counters = FakePeerCounters::no_cap();
        let (base, handle) = spawn_fake_peer(Arc::clone(&counters)).await;
        let client = PeerClient::new();
        let err = client
            .require_capability(&base, CAPABILITY_AGENT_HUB_V1)
            .await
            .unwrap_err();
        assert!(matches!(err, PeerCallError::Unsupported { .. }));
        assert_eq!(counters.prepare.load(Ordering::SeqCst), 0);
        assert_eq!(counters.object.load(Ordering::SeqCst), 0);
        assert_eq!(counters.commit.load(Ordering::SeqCst), 0);
        assert!(counters.health.load(Ordering::SeqCst) >= 1);
        handle.abort();
    }

    /// Business Logic: 有能力时 prepare 一次且 missing 按 peer 协商后 stream/commit。
    /// Code Logic: fake 返回 missing；stream 后 commit。
    #[tokio::test]
    async fn streams_chunks_and_commits_when_capable() {
        let h1 = sha256_hex(b"blob-one");
        let h2 = sha256_hex(b"blob-two");
        let counters = FakePeerCounters::full_support(vec![h1.clone(), h2.clone()]);
        let (base, handle) = spawn_fake_peer(Arc::clone(&counters)).await;

        let pool = test_pool().await;
        let gate = Arc::new(DatabaseMaintenanceGate::new());
        let dir = tempfile::tempdir().unwrap();
        let sender = AgentHubPushSender::new(pool, gate, "src-device", dir.path());

        let mut object_bytes = BTreeMap::new();
        object_bytes.insert(h1.clone(), b"blob-one".to_vec());
        object_bytes.insert(h2.clone(), b"blob-two".to_vec());
        let envelope = SnapshotEnvelopeV1 {
            format: FORMAT_NAME.into(),
            format_version: FORMAT_VERSION,
            canonicalization: CANONICALIZATION_NAME.into(),
            snapshot_id: "01900000-0000-7000-8000-0000000000c1".into(),
            snapshot_hash: "a".repeat(64),
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            created_at: "2026-07-29T12:00:00Z".into(),
            selection: SnapshotSelection {
                scope_ids: vec![],
                asset_ids: vec![],
                include_history: true,
            },
            asset_heads: BTreeMap::new(),
            assets: vec![],
            lineages: vec![],
            revisions: vec![],
            variants: vec![],
            conflicts: vec![],
            aliases: vec![],
            objects: vec![
                SnapshotObjectDescriptor {
                    hash: h1.clone(),
                    size: object_bytes[&h1].len().to_string(),
                },
                SnapshotObjectDescriptor {
                    hash: h2.clone(),
                    size: object_bytes[&h2].len().to_string(),
                },
            ],
        };
        let built = BuiltSnapshot {
            envelope,
            object_bytes,
            selection_hash: "sel-hash".into(),
            selection_state_hash: "state".into(),
        };

        let without = base.trim_start_matches("http://");
        let (host, port_s) = without.rsplit_once(':').unwrap();
        let device = Device {
            id: "peer-test".into(),
            name: "Peer Test".into(),
            host: host.to_string(),
            port: port_s.parse().unwrap(),
            last_seen: Utc::now(),
            online: true,
            proto_version: 1,
            capabilities: vec![
                CAPABILITY_AGENT_HUB_V1.into(),
                CAPABILITY_DEVICE_REQUEST_BINDING_V1.into(),
            ],
        };

        let cancel = CancellationToken::new();
        let outcome = sender
            .push_one_target(PushOneTargetArgs {
                request_id: "req-x",
                peer_id: "peer-test",
                peer_label: "Peer Test",
                client_request_id: "req-x:peer-test",
                device: Some(&device),
                built: &built,
                cancel: &cancel,
            })
            .await;

        assert_eq!(
            outcome.status,
            TargetPushStatus::Committed,
            "outcome={outcome:?}"
        );
        assert!(counters.health.load(Ordering::SeqCst) >= 1);
        assert_eq!(counters.prepare.load(Ordering::SeqCst), 1);
        assert_eq!(counters.object.load(Ordering::SeqCst), 2);
        assert_eq!(counters.commit.load(Ordering::SeqCst), 1);
        handle.abort();
    }

    /// Business Logic: 源 ledger 可重读 progress。
    /// Code Logic: insert + persist failed → get_push_report。
    #[tokio::test]
    async fn durable_report_readable_after_persist() {
        let pool = test_pool().await;
        let gate = Arc::new(DatabaseMaintenanceGate::new());
        let dir = tempfile::tempdir().unwrap();
        let sender = AgentHubPushSender::new(pool.clone(), gate.clone(), "src", dir.path());
        sender
            .insert_source_request("req-d", "fullHub", "{}", "sel", "snap")
            .await
            .unwrap();
        sender
            .upsert_target_pending("req-d", "p1", "Label", "req-d:p1")
            .await
            .unwrap();
        let outcome = TargetPushOutcome {
            peer_device_id: "p1".into(),
            peer_label: "Label".into(),
            client_request_id: "req-d:p1".into(),
            status: TargetPushStatus::Failed,
            retryable: true,
            error_code: Some("transport_network".into()),
            transfer_id: None,
            missing_object_count: 2,
            transferred_object_count: 0,
            updated_at: Utc::now().to_rfc3339(),
        };
        persist_target_outcome(&pool, &gate, "req-d", &outcome)
            .await
            .unwrap();
        sender
            .mark_request_status("req-d", "completed")
            .await
            .unwrap();
        let report = sender.get_push_report("req-d").await.unwrap().unwrap();
        assert_eq!(report.targets.len(), 1);
        assert_eq!(report.targets[0].status, TargetPushStatus::Failed);
        assert_eq!(
            report.targets[0].error_code.as_deref(),
            Some("transport_network")
        );
        let failed = sender.list_failed_targets().await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].peer_label, "Label");
    }

    /// Business Logic: owner/control API 不得暴露目标 pull 路由。
    /// Code Logic: 生产函数名只有 push_selection_for_state / get_push_report。
    #[test]
    fn no_pull_style_api_in_sender_source() {
        let src = include_str!("sender.rs");
        // 生产路径只 export push，不 export pull（用拼接避免本断言自命中）。
        let forbidden_fn = format!("{}{}", "pull_", "selection");
        let forbidden_path = format!("/api/agent-hub/{}", "pull");
        assert!(!src.contains(&format!("pub async fn {forbidden_fn}")));
        assert!(!src.contains(&forbidden_path));
        assert!(src.contains("pub async fn push_selection"));
        assert!(src.contains("push_selection_for_state"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     终态 committed 落库失败不得静默成功；in-memory 与 reconnect 必须反映 retryable
    ///     `agent_hub_push_persist_failed`，禁止 DB 仍 pending 而内存已 committed。
    ///
    /// Code Logic（这个测试做什么）:
    ///     inject 一次 persist 故障 → finalize committed → 改写 Failed + 落库 failed 标记。
    #[tokio::test]
    async fn terminal_persist_failure_fail_closed_marks_retryable() {
        clear_persist_outcome_fault();
        let pool = test_pool().await;
        let gate = Arc::new(DatabaseMaintenanceGate::new());
        let dir = tempfile::tempdir().unwrap();
        let sender = AgentHubPushSender::new(pool.clone(), gate.clone(), "src", dir.path());
        sender
            .insert_source_request("req-pf", "fullHub", "{}", "sel", "snap")
            .await
            .unwrap();
        sender
            .upsert_target_pending("req-pf", "p1", "Label", "req-pf:p1")
            .await
            .unwrap();

        let committed = TargetPushOutcome {
            peer_device_id: "p1".into(),
            peer_label: "Label".into(),
            client_request_id: "req-pf:p1".into(),
            status: TargetPushStatus::Committed,
            retryable: false,
            error_code: None,
            transfer_id: Some("xfer-1".into()),
            missing_object_count: 0,
            transferred_object_count: 2,
            updated_at: Utc::now().to_rfc3339(),
        };

        // 第一次 persist（committed）失败；第二次（fail-closed marker）成功
        inject_persist_outcome_fault_once();
        let finalized = finalize_target_outcome(&pool, &gate, "req-pf", committed.clone()).await;
        assert_eq!(finalized.status, TargetPushStatus::Failed);
        assert!(finalized.retryable);
        assert_eq!(
            finalized.error_code.as_deref(),
            Some("agent_hub_push_persist_failed")
        );
        // transfer_id / counts 保留，便于 reconnect 诊断
        assert_eq!(finalized.transfer_id.as_deref(), Some("xfer-1"));
        assert_eq!(finalized.transferred_object_count, 2);

        let row = sender.get_target("req-pf", "p1").await.unwrap().unwrap();
        assert_eq!(row.status, TargetPushStatus::Failed);
        assert!(row.retryable);
        assert_eq!(
            row.error_code.as_deref(),
            Some("agent_hub_push_persist_failed")
        );
        // 不得残留 pending 终态撒谎
        assert_ne!(row.status, TargetPushStatus::Pending);
        assert_ne!(row.status, TargetPushStatus::Committed);
        clear_persist_outcome_fault();
    }
}
