//! portable inventory 进程级只读缓存。
//!
//! Business Logic（为什么需要这个模块）:
//!     inspect 扫盘 + ensure_managed + reconcile 较重；同进程短时间内重复 inspect
//!     （skill↔plugin 回切、mutation 前 refresh）应命中缓存。
//!
//! Code Logic（这个模块做什么）:
//!     全局 Mutex 单槽缓存（本机 inventory）；TTL 到期或显式 invalidate 后 miss。
//!     不缓存 peer 路径（当前 inspect 仅本机）。

use crate::agent_hub::portable_inventory::models::PortableInventorySnapshotDto;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 进程内短窗去重（并发/连点 inspect）；长时 retain 由前端 soft TTL 负责。
/// mutation 后必须 invalidate，不依赖本 TTL。
const PORTABLE_INVENTORY_CACHE_TTL: Duration = Duration::from_secs(2);

struct CacheEntry {
    snapshot: PortableInventorySnapshotDto,
    fetched_at: Instant,
}

static CACHE: Mutex<Option<CacheEntry>> = Mutex::new(None);

/**
 * Business Logic: mutation / 强制 refresh 后丢弃缓存，避免脏列表。
 * Code Logic: 清空 Mutex 槽。
 */
pub fn invalidate_portable_inventory_cache() {
    if let Ok(mut guard) = CACHE.lock() {
        *guard = None;
    }
}

/**
 * Business Logic: 返回未过期的本机 inventory snapshot。
 * Code Logic: 检查 TTL；锁毒化时视为 miss。
 */
pub fn get_cached_portable_inventory() -> Option<PortableInventorySnapshotDto> {
    let guard = CACHE.lock().ok()?;
    let entry = guard.as_ref()?;
    if entry.fetched_at.elapsed() > PORTABLE_INVENTORY_CACHE_TTL {
        return None;
    }
    Some(entry.snapshot.clone())
}

/**
 * Business Logic: 成功 inspect 后写入缓存。
 * Code Logic: 覆盖单槽；锁失败静默。
 */
pub fn store_cached_portable_inventory(snapshot: PortableInventorySnapshotDto) {
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some(CacheEntry {
            snapshot,
            fetched_at: Instant::now(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::portable_inventory::models::PortableInventorySnapshotDto;

    fn empty_snap(hash: &str) -> PortableInventorySnapshotDto {
        PortableInventorySnapshotDto {
            inventory_snapshot_hash: hash.into(),
            refreshed_at: "2026-08-08T00:00:00Z".into(),
            stale: false,
            targets: vec![],
            items: vec![],
        }
    }

    #[test]
    fn invalidate_clears_hit() {
        invalidate_portable_inventory_cache();
        store_cached_portable_inventory(empty_snap("a"));
        assert_eq!(
            get_cached_portable_inventory()
                .map(|s| s.inventory_snapshot_hash)
                .as_deref(),
            Some("a")
        );
        invalidate_portable_inventory_cache();
        assert!(get_cached_portable_inventory().is_none());
    }
}
