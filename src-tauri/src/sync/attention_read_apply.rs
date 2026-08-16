//! sync/attention_read_apply.rs — Attention 已读元数据 push-batch 落账
//!
//! Business Logic（为什么需要这个模块）:
//!     已读是 per-device 元数据，不是内容 CRDT。任一设备标已读后，其它设备必须看到同一条目已读；
//!     标未读则对端删除本机视角的行。本地 mark 成功不得被对端 push 失败回滚。
//!
//! Code Logic（这个模块做什么）:
//!     - 稳定 payload hash + `sync_request_ledger` 幂等 claim；
//!     - 接收端把每条 op 落到**本机 device_id**（聚合器只读本设备 read_set）；
//!     - 本地写成功后 fire-and-forget 推给在线 peer；`trigger_sync` 再推全量本机已读作追平。

use crate::error::AppError;
use crate::net::protocol::CAPABILITY_ATTENTION_READ_V1;
use crate::state::AppState;
use crate::storage::sync_request_ledger_repo::{
    SyncBatchOutcome, SyncRequestLedgerRepo, DOMAIN_ATTENTION_READ,
};
use crate::storage::AttentionReadRepo;
use crate::sync::protocol::{content_sha256_hex, PUSH_BATCH_BYTES, PUSH_BATCH_ITEMS};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

/// 本机 UI 刷新事件名（读同步落地后 best-effort 广播）。
pub const ATTENTION_CHANGED_EVENT: &str = "attention:changed";

/// push-batch 单条已读/未读。
///
/// Business Logic: 对端需要知道对哪些稳定 item_id 执行 read/unread；origin `device_id`
///     只作审计/指纹，落库一律用接收端本机 device_id，保证聚合器 `load_read_ids(my)` 命中。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionReadPushItem {
    /// 稳定 Inbox item id（如 `orchestrator:human-review:<taskId>`）
    pub item_id: String,
    /// 发起变更的设备（收敛标签，非认证）
    pub device_id: String,
    /// 首次已读 RFC3339；unread 可复用旧值或空串
    pub read_at: String,
    /// `read` 或 `unread`
    pub op: AttentionReadOp,
}

/// 已读同步操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionReadOp {
    /// INSERT OR IGNORE 本机已读行
    Read,
    /// DELETE 本机已读行
    Unread,
}

/// push-batch 请求（snake_case，对齐其它 `/api/sync/*`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionReadPushBatchReq {
    #[serde(default)]
    pub items: Vec<AttentionReadPushItem>,
    pub client_request_id: String,
    #[serde(default)]
    pub claimed_device_id: String,
}

/// push-batch 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionReadPushBatchResp {
    pub accepted: usize,
}

/// 计算 attention-read batch 载荷指纹（与顺序无关）。
///
/// Business Logic（为什么需要这个函数）:
///     ledger 用 payload hash 区分同 `client_request_id` 的不同正文，防止错误重放覆盖。
///
/// Code Logic（这个函数做什么）:
///     按 item_id/op/device_id/read_at 排序后逐字段 SHA-256。
pub fn attention_read_batch_payload_hash(items: &[AttentionReadPushItem]) -> String {
    let mut sorted: Vec<&AttentionReadPushItem> = items.iter().collect();
    sorted.sort_by(|a, b| {
        a.item_id
            .cmp(&b.item_id)
            .then(op_key(a.op).cmp(op_key(b.op)))
            .then(a.device_id.cmp(&b.device_id))
            .then(a.read_at.cmp(&b.read_at))
    });
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for item in sorted {
        parts.push(item.item_id.as_bytes().to_vec());
        parts.push(b"\0".to_vec());
        parts.push(op_key(item.op).as_bytes().to_vec());
        parts.push(b"\0".to_vec());
        parts.push(item.device_id.as_bytes().to_vec());
        parts.push(b"\0".to_vec());
        parts.push(item.read_at.as_bytes().to_vec());
        parts.push(b"\n".to_vec());
    }
    let refs: Vec<&[u8]> = parts.iter().map(|p| p.as_slice()).collect();
    content_sha256_hex(&refs)
}

fn op_key(op: AttentionReadOp) -> &'static str {
    match op {
        AttentionReadOp::Read => "read",
        AttentionReadOp::Unread => "unread",
    }
}

/// 校验 batch 边界与 item 字段。
///
/// Business Logic（为什么需要这个函数）:
///     空键 / 过大 batch 必须在写库前拒绝，避免 ledger 污染。
///
/// Code Logic（这个函数做什么）:
///     条数/估算字节上限；item_id 非空去重；read 要求非空 read_at。
pub fn validate_attention_read_batch(items: &[AttentionReadPushItem]) -> Result<(), AppError> {
    if items.is_empty() {
        return Err(AppError::validation(
            "attention-read push-batch items 不能为空",
        ));
    }
    if items.len() > PUSH_BATCH_ITEMS {
        return Err(AppError::validation(format!(
            "push-batch 最多 {PUSH_BATCH_ITEMS} 条，收到 {}",
            items.len()
        )));
    }
    let mut estimated = 0usize;
    let mut seen = HashSet::with_capacity(items.len());
    for item in items {
        let id = item.item_id.trim();
        if id.is_empty() {
            return Err(AppError::validation("item_id 不能为空"));
        }
        if id.len() > 256 {
            return Err(AppError::validation("item_id 过长"));
        }
        if !seen.insert(id.to_string()) {
            return Err(AppError::validation("items 含重复 item_id"));
        }
        if matches!(item.op, AttentionReadOp::Read) && item.read_at.trim().is_empty() {
            return Err(AppError::validation("read 操作的 read_at 不能为空"));
        }
        estimated = estimated
            .saturating_add(id.len())
            .saturating_add(item.device_id.len())
            .saturating_add(item.read_at.len())
            .saturating_add(8);
        if estimated > PUSH_BATCH_BYTES {
            return Err(AppError::validation(format!(
                "push-batch 估算超过 {PUSH_BATCH_BYTES} 字节"
            )));
        }
    }
    Ok(())
}

/// 在 ledger 单事务内把对端已读变更落到本机 device_id。
///
/// Business Logic（为什么需要这个函数）:
///     同 key/同 hash 重放必须返回原 accepted 且不重复写；不同 hash 409；中途失败整批回滚。
///
/// Code Logic（这个函数做什么）:
///     `apply_batch_idempotent` → 按 op 调用 `mark_read_on_tx` / `mark_unread_on_tx`（目标=本机 device_id）。
pub async fn apply_attention_read_push_batch(
    repo: &AttentionReadRepo,
    local_device_id: &str,
    claimed_device_id: &str,
    client_request_id: &str,
    payload_hash: &str,
    items: &[AttentionReadPushItem],
) -> Result<SyncBatchOutcome, AppError> {
    validate_attention_read_batch(items)?;
    SyncRequestLedgerRepo::ensure_schema(repo.pool()).await?;
    let local = local_device_id.to_string();
    let items = items.to_vec();
    let ledger = SyncRequestLedgerRepo::with_gate(repo.pool().clone(), repo.gate());
    ledger
        .apply_batch_idempotent(
            claimed_device_id,
            DOMAIN_ATTENTION_READ,
            client_request_id,
            payload_hash,
            |tx| {
                let local = local.clone();
                let items = items.clone();
                async move {
                    let mut accepted = 0usize;
                    let mut reads: Vec<String> = Vec::new();
                    let mut read_at = String::new();
                    let mut unreads: Vec<String> = Vec::new();
                    for item in &items {
                        match item.op {
                            AttentionReadOp::Read => {
                                if read_at.is_empty() {
                                    read_at = item.read_at.clone();
                                }
                                reads.push(item.item_id.clone());
                            }
                            AttentionReadOp::Unread => unreads.push(item.item_id.clone()),
                        }
                    }
                    if !reads.is_empty() {
                        accepted += AttentionReadRepo::mark_read_on_tx(
                            tx,
                            &local,
                            &reads,
                            if read_at.is_empty() {
                                "1970-01-01T00:00:00Z"
                            } else {
                                &read_at
                            },
                        )
                        .await?;
                    }
                    if !unreads.is_empty() {
                        accepted +=
                            AttentionReadRepo::mark_unread_on_tx(tx, &local, &unreads).await?;
                    }
                    Ok(SyncBatchOutcome { accepted })
                }
                .boxed()
            },
        )
        .await
}

/// 构造一条本机已读/未读变更（供命令层 outbound）。
///
/// Business Logic: 本地写成功后把同一批 id 推给 peer。
/// Code Logic: 填充 origin=本机 device_id。
pub fn local_attention_read_items(
    device_id: &str,
    item_ids: &[String],
    op: AttentionReadOp,
    read_at: &str,
) -> Vec<AttentionReadPushItem> {
    item_ids
        .iter()
        .map(|item_id| AttentionReadPushItem {
            item_id: item_id.clone(),
            device_id: device_id.to_string(),
            read_at: read_at.to_string(),
            op,
        })
        .collect()
}

/// 本地 mark 成功后异步把变更推给在线 peer（失败不回滚本地）。
///
/// Business Logic（为什么需要这个函数）:
///     「本地成功即视为已读，远端异步追平」；断网不得让徽章回跳。
///
/// Code Logic（这个函数做什么）:
///     `tauri::async_runtime::spawn` fire-and-forget。
pub fn spawn_push_attention_read_to_peers(state: AppState, items: Vec<AttentionReadPushItem>) {
    if items.is_empty() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        push_attention_read_to_peers(&state, &items).await;
    });
}

/// 把一批已读变更推给当前在线且宣告 `attention.read.v1` 的 peer。
///
/// Business Logic（为什么需要这个函数）:
///     命令层与 `trigger_sync` 追平共用同一出站路径。
///
/// Code Logic（这个函数做什么）:
///     快照 devices → 每设备 health/capability → 按 100 条拆批 POST。
pub async fn push_attention_read_to_peers(state: &AppState, items: &[AttentionReadPushItem]) {
    if items.is_empty() {
        return;
    }
    let devices: Vec<crate::models::device::Device> = {
        let guard = match state.devices.read() {
            Ok(g) => g,
            Err(_) => return,
        };
        guard.values().cloned().collect()
    };
    if devices.is_empty() {
        return;
    }
    let claimed = state.device_id.as_str().to_string();
    for device in devices {
        let base_url = device.base_url();
        match state
            .peer_client
            .require_capability(&base_url, CAPABILITY_ATTENTION_READ_V1)
            .await
        {
            Ok(_) => {}
            Err(e) => {
                tracing::debug!("attention-read 跳过设备 {}: {e}", device.name);
                continue;
            }
        }
        for chunk in items.chunks(PUSH_BATCH_ITEMS) {
            let client_request_id = Uuid::new_v4().to_string();
            if let Err(e) = state
                .peer_client
                .push_attention_read_batch(&base_url, chunk, &client_request_id, &claimed)
                .await
            {
                tracing::debug!(
                    "attention-read push 设备 {} 失败（本地不回滚）: {e}",
                    device.name
                );
            }
        }
    }
}

/// `trigger_sync` 追平：把本机当前全部已读以 read op 推给单设备。
///
/// Business Logic（为什么需要这个函数）:
///     写后即时 push 可能因对端离线丢失；全量已读 INSERT OR IGNORE 可安全补齐。
///     未读撤销不在追平范围（避免把对端本地已读冲掉以外的复杂 tombstone）。
///
/// Code Logic（这个函数做什么）:
///     `load_read_ids` → 组装 read items → 拆批 push。失败仅 debug。
pub async fn catch_up_attention_read_with_peer(state: &AppState, base_url: &str) {
    let local_id = state.device_id.as_str();
    let map = match state.attention_read_repo.load_read_ids(local_id).await {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!("attention-read catch-up 读本地失败: {e}");
            return;
        }
    };
    if map.is_empty() {
        return;
    }
    let mut items: Vec<AttentionReadPushItem> = map
        .into_iter()
        .map(|(item_id, read_at)| AttentionReadPushItem {
            item_id,
            device_id: local_id.to_string(),
            read_at,
            op: AttentionReadOp::Read,
        })
        .collect();
    items.sort_by(|a, b| a.item_id.cmp(&b.item_id));
    let claimed = local_id.to_string();
    for chunk in items.chunks(PUSH_BATCH_ITEMS) {
        let client_request_id = Uuid::new_v4().to_string();
        if let Err(e) = state
            .peer_client
            .push_attention_read_batch(base_url, chunk, &client_request_id, &claimed)
            .await
        {
            tracing::debug!("attention-read catch-up push 失败: {e}");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup_repo() -> AttentionReadRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AttentionReadRepo::ensure_schema(&pool).await.unwrap();
        crate::storage::SyncRequestLedgerRepo::ensure_schema(&pool)
            .await
            .unwrap();
        AttentionReadRepo::new(pool)
    }

    fn item(id: &str, op: AttentionReadOp) -> AttentionReadPushItem {
        AttentionReadPushItem {
            item_id: id.to_string(),
            device_id: "origin-a".to_string(),
            read_at: "2026-08-16T10:00:00Z".to_string(),
            op,
        }
    }

    #[test]
    fn payload_hash_is_order_independent() {
        let a = vec![
            item("i1", AttentionReadOp::Read),
            item("i2", AttentionReadOp::Unread),
        ];
        let b = vec![
            item("i2", AttentionReadOp::Unread),
            item("i1", AttentionReadOp::Read),
        ];
        assert_eq!(
            attention_read_batch_payload_hash(&a),
            attention_read_batch_payload_hash(&b)
        );
        let mut c = a.clone();
        c[0].read_at = "2026-08-16T11:00:00Z".to_string();
        assert_ne!(
            attention_read_batch_payload_hash(&a),
            attention_read_batch_payload_hash(&c)
        );
    }

    #[test]
    fn validate_rejects_empty_and_duplicate() {
        assert!(validate_attention_read_batch(&[]).is_err());
        assert!(validate_attention_read_batch(&[
            item("i1", AttentionReadOp::Read),
            item("i1", AttentionReadOp::Unread)
        ])
        .is_err());
        let mut missing_ts = item("i1", AttentionReadOp::Read);
        missing_ts.read_at.clear();
        assert!(validate_attention_read_batch(&[missing_ts]).is_err());
        assert!(validate_attention_read_batch(&[item("i1", AttentionReadOp::Read)]).is_ok());
    }

    #[tokio::test]
    async fn apply_read_lands_on_local_device_not_origin() {
        let repo = setup_repo().await;
        let outcome = apply_attention_read_push_batch(
            &repo,
            "local-b",
            "origin-a",
            "req-1",
            "hash-1",
            &[item("orchestrator:human-review:t1", AttentionReadOp::Read)],
        )
        .await
        .unwrap();
        assert_eq!(outcome.accepted, 1);
        let local = repo.load_read_ids("local-b").await.unwrap();
        assert!(local.contains_key("orchestrator:human-review:t1"));
        let origin = repo.load_read_ids("origin-a").await.unwrap();
        assert!(
            origin.is_empty(),
            "必须落到接收端 device_id，否则聚合器看不见已读"
        );
    }

    #[tokio::test]
    async fn same_key_same_hash_replays_without_rewriting() {
        let repo = setup_repo().await;
        let items = vec![item("i1", AttentionReadOp::Read)];
        let hash = attention_read_batch_payload_hash(&items);
        let first = apply_attention_read_push_batch(&repo, "local-b", "a", "req-1", &hash, &items)
            .await
            .unwrap();
        let second = apply_attention_read_push_batch(&repo, "local-b", "a", "req-1", &hash, &items)
            .await
            .unwrap();
        assert_eq!(first.accepted, 1);
        assert_eq!(second.accepted, first.accepted);
        let map = repo.load_read_ids("local-b").await.unwrap();
        assert_eq!(map.get("i1"), Some(&"2026-08-16T10:00:00Z".to_string()));
    }

    #[tokio::test]
    async fn same_key_different_hash_conflicts() {
        let repo = setup_repo().await;
        let items = vec![item("i1", AttentionReadOp::Read)];
        apply_attention_read_push_batch(&repo, "local-b", "a", "req-1", "hash-aaa", &items)
            .await
            .unwrap();
        let err =
            apply_attention_read_push_batch(&repo, "local-b", "a", "req-1", "hash-bbb", &items)
                .await
                .unwrap_err();
        assert!(
            matches!(err, AppError::Conflict(_)),
            "expected Conflict, got {err:?}"
        );
    }

    #[tokio::test]
    async fn apply_unread_deletes_local_row() {
        let repo = setup_repo().await;
        apply_attention_read_push_batch(
            &repo,
            "local-b",
            "a",
            "req-read",
            "h-read",
            &[item("i1", AttentionReadOp::Read)],
        )
        .await
        .unwrap();
        apply_attention_read_push_batch(
            &repo,
            "local-b",
            "a",
            "req-unread",
            "h-unread",
            &[item("i1", AttentionReadOp::Unread)],
        )
        .await
        .unwrap();
        let map = repo.load_read_ids("local-b").await.unwrap();
        assert!(!map.contains_key("i1"));
    }
}
