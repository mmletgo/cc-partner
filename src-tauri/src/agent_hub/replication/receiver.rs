//! agent_hub/replication/receiver — prepare / object chunk / commit 业务逻辑
//!
//! Business Logic（为什么需要这个模块）:
//!     LAN 源侧 push 的目标端：校验 SnapshotEnvelope、返回 missing 对象、分块落盘并哈希、
//!     全部 verified 后调用 SnapshotImporter::commit_import，再登记幂等 outcome。
//!     projection 异步 reconcile，不混入协议成功。
//!
//! Code Logic（这个模块做什么）:
//!     staging 在 `<data_dir>/agent-hub/replication/incoming/<transferId>`；
//!     chunk 流式写盘 + 增量 hash；≤8 MiB；复用已 verified object；GC 24h 未验证 staging。

use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::replication::ledger::{
    PushRequestStatus, ReplicationLedger, MAX_STAGING_AGE,
};
use crate::agent_hub::snapshot::builder::hash_selection;
use crate::agent_hub::snapshot::envelope::{
    default_snapshot_limits, validate_snapshot, SnapshotEnvelopeV1, SnapshotObjectDescriptor,
    DEFAULT_MAX_CHUNK_BYTES,
};
use crate::agent_hub::snapshot::importer::{
    ConfirmedImportSelection, SnapshotImportOutcome, SnapshotImporter, ValidatedSnapshot,
};
use crate::error::AppError;
use crate::storage::AgentHubRepo;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// LAN Hub chunk 硬上限（8 MiB），与 SnapshotLimits 一致。
pub const AGENT_HUB_MAX_CHUNK_BYTES: usize = DEFAULT_MAX_CHUNK_BYTES as usize;

/// Dest user-mirror staging 子目录（与旧 push incoming 隔离）。
pub const USER_MIRROR_STAGING_PREFIX: &str = "user-mirror";

/// user-mirror transfer_id 前缀（路径安全，不含 `/`）。
pub const USER_MIRROR_TRANSFER_ID_PREFIX: &str = "umirror-";

/// Ledger 幂等键前缀，避免与旧 push `(sourceDeviceId, clientRequestId)` 混用。
pub const USER_MIRROR_LEDGER_KEY_PREFIX: &str = "user-mirror/";

/// prepare 请求体。
///
/// Business Logic: envelope + 幂等键 + selectionHash；sourceDeviceId/clientRequestId 非认证。
/// Code Logic: camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparePushRequest {
    /// SnapshotEnvelope v1（含 snapshotHash）
    pub envelope: SnapshotEnvelopeV1,
    /// 源设备 id（幂等标签）
    pub source_device_id: String,
    /// 客户端请求 id
    pub client_request_id: String,
    /// SHA-256(canonical SnapshotSelection)
    pub selection_hash: String,
}

/// prepare 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparePushResponse {
    pub transfer_id: String,
    pub status: String,
    pub selection_hash: String,
    pub snapshot_hash: String,
    /// 本地 CAS/staging 均缺失的 object hash
    pub missing_object_hashes: Vec<String>,
    /// 本地 DAG 缺失的 revision id（envelope 已带全量时通常为空；仍列出以便 sender 诊断）
    pub missing_revision_ids: Vec<String>,
    /// 若已 committed，回放 outcome
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Value>,
}

/// object chunk 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutObjectResponse {
    pub transfer_id: String,
    pub object_hash: String,
    pub received_bytes: u64,
    pub expected_size: u64,
    pub verified: bool,
}

/// commit 请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitPushRequest {
    pub source_device_id: String,
    pub client_request_id: String,
    pub selection_hash: String,
    pub snapshot_hash: String,
}

/// commit 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitPushResponse {
    pub transfer_id: String,
    pub status: String,
    pub selection_hash: String,
    pub snapshot_hash: String,
    pub outcome: SnapshotImportOutcome,
    /// 投影是否排队（协议成功独立；queued/blocked 不否决 commit）
    pub projection: String,
}

/// 路由层 chunk 上限类型别名（供 http_server DefaultBodyLimit）。
pub type AgentHubChunkLimit = usize;

/// 计算 incoming staging 根目录。
///
/// Business Logic: 路径固定在 data_dir 下，private 权限。
/// Code Logic: `<data_dir>/agent-hub/replication/incoming`。
pub fn incoming_root(data_dir: &Path) -> PathBuf {
    data_dir
        .join("agent-hub")
        .join("replication")
        .join("incoming")
}

/// transfer staging 目录。
///
/// Business Logic: user-mirror 前缀目录与旧 push incoming 隔离，避免混用明文对象。
/// Code Logic: `umirror-*` → `incoming/user-mirror/<id>`，其余仍 `incoming/<id>`。
fn transfer_dir(data_dir: &Path, transfer_id: &str) -> Result<PathBuf, AppError> {
    validate_transfer_id(transfer_id)?;
    if transfer_id.starts_with(USER_MIRROR_TRANSFER_ID_PREFIX) {
        Ok(incoming_root(data_dir)
            .join(USER_MIRROR_STAGING_PREFIX)
            .join(transfer_id))
    } else {
        Ok(incoming_root(data_dir).join(transfer_id))
    }
}

/// object staging 文件路径（不预 join CAS objects）。
fn object_staging_path(
    data_dir: &Path,
    transfer_id: &str,
    object_hash: &str,
) -> Result<PathBuf, AppError> {
    validate_object_hash(object_hash)?;
    Ok(transfer_dir(data_dir, transfer_id)?.join(format!("{object_hash}.part")))
}

/// transfer_id 仅允许安全路径组件。
fn validate_transfer_id(transfer_id: &str) -> Result<(), AppError> {
    if transfer_id.is_empty()
        || transfer_id.contains('/')
        || transfer_id.contains('\\')
        || transfer_id.contains("..")
        || transfer_id.len() > 128
    {
        return Err(AppError::validation(
            "agent_hub_push_transfer_id_invalid".to_string(),
        ));
    }
    Ok(())
}

/// object hash 必须为 64 hex。
fn validate_object_hash(hash: &str) -> Result<(), AppError> {
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::validation(
            "agent_hub_push_object_hash_invalid".to_string(),
        ));
    }
    Ok(())
}

/// 确保目录存在并设 private 权限（Unix 0700）。
fn ensure_private_dir(path: &Path) -> Result<(), AppError> {
    std::fs::create_dir_all(path)
        .map_err(|e| AppError::generic(format!("agent_hub_push_staging_mkdir:{e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// 本地 CAS 是否已有该 blob。
///
/// Business Logic: prepare 应跳过已在 CAS 的对象。
/// Code Logic: get_blob 成功即存在。
async fn cas_has_blob(store: &ObjectStore, hash: &str) -> bool {
    store.get_blob(hash).await.is_ok()
}

/// prepare：校验 envelope/selection，创建或回放 ledger，返回 missing hashes。
///
/// Business Logic:
///     - 同 (source,request) + 同 selection/snapshot → 回放 prior prepared/committed，
///       并 **始终** re-ensure object 行（修复中途崩溃后缺 object 登记）；
///     - 不同 hash → conflict；
///     - invalid manifest → validation，不建 active ledger；
///     - 并发 UNIQUE：re-get 同 hash 当 replay，不同 hash 仍 conflict。
///
/// Code Logic:
///     validate_snapshot → hash_selection 比对 → ledger get 或
///     insert_prepared_with_objects（同 TX）→ 冲突 re-read → build_prepare_response。
pub async fn prepare_push(
    repo: &AgentHubRepo,
    objects: &ObjectStore,
    ledger: &ReplicationLedger,
    data_dir: &Path,
    req: PreparePushRequest,
) -> Result<PreparePushResponse, AppError> {
    prepare_push_with_transfer_prefix(repo, objects, ledger, data_dir, req, "").await
}

/// prepare，允许为 user-mirror 指定 transfer_id 前缀。
///
/// Business Logic: dest 镜像接收必须生成 `umirror-*` transfer，才能落到独立 staging 前缀。
/// Code Logic: 非空 prefix 时 `transfer_id = prefix + uuid`；空 prefix 保持旧 push UUID。
pub async fn prepare_push_with_transfer_prefix(
    _repo: &AgentHubRepo,
    objects: &ObjectStore,
    ledger: &ReplicationLedger,
    data_dir: &Path,
    req: PreparePushRequest,
    transfer_id_prefix: &str,
) -> Result<PreparePushResponse, AppError> {
    let source = req.source_device_id.trim();
    let client_req = req.client_request_id.trim();
    if source.is_empty() || client_req.is_empty() {
        return Err(AppError::validation(
            "agent_hub_push_source_or_request_empty".to_string(),
        ));
    }
    if source.len() > 256 || client_req.len() > 256 {
        return Err(AppError::validation(
            "agent_hub_push_idempotency_key_too_long".to_string(),
        ));
    }

    // ── validate envelope（失败不写 ledger）────────────────────────────
    let limits = default_snapshot_limits();
    let envelope_json = serde_json::to_string(&req.envelope).map_err(|_| {
        AppError::validation("agent_hub_push_envelope_serialize_failed".to_string())
    })?;
    if envelope_json.len() as u64 > limits.max_manifest_bytes {
        return Err(AppError::validation(format!(
            "agent_hub_push_manifest_too_large:actual={}:limit={}",
            envelope_json.len(),
            limits.max_manifest_bytes
        )));
    }
    let envelope = validate_snapshot(&envelope_json, &limits)
        .map_err(|e| AppError::validation(format!("agent_hub_push_invalid_manifest:{e}")))?;

    let computed_sel = hash_selection(&envelope.selection)?;
    if computed_sel != req.selection_hash {
        return Err(AppError::validation(
            "agent_hub_push_selection_hash_mismatch".to_string(),
        ));
    }
    let snapshot_hash = envelope.snapshot_hash.clone();

    // 预解析 object size（insert 前校验，避免半截 ledger）
    let mut object_specs: Vec<(String, u64)> = Vec::with_capacity(envelope.objects.len());
    for desc in &envelope.objects {
        let size: u64 = desc.size.parse().map_err(|_| {
            AppError::validation(format!("agent_hub_push_object_size_invalid:{}", desc.hash))
        })?;
        if size > limits.max_blob_bytes {
            return Err(AppError::validation(format!(
                "agent_hub_push_blob_too_large:hash={}:size={size}:limit={}",
                desc.hash, limits.max_blob_bytes
            )));
        }
        object_specs.push((desc.hash.clone(), size));
    }

    // ── 幂等查重 ───────────────────────────────────────────────────────
    if let Some(existing) = ledger.get_request(source, client_req).await? {
        if existing.selection_hash != req.selection_hash || existing.snapshot_hash != snapshot_hash
        {
            return Err(AppError::conflict(
                "agent_hub_push_idempotency_hash_conflict".to_string(),
            ));
        }
        // same-hash replay：补齐可能缺失的 object 行（中途崩溃修复）
        return build_prepare_response(
            objects,
            ledger,
            data_dir,
            &existing,
            &envelope,
            &object_specs,
        )
        .await;
    }

    // ── 新建 prepared ──────────────────────────────────────────────────
    let transfer_id = if transfer_id_prefix.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        format!("{transfer_id_prefix}{}", Uuid::new_v4())
    };
    let dir = transfer_dir(data_dir, &transfer_id)?;
    ensure_private_dir(&dir)?;
    // 保存 envelope 到 staging 便于诊断（非权威；ledger 有 envelope_json）
    let env_path = dir.join("envelope.json");
    std::fs::write(&env_path, &envelope_json)
        .map_err(|e| AppError::generic(format!("agent_hub_push_write_envelope:{e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(0o600));
    }

    let row = match ledger
        .insert_prepared_with_objects(
            source,
            client_req,
            &transfer_id,
            &req.selection_hash,
            &snapshot_hash,
            &envelope_json,
            &object_specs,
        )
        .await
    {
        Ok(row) => row,
        Err(err) if err.ipc_category_code() == "conflict" => {
            // 并发同 key：re-get；同 hash → replay；不同 hash → conflict
            match ledger
                .resolve_after_insert_conflict(
                    source,
                    client_req,
                    &req.selection_hash,
                    &snapshot_hash,
                )
                .await?
            {
                Some(existing) => {
                    // 丢弃本 racer 的 staging 目录（orphan 由 GC 兜底）
                    let _ = std::fs::remove_dir_all(&dir);
                    return build_prepare_response(
                        objects,
                        ledger,
                        data_dir,
                        &existing,
                        &envelope,
                        &object_specs,
                    )
                    .await;
                }
                None => {
                    // 极罕见：UNIQUE 后又被 GC/删；上抛原 conflict
                    return Err(err);
                }
            }
        }
        Err(err) => return Err(err),
    };

    build_prepare_response(objects, ledger, data_dir, &row, &envelope, &object_specs).await
}

/// 构建 prepare 响应；prepared 路径始终 re-ensure envelope 内 object 行。
///
/// Business Logic: same-hash replay 必须恢复 usable put_chunk 路径。
/// Code Logic: 对 prepared 逐 object ensure_object（idempotent），再 diff CAS/verified。
async fn build_prepare_response(
    objects: &ObjectStore,
    ledger: &ReplicationLedger,
    data_dir: &Path,
    row: &crate::agent_hub::replication::ledger::PushRequestRow,
    envelope: &SnapshotEnvelopeV1,
    object_specs: &[(String, u64)],
) -> Result<PreparePushResponse, AppError> {
    // prepared 回放/新建均 re-ensure：修复 insert 后中途崩溃导致缺 object 行
    if row.status == PushRequestStatus::Prepared {
        for (hash, size) in object_specs {
            ledger.ensure_object(&row.transfer_id, hash, *size).await?;
        }
    }

    let mut missing_object_hashes = Vec::new();
    for desc in &envelope.objects {
        if object_available(objects, ledger, data_dir, &row.transfer_id, desc).await? {
            continue;
        }
        missing_object_hashes.push(desc.hash.clone());
    }
    missing_object_hashes.sort();
    missing_object_hashes.dedup();

    // v1：revision 全文在 envelope；validate_snapshot 已保证 referential。
    // missing_revision_ids 预留给 sender 诊断（当前恒空）。
    let missing_revision_ids: Vec<String> = Vec::new();

    let outcome = if row.status == PushRequestStatus::Committed {
        row.outcome_json
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok())
    } else {
        None
    };

    Ok(PreparePushResponse {
        transfer_id: row.transfer_id.clone(),
        status: row.status.as_str().to_string(),
        selection_hash: row.selection_hash.clone(),
        snapshot_hash: row.snapshot_hash.clone(),
        missing_object_hashes,
        missing_revision_ids,
        outcome,
    })
}

/// 对象是否可在 commit 时使用（CAS 或 staging verified）。
async fn object_available(
    objects: &ObjectStore,
    ledger: &ReplicationLedger,
    data_dir: &Path,
    transfer_id: &str,
    desc: &SnapshotObjectDescriptor,
) -> Result<bool, AppError> {
    if cas_has_blob(objects, &desc.hash).await {
        // 同步 ledger verified 标记，便于 interrupted 复用
        if let Some(obj) = ledger.get_object(transfer_id, &desc.hash).await? {
            if !obj.verified {
                let size: u64 = desc.size.parse().unwrap_or(obj.expected_size);
                let _ = ledger
                    .update_object_progress(transfer_id, &desc.hash, size, true)
                    .await;
            }
        }
        return Ok(true);
    }
    if let Some(obj) = ledger.get_object(transfer_id, &desc.hash).await? {
        if obj.verified {
            // 确认 staging 文件仍在
            let path = object_staging_path(data_dir, transfer_id, &desc.hash)?;
            if path.is_file() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// 写入 object chunk。
///
/// Business Logic:
///     offset 必须连续；重叠内容必须一致；chunk≤8MiB；chunk SHA 与 declared 一致；
///     收齐后整对象 SHA 必须等于 objectHash；已 verified 可 no-op。
///
/// Code Logic:
///     打开 staging `.part`，seek offset，写盘，更新 received_bytes；满则 rehash 文件。
pub async fn put_object_chunk(
    ledger: &ReplicationLedger,
    data_dir: &Path,
    transfer_id: &str,
    object_hash: &str,
    offset: u64,
    body: &[u8],
    declared_chunk_sha256: Option<&str>,
) -> Result<PutObjectResponse, AppError> {
    validate_transfer_id(transfer_id)?;
    validate_object_hash(object_hash)?;

    if body.len() > AGENT_HUB_MAX_CHUNK_BYTES {
        return Err(AppError::validation(format!(
            "agent_hub_push_chunk_too_large:actual={}:limit={AGENT_HUB_MAX_CHUNK_BYTES}",
            body.len()
        )));
    }

    if let Some(expected) = declared_chunk_sha256 {
        let actual = sha256_hex(body);
        if actual != expected {
            return Err(AppError::validation(
                "agent_hub_push_chunk_hash_mismatch".to_string(),
            ));
        }
    }

    let req = ledger
        .get_request_by_transfer(transfer_id)
        .await?
        .ok_or_else(|| AppError::not_found("agent_hub_push_transfer_not_found".to_string()))?;
    if req.status == PushRequestStatus::Committed {
        // 已提交：幂等返回 verified
        return Ok(PutObjectResponse {
            transfer_id: transfer_id.to_string(),
            object_hash: object_hash.to_string(),
            received_bytes: 0,
            expected_size: 0,
            verified: true,
        });
    }

    let mut obj = ledger
        .get_object(transfer_id, object_hash)
        .await?
        .ok_or_else(|| {
            AppError::not_found(format!("agent_hub_push_object_not_declared:{object_hash}"))
        })?;

    if obj.verified {
        return Ok(PutObjectResponse {
            transfer_id: transfer_id.to_string(),
            object_hash: object_hash.to_string(),
            received_bytes: obj.received_bytes,
            expected_size: obj.expected_size,
            verified: true,
        });
    }

    // offset 必须 == received_bytes（严格连续；重叠需内容一致）
    if offset > obj.received_bytes {
        return Err(AppError::validation(format!(
            "agent_hub_push_chunk_offset_gap:offset={offset}:received={}",
            obj.received_bytes
        )));
    }
    if offset + body.len() as u64 > obj.expected_size {
        return Err(AppError::validation(
            "agent_hub_push_chunk_exceeds_object_size".to_string(),
        ));
    }

    let path = object_staging_path(data_dir, transfer_id, object_hash)?;
    ensure_private_dir(path.parent().unwrap())?;

    // 重叠区：读出现有字节比对
    if offset < obj.received_bytes {
        let overlap_end = std::cmp::min(offset + body.len() as u64, obj.received_bytes);
        let overlap_len = (overlap_end - offset) as usize;
        if path.is_file() && overlap_len > 0 {
            let mut f = std::fs::File::open(&path)
                .map_err(|e| AppError::generic(format!("agent_hub_push_open_staging:{e}")))?;
            f.seek(SeekFrom::Start(offset))
                .map_err(|e| AppError::generic(format!("agent_hub_push_seek_staging:{e}")))?;
            let mut existing = vec![0u8; overlap_len];
            f.read_exact(&mut existing)
                .map_err(|e| AppError::generic(format!("agent_hub_push_read_staging:{e}")))?;
            if existing != body[..overlap_len] {
                return Err(AppError::conflict(
                    "agent_hub_push_chunk_overlap_mismatch".to_string(),
                ));
            }
        }
        // 仅新尾部需要写入
        if offset + body.len() as u64 <= obj.received_bytes {
            // 完全重叠重放
            return Ok(PutObjectResponse {
                transfer_id: transfer_id.to_string(),
                object_hash: object_hash.to_string(),
                received_bytes: obj.received_bytes,
                expected_size: obj.expected_size,
                verified: false,
            });
        }
    }

    let write_offset = obj.received_bytes;
    let skip = (write_offset - offset) as usize;
    let to_write = &body[skip..];

    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| AppError::generic(format!("agent_hub_push_open_write:{e}")))?;
        f.seek(SeekFrom::Start(write_offset))
            .map_err(|e| AppError::generic(format!("agent_hub_push_seek_write:{e}")))?;
        f.write_all(to_write)
            .map_err(|e| AppError::generic(format!("agent_hub_push_write_chunk:{e}")))?;
        f.sync_all()
            .map_err(|e| AppError::generic(format!("agent_hub_push_sync_chunk:{e}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
    }

    let new_received = write_offset + to_write.len() as u64;
    let mut verified = false;
    if new_received == obj.expected_size {
        // 整文件 rehash
        let file_hash = hash_file(&path)?;
        if file_hash != object_hash {
            // 删坏文件
            let _ = std::fs::remove_file(&path);
            ledger
                .update_object_progress(transfer_id, object_hash, 0, false)
                .await?;
            return Err(AppError::validation(
                "agent_hub_push_object_hash_mismatch".to_string(),
            ));
        }
        verified = true;
    }

    ledger
        .update_object_progress(transfer_id, object_hash, new_received, verified)
        .await?;
    obj.received_bytes = new_received;
    obj.verified = verified;

    Ok(PutObjectResponse {
        transfer_id: transfer_id.to_string(),
        object_hash: object_hash.to_string(),
        received_bytes: new_received,
        expected_size: obj.expected_size,
        verified,
    })
}

fn hash_file(path: &Path) -> Result<String, AppError> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| AppError::generic(format!("agent_hub_push_hash_open:{e}")))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| AppError::generic(format!("agent_hub_push_hash_read:{e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// commit：校验全部 object 可达，import，登记 outcome，enqueue reconcile。
///
/// Business Logic:
///     半截 import 永不暴露；committed 同 hash 回放；同一幂等键只产生一次 import 副作用。
///
/// Code Logic:
///     收集 object bytes（无 DB 写）→ 同一 SQLite 写事务 claim prepared → import bundle →
///     mark_committed → commit；竞争失败者只能回放已保存 outcome。
pub async fn commit_push(
    repo: &AgentHubRepo,
    objects: &ObjectStore,
    ledger: &ReplicationLedger,
    data_dir: &Path,
    transfer_id: &str,
    req: CommitPushRequest,
    enqueue_reconcile: impl FnOnce(&[String]),
) -> Result<CommitPushResponse, AppError> {
    validate_transfer_id(transfer_id)?;
    let source = req.source_device_id.trim();
    let client_req = req.client_request_id.trim();

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

    // 已 committed → 回放 outcome；补偿未完成 projection intent（不得谎称 queued）
    if row.status == PushRequestStatus::Committed {
        let outcome: SnapshotImportOutcome = row
            .outcome_json
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok())
            .ok_or_else(|| {
                AppError::generic("agent_hub_push_committed_outcome_corrupt".to_string())
            })?;
        let pending_intents = repo
            .list_queued_lan_projection_intents(transfer_id)
            .await
            .unwrap_or_default();
        if !pending_intents.is_empty() {
            enqueue_reconcile(&pending_intents);
        }
        // staging cleanup 补偿：committed 后尽量删除 incoming
        let _ = cleanup_transfer_staging(data_dir, transfer_id);
        let projection = if pending_intents.is_empty() {
            "idle".into()
        } else {
            "queued".into()
        };
        return Ok(CommitPushResponse {
            transfer_id: transfer_id.to_string(),
            status: "committed".into(),
            selection_hash: row.selection_hash,
            snapshot_hash: row.snapshot_hash,
            outcome,
            projection,
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

    // 收集全部 object bytes（CAS 或 staging verified）——仅读，无 ledger/import 写
    let object_bytes =
        collect_verified_object_bytes(objects, ledger, data_dir, transfer_id, &envelope).await?;

    // Phase A CAS 写 object store + plan 均在 import TX 外完成（max_connections=1 禁止 TX 内再读 pool）
    let validated = ValidatedSnapshot::from_parts(envelope, object_bytes, Some(limits))?;
    let importer = SnapshotImporter::new(repo.clone(), objects.clone(), data_dir);
    let staged = importer.stage_objects_for_import(&validated).await?;
    let plan = importer
        .plan_import_bundle_public(&validated.envelope, &ConfirmedImportSelection::default())
        .await?;
    let projections_scheduled = plan.projections_scheduled;
    let mut bundle = plan.bundle;
    importer
        .enrich_plugin_edges_for_import_public(
            &mut bundle,
            &validated.envelope,
            &validated.object_bytes,
        )
        .await?;

    // 同一写事务：claim prepared → apply import bundle → mark committed
    // head CAS 会拒绝规划后被本地并发推进的 head；失败后调用方重试可重新 plan。
    use crate::agent_hub::replication::ledger::CommitClaim;
    use crate::storage::maintenance_gate::with_shared_write_lease;
    let outcome = with_shared_write_lease(&repo.gate(), async {
        let mut tx = repo.pool().begin().await?;
        match ledger
            .inspect_commit_claim_on_tx(
                &mut tx,
                source,
                client_req,
                &req.selection_hash,
                &req.snapshot_hash,
            )
            .await?
        {
            CommitClaim::Replay(row) => {
                let outcome: SnapshotImportOutcome = row
                    .outcome_json
                    .as_ref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .ok_or_else(|| {
                        AppError::generic("agent_hub_push_committed_outcome_corrupt".to_string())
                    })?;
                drop(tx);
                return Ok::<(SnapshotImportOutcome, u64), AppError>((outcome, 0));
            }
            CommitClaim::Claimed(_) => {}
        }

        let result = repo.apply_import_bundle_in_tx(&mut tx, &bundle).await?;
        // 与 import/ledger 同 TX 写入 durable projection intent
        let intent_count = repo
            .insert_lan_projection_intents_on_tx(&mut tx, transfer_id, &result.imported_asset_ids)
            .await?;
        let outcome = SnapshotImportOutcome {
            snapshot_id: validated.envelope.snapshot_id.clone(),
            snapshot_hash: validated.envelope.snapshot_hash.clone(),
            imported_asset_ids: result.imported_asset_ids,
            inserted_revisions: result.inserted_revisions,
            deduped_revisions: result.deduped_revisions,
            heads_advanced: result.heads_advanced,
            conflicts_opened: result.conflicts_opened,
            projections_scheduled,
            imported_object_hashes: staged.clone(),
        };
        let outcome_json = serde_json::to_string(&outcome)
            .map_err(|e| AppError::generic(format!("agent_hub_push_outcome_serialize:{e}")))?;
        ledger
            .mark_committed_on_tx(&mut tx, source, client_req, &outcome_json)
            .await?;
        tx.commit().await?;
        Ok((outcome, intent_count))
    })
    .await?;

    let (outcome, intent_count) = outcome;

    // 仅当 durable intent 存在时才 claim queued 并 enqueue
    let projection = if intent_count > 0 || !outcome.imported_asset_ids.is_empty() {
        let pending = repo
            .list_queued_lan_projection_intents(transfer_id)
            .await
            .unwrap_or_else(|_| outcome.imported_asset_ids.clone());
        if !pending.is_empty() {
            enqueue_reconcile(&pending);
            "queued".into()
        } else {
            "idle".into()
        }
    } else {
        "idle".into()
    };

    // 成功 commit 后幂等删除 staging（失败由 GC 补偿）
    let _ = cleanup_transfer_staging(data_dir, transfer_id);

    Ok(CommitPushResponse {
        transfer_id: transfer_id.to_string(),
        status: "committed".into(),
        selection_hash: req.selection_hash,
        snapshot_hash: req.snapshot_hash,
        outcome,
        projection,
    })
}

/// 从 CAS 或已 verified staging 收集 envelope 内全部 object 字节。
///
/// Business Logic: dest user-mirror commit 需要同一套对象校验，但随后走 apply 而非 SnapshotImporter。
/// Code Logic: 优先 ObjectStore；否则要求 ledger verified 且 staging SHA 匹配。
pub async fn collect_verified_object_bytes(
    objects: &ObjectStore,
    ledger: &ReplicationLedger,
    data_dir: &Path,
    transfer_id: &str,
    envelope: &SnapshotEnvelopeV1,
) -> Result<BTreeMap<String, Vec<u8>>, AppError> {
    let mut object_bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let obj_rows = ledger.list_objects(transfer_id).await?;
    let declared: BTreeSet<String> = envelope.objects.iter().map(|o| o.hash.clone()).collect();

    for desc in &envelope.objects {
        if let Ok(bytes) = objects.get_blob(&desc.hash).await {
            object_bytes.insert(desc.hash.clone(), bytes);
            continue;
        }
        let progress = obj_rows.iter().find(|o| o.object_hash == desc.hash);
        let Some(p) = progress else {
            return Err(AppError::validation(format!(
                "agent_hub_push_commit_object_missing:{}",
                desc.hash
            )));
        };
        if !p.verified {
            // 空文件 blob 源端旧 PUT 循环会漏传；0 字节 SHA 可就地合成。
            if desc.size == "0" && desc.hash == sha256_hex(&[]) {
                object_bytes.insert(desc.hash.clone(), Vec::new());
                continue;
            }
            return Err(AppError::validation(format!(
                "agent_hub_push_commit_object_unverified:{}",
                desc.hash
            )));
        }
        let path = object_staging_path(data_dir, transfer_id, &desc.hash)?;
        let bytes = std::fs::read(&path).map_err(|e| {
            AppError::validation(format!(
                "agent_hub_push_commit_staging_read:{}:{e}",
                desc.hash
            ))
        })?;
        let actual = sha256_hex(&bytes);
        if actual != desc.hash {
            return Err(AppError::validation(
                "agent_hub_push_commit_staging_hash_mismatch".to_string(),
            ));
        }
        object_bytes.insert(desc.hash.clone(), bytes);
    }
    let _ = declared;
    Ok(object_bytes)
}

/// 幂等删除 transfer staging 目录。
///
/// Business Logic: committed 后不得永久保留 incoming 明文/重复 CAS 数据。
/// Code Logic: remove_dir_all(incoming/<transferId> 或 incoming/user-mirror/<transferId>)。
pub fn cleanup_transfer_staging(data_dir: &Path, transfer_id: &str) -> Result<(), AppError> {
    let dir = transfer_dir(data_dir, transfer_id)?;
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| AppError::generic(format!("agent_hub_push_staging_cleanup_failed:{e}")))?;
    }
    Ok(())
}

/// GC：删除超过 24h 的 prepared staging + 清理已 committed 残留 staging。
///
/// Business Logic: 中断传输的未验证 part 可清理；成功 commit 后残留 incoming 也必须收。
///     CAS 永不因 GC 删除。
/// Code Logic: list_stale_prepared + list committed transfers → remove_dir_all staging。
pub async fn gc_abandoned_incoming_staging(
    ledger: &ReplicationLedger,
    data_dir: &Path,
) -> Result<u32, AppError> {
    let cutoff = (Utc::now() - MAX_STAGING_AGE).to_rfc3339();
    let stale = ledger.list_stale_prepared_transfers(&cutoff).await?;
    let mut removed = 0u32;
    for transfer_id in stale {
        let dir = match transfer_dir(data_dir, &transfer_id) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if dir.is_dir() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        ledger.delete_prepared_transfer(&transfer_id).await?;
        removed += 1;
    }
    Ok(removed)
}

/// GC：清理已 committed 且 CAS 完整的 incoming staging 残留。
///
/// Business Logic: 成功 push 后进程崩溃可能留下 staging；周期/启动补偿。
/// Code Logic: 枚举 committed transfer → cleanup_transfer_staging。
pub async fn gc_committed_incoming_staging(
    repo: &AgentHubRepo,
    data_dir: &Path,
) -> Result<u32, AppError> {
    let transfers = repo.list_committed_transfer_ids_for_cleanup(256).await?;
    let mut removed = 0u32;
    for transfer_id in transfers {
        let dir = match transfer_dir(data_dir, &transfer_id) {
            Ok(p) => p,
            Err(_) => {
                // 路径非法也标记完成，避免永远卡住本批
                let _ = repo
                    .mark_committed_transfer_staging_cleaned(&transfer_id)
                    .await;
                continue;
            }
        };
        // 目录不存在或删除成功 → 原子标记 cleanup 完成，推进 keyset
        if !dir.is_dir() {
            if repo
                .mark_committed_transfer_staging_cleaned(&transfer_id)
                .await
                .is_ok()
            {
                removed += 1;
            }
            continue;
        }
        if cleanup_transfer_staging(data_dir, &transfer_id).is_ok()
            && repo
                .mark_committed_transfer_staging_cleaned(&transfer_id)
                .await
                .is_ok()
        {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{AssetKind, AssetPolicy, RevisionOperation, RevisionOriginKind};
    use crate::agent_hub::snapshot::envelope::{
        compute_snapshot_hash, SnapshotAlias, SnapshotAsset, SnapshotEnvelopeV1, SnapshotLineage,
        SnapshotObjectDescriptor, SnapshotRevision, SnapshotSelection, CANONICALIZATION_NAME,
        FORMAT_NAME, FORMAT_VERSION,
    };
    use crate::storage::AgentHubRepo;
    use std::collections::BTreeMap;

    const SECRET: &[u8] = b"plain-fixture-secret";

    async fn test_env() -> (
        AgentHubRepo,
        ObjectStore,
        tempfile::TempDir,
        ReplicationLedger,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let pool = sqlite_pool_connect_memory().await;
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        let repo = AgentHubRepo::new(pool.clone());
        let store = ObjectStore::open(dir.path()).unwrap();
        let ledger = ReplicationLedger::new_standalone(pool);
        (repo, store, dir, ledger)
    }

    async fn sqlite_pool_connect_memory() -> sqlx::SqlitePool {
        sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap()
    }

    fn sample_envelope_with_bytes() -> (SnapshotEnvelopeV1, Vec<u8>) {
        let object_hash = sha256_hex(SECRET);
        let rev_id = "01900000-0000-7000-8000-000000000001";
        let asset_id = "01900000-0000-7000-8000-0000000000a1";
        let lineage_id = asset_id;
        let replica = "01900000-0000-7000-8000-0000000000b1";
        let snap_id = "01900000-0000-7000-8000-0000000000c1";
        let created = "2026-07-29T12:00:00Z";

        let mut envelope = SnapshotEnvelopeV1 {
            format: FORMAT_NAME.into(),
            format_version: FORMAT_VERSION,
            canonicalization: CANONICALIZATION_NAME.into(),
            snapshot_id: snap_id.into(),
            snapshot_hash: "0".repeat(64),
            source_replica_id: replica.into(),
            created_at: created.into(),
            selection: SnapshotSelection {
                scope_ids: vec!["scope-user".into()],
                asset_ids: vec![asset_id.into()],
                include_history: true,
            },
            asset_heads: BTreeMap::from([(asset_id.into(), vec![rev_id.into()])]),
            assets: vec![SnapshotAsset {
                id: asset_id.into(),
                scope_id: "scope-user".into(),
                kind: AssetKind::Mcp,
                origin_namespace: "standalone".into(),
                logical_key: "mcp-secret".into(),
                display_name: "Secret MCP".into(),
                policy: AssetPolicy::Shared,
                deleted_at: None,
            }],
            lineages: vec![SnapshotLineage {
                id: lineage_id.into(),
                root_asset_id: asset_id.into(),
            }],
            revisions: vec![SnapshotRevision {
                id: rev_id.into(),
                asset_lineage_id: lineage_id.into(),
                parents: vec![],
                generation: "0".into(),
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: replica.into(),
                payload_hash: Some(object_hash.clone()),
                tree_manifest_hash: None,
                created_at: created.into(),
            }],
            variants: vec![],
            conflicts: vec![],
            aliases: vec![SnapshotAlias {
                kind: "hubProjectId".into(),
                external_id: "ext-1".into(),
                local_id: "local-1".into(),
            }],
            objects: vec![SnapshotObjectDescriptor {
                hash: object_hash,
                size: (SECRET.len() as u64).to_string(),
            }],
        };
        envelope.snapshot_hash = compute_snapshot_hash(&envelope).expect("hash");
        (envelope, SECRET.to_vec())
    }

    #[tokio::test]
    async fn prepare_idempotent_same_hashes() {
        let (repo, store, dir, ledger) = test_env().await;
        let (envelope, _) = sample_envelope_with_bytes();
        let sel = hash_selection(&envelope.selection).unwrap();
        let req = PreparePushRequest {
            envelope: envelope.clone(),
            source_device_id: "dev-a".into(),
            client_request_id: "req-1".into(),
            selection_hash: sel.clone(),
        };
        let r1 = prepare_push(&repo, &store, &ledger, dir.path(), req.clone())
            .await
            .unwrap();
        assert_eq!(r1.status, "prepared");
        assert_eq!(r1.missing_object_hashes.len(), 1);
        let r2 = prepare_push(&repo, &store, &ledger, dir.path(), req)
            .await
            .unwrap();
        assert_eq!(r2.transfer_id, r1.transfer_id);
        assert_eq!(r2.missing_object_hashes, r1.missing_object_hashes);
    }

    #[tokio::test]
    async fn prepare_conflict_on_different_hash() {
        let (repo, store, dir, ledger) = test_env().await;
        let (envelope, _) = sample_envelope_with_bytes();
        let sel = hash_selection(&envelope.selection).unwrap();
        let req = PreparePushRequest {
            envelope: envelope.clone(),
            source_device_id: "dev-a".into(),
            client_request_id: "req-1".into(),
            selection_hash: sel,
        };
        prepare_push(&repo, &store, &ledger, dir.path(), req)
            .await
            .unwrap();
        let mut env2 = envelope;
        env2.assets[0].display_name = "changed".into();
        env2.snapshot_hash = compute_snapshot_hash(&env2).unwrap();
        let sel2 = hash_selection(&env2.selection).unwrap();
        let err = prepare_push(
            &repo,
            &store,
            &ledger,
            dir.path(),
            PreparePushRequest {
                envelope: env2,
                source_device_id: "dev-a".into(),
                client_request_id: "req-1".into(),
                selection_hash: sel2,
            },
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("conflict"),
            "expected conflict: {err}"
        );
    }

    #[tokio::test]
    async fn prepare_invalid_does_not_create_ledger() {
        let (repo, store, dir, ledger) = test_env().await;
        let (mut envelope, _) = sample_envelope_with_bytes();
        envelope.snapshot_hash = "f".repeat(64); // 破坏 hash
        let sel = hash_selection(&envelope.selection).unwrap();
        let err = prepare_push(
            &repo,
            &store,
            &ledger,
            dir.path(),
            PreparePushRequest {
                envelope,
                source_device_id: "dev-a".into(),
                client_request_id: "bad".into(),
                selection_hash: sel,
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("invalid") || err.to_string().contains("hash"));
        assert!(ledger.get_request("dev-a", "bad").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn chunk_and_commit_import_and_replay() {
        let (repo, store, dir, ledger) = test_env().await;
        let (envelope, bytes) = sample_envelope_with_bytes();
        let object_hash = envelope.objects[0].hash.clone();
        let sel = hash_selection(&envelope.selection).unwrap();
        let snap = envelope.snapshot_hash.clone();
        let prep = prepare_push(
            &repo,
            &store,
            &ledger,
            dir.path(),
            PreparePushRequest {
                envelope: envelope.clone(),
                source_device_id: "dev-a".into(),
                client_request_id: "req-c".into(),
                selection_hash: sel.clone(),
            },
        )
        .await
        .unwrap();

        // half chunk then full
        let half = &bytes[..bytes.len() / 2];
        let r_half = put_object_chunk(
            &ledger,
            dir.path(),
            &prep.transfer_id,
            &object_hash,
            0,
            half,
            Some(&sha256_hex(half)),
        )
        .await
        .unwrap();
        assert!(!r_half.verified);

        // 中断：复用 offset
        let rest = &bytes[half.len()..];
        let r_full = put_object_chunk(
            &ledger,
            dir.path(),
            &prep.transfer_id,
            &object_hash,
            half.len() as u64,
            rest,
            Some(&sha256_hex(rest)),
        )
        .await
        .unwrap();
        assert!(r_full.verified);

        let mut scheduled = Vec::new();
        let commit = commit_push(
            &repo,
            &store,
            &ledger,
            dir.path(),
            &prep.transfer_id,
            CommitPushRequest {
                source_device_id: "dev-a".into(),
                client_request_id: "req-c".into(),
                selection_hash: sel.clone(),
                snapshot_hash: snap.clone(),
            },
            |ids| scheduled = ids.to_vec(),
        )
        .await
        .unwrap();
        assert_eq!(commit.status, "committed");
        assert!(
            commit.outcome.inserted_revisions >= 1 || commit.outcome.deduped_revisions >= 1,
            "commit must import or dedupe revisions: {commit:?}"
        );
        // projection enqueue is best-effort; protocol success is independent
        let _ = scheduled;

        // 半 import 不可见：commit 后 CAS 应有 blob
        assert!(store.get_blob(&object_hash).await.is_ok());

        // replay commit
        let replay = commit_push(
            &repo,
            &store,
            &ledger,
            dir.path(),
            &prep.transfer_id,
            CommitPushRequest {
                source_device_id: "dev-a".into(),
                client_request_id: "req-c".into(),
                selection_hash: sel,
                snapshot_hash: snap,
            },
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(replay.outcome.snapshot_hash, commit.outcome.snapshot_hash);
    }

    #[tokio::test]
    async fn reject_out_of_order_and_oversize_chunk() {
        let (repo, store, dir, ledger) = test_env().await;
        let (envelope, bytes) = sample_envelope_with_bytes();
        let object_hash = envelope.objects[0].hash.clone();
        let sel = hash_selection(&envelope.selection).unwrap();
        let prep = prepare_push(
            &repo,
            &store,
            &ledger,
            dir.path(),
            PreparePushRequest {
                envelope,
                source_device_id: "dev-a".into(),
                client_request_id: "req-o".into(),
                selection_hash: sel,
            },
        )
        .await
        .unwrap();

        let err = put_object_chunk(
            &ledger,
            dir.path(),
            &prep.transfer_id,
            &object_hash,
            5, // gap
            &bytes,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("offset") || err.to_string().contains("gap"));

        let big = vec![0u8; AGENT_HUB_MAX_CHUNK_BYTES + 1];
        let err2 = put_object_chunk(
            &ledger,
            dir.path(),
            &prep.transfer_id,
            &object_hash,
            0,
            &big,
            None,
        )
        .await
        .unwrap_err();
        assert!(err2.to_string().contains("too_large") || err2.to_string().contains("chunk"));
    }

    #[tokio::test]
    async fn verified_object_reused_after_interrupt() {
        let (repo, store, dir, ledger) = test_env().await;
        let (envelope, bytes) = sample_envelope_with_bytes();
        let object_hash = envelope.objects[0].hash.clone();
        let sel = hash_selection(&envelope.selection).unwrap();
        let prep = prepare_push(
            &repo,
            &store,
            &ledger,
            dir.path(),
            PreparePushRequest {
                envelope: envelope.clone(),
                source_device_id: "dev-a".into(),
                client_request_id: "req-reuse".into(),
                selection_hash: sel.clone(),
            },
        )
        .await
        .unwrap();
        put_object_chunk(
            &ledger,
            dir.path(),
            &prep.transfer_id,
            &object_hash,
            0,
            &bytes,
            Some(&sha256_hex(&bytes)),
        )
        .await
        .unwrap();
        // 再次 prepare 同请求 → missing 应为空
        let prep2 = prepare_push(
            &repo,
            &store,
            &ledger,
            dir.path(),
            PreparePushRequest {
                envelope,
                source_device_id: "dev-a".into(),
                client_request_id: "req-reuse".into(),
                selection_hash: sel,
            },
        )
        .await
        .unwrap();
        assert!(
            prep2.missing_object_hashes.is_empty(),
            "verified staging should be reused: {:?}",
            prep2.missing_object_hashes
        );
    }

    /// Business Logic: insert 后 ensure 中途崩溃导致无 object 行时，same-hash prepare
    /// 必须 re-ensure 并恢复 usable put_chunk 路径。
    /// Code Logic: 只 insert request、不写 objects → prepare replay → put_object_chunk Ok。
    #[tokio::test]
    async fn prepare_replay_repairs_missing_object_rows() {
        let (repo, store, dir, ledger) = test_env().await;
        let (envelope, bytes) = sample_envelope_with_bytes();
        let object_hash = envelope.objects[0].hash.clone();
        let sel = hash_selection(&envelope.selection).unwrap();
        let snap = envelope.snapshot_hash.clone();
        let transfer_id = "repair-xfer-1".to_string();
        let envelope_json = serde_json::to_string(&envelope).unwrap();

        // 模拟 insert_prepared 成功但 ensure_object 未完成
        ledger
            .insert_prepared(
                "dev-a",
                "req-repair",
                &transfer_id,
                &sel,
                &snap,
                &envelope_json,
            )
            .await
            .unwrap();
        assert!(
            ledger
                .get_object(&transfer_id, &object_hash)
                .await
                .unwrap()
                .is_none(),
            "precondition: no object rows"
        );

        // 未 repair 前 put 应失败
        let bare = put_object_chunk(
            &ledger,
            dir.path(),
            &transfer_id,
            &object_hash,
            0,
            &bytes,
            Some(&sha256_hex(&bytes)),
        )
        .await
        .unwrap_err();
        assert!(
            bare.to_string().contains("object_not_declared"),
            "expected not_declared before repair: {bare}"
        );

        // same-hash prepare replay 应 re-ensure
        let prep = prepare_push(
            &repo,
            &store,
            &ledger,
            dir.path(),
            PreparePushRequest {
                envelope: envelope.clone(),
                source_device_id: "dev-a".into(),
                client_request_id: "req-repair".into(),
                selection_hash: sel,
            },
        )
        .await
        .unwrap();
        assert_eq!(prep.transfer_id, transfer_id);
        assert_eq!(prep.missing_object_hashes, vec![object_hash.clone()]);
        assert!(
            ledger
                .get_object(&transfer_id, &object_hash)
                .await
                .unwrap()
                .is_some(),
            "replay must re-register object rows"
        );

        let put = put_object_chunk(
            &ledger,
            dir.path(),
            &transfer_id,
            &object_hash,
            0,
            &bytes,
            Some(&sha256_hex(&bytes)),
        )
        .await
        .unwrap();
        assert!(put.verified, "chunk path usable after repair: {put:?}");
    }

    /// Business Logic: 并发同 key prepare 的 UNIQUE 路径必须 re-read 成 same-hash replay。
    /// Code Logic: 先 insert；第二次 prepare 命中 get 或 unique→resolve 返回同一 transfer_id。
    #[tokio::test]
    async fn concurrent_same_key_prepare_unique_reread_replays() {
        let (repo, store, dir, ledger) = test_env().await;
        let (envelope, _) = sample_envelope_with_bytes();
        let sel = hash_selection(&envelope.selection).unwrap();
        let req = PreparePushRequest {
            envelope: envelope.clone(),
            source_device_id: "dev-a".into(),
            client_request_id: "req-race".into(),
            selection_hash: sel.clone(),
        };
        let first = prepare_push(&repo, &store, &ledger, dir.path(), req.clone())
            .await
            .unwrap();

        // 直接走 unique 插入路径（模拟两方都 miss get 后的后到者）
        let objects: Vec<(String, u64)> = envelope
            .objects
            .iter()
            .map(|d| (d.hash.clone(), d.size.parse().unwrap()))
            .collect();
        let unique_err = ledger
            .insert_prepared_with_objects(
                "dev-a",
                "req-race",
                "loser-xfer",
                &sel,
                &envelope.snapshot_hash,
                &serde_json::to_string(&envelope).unwrap(),
                &objects,
            )
            .await
            .unwrap_err();
        assert_eq!(unique_err.ipc_category_code(), "conflict");
        let resolved = ledger
            .resolve_after_insert_conflict("dev-a", "req-race", &sel, &envelope.snapshot_hash)
            .await
            .unwrap()
            .expect("same-hash unique must re-read");
        assert_eq!(resolved.transfer_id, first.transfer_id);

        // prepare 入口本身也应幂等回放
        let second = prepare_push(&repo, &store, &ledger, dir.path(), req)
            .await
            .unwrap();
        assert_eq!(second.transfer_id, first.transfer_id);
    }

    /// Business Logic: user-mirror staging 不得混入旧 push incoming 目录。
    /// Code Logic: `umirror-*` 落在 `incoming/user-mirror/`；普通 id 仍在 `incoming/`。
    #[test]
    fn user_mirror_transfer_id_stages_under_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror_id = format!("{USER_MIRROR_TRANSFER_ID_PREFIX}{}", Uuid::new_v4());
        let mirror_dir = transfer_dir(tmp.path(), &mirror_id).unwrap();
        assert!(
            mirror_dir.ends_with(
                std::path::Path::new("agent-hub")
                    .join("replication")
                    .join("incoming")
                    .join(USER_MIRROR_STAGING_PREFIX)
                    .join(&mirror_id)
            ),
            "user-mirror staging path = {mirror_dir:?}"
        );
        let push_id = uuid::Uuid::new_v4().to_string();
        let push_dir = transfer_dir(tmp.path(), &push_id).unwrap();
        assert!(
            !push_dir
                .to_string_lossy()
                .contains(&format!("/{USER_MIRROR_STAGING_PREFIX}/")),
            "old push staging must not use user-mirror prefix: {push_dir:?}"
        );
    }
}
