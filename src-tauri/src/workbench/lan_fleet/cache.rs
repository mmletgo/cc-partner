//! workbench/lan_fleet/cache — Fleet display cache（非调度权威）
//!
//! Business Logic（为什么需要这个模块）:
//!     远端 offline 时 UI 需要 last successful sanitized device DTO 与 capturedAt；
//!     缓存绝不能作为 scheduler 真值或持久化信任表。
//!
//! Code Logic（这个模块做什么）:
//!     进程内 Mutex<HashMap<device_id, LanFleetDeviceSummary>>；put/get/clear。

use super::models::LanFleetDeviceSummary;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// 共享 display cache 句柄。
pub type SharedFleetDisplayCache = Arc<FleetDisplayCache>;

/// 进程内全局 Fleet display cache（非调度权威、不持久化）。
static GLOBAL_FLEET_DISPLAY_CACHE: OnceLock<SharedFleetDisplayCache> = OnceLock::new();

/// 返回进程内全局 display cache。
///
/// Business Logic（为什么需要这个函数）:
///     控制设备 collector / command 共享 last live device DTO，避免塞入 AppState 所有构造点。
///
/// Code Logic（这个函数做什么）:
///     OnceLock 懒初始化 Arc<FleetDisplayCache> 并 clone。
pub fn global_fleet_display_cache() -> SharedFleetDisplayCache {
    GLOBAL_FLEET_DISPLAY_CACHE
        .get_or_init(FleetDisplayCache::shared)
        .clone()
}

/// Fleet 设备级 display cache。
///
/// Business Logic（为什么需要这个结构体）:
///     控制设备聚合时单 device 失败需回落 last live，其它 device 继续 live 合并。
///
/// Code Logic（这个结构体做什么）:
///     内部 Mutex 保护 HashMap；只存已消毒的 device summary。
#[derive(Debug, Default)]
pub struct FleetDisplayCache {
    inner: Mutex<HashMap<String, LanFleetDeviceSummary>>,
}

impl FleetDisplayCache {
    /// 创建空 cache。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     AppState / 测试需要独立 cache 实例。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Default 构造。
    pub fn new() -> Self {
        Self::default()
    }

    /// 包装为共享 Arc。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     collector 与测试共享同一 cache。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Arc::new(Self::new())。
    pub fn shared() -> SharedFleetDisplayCache {
        Arc::new(Self::new())
    }

    /// 写入/覆盖某 device 的 last successful live 摘要。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     live 成功后更新 display cache，供 offline 回落。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 device_id insert；持锁。
    pub fn put(&self, summary: LanFleetDeviceSummary) {
        let mut guard = self.inner.lock().expect("fleet cache 锁中毒");
        guard.insert(summary.device_id.clone(), summary);
    }

    /// 读取某 device 的 cache 副本。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     offline/timeout 路径需要 last display DTO。
    ///
    /// Code Logic（这个函数做什么）:
    ///     get + clone。
    pub fn get(&self, device_id: &str) -> Option<LanFleetDeviceSummary> {
        let guard = self.inner.lock().expect("fleet cache 锁中毒");
        guard.get(device_id).cloned()
    }

    /// 清空全部 cache（测试隔离）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单测之间不得污染 last display。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clear。
    #[allow(dead_code)] // 单测隔离清空 cache API surface
    pub fn clear(&self) {
        let mut guard = self.inner.lock().expect("fleet cache 锁中毒");
        guard.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::lan_fleet::models::{FleetFreshness, FleetReachability};

    /// Business Logic（为什么需要这个测试）:
    ///     cache 必须按 device 独立读写。
    ///
    /// Code Logic（这个测试做什么）:
    ///     put 两个 device，get 断言，clear 后为空。
    #[test]
    fn put_get_and_clear_are_device_scoped() {
        let cache = FleetDisplayCache::new();
        cache.put(LanFleetDeviceSummary {
            device_id: "d1".into(),
            device_name: "One".into(),
            reachability: FleetReachability::Live,
            freshness: FleetFreshness::Live,
            scheduler_slots_used: Some(1),
            scheduler_slots_max: Some(3),
            projects: Vec::new(),
            error_code: None,
            captured_at: Some("2026-07-15T00:00:00Z".into()),
        });
        assert_eq!(cache.get("d1").unwrap().device_name, "One");
        assert!(cache.get("d2").is_none());
        cache.clear();
        assert!(cache.get("d1").is_none());
    }
}
