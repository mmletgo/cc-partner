//! portable inventory 进程级只读缓存。
//!
//! Business Logic（为什么需要这个模块）:
//!     inspect 扫盘 + ensure_managed + reconcile 较重；同进程短时间内重复 inspect
//!     （skill↔plugin 回切、mutation 前 refresh）应命中缓存。
//!
//! Code Logic（这个模块做什么）:
//!     全局 Mutex 按 query 分槽缓存（本机 inventory）；TTL 到期或显式 invalidate 后 miss。
//!     不缓存 peer 路径（当前 inspect 仅本机）。

use crate::agent_hub::portable_inventory::models::{
    PortableInventoryQuery, PortableInventorySnapshotDto,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 进程内短窗去重（并发/连点 inspect）；长时 retain 由前端 soft TTL 负责。
/// mutation 后必须 invalidate，不依赖本 TTL。
const PORTABLE_INVENTORY_CACHE_TTL: Duration = Duration::from_secs(2);

struct CacheEntry {
    snapshot: PortableInventorySnapshotDto,
    fetched_at: Instant,
}

struct CacheState {
    entries: BTreeMap<PortableInventoryQuery, CacheEntry>,
    /// 失效代数。扫描开始时捕获，只有代数未改变才能回填缓存。
    generation: u64,
    /// 当前正在执行的扫描。并发 miss 共享同一个通知，避免重复扫盘。
    in_flight: BTreeMap<PortableInventoryQuery, Arc<InFlightScan>>,
}

struct InFlightScan {
    generation: u64,
    notify: Arc<tokio::sync::Notify>,
    completed: AtomicBool,
}

static CACHE: Mutex<CacheState> = Mutex::new(CacheState {
    entries: BTreeMap::new(),
    generation: 0,
    in_flight: BTreeMap::new(),
});

/// 缓存查找结果：命中、等待现有扫描或成为当前扫描 owner。
pub enum CacheLookup {
    /// 在 TTL 内找到快照。
    Hit(PortableInventorySnapshotDto),
    /// 另一个任务正在扫描，等待其完成后重试。
    Wait(CacheWait),
    /// 当前任务负责执行扫描；完成时必须调用 `complete_scan`。
    Leader(CacheScanGuard),
}

/// 等待 single-flight owner 完成，同时避免 notify 与 await 之间的丢唤醒竞态。
pub struct CacheWait {
    scan: Arc<InFlightScan>,
}

impl CacheWait {
    /// 等待 owner 完成；若 owner 已完成则立即返回。
    pub async fn wait(self) {
        // 先创建 notified future，再检查 completed：owner 若在检查前完成会唤醒
        // 已注册的 future；若在创建前完成则 completed 检查会短路。
        let notified = self.scan.notify.notified();
        if self.scan.completed.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

/// 当前扫描的代数与通知句柄。
pub struct CacheScanGuard {
    query: PortableInventoryQuery,
    scan: Arc<InFlightScan>,
}

impl CacheScanGuard {
    /// 本次扫描开始时的缓存代数。
    pub fn generation(&self) -> u64 {
        self.scan.generation
    }
}

/**
 * Business Logic: mutation / 强制 refresh 后丢弃缓存，避免脏列表。
 * Code Logic: 清空全部 query 缓存槽并抬高 generation。
 */
pub fn invalidate_portable_inventory_cache() {
    if let Ok(mut guard) = CACHE.lock() {
        guard.entries.clear();
        guard.generation = guard.generation.wrapping_add(1);
    }
}

/**
 * Business Logic: 返回未过期的本机 inventory snapshot。
 * Code Logic: 检查 TTL；锁毒化时视为 miss。
 */
pub fn get_cached_portable_inventory(
    query: PortableInventoryQuery,
) -> Option<PortableInventorySnapshotDto> {
    let guard = CACHE.lock().ok()?;
    let entry = guard.entries.get(&query)?;
    if entry.fetched_at.elapsed() > PORTABLE_INVENTORY_CACHE_TTL {
        return None;
    }
    Some(entry.snapshot.clone())
}

/// 开始一次 single-flight 扫描。
///
/// Business Logic（为什么需要这个函数）:
///     并发 miss 只能执行一次真实扫描；mutation 失效期间旧扫描不得再次写入缓存。
///
/// Code Logic（这个函数做什么）:
///     在 Mutex 内检查 TTL、登记 leader 或返回同一通知；锁毒化时按 miss 处理。
pub fn begin_scan(query: PortableInventoryQuery) -> CacheLookup {
    let Ok(mut guard) = CACHE.lock() else {
        // 锁毒化无法安全共享状态，退化为独立扫描 owner。
        let scan = Arc::new(InFlightScan {
            generation: 0,
            notify: Arc::new(tokio::sync::Notify::new()),
            completed: AtomicBool::new(false),
        });
        return CacheLookup::Leader(CacheScanGuard { query, scan });
    };
    if let Some(entry) = guard.entries.get(&query) {
        if entry.fetched_at.elapsed() <= PORTABLE_INVENTORY_CACHE_TTL {
            return CacheLookup::Hit(entry.snapshot.clone());
        }
    }
    if let Some(in_flight) = guard.in_flight.get(&query) {
        return CacheLookup::Wait(CacheWait {
            scan: in_flight.clone(),
        });
    }
    let scan = Arc::new(InFlightScan {
        generation: guard.generation,
        notify: Arc::new(tokio::sync::Notify::new()),
        completed: AtomicBool::new(false),
    });
    guard.in_flight.insert(query, scan.clone());
    CacheLookup::Leader(CacheScanGuard { query, scan })
}

/// 完成 single-flight 扫描并按 generation 条件回填。
///
/// Business Logic（为什么需要这个函数）:
///     mutation 可能在扫描期间发生；旧结果只能唤醒等待者，不能覆盖 mutation 后状态。
///
/// Code Logic（这个函数做什么）:
///     仅 owner 清理 in-flight；代数相同且扫描成功时写入新快照，最后通知等待者。
pub fn complete_scan(guard: CacheScanGuard, snapshot: Option<PortableInventorySnapshotDto>) {
    if let Ok(mut state) = CACHE.lock() {
        let is_owner = state
            .in_flight
            .get(&guard.query)
            .is_some_and(|current| Arc::ptr_eq(current, &guard.scan));
        if is_owner {
            if state.generation == guard.scan.generation {
                if let Some(snapshot) = snapshot {
                    state.entries.insert(
                        guard.query,
                        CacheEntry {
                            snapshot,
                            fetched_at: Instant::now(),
                        },
                    );
                }
            }
            state.in_flight.remove(&guard.query);
        }
        guard.scan.completed.store(true, Ordering::Release);
        guard.scan.notify.notify_waiters();
    } else {
        guard.scan.completed.store(true, Ordering::Release);
        guard.scan.notify.notify_waiters();
    }
}

/**
 * Business Logic: 成功 inspect 后写入缓存。
 * Code Logic: 覆盖对应 query 槽；锁失败静默。
 */
pub fn store_cached_portable_inventory(
    query: PortableInventoryQuery,
    snapshot: PortableInventorySnapshotDto,
) {
    if let Ok(mut guard) = CACHE.lock() {
        guard.entries.insert(
            query,
            CacheEntry {
                snapshot,
                fetched_at: Instant::now(),
            },
        );
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
        let query = PortableInventoryQuery::default();
        store_cached_portable_inventory(query, empty_snap("a"));
        assert_eq!(
            get_cached_portable_inventory(query)
                .map(|s| s.inventory_snapshot_hash)
                .as_deref(),
            Some("a")
        );
        invalidate_portable_inventory_cache();
        assert!(get_cached_portable_inventory(query).is_none());
    }

    #[test]
    fn generation_invalidates_in_flight_result() {
        invalidate_portable_inventory_cache();
        let query = PortableInventoryQuery::default();
        let CacheLookup::Leader(leader) = begin_scan(query) else {
            panic!("expected scan leader");
        };
        invalidate_portable_inventory_cache();
        complete_scan(leader, Some(empty_snap("stale")));
        assert!(get_cached_portable_inventory(query).is_none());
    }

    #[tokio::test]
    async fn concurrent_miss_shares_single_scan_and_wakes_waiter() {
        invalidate_portable_inventory_cache();
        let query = PortableInventoryQuery::default();
        let CacheLookup::Leader(leader) = begin_scan(query) else {
            panic!("expected scan leader");
        };
        let CacheLookup::Wait(waiter) = begin_scan(query) else {
            panic!("second miss must wait for the leader");
        };
        complete_scan(leader, Some(empty_snap("shared")));
        waiter.wait().await;
        assert!(
            matches!(begin_scan(query), CacheLookup::Hit(snapshot) if snapshot.inventory_snapshot_hash == "shared")
        );
    }

    #[test]
    fn distinct_queries_keep_distinct_cache_entries() {
        invalidate_portable_inventory_cache();
        let skill = PortableInventoryQuery {
            kind: Some(crate::agent_hub::portable_inventory::PortableAssetKind::Skill),
            ..PortableInventoryQuery::default()
        };
        let plugin = PortableInventoryQuery {
            kind: Some(crate::agent_hub::portable_inventory::PortableAssetKind::Plugin),
            ..PortableInventoryQuery::default()
        };
        store_cached_portable_inventory(skill, empty_snap("skill"));
        store_cached_portable_inventory(plugin, empty_snap("plugin"));
        assert_eq!(
            get_cached_portable_inventory(skill).map(|s| s.inventory_snapshot_hash),
            Some("skill".into())
        );
        assert_eq!(
            get_cached_portable_inventory(plugin).map(|s| s.inventory_snapshot_hash),
            Some("plugin".into())
        );
    }
}
