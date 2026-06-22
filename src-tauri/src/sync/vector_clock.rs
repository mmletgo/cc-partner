//! sync/vector_clock.rs — 向量时钟：因果关系跟踪（纯算法）
//!
//! Business Logic（为什么需要这个模块）:
//!     多设备同时编辑 Prompt 时，需要判断哪个版本更新或是否存在冲突。向量时钟是经典的
//!     分布式因果序算法，通过记录每个设备的操作计数来追踪变更历史。对照 Python
//!     `sync/vector_clock.py`，逐字等价（CRDT 正确性根基）。
//!
//! Code Logic（这个模块做什么）:
//!     向量时钟以 `HashMap<String, u64>`（{device_id: counter}）表示，提供三个核心操作：
//!     - `compare`：判断两时钟的偏序关系（Before/After/Equal/Concurrent）；
//!     - `increment`：本地修改后递增本设备计数器（返回新时钟，不改原值）；
//!     - `merge`：同步时逐 key 取 max，生成包含双方因果历史的新时钟。
//!     所有函数均不修改输入，返回新时钟（与 Python 静态方法语义一致）。

use std::collections::HashMap;

/// 向量时钟比较结果（偏序关系）。
///
/// Business Logic: 同步时需据此决定是直接覆盖还是触发冲突解决（LWW）。
///     命名以"参数 a 相对 b"的视角描述，与 `compare(a, b)` 调用方向一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockOrder {
    /// a 每个分量 <= b 且至少一个 <（a 严格落后于 b，即 b 领先）
    Before,
    /// a 每个分量 >= b 且至少一个 >（a 严格领先于 b）
    After,
    /// 两者完全相同
    Equal,
    /// 互有领先（存在冲突，需 LWW 解决）
    Concurrent,
}

/// 取两时钟 key 的并集，逐 key 比较，返回偏序关系。
///
/// Business Logic: 同步时判断两条 Prompt 版本的先后：一方严格领先可直接覆盖，
///     并发则需冲突解决。对照 Python `VectorClock.compare`。
///
/// Code Logic: 遍历 a∪b 的所有 key，缺省值 0。用两个 bool 标记是否存在 a>b 和 b>a 的分量：
///     - 两者都存在 → Concurrent；
///     - 仅 a 有更大数据 → After（a 领先）；
///     - 仅 b 有更大数据 → Before（a 落后）；
///     - 都没有 → Equal。
pub fn compare(a: &HashMap<String, u64>, b: &HashMap<String, u64>) -> ClockOrder {
    let mut a_greater = false;
    let mut b_greater = false;

    // 遍历两时钟 key 的并集（用迭代器链覆盖，缺省 0）
    for key in a.keys().chain(b.keys()) {
        let val_a = a.get(key).copied().unwrap_or(0);
        let val_b = b.get(key).copied().unwrap_or(0);
        if val_a > val_b {
            a_greater = true;
        } else if val_b > val_a {
            b_greater = true;
        }
    }

    match (a_greater, b_greater) {
        (true, true) => ClockOrder::Concurrent,
        (true, false) => ClockOrder::After,
        (false, true) => ClockOrder::Before,
        (false, false) => ClockOrder::Equal,
    }
}

/// 递增指定设备的计数器，返回新时钟（不修改输入）。
///
/// Business Logic: 本地设备修改 Prompt 后需递增该设备的计数器，表示产生一次新的因果事件。
///     对照 Python `VectorClock.increment`。
///
/// Code Logic: 克隆输入时钟，将 device_id 计数器 +1（不存在则从 0 起加）。
#[allow(dead_code)]
pub fn increment(clock: &HashMap<String, u64>, device_id: &str) -> HashMap<String, u64> {
    let mut new_clock = clock.clone();
    let counter = new_clock.entry(device_id.to_string()).or_insert(0);
    *counter += 1;
    new_clock
}

/// 合并两时钟：取所有 key 的并集，每个 key 取最大值。返回新时钟。
///
/// Business Logic: 同步两个设备的 Prompt 时需合并各自向量时钟，生成包含双方所有因果历史
///     的新时钟。对照 Python `VectorClock.merge`。
///
/// Code Logic: 遍历 a∪b 的 key，逐 key 取 max(a.get(key,0), b.get(key,0))。
pub fn merge(a: &HashMap<String, u64>, b: &HashMap<String, u64>) -> HashMap<String, u64> {
    let mut merged = HashMap::new();
    for key in a.keys().chain(b.keys()) {
        let val_a = a.get(key).copied().unwrap_or(0);
        let val_b = b.get(key).copied().unwrap_or(0);
        // 同一 key 在迭代器链中可能出现两次（a 和 b 都有），用 max 合并保证最终取最大值
        let entry = merged.entry(key.clone()).or_insert(0u64);
        *entry = (*entry).max(val_a).max(val_b);
    }
    merged
}

#[cfg(test)]
mod tests {
    //! 向量时钟单测：覆盖四种偏序关系 + increment + merge，对照 Python vector_clock.py 行为。

    use super::*;

    /// 工具：从 [(k, v), ...] 构造向量时钟，简化测试书写。
    fn vc(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn compare_equal() {
        // 完全相同 → Equal
        let a = vc(&[("d1", 1), ("d2", 2)]);
        let b = vc(&[("d1", 1), ("d2", 2)]);
        assert_eq!(compare(&a, &b), ClockOrder::Equal);
        // key 顺序不同也应 Equal
        let c = vc(&[("d2", 2), ("d1", 1)]);
        assert_eq!(compare(&a, &c), ClockOrder::Equal);
    }

    #[test]
    fn compare_strictly_after() {
        // a 每个分量 >= b 且至少一个 > → a After（a 严格领先）
        let a = vc(&[("d1", 2), ("d2", 1)]);
        let b = vc(&[("d1", 1), ("d2", 1)]);
        assert_eq!(compare(&a, &b), ClockOrder::After);
        // a 有 b 没有的 key（b 缺省 0），且 a 其他分量 >= b → a After
        let a2 = vc(&[("d1", 1), ("d2", 1)]);
        let b2 = vc(&[("d1", 1)]);
        assert_eq!(compare(&a2, &b2), ClockOrder::After);
    }

    #[test]
    fn compare_strictly_before() {
        // a 落后于 b → Before
        let a = vc(&[("d1", 1), ("d2", 1)]);
        let b = vc(&[("d1", 2), ("d2", 1)]);
        assert_eq!(compare(&a, &b), ClockOrder::Before);
        // a 缺少 b 的 key（a 缺省 0）→ a Before
        let a2 = vc(&[("d1", 1)]);
        let b2 = vc(&[("d1", 1), ("d2", 1)]);
        assert_eq!(compare(&a2, &b2), ClockOrder::Before);
    }

    #[test]
    fn compare_concurrent() {
        // 互有领先 → Concurrent（存在冲突）
        let a = vc(&[("d1", 2), ("d2", 1)]);
        let b = vc(&[("d1", 1), ("d2", 2)]);
        assert_eq!(compare(&a, &b), ClockOrder::Concurrent);
    }

    #[test]
    fn compare_empty() {
        // 两个空时钟 → Equal
        let empty: HashMap<String, u64> = HashMap::new();
        assert_eq!(compare(&empty, &empty), ClockOrder::Equal);
        // 空 vs 非空（缺省 0 对比）→ 非空 After
        let a = vc(&[("d1", 1)]);
        assert_eq!(compare(&a, &empty), ClockOrder::After);
        assert_eq!(compare(&empty, &a), ClockOrder::Before);
    }

    #[test]
    fn increment_works() {
        // 已有 key：+1
        let clock = vc(&[("d1", 1), ("d2", 5)]);
        let inc = increment(&clock, "d1");
        assert_eq!(inc.get("d1"), Some(&2));
        assert_eq!(inc.get("d2"), Some(&5)); // 其他 key 不变
        // 不修改原时钟（纯函数）
        assert_eq!(clock.get("d1"), Some(&1));
        // 新 key：从 0 起加为 1
        let inc2 = increment(&clock, "d3");
        assert_eq!(inc2.get("d3"), Some(&1));
    }

    #[test]
    fn merge_takes_max_per_key() {
        // 逐 key 取 max，并集所有 key
        let a = vc(&[("d1", 1), ("d2", 3)]);
        let b = vc(&[("d2", 2), ("d3", 5)]);
        let m = merge(&a, &b);
        assert_eq!(m.get("d1"), Some(&1));
        assert_eq!(m.get("d2"), Some(&3)); // max(3,2)=3
        assert_eq!(m.get("d3"), Some(&5));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn merge_idempotent_and_empty() {
        // 与空时钟 merge = 自身
        let a = vc(&[("d1", 1)]);
        let empty: HashMap<String, u64> = HashMap::new();
        assert_eq!(merge(&a, &empty), a);
        // 两个空 merge = 空
        assert_eq!(merge(&empty, &empty), empty);
    }
}
