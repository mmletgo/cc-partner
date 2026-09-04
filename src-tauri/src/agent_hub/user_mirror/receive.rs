//! user_mirror/receive — dest Push 接收（prepare/objects/commit）
//!
//! Business Logic（为什么需要这个模块）:
//!     Push 的 apply 必须在 owning dest 进程执行：收 envelope/objects 后写原生文件与 extras，
//!     不得只把 SnapshotImporter 当成功。staging 与旧 push ledger 隔离。
//!
//! Code Logic（这个模块做什么）:
//!     复用 receiver staging（`incoming/user-mirror/` + ledger 键前缀 `user-mirror/`）；
//!     commit 收集 verified 对象后调用 `apply_user_mirror`。

use super::ledger::UserMirrorPlanRecord;
use super::models::{
    ApplyUserMirrorRequest, UserMirrorInventoryDto, UserMirrorPlanDto, UserMirrorResultDto,
    UserMirrorSelectionFilterDto,
};
use super::selection::UserMirrorObjectBinding;
use super::service::apply_user_mirror;
use crate::agent_hub::object_store::ObjectStore;
use crate::agent_hub::replication::ledger::{PushRequestStatus, ReplicationLedger};
use crate::agent_hub::replication::receiver::{
    cleanup_transfer_staging, collect_verified_object_bytes, incoming_root,
    prepare_push_with_transfer_prefix, put_object_chunk, PreparePushRequest, PreparePushResponse,
    PutObjectResponse, USER_MIRROR_LEDGER_KEY_PREFIX, USER_MIRROR_STAGING_PREFIX,
    USER_MIRROR_TRANSFER_ID_PREFIX,
};
use crate::agent_hub::snapshot::builder::hash_selection;
use crate::agent_hub::snapshot::envelope::{
    default_snapshot_limits, validate_snapshot, SnapshotEnvelopeV1,
};
use crate::error::AppError;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 源端 selection 冻结请求（调用方回传 inventory 快照）。
///
/// Business Logic: 冻结必须对准已预览的 inventory 身份，禁止再勾选条目；
///     selection 只裁剪打包范围，不得改变 inventory 身份 hash。
/// Code Logic: camelCase；`inventory` 为 metadata-only DTO；`selection` 缺省 None = 全量
///     （旧客户端不带字段行为不变）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorSelectionQuery {
    pub inventory: UserMirrorInventoryDto,
    /// 同步范围过滤器；None / 缺省 = 全部资产。
    #[serde(default)]
    pub selection: Option<UserMirrorSelectionFilterDto>,
}

/// 源端 selection 响应。
///
/// Business Logic: dest Pull 用 transfer + envelope + bindings 拉对象后本机 apply。
/// Code Logic: `missing_object_hashes` 为 envelope 全部 object（源端刚冻结）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorSelectionResponse {
    pub transfer_id: String,
    pub envelope: SnapshotEnvelopeV1,
    pub item_bindings: Vec<UserMirrorObjectBinding>,
    pub missing_object_hashes: Vec<String>,
}

/// dest prepare 请求。
///
/// Business Logic: envelope + 幂等键 + planToken + bindings；sourceDeviceId/clientRequestId 非认证。
/// Code Logic: camelCase；bindings 写入独立 staging，commit 时再读。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareUserMirrorRequest {
    pub envelope: SnapshotEnvelopeV1,
    pub source_device_id: String,
    pub client_request_id: String,
    pub selection_hash: String,
    pub plan_token: String,
    #[serde(default)]
    pub item_bindings: Vec<UserMirrorObjectBinding>,
    /// 源侧 preview plan；dest 落库后 commit 才能 claim apply。
    pub plan: UserMirrorPlanDto,
}

/// dest commit 请求。
///
/// Business Logic: 校验 objects 后 dest apply；同幂等键回放，不得只 import canonical。
/// Code Logic: camelCase；`plan_token` 必须与 prepare 写入的一致。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitUserMirrorRequest {
    pub source_device_id: String,
    pub client_request_id: String,
    pub selection_hash: String,
    pub snapshot_hash: String,
    pub plan_token: String,
}

/// dest commit 响应。
///
/// Business Logic: `result` 含原生写盘/portable extras 分项状态，不是 SnapshotImportOutcome。
/// Code Logic: camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitUserMirrorResponse {
    pub transfer_id: String,
    pub status: String,
    pub result: UserMirrorResultDto,
}

/// 把 clientRequestId 命名到 user-mirror ledger 空间。
///
/// Business Logic: 同一 `(source, request)` 不得与旧 push 冲突或回放旧 outcome。
/// Code Logic: `user-mirror/` + 原始 id。
fn namespaced_client_request_id(client_request_id: &str) -> String {
    format!("{USER_MIRROR_LEDGER_KEY_PREFIX}{client_request_id}")
}

/// dest transfer staging 目录（`incoming/user-mirror/<transferId>`）。
fn user_mirror_transfer_dir(
    data_dir: &Path,
    transfer_id: &str,
) -> Result<std::path::PathBuf, AppError> {
    if transfer_id.is_empty()
        || transfer_id.contains('/')
        || transfer_id.contains('\\')
        || transfer_id.contains("..")
    {
        return Err(AppError::validation(
            "agent_hub_push_transfer_id_invalid".to_string(),
        ));
    }
    Ok(incoming_root(data_dir)
        .join(USER_MIRROR_STAGING_PREFIX)
        .join(transfer_id))
}

/// 把 bindings 与 planToken 落到独立 staging，供 commit 读取。
fn write_prepare_sidecar(
    data_dir: &Path,
    transfer_id: &str,
    plan_token: &str,
    bindings: &[UserMirrorObjectBinding],
) -> Result<(), AppError> {
    let dir = user_mirror_transfer_dir(data_dir, transfer_id)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::generic(format!("user_mirror_prepare_sidecar_mkdir:{e}")))?;
    let bindings_json = serde_json::to_vec(bindings)
        .map_err(|e| AppError::generic(format!("user_mirror_bindings_serialize:{e}")))?;
    std::fs::write(dir.join("bindings.json"), bindings_json)
        .map_err(|e| AppError::generic(format!("user_mirror_bindings_write:{e}")))?;
    std::fs::write(dir.join("plan_token"), plan_token.as_bytes())
        .map_err(|e| AppError::generic(format!("user_mirror_plan_token_write:{e}")))?;
    Ok(())
}

/// 读取 prepare 时写入的 sidecar。
fn read_prepare_sidecar(
    data_dir: &Path,
    transfer_id: &str,
) -> Result<(String, Vec<UserMirrorObjectBinding>), AppError> {
    let dir = user_mirror_transfer_dir(data_dir, transfer_id)?;
    let plan_token = std::fs::read_to_string(dir.join("plan_token"))
        .map_err(|e| AppError::validation(format!("user_mirror_plan_token_missing:{e}")))?;
    let bindings_raw = std::fs::read(dir.join("bindings.json"))
        .map_err(|e| AppError::validation(format!("user_mirror_bindings_missing:{e}")))?;
    let bindings: Vec<UserMirrorObjectBinding> = serde_json::from_slice(&bindings_raw)
        .map_err(|e| AppError::validation(format!("user_mirror_bindings_invalid:{e}")))?;
    Ok((plan_token, bindings))
}

/// dest prepare：校验 envelope，创建独立前缀 staging，返回 missing hashes。
///
/// Business Logic（为什么需要这个函数）:
///     Push 对端必须先登记 transfer，才能分块收对象；幂等键与旧 push 隔离。
///
/// Code Logic（这个函数做什么）:
///     namespace clientRequestId → `prepare_push_with_transfer_prefix(umirror-)` → 写 sidecar。
pub async fn prepare_user_mirror(
    state: &AppState,
    req: PrepareUserMirrorRequest,
) -> Result<PreparePushResponse, AppError> {
    if req.plan_token.trim().is_empty() || req.plan.plan_token.trim().is_empty() {
        return Err(AppError::validation(
            super::models::USER_MIRROR_PREVIEW_REQUIRED.to_string(),
        ));
    }
    if req.plan.plan_token.trim() != req.plan_token.trim() {
        return Err(AppError::conflict(
            "agent_hub_user_mirror_plan_token_mismatch".to_string(),
        ));
    }
    persist_dest_plan(state, &req.plan).await?;
    let data_dir = crate::config::data_dir()?;
    let objects = ObjectStore::open(&data_dir)?;
    let ledger =
        ReplicationLedger::new(state.agent_hub_repo.pool(), state.maintenance_gate.clone());
    let push_req = PreparePushRequest {
        envelope: req.envelope,
        source_device_id: req.source_device_id,
        client_request_id: namespaced_client_request_id(&req.client_request_id),
        selection_hash: req.selection_hash,
    };
    let resp = prepare_push_with_transfer_prefix(
        &state.agent_hub_repo,
        &objects,
        &ledger,
        &data_dir,
        push_req,
        USER_MIRROR_TRANSFER_ID_PREFIX,
    )
    .await?;
    write_prepare_sidecar(
        &data_dir,
        &resp.transfer_id,
        req.plan_token.trim(),
        &req.item_bindings,
    )?;
    Ok(resp)
}

/// dest PUT object chunk（≤8 MiB，offset 续传）。
///
/// Business Logic（为什么需要这个函数）:
///     镜像对象必须落到 user-mirror staging，不得写入旧 push incoming。
///
/// Code Logic（这个函数做什么）:
///     拒绝非 `umirror-` transfer_id；委托 `put_object_chunk`（路径按前缀分流）。
pub async fn put_user_mirror_object(
    state: &AppState,
    transfer_id: &str,
    object_hash: &str,
    offset: u64,
    body: &[u8],
    chunk_sha256: Option<&str>,
) -> Result<PutObjectResponse, AppError> {
    if !transfer_id.starts_with(USER_MIRROR_TRANSFER_ID_PREFIX) {
        return Err(AppError::not_found(
            "agent_hub_push_transfer_not_found".to_string(),
        ));
    }
    let data_dir = crate::config::data_dir()?;
    let ledger =
        ReplicationLedger::new(state.agent_hub_repo.pool(), state.maintenance_gate.clone());
    put_object_chunk(
        &ledger,
        &data_dir,
        transfer_id,
        object_hash,
        offset,
        body,
        chunk_sha256,
    )
    .await
}

/// dest commit：校验对象后调用 dest apply（指令+portable+extras）。
///
/// Business Logic（为什么需要这个函数）:
///     commit 成功必须包含原生写盘与多余删除；禁止只 `SnapshotImporter::commit_import`。
///
/// Code Logic（这个函数做什么）:
///     回放 committed outcome；否则收集 verified bytes → `apply_user_mirror` → mark_committed。
pub async fn commit_user_mirror(
    state: &AppState,
    transfer_id: &str,
    req: CommitUserMirrorRequest,
) -> Result<CommitUserMirrorResponse, AppError> {
    if !transfer_id.starts_with(USER_MIRROR_TRANSFER_ID_PREFIX) {
        return Err(AppError::not_found(
            "agent_hub_push_transfer_not_found".to_string(),
        ));
    }
    let data_dir = crate::config::data_dir()?;
    let objects = ObjectStore::open(&data_dir)?;
    let ledger =
        ReplicationLedger::new(state.agent_hub_repo.pool(), state.maintenance_gate.clone());
    let namespaced = namespaced_client_request_id(&req.client_request_id);
    let source = req.source_device_id.trim();
    let client_req = namespaced.as_str();

    let row = ledger
        .get_request_by_transfer(transfer_id)
        .await?
        .ok_or_else(|| AppError::not_found("agent_hub_push_transfer_not_found".to_string()))?;

    if row.source_device_id != source || row.client_request_id != client_req {
        return Err(AppError::conflict(
            "agent_hub_push_commit_identity_mismatch".to_string(),
        ));
    }
    if row.selection_hash != req.selection_hash || row.snapshot_hash != req.snapshot_hash {
        return Err(AppError::conflict(
            "agent_hub_push_commit_hash_conflict".to_string(),
        ));
    }

    if row.status == PushRequestStatus::Committed {
        let result: UserMirrorResultDto = row
            .outcome_json
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok())
            .ok_or_else(|| {
                AppError::generic("agent_hub_push_committed_outcome_corrupt".to_string())
            })?;
        let _ = cleanup_transfer_staging(&data_dir, transfer_id);
        return Ok(CommitUserMirrorResponse {
            transfer_id: transfer_id.to_string(),
            status: "committed".into(),
            result,
        });
    }

    let limits = default_snapshot_limits();
    let envelope = validate_snapshot(&row.envelope_json, &limits)
        .map_err(|e| AppError::validation(format!("agent_hub_push_commit_envelope_invalid:{e}")))?;
    if envelope.snapshot_hash != req.snapshot_hash {
        return Err(AppError::conflict(
            "agent_hub_push_commit_snapshot_hash_mismatch".to_string(),
        ));
    }
    let computed_sel = hash_selection(&envelope.selection)?;
    if computed_sel != req.selection_hash {
        return Err(AppError::conflict(
            "agent_hub_push_commit_selection_hash_mismatch".to_string(),
        ));
    }

    let (stored_plan_token, bindings) = read_prepare_sidecar(&data_dir, transfer_id)?;
    if stored_plan_token.trim() != req.plan_token.trim() {
        return Err(AppError::conflict(
            "agent_hub_user_mirror_plan_token_mismatch".to_string(),
        ));
    }

    let object_bytes =
        collect_verified_object_bytes(&objects, &ledger, &data_dir, transfer_id, &envelope).await?;

    let result = apply_user_mirror(
        state,
        ApplyUserMirrorRequest {
            plan_token: req.plan_token.trim().to_string(),
            client_request_id: req.client_request_id.trim().to_string(),
            // push-dest 不带 request selection；apply 从 dest plan JSON 里读源端携带的 selection。
            selection: None,
        },
        &object_bytes,
        &bindings,
    )
    .await?;

    let outcome_json = serde_json::to_string(&result)
        .map_err(|e| AppError::generic(format!("user_mirror_commit_outcome_serialize:{e}")))?;
    ledger
        .mark_committed(source, client_req, &outcome_json)
        .await?;
    let _ = cleanup_transfer_staging(&data_dir, transfer_id);

    Ok(CommitUserMirrorResponse {
        transfer_id: transfer_id.to_string(),
        status: "committed".into(),
        result,
    })
}

/// dest 落 preview plan，供 commit 的 dest apply claim。
///
/// Business Logic: Push apply 永远在 dest owning process；源侧 plan 必须拷到 dest ledger。
/// Code Logic: INSERT OR IGNORE 同 token，幂等重放 prepare。
async fn persist_dest_plan(state: &AppState, plan: &UserMirrorPlanDto) -> Result<(), AppError> {
    state
        .agent_hub_repo
        .ensure_user_mirror_plan(UserMirrorPlanRecord {
            plan_token: plan.plan_token.clone(),
            expires_at: plan.expires_at.clone(),
            plan_json: serde_json::to_string(plan)?,
            client_request_id: None,
            claimed_at: None,
            consumed_at: None,
            result_json: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        })
        .await
}
