//! backend/runtime_metrics.rs — 进程内有界本地运行时性能指标。
//!
//! Business Logic（为什么需要这个模块）:
//!     Orchestrator claim、SQLite 事务与 CC History 同步需要稳定的本机耗时/计数证据，
//!     用于判断是否扩池或排查调度延迟；禁止上传遥测，也禁止记录正文/路径/凭据。
//!
//! Code Logic（这个模块做什么）:
//!     以 `Mutex<BTreeMap<&'static str, MetricAccumulator>>` 保存最多 64 个固定名指标，
//!     支持 duration/count 记录与 EWMA(α=0.2) snapshot；非法名与超额名静默丢弃；
//!     warning helper 仅输出固定字段 metric/count/last_ms/max_ms。

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

/// 进程内最多保留的 distinct metric 名数量。
const MAX_METRICS: usize = 64;

/// EWMA 平滑系数 α。
const EWMA_ALPHA: f64 = 0.2;

/// 单指标快照：调用次数与最近/最大/指数滑动均值。
///
/// Business Logic（为什么需要这个结构）:
///     诊断与后续压测需要读取某固定名指标的 count/last/max/ewma，且不暴露用户上下文。
///
/// Code Logic（这个结构做什么）:
///     以毫秒为统一数值单位保存 `count/last_ms/max_ms/ewma_ms`（计数类指标复用同字段）。
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeMetricSnapshot {
    pub count: u64,
    pub last_ms: u64,
    pub max_ms: u64,
    pub ewma_ms: f64,
}

/// 全部指标快照（进程重启清空）。
///
/// Business Logic（为什么需要这个结构）:
///     需要一次性导出当前进程内全部有界指标，供本地诊断或后续任务消费。
///
/// Code Logic（这个结构做什么）:
///     持有按名排序的 `BTreeMap<String, RuntimeMetricSnapshot>`。
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeMetricsSnapshot {
    pub metrics: BTreeMap<String, RuntimeMetricSnapshot>,
}

/// 单指标累加器（进程内可变状态）。
///
/// Business Logic（为什么需要这个结构）:
///     每个固定名指标需要在多次 record 后维护 count/last/max/ewma。
///
/// Code Logic（这个结构做什么）:
///     保存原始累加字段；首次样本 ewma 直接取 sample，后续按 α=0.2 更新。
#[derive(Debug, Clone)]
struct MetricAccumulator {
    count: u64,
    last_ms: u64,
    max_ms: u64,
    ewma_ms: f64,
}

impl MetricAccumulator {
    /// 用单个样本初始化累加器。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     首次记录某指标时必须建立基线，使后续 EWMA 有起点。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 count=1、last/max/ewma 全部设为 sample_ms。
    fn new(sample_ms: u64) -> Self {
        Self {
            count: 1,
            last_ms: sample_ms,
            max_ms: sample_ms,
            ewma_ms: sample_ms as f64,
        }
    }

    /// 追加一个样本并更新 EWMA。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     后续 duration/count 需要累加到同一固定名指标。
    ///
    /// Code Logic（这个函数做什么）:
    ///     count+1；更新 last/max；`ewma = α*sample + (1-α)*ewma`。
    fn record(&mut self, sample_ms: u64) {
        self.count = self.count.saturating_add(1);
        self.last_ms = sample_ms;
        if sample_ms > self.max_ms {
            self.max_ms = sample_ms;
        }
        self.ewma_ms = EWMA_ALPHA * (sample_ms as f64) + (1.0 - EWMA_ALPHA) * self.ewma_ms;
    }

    /// 转为对外快照。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     读取路径不能暴露内部可变累加器。
    ///
    /// Code Logic（这个函数做什么）:
    ///     复制 count/last/max/ewma 到 `RuntimeMetricSnapshot`。
    fn to_snapshot(&self) -> RuntimeMetricSnapshot {
        RuntimeMetricSnapshot {
            count: self.count,
            last_ms: self.last_ms,
            max_ms: self.max_ms,
            ewma_ms: self.ewma_ms,
        }
    }
}

/// 有界本地运行时指标注册表。
///
/// Business Logic（为什么需要这个结构）:
///     后端需要共享一份进程内指标，供 claim/sync 等路径记录耗时与规模，且永不上传。
///
/// Code Logic（这个结构做什么）:
///     用互斥 `BTreeMap` 保存最多 64 个 `&'static str` 名；非法名与超额名静默忽略。
#[derive(Debug)]
pub struct RuntimeMetrics {
    inner: Mutex<BTreeMap<&'static str, MetricAccumulator>>,
}

impl Default for RuntimeMetrics {
    /// 创建空指标表。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 与测试夹具需要零成本默认构造共享 metrics。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `RuntimeMetrics::new`。
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeMetrics {
    /// 创建空的有界指标表。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI/headless 启动与测试需要注入一份新的进程内 metrics 实例。
    ///
    /// Code Logic（这个函数做什么）:
    ///     初始化空 `Mutex<BTreeMap>`。
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(BTreeMap::new()),
        }
    }

    /// 记录一次耗时样本（毫秒取整）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     DB acquire/事务/scheduler tick/同步轮次需要注入确定性 duration，而不是 sleep。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验固定名；`as_millis` 转 u64 后写入累加器；非法名或表已满且新名则丢弃。
    pub fn record_duration(&self, name: &'static str, duration: Duration) {
        if !is_accepted_metric_name(name) {
            return;
        }
        let sample_ms = duration.as_millis() as u64;
        self.record_sample(name, sample_ms);
    }

    /// 记录一次计数值样本。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     claim 候选数、同步批次规模等需要按固定名累计 last/max/ewma，而非仅耗时。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验固定名后把 value 写入同一套 last/max/ewma 字段。
    pub fn record_count(&self, name: &'static str, value: u64) {
        if !is_accepted_metric_name(name) {
            return;
        }
        self.record_sample(name, value);
    }

    /// 导出当前全部指标快照。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本地诊断与后续任务需要只读查看 count/last/max/ewma，不改写状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     加锁遍历 map，把 key 克隆为 String 并复制累加器为 snapshot。
    pub fn snapshot(&self) -> RuntimeMetricsSnapshot {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut metrics = BTreeMap::new();
        for (name, acc) in guard.iter() {
            metrics.insert((*name).to_string(), acc.to_snapshot());
        }
        RuntimeMetricsSnapshot { metrics }
    }

    /// 对已存在指标输出固定字段 warning。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     阈值越界时需要结构化 warning，且字段必须固定，避免泄露路径/正文/SQL。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取 name 对应 snapshot；存在时仅输出 metric/count/last_ms/max_ms 四个字段。
    pub fn warn_metric(&self, name: &'static str) {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(acc) = guard.get(name) {
            let snap = acc.to_snapshot();
            warn_metric_fields(name, snap.count, snap.last_ms, snap.max_ms);
        }
    }

    /// 向累加器写入样本（调用方已校验名字）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     duration 与 count 共享同一有界表与 EWMA 更新规则。
    ///
    /// Code Logic（这个函数做什么）:
    ///     已有名则更新；新名且未达 64 则插入；已满则丢弃。
    fn record_sample(&self, name: &'static str, sample_ms: u64) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(acc) = guard.get_mut(name) {
            acc.record(sample_ms);
            return;
        }
        if guard.len() >= MAX_METRICS {
            return;
        }
        guard.insert(name, MetricAccumulator::new(sample_ms));
    }
}

/// 判断固定名是否允许写入指标表。
///
/// Business Logic（为什么需要这个函数）:
///     即使 API 只收 `&'static str`，仍需拒绝含 `/`、空白或自由文本字符的名字，防止路径/用户文案混入。
///
/// Code Logic（这个函数做什么）:
///     非空且仅含 ASCII 字母数字、`.`、`_`、`-` 时返回 true。
fn is_accepted_metric_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// 输出脱敏指标 warning（固定字段集）。
///
/// Business Logic（为什么需要这个函数）:
///     阈值越界日志只能携带 metric/count/last_ms/max_ms，不得附加任意 context。
///
/// Code Logic（这个函数做什么）:
///     调用 `tracing::warn!` 且仅绑定上述四字段与固定消息。
fn warn_metric_fields(metric: &'static str, count: u64, last_ms: u64, max_ms: u64) {
    tracing::warn!(
        metric = metric,
        count = count,
        last_ms = last_ms,
        max_ms = max_ms,
        "runtime metric threshold exceeded"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Business Logic（为什么需要这个测试）:
    ///     duration 10/30/20ms 必须得到稳定 count/last/max 与有限范围内的 EWMA。
    ///
    /// Code Logic（这个测试做什么）:
    ///     连续 record 三笔 duration，断言 count=3、last=20、max=30，且 ewma 有限并落在 [10,30]。
    #[test]
    fn record_duration_tracks_count_last_max_and_finite_ewma() {
        let metrics = RuntimeMetrics::new();
        metrics.record_duration("db.acquire_wait_ms", Duration::from_millis(10));
        metrics.record_duration("db.acquire_wait_ms", Duration::from_millis(30));
        metrics.record_duration("db.acquire_wait_ms", Duration::from_millis(20));

        let snap = metrics.snapshot();
        let item = snap
            .metrics
            .get("db.acquire_wait_ms")
            .expect("metric should exist");
        assert_eq!(item.count, 3);
        assert_eq!(item.last_ms, 20);
        assert_eq!(item.max_ms, 30);
        assert!(item.ewma_ms.is_finite());
        assert!(item.ewma_ms >= 10.0 && item.ewma_ms <= 30.0);

        // α=0.2: ewma1=10; ewma2=0.2*30+0.8*10=14; ewma3=0.2*20+0.8*14=15.2
        assert!((item.ewma_ms - 15.2).abs() < 1e-9);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     指标表必须有硬上限，防止任意增长占用内存。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入超过 64 个不同合法名，断言 snapshot 最多 64 条。
    #[test]
    fn snapshot_retains_at_most_64_distinct_metrics() {
        let metrics = RuntimeMetrics::new();
        // 65 个编译期静态名：前 64 写入，第 65 被丢弃。
        const NAMES: [&str; 65] = [
            "m00", "m01", "m02", "m03", "m04", "m05", "m06", "m07", "m08", "m09", "m10", "m11",
            "m12", "m13", "m14", "m15", "m16", "m17", "m18", "m19", "m20", "m21", "m22", "m23",
            "m24", "m25", "m26", "m27", "m28", "m29", "m30", "m31", "m32", "m33", "m34", "m35",
            "m36", "m37", "m38", "m39", "m40", "m41", "m42", "m43", "m44", "m45", "m46", "m47",
            "m48", "m49", "m50", "m51", "m52", "m53", "m54", "m55", "m56", "m57", "m58", "m59",
            "m60", "m61", "m62", "m63", "m64",
        ];
        for name in NAMES {
            metrics.record_count(name, 1);
        }
        let snap = metrics.snapshot();
        assert!(snap.metrics.len() <= 64);
        assert_eq!(snap.metrics.len(), 64);
        assert!(!snap.metrics.contains_key("m64"));
        assert!(snap.metrics.contains_key("m00"));
        assert!(snap.metrics.contains_key("m63"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     含 `/`、空白或自由文本字符的名字不得进入指标表，避免路径/用户文案泄露。
    ///
    /// Code Logic（这个测试做什么）:
    ///     分别 record 含 slash、space 与中文/特殊字符的静态名，断言 snapshot 为空；合法名可写入。
    #[test]
    fn rejects_names_with_slash_space_or_user_text() {
        let metrics = RuntimeMetrics::new();
        metrics.record_duration("path/to/metric", Duration::from_millis(5));
        metrics.record_count("has space", 3);
        metrics.record_count("用户文本", 7);
        metrics.record_count("user:text!", 9);
        metrics.record_count("ok.metric_name-1", 2);

        let snap = metrics.snapshot();
        assert!(!snap.metrics.contains_key("path/to/metric"));
        assert!(!snap.metrics.contains_key("has space"));
        assert!(!snap.metrics.contains_key("用户文本"));
        assert!(!snap.metrics.contains_key("user:text!"));
        assert_eq!(snap.metrics.len(), 1);
        let ok = snap.metrics.get("ok.metric_name-1").expect("valid name");
        assert_eq!(ok.count, 1);
        assert_eq!(ok.last_ms, 2);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     record_count 应与 duration 共用累加语义，供 claim/sync 规模指标使用。
    ///
    /// Code Logic（这个测试做什么）:
    ///     连续写 1/5/3，断言 count/last/max 正确。
    #[test]
    fn record_count_updates_last_and_max() {
        let metrics = RuntimeMetrics::new();
        metrics.record_count("orchestrator.claim_scan_count", 1);
        metrics.record_count("orchestrator.claim_scan_count", 5);
        metrics.record_count("orchestrator.claim_scan_count", 3);
        let snap = metrics.snapshot();
        let item = snap
            .metrics
            .get("orchestrator.claim_scan_count")
            .expect("count metric");
        assert_eq!(item.count, 3);
        assert_eq!(item.last_ms, 3);
        assert_eq!(item.max_ms, 5);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     warning helper 必须能在已有指标上被调用且不 panic（字段集固定）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     record 后调用 warn_metric；对不存在名调用也应安全 no-op。
    #[test]
    fn warn_metric_is_safe_for_existing_and_missing() {
        let metrics = RuntimeMetrics::new();
        metrics.record_duration("db.transaction_ms", Duration::from_millis(120));
        metrics.warn_metric("db.transaction_ms");
        metrics.warn_metric("missing.metric");
    }
}
