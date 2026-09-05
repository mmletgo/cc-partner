//! agent_hub/replication/pull/staging — 源端选择暂存（进程内 transfer 缓冲）
//!
//! Business Logic（为什么需要这个模块）:
//!     对端 Pull selection 时，本机作为源端需要把勾选资产的对象字节暂存在进程内，
//!     供分块读取（offset 续传）；LAN 无鉴权，缓冲必须有界（条目数/总字节/TTL），
//!     且读完不能立刻释放（客户端可能丢最终响应需按 offset 重试）。
//!
//! Code Logic（这个模块做什么）:
//!     StoredPortablePullPlan / StoredRemoteItemBinding 持久化 plan 形状；
//!     全局 staging map + insert/remove/evict，上限淘汰最旧，超限 fail-closed。

use super::dto::PortablePullPlanDto;
use crate::agent_hub::snapshot::portable_builder::BuiltPortableSelection;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 内部持久化 plan（JSON 存 SQLite）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StoredPortablePullPlan {
    pub(super) public: PortablePullPlanDto,
    pub(super) remote_item_ids: Vec<String>,
    /// preview 时冻结的 remote item content/tree hash，apply 绑定 selection 用。
    #[serde(default)]
    pub(super) remote_item_bindings: Vec<StoredRemoteItemBinding>,
}

/// preview 冻结的远端 item content 绑定。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StoredRemoteItemBinding {
    pub(super) inventory_item_id: String,
    pub(super) content_hash: Option<String>,
    pub(super) tree_hash: Option<String>,
}

// ───────────────────────── 源端 object staging（进程内 transfer 缓冲） ─────────────────────────

/// Staging 上限：条目数 / 总字节 / TTL。LAN 无鉴权，必须有界。
pub(super) const STAGING_MAX_ENTRIES: usize = 8;
pub(super) const STAGING_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB
const STAGING_TTL: Duration = Duration::from_secs(15 * 60);

pub(super) struct StagedSelection {
    pub(super) built: BuiltPortableSelection,
    created_at: Instant,
    total_bytes: u64,
    /// 已完整读完的 object hash 集合（多对象 transfer 释放依据）。
    pub(super) fully_read_hashes: BTreeSet<String>,
}

pub(super) fn staging() -> &'static Mutex<BTreeMap<String, StagedSelection>> {
    static MAP: OnceLock<Mutex<BTreeMap<String, StagedSelection>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn staged_total_bytes(built: &BuiltPortableSelection) -> u64 {
    built.object_bytes.values().map(|b| b.len() as u64).sum()
}

pub(super) fn evict_expired_staging(map: &mut BTreeMap<String, StagedSelection>) {
    let now = Instant::now();
    map.retain(|_, v| now.duration_since(v.created_at) < STAGING_TTL);
}

pub(super) fn staging_insert(
    transfer_id: String,
    built: BuiltPortableSelection,
) -> Result<(), AppError> {
    let total_bytes = staged_total_bytes(&built);
    let mut g = staging().lock().expect("staging");
    evict_expired_staging(&mut g);
    // 已有同 id 覆盖
    g.remove(&transfer_id);
    let current_bytes: u64 = g.values().map(|s| s.total_bytes).sum();
    if g.len() >= STAGING_MAX_ENTRIES
        || current_bytes.saturating_add(total_bytes) > STAGING_MAX_TOTAL_BYTES
    {
        // 再尝试淘汰最旧
        if let Some(oldest_key) = g
            .iter()
            .min_by_key(|(_, v)| v.created_at)
            .map(|(k, _)| k.clone())
        {
            g.remove(&oldest_key);
        }
    }
    let current_bytes: u64 = g.values().map(|s| s.total_bytes).sum();
    if g.len() >= STAGING_MAX_ENTRIES
        || current_bytes.saturating_add(total_bytes) > STAGING_MAX_TOTAL_BYTES
    {
        return Err(AppError::validation(
            "PORTABLE_PULL_STAGING_LIMIT".to_string(),
        ));
    }
    g.insert(
        transfer_id,
        StagedSelection {
            built,
            created_at: Instant::now(),
            total_bytes,
            fully_read_hashes: BTreeSet::new(),
        },
    );
    Ok(())
}

pub(super) fn staging_remove(transfer_id: &str) {
    if let Ok(mut g) = staging().lock() {
        g.remove(transfer_id);
    }
}
