//! sync/protocol.rs — 有界、可验证的同步计划协议（纯类型 + 纯算法）
//!
//! Business Logic（为什么需要这个模块）:
//!     Prompt/SSH/Scratchpad 旧 peer client 会把网络/JSON 错误折叠为空列表，
//!     同步引擎再把“空”误判为远端不存在并重复全量推送。N2 要求用 typed result
//!     与完整排序 manifest 比较，精确相等时零正文交换，并在 page/batch 预算内失败可辨。
//!
//! Code Logic（这个模块做什么）:
//!     定义 manifest 摘要、分页、同步计划、领域结果与 page/batch 常量；
//!     在**完整排序**的本地/远端 manifest 上计算 `compute_sync_plan`；
//!     提供 opaque cursor 流式校验与 page/batch 边界校验（无 DB、无网络）。

use std::collections::{HashMap, HashSet};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::vector_clock::{compare, ClockOrder};

/// opaque keyset cursor v1 载荷（编码前 JSON）。
///
/// Business Logic: 客户端不得解析/发明 cursor；服务端用 last_id 做稳定 keyset 翻页。
/// Code Logic: `{v:1,last_id}` 经 base64url(NO_PAD) 编码为 opaque 字符串。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeysetCursorV1 {
    v: u32,
    last_id: String,
}

/// 编码 opaque keyset cursor。
///
/// Business Logic（为什么需要这个函数）:
///     Prompt/SSH/Scratchpad v2 manifest-page 与 CC History 同构，需统一 opaque cursor，
///     避免各领域自造可解析游标被客户端误用。
///
/// Code Logic（这个函数做什么）:
///     序列化 `{v:1,last_id}` 为 JSON 字节，再 URL_SAFE_NO_PAD base64 编码。
pub fn encode_keyset_cursor(last_id: &str) -> String {
    let payload = KeysetCursorV1 {
        v: 1,
        last_id: last_id.to_string(),
    };
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    URL_SAFE_NO_PAD.encode(json)
}

/// 解码 opaque keyset cursor，成功返回 last_id。
///
/// Business Logic（为什么需要这个函数）:
///     非法/损坏/错误版本 cursor 必须在访问 DB 前拒绝，映射为 stable invalid_cursor。
///
/// Code Logic（这个函数做什么）:
///     base64url 解码 → JSON 解析 → 要求 v==1 且 last_id 非空。
pub fn decode_keyset_cursor(cursor: &str) -> Result<String, ()> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor.as_bytes()).map_err(|_| ())?;
    let payload: KeysetCursorV1 = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if payload.v != 1 || payload.last_id.is_empty() {
        return Err(());
    }
    Ok(payload.last_id)
}

/// 计算多段载荷的 SHA-256 十六进制 content_hash。
///
/// Business Logic（为什么需要这个函数）:
///     manifest 摘要需要稳定 content hash 做 exact equality，避免无谓正文交换。
///
/// Code Logic（这个函数做什么）:
///     按顺序 update 各字节段（调用方自行加分隔符），返回小写 hex。
pub fn content_sha256_hex(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    format!("{:x}", hasher.finalize())
}

/// Manifest 单页最大 item 数（先达到 item/字节预算者为准）。
pub const MANIFEST_PAGE_ITEMS: usize = 500;

/// Manifest 单页最大序列化估算字节（1 MiB）。
pub const MANIFEST_PAGE_BYTES: usize = 1_048_576;

/// 正文 push-batch 最大 item 数。
pub const PUSH_BATCH_ITEMS: usize = 100;

/// 正文 push-batch 最大估算字节（4 MiB）。
pub const PUSH_BATCH_BYTES: usize = 4 * 1_048_576;

/// 单条同步摘要：仅元数据，不含正文。
///
/// Business Logic（为什么需要这个类型）:
///     客户端先用摘要做向量时钟比较，只有真正需要正文时才 items/push，
///     精确相等时零 payload 交换，降低带宽与误推风险。
///
/// Code Logic（这个类型做什么）:
///     按 id 稳定排序的轻量摘要：向量时钟 + content hash + size + 删除/更新元数据。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SyncSummary<K> {
    /// 领域主键（Prompt id / Scratchpad page id / SSH host 等）
    pub id: K,
    /// 向量时钟 {device_id: counter}
    pub vector_clock: HashMap<String, u64>,
    /// 正文内容哈希（用于 exact equality 与 ledger 载荷指纹）
    pub content_hash: String,
    /// 正文近似字节大小（用于 batch 预算）
    pub size: u64,
    /// 最后更新时间（RFC3339 或既有库字符串；参与审计，不参与 clock 比较）
    pub updated_at: String,
    /// 是否软删除 tombstone
    pub deleted: bool,
    /// 删除/ floor 的 domain delete epoch（非删除项为 0；旧 wire 缺字段时 default 0）
    #[serde(default)]
    pub delete_epoch: u64,
}

/// 无状态 manifest 分页响应。
///
/// Business Logic（为什么需要这个类型）:
///     server 只提供 cursor 分页，不根据 caller 单页推断缺失项；
///     client 必须流式拉完 `next_cursor=None` 才可与本地完整 manifest 比较。
///
/// Code Logic（这个类型做什么）:
///     `items` 为本页摘要；`next_cursor` 为 opaque keyset 游标，`None` 表示流结束。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SyncManifestPage<K> {
    /// 本页摘要，按 id 升序
    pub items: Vec<SyncSummary<K>>,
    /// 下一页 opaque cursor；`None` 表示已完整结束
    pub next_cursor: Option<String>,
}

/// 完整 manifest 比较后的双向同步计划。
///
/// Business Logic（为什么需要这个类型）:
///     计划明确区分 push / fetch / unchanged，避免把网络空集当“远端全无”。
///
/// Code Logic（这个类型做什么）:
///     `push_to_remote`/`fetch_from_remote` 存主键列表；`unchanged` 为精确相等条数。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncPlan<K> {
    /// 本地独有或本地领先/并发，需推送到远端的 id
    pub push_to_remote: Vec<K>,
    /// 远端独有或远端领先/并发，需从远端拉取的 id
    pub fetch_from_remote: Vec<K>,
    /// 双方摘要精确相等（同 clock + hash + deleted）的条数
    pub unchanged: u32,
}

/// 单条同步失败明细（partial 场景）。
///
/// Business Logic（为什么需要这个类型）:
///     Partial 不得折叠为 Ok；UI/日志需展示失败项且不计成功设备。
///
/// Code Logic（这个类型做什么）:
///     稳定错误码 + 脱敏消息，不含正文/token。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SyncItemFailure {
    /// 失败项主键（字符串化后便于跨领域聚合）
    pub id: String,
    /// 稳定错误码（如 `item_too_large` / `merge_failed`）
    pub code: String,
    /// 可展示的脱敏说明（禁止正文/凭据）
    pub message: String,
}

/// 传输层失败分类（不可达场景）。
///
/// Business Logic（为什么需要这个枚举）:
///     UI 需要区分超时/网络断开/HTTP 层失败，但不能把它们伪装成成功空集。
///
/// Code Logic（这个枚举做什么）:
///     粗粒度 transport 分类，供 `SyncDomainOutcome::Unreachable` 使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportClass {
    /// 连接/DNS/TLS 等网络层失败
    Network,
    /// 超时
    Timeout,
    /// HTTP 状态/信封层失败（非业务协议字段）
    Http,
}

/// 单领域同步终态结果。
///
/// Business Logic（为什么需要这个枚举）:
///     只有 Succeeded 计入 synced；Partial/Unreachable/ProtocolError/ResourceLimit
///     必须在 UI 与日志可见，不得转成 Ok(()) 或空成功。
///
/// Code Logic（这个枚举做什么）:
///     与设计 §4.1 一致的 typed domain outcome。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SyncDomainOutcome {
    /// 完整成功：pulled/pushed/unchanged 计数
    Succeeded {
        pulled: u32,
        pushed: u32,
        unchanged: u32,
    },
    /// 部分成功：已应用条数 + 失败明细（不得记为全成功）
    Partial {
        applied: u32,
        failed: Vec<SyncItemFailure>,
    },
    /// 对端不可达（传输层）
    Unreachable { class: TransportClass },
    /// 协议失败（非法 cursor、未完成流、非法 JSON 语义等）
    ProtocolError { code: String },
    /// 资源/预算超限（page/batch 字节或条数）
    ResourceLimit { limit: String },
}

/// Manifest 流式游标状态机。
///
/// Business Logic（为什么需要这个类型）:
///     opaque cursor 由服务端颁发；客户端不得解析/发明。重复 cursor 或在
///     `next_cursor=None` 之前中止都会导致假空集或死循环，必须 typed 失败。
///
/// Code Logic（这个类型做什么）:
///     记录已见 next_cursor 集合与是否完成；`observe_next_cursor` 检测环路；
///     `require_complete` 在进入 planner 前强制 `next_cursor=None` 结束。
#[derive(Debug, Clone, Default)]
pub struct ManifestStreamState {
    /// 已出现过的 next_cursor（opaque 字符串原样比较）
    seen_cursors: HashSet<String>,
    /// 是否已观察到 `next_cursor=None`
    finished: bool,
}

impl ManifestStreamState {
    /// 创建未开始的流状态。
    ///
    /// Business Logic: 每次拉远端 manifest 前新建，避免跨轮次污染。
    ///
    /// Code Logic: 全字段默认未开始/未完成。
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否已以 `next_cursor=None` 正常结束。
    ///
    /// Business Logic: planner 仅允许在完整流结束后运行。
    ///
    /// Code Logic: 返回 `finished` 标志。
    pub fn is_complete(&self) -> bool {
        self.finished
    }

    /// 观察一页响应的 `next_cursor` 并推进状态。
    ///
    /// Business Logic: 禁止 cursor 死循环；`None` 表示远端流完整结束。
    ///
    /// Code Logic:
    ///     - 若已 finished 再观察 → ProtocolError `stream_already_finished`；
    ///     - `Some(c)` 若已见过 → ProtocolError `cursor_loop`；
    ///     - `Some(c)` 合法则插入 seen；
    ///     - `None` 标记 finished。
    pub fn observe_next_cursor(
        &mut self,
        next_cursor: Option<String>,
    ) -> Result<(), SyncDomainOutcome> {
        if self.finished {
            return Err(SyncDomainOutcome::ProtocolError {
                code: "stream_already_finished".to_string(),
            });
        }

        match next_cursor {
            None => {
                self.finished = true;
                Ok(())
            }
            Some(cursor) => {
                if cursor.is_empty() {
                    return Err(SyncDomainOutcome::ProtocolError {
                        code: "empty_cursor".to_string(),
                    });
                }
                if !self.seen_cursors.insert(cursor) {
                    return Err(SyncDomainOutcome::ProtocolError {
                        code: "cursor_loop".to_string(),
                    });
                }
                Ok(())
            }
        }
    }

    /// 进入 planner 前要求流已完整结束。
    ///
    /// Business Logic: 截断流（stopped before nextCursor=None）不得当作远端全量。
    ///
    /// Code Logic: `finished==false` → ProtocolError `incomplete_manifest`。
    pub fn require_complete(&self) -> Result<(), SyncDomainOutcome> {
        if self.finished {
            Ok(())
        } else {
            Err(SyncDomainOutcome::ProtocolError {
                code: "incomplete_manifest".to_string(),
            })
        }
    }
}

/// 校验 manifest 单页 item/字节预算。
///
/// Business Logic（为什么需要这个函数）:
///     超限页不得进入 planner；必须返回 ResourceLimit，避免 OOM 与假成功。
///
/// Code Logic（这个函数做什么）:
///     item_count > MANIFEST_PAGE_ITEMS 或 estimated_bytes > MANIFEST_PAGE_BYTES → Err。
pub fn validate_manifest_page_bounds(
    item_count: usize,
    estimated_bytes: usize,
) -> Result<(), SyncDomainOutcome> {
    if item_count > MANIFEST_PAGE_ITEMS {
        return Err(SyncDomainOutcome::ResourceLimit {
            limit: "manifest_page_items".to_string(),
        });
    }
    if estimated_bytes > MANIFEST_PAGE_BYTES {
        return Err(SyncDomainOutcome::ResourceLimit {
            limit: "manifest_page_bytes".to_string(),
        });
    }
    Ok(())
}

/// 校验正文 push-batch item/字节预算。
///
/// Business Logic（为什么需要这个函数）:
///     batch 超限应 typed ResourceLimit，禁止静默截断导致半批次成功。
///
/// Code Logic（这个函数做什么）:
///     item_count > PUSH_BATCH_ITEMS 或 estimated_bytes > PUSH_BATCH_BYTES → Err。
pub fn validate_push_batch_bounds(
    item_count: usize,
    estimated_bytes: usize,
) -> Result<(), SyncDomainOutcome> {
    if item_count > PUSH_BATCH_ITEMS {
        return Err(SyncDomainOutcome::ResourceLimit {
            limit: "push_batch_items".to_string(),
        });
    }
    if estimated_bytes > PUSH_BATCH_BYTES {
        return Err(SyncDomainOutcome::ResourceLimit {
            limit: "push_batch_bytes".to_string(),
        });
    }
    Ok(())
}

/// 检测“空页却声称还有下一页”的截断协议错误。
///
/// Business Logic（为什么需要这个函数）:
///     items 为空且 next_cursor 仍存在时，客户端无法前进，必须失败而非死循环。
///
/// Code Logic（这个函数做什么）:
///     empty items + Some(cursor) → ProtocolError `truncated_page`。
pub fn validate_page_not_truncated<K>(page: &SyncManifestPage<K>) -> Result<(), SyncDomainOutcome> {
    if page.items.is_empty() && page.next_cursor.is_some() {
        return Err(SyncDomainOutcome::ProtocolError {
            code: "truncated_page".to_string(),
        });
    }
    Ok(())
}

/// 仅在 manifest 完整且 apply 成功后才允许推进 peer 的 delete watermark。
///
/// Business Logic（为什么需要这个函数）:
///     未拉完远端 manifest 或中途 apply 失败时，客户端不得向对端 ack delete_epoch，
///     否则对端可能把尚未传播到本机的 tombstone 当成“全网已收齐”而 GC 掉。
///
/// Code Logic（这个函数做什么）:
///     `manifest_complete && apply_ok` → `Some(max_epoch)`（含 0，表示该域当前无删除/水位底）；
///     任一条件失败 → `None`（本轮 push 不得携带 acked_delete_epoch）。
pub fn decide_acked_delete_epoch(
    manifest_complete: bool,
    apply_ok: bool,
    max_epoch: u64,
) -> Option<u64> {
    if manifest_complete && apply_ok {
        Some(max_epoch)
    } else {
        None
    }
}

/// 从完整远端 manifest 摘要中取最大 delete_epoch。
///
/// Business Logic（为什么需要这个函数）:
///     客户端对远端水位 ack 的目标是“本轮所见远端删除序号上界”，用于对端安全 GC。
///
/// Code Logic（这个函数做什么）:
///     遍历 `delete_epoch` 取 max；空 manifest 返回 0。
pub fn max_delete_epoch_from_summaries<K>(items: &[SyncSummary<K>]) -> u64 {
    items.iter().map(|s| s.delete_epoch).max().unwrap_or(0)
}

/// 估算单条摘要的序列化近似字节（用于 page 预算，非精确 JSON 长度）。
///
/// Business Logic（为什么需要这个函数）:
///     客户端/服务端需在不完整序列化前判断是否超 1 MiB 页预算。
///
/// Code Logic（这个函数做什么）:
///     id 字节 + hash + updated_at + 每 clock 项 (key+8) + 固定 overhead。
pub fn estimate_summary_wire_bytes<K: AsRef<str>>(summary: &SyncSummary<K>) -> usize {
    let clock_bytes: usize = summary
        .vector_clock
        .iter()
        .map(|(k, _)| k.len() + 8)
        .sum();
    summary.id.as_ref().len()
        + summary.content_hash.len()
        + summary.updated_at.len()
        + clock_bytes
        + 64 // 字段名与 JSON 结构 overhead
}

/// 在两个**完整且按 id 升序**的 manifest 上计算双向同步计划。
///
/// Business Logic（为什么需要这个函数）:
///     只有双方完整摘要到齐后才能判断 local-only / remote-only / 领先 / 并发；
///     exact equality（同 clock + hash + deleted）不交换正文。
///
/// Code Logic（这个函数做什么）:
///     双指针合并有序 id：
///     - 仅本地 → push；
///     - 仅远端 → fetch；
///     - 双方都有：Equal 且 hash/deleted 相同 → unchanged；
///       After → push；Before → fetch；Concurrent 或 Equal 但载荷不一致 → 双向。
///     调用方须已保证流完整结束且页校验通过；本函数不做网络/DB。
pub fn compute_sync_plan<K>(
    local: &[SyncSummary<K>],
    remote: &[SyncSummary<K>],
) -> SyncPlan<K>
where
    K: Clone + Ord,
{
    let mut plan = SyncPlan {
        push_to_remote: Vec::new(),
        fetch_from_remote: Vec::new(),
        unchanged: 0,
    };

    let mut i = 0usize;
    let mut j = 0usize;
    while i < local.len() && j < remote.len() {
        match local[i].id.cmp(&remote[j].id) {
            std::cmp::Ordering::Less => {
                plan.push_to_remote.push(local[i].id.clone());
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                plan.fetch_from_remote.push(remote[j].id.clone());
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                classify_both_sides(&local[i], &remote[j], &mut plan);
                i += 1;
                j += 1;
            }
        }
    }
    while i < local.len() {
        plan.push_to_remote.push(local[i].id.clone());
        i += 1;
    }
    while j < remote.len() {
        plan.fetch_from_remote.push(remote[j].id.clone());
        j += 1;
    }
    plan
}

/// 对同 id 的本地/远端摘要分类写入计划。
///
/// Business Logic: 向量时钟可排序时单向交换；并发或 Equal 但正文指纹不一致需双向。
///
/// Code Logic: 调 `vector_clock::compare`，再按 hash/deleted 判定 unchanged。
fn classify_both_sides<K: Clone>(
    local: &SyncSummary<K>,
    remote: &SyncSummary<K>,
    plan: &mut SyncPlan<K>,
) {
    match compare(&local.vector_clock, &remote.vector_clock) {
        ClockOrder::Equal => {
            if local.content_hash == remote.content_hash && local.deleted == remote.deleted {
                plan.unchanged = plan.unchanged.saturating_add(1);
            } else {
                // 因果历史相同但载荷不一致：双方交换以便修复/冲突副本
                plan.push_to_remote.push(local.id.clone());
                plan.fetch_from_remote.push(remote.id.clone());
            }
        }
        ClockOrder::After => {
            plan.push_to_remote.push(local.id.clone());
        }
        ClockOrder::Before => {
            plan.fetch_from_remote.push(remote.id.clone());
        }
        ClockOrder::Concurrent => {
            plan.push_to_remote.push(local.id.clone());
            plan.fetch_from_remote.push(remote.id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    //! 协议纯函数表驱动测试：向量时钟计划 + cursor/预算校验。

    use super::*;

    /// 构造单设备时钟 `{d1: n}`。
    fn clock(n: u64) -> HashMap<String, u64> {
        let mut m = HashMap::new();
        m.insert("d1".to_string(), n);
        m
    }

    /// 构造双设备时钟。
    fn clock2(a: u64, b: u64) -> HashMap<String, u64> {
        let mut m = HashMap::new();
        m.insert("d1".to_string(), a);
        m.insert("d2".to_string(), b);
        m
    }

    /// 构造测试摘要。
    fn summary(id: &str, vc: HashMap<String, u64>, hash: &str) -> SyncSummary<String> {
        SyncSummary {
            id: id.to_string(),
            vector_clock: vc,
            content_hash: hash.to_string(),
            size: 10,
            updated_at: "2026-07-14T00:00:00Z".to_string(),
            deleted: false,
            delete_epoch: 0,
        }
    }

    #[test]
    fn equal_items_require_no_payload_exchange() {
        let plan = compute_sync_plan(
            &[summary("a", clock(1), "h")],
            &[summary("a", clock(1), "h")],
        );
        assert!(plan.push_to_remote.is_empty());
        assert!(plan.fetch_from_remote.is_empty());
        assert_eq!(plan.unchanged, 1);
    }

    #[test]
    fn local_only_is_pushed() {
        let plan = compute_sync_plan(&[summary("a", clock(1), "h")], &[]);
        assert_eq!(plan.push_to_remote, vec!["a".to_string()]);
        assert!(plan.fetch_from_remote.is_empty());
        assert_eq!(plan.unchanged, 0);
    }

    #[test]
    fn remote_only_is_fetched() {
        let plan = compute_sync_plan(&[], &[summary("b", clock(1), "h")]);
        assert!(plan.push_to_remote.is_empty());
        assert_eq!(plan.fetch_from_remote, vec!["b".to_string()]);
        assert_eq!(plan.unchanged, 0);
    }

    #[test]
    fn local_newer_is_pushed() {
        let plan = compute_sync_plan(
            &[summary("a", clock(2), "h2")],
            &[summary("a", clock(1), "h1")],
        );
        assert_eq!(plan.push_to_remote, vec!["a".to_string()]);
        assert!(plan.fetch_from_remote.is_empty());
        assert_eq!(plan.unchanged, 0);
    }

    #[test]
    fn remote_newer_is_fetched() {
        let plan = compute_sync_plan(
            &[summary("a", clock(1), "h1")],
            &[summary("a", clock(3), "h3")],
        );
        assert!(plan.push_to_remote.is_empty());
        assert_eq!(plan.fetch_from_remote, vec!["a".to_string()]);
        assert_eq!(plan.unchanged, 0);
    }

    #[test]
    fn concurrent_exchanges_both_directions() {
        let local = summary("a", clock2(2, 1), "hl");
        let remote = summary("a", clock2(1, 2), "hr");
        let plan = compute_sync_plan(&[local], &[remote]);
        assert_eq!(plan.push_to_remote, vec!["a".to_string()]);
        assert_eq!(plan.fetch_from_remote, vec!["a".to_string()]);
        assert_eq!(plan.unchanged, 0);
    }

    #[test]
    fn mixed_sorted_manifests_merge_in_id_order() {
        // local: a(eq), c(local-only), e(local-newer)
        // remote: a(eq), b(remote-only), e(remote older), f(remote-only)
        let local = vec![
            summary("a", clock(1), "ha"),
            summary("c", clock(1), "hc"),
            summary("e", clock(2), "he2"),
        ];
        let remote = vec![
            summary("a", clock(1), "ha"),
            summary("b", clock(1), "hb"),
            summary("e", clock(1), "he1"),
            summary("f", clock(1), "hf"),
        ];
        let plan = compute_sync_plan(&local, &remote);
        assert_eq!(plan.unchanged, 1);
        assert_eq!(plan.push_to_remote, vec!["c".to_string(), "e".to_string()]);
        assert_eq!(plan.fetch_from_remote, vec!["b".to_string(), "f".to_string()]);
    }

    #[test]
    fn equal_clock_but_different_hash_exchanges_both() {
        let plan = compute_sync_plan(
            &[summary("a", clock(1), "h-local")],
            &[summary("a", clock(1), "h-remote")],
        );
        assert_eq!(plan.push_to_remote, vec!["a".to_string()]);
        assert_eq!(plan.fetch_from_remote, vec!["a".to_string()]);
        assert_eq!(plan.unchanged, 0);
    }

    #[test]
    fn opaque_cursor_loop_is_protocol_error() {
        let mut state = ManifestStreamState::new();
        state
            .observe_next_cursor(Some("cursor-1".to_string()))
            .expect("first page ok");
        // 服务端再次返回同一 cursor → 环路
        let err = state
            .observe_next_cursor(Some("cursor-1".to_string()))
            .expect_err("must detect loop");
        assert_eq!(
            err,
            SyncDomainOutcome::ProtocolError {
                code: "cursor_loop".to_string()
            }
        );
    }

    #[test]
    fn stopped_before_next_cursor_none_is_protocol_error() {
        let mut state = ManifestStreamState::new();
        state
            .observe_next_cursor(Some("cursor-1".to_string()))
            .expect("page ok");
        // 中途中止，未收到 next_cursor=None
        let err = state.require_complete().expect_err("must be incomplete");
        assert_eq!(
            err,
            SyncDomainOutcome::ProtocolError {
                code: "incomplete_manifest".to_string()
            }
        );
        assert!(!state.is_complete());
    }

    #[test]
    fn complete_stream_allows_planner_gate() {
        let mut state = ManifestStreamState::new();
        state
            .observe_next_cursor(Some("c1".to_string()))
            .expect("page1");
        state
            .observe_next_cursor(Some("c2".to_string()))
            .expect("page2");
        state.observe_next_cursor(None).expect("done");
        state.require_complete().expect("complete");
        assert!(state.is_complete());
    }

    #[test]
    fn truncated_empty_page_with_cursor_is_protocol_error() {
        let page = SyncManifestPage::<String> {
            items: vec![],
            next_cursor: Some("still-going".to_string()),
        };
        let err = validate_page_not_truncated(&page).expect_err("truncated");
        assert_eq!(
            err,
            SyncDomainOutcome::ProtocolError {
                code: "truncated_page".to_string()
            }
        );
    }

    #[test]
    fn page_item_limit_is_resource_limit() {
        let err = validate_manifest_page_bounds(MANIFEST_PAGE_ITEMS + 1, 1)
            .expect_err("items over");
        assert_eq!(
            err,
            SyncDomainOutcome::ResourceLimit {
                limit: "manifest_page_items".to_string()
            }
        );
    }

    #[test]
    fn page_byte_limit_is_resource_limit() {
        let err = validate_manifest_page_bounds(1, MANIFEST_PAGE_BYTES + 1)
            .expect_err("bytes over");
        assert_eq!(
            err,
            SyncDomainOutcome::ResourceLimit {
                limit: "manifest_page_bytes".to_string()
            }
        );
    }

    #[test]
    fn push_batch_limits_are_resource_limit() {
        let items_err =
            validate_push_batch_bounds(PUSH_BATCH_ITEMS + 1, 1).expect_err("items");
        assert_eq!(
            items_err,
            SyncDomainOutcome::ResourceLimit {
                limit: "push_batch_items".to_string()
            }
        );
        let bytes_err =
            validate_push_batch_bounds(1, PUSH_BATCH_BYTES + 1).expect_err("bytes");
        assert_eq!(
            bytes_err,
            SyncDomainOutcome::ResourceLimit {
                limit: "push_batch_bytes".to_string()
            }
        );
    }

    #[test]
    fn empty_manifests_yield_empty_plan() {
        let plan = compute_sync_plan::<String>(&[], &[]);
        assert!(plan.push_to_remote.is_empty());
        assert!(plan.fetch_from_remote.is_empty());
        assert_eq!(plan.unchanged, 0);
    }

    #[test]
    fn constants_match_design_budgets() {
        assert_eq!(MANIFEST_PAGE_ITEMS, 500);
        assert_eq!(MANIFEST_PAGE_BYTES, 1_048_576);
        assert_eq!(PUSH_BATCH_ITEMS, 100);
        assert_eq!(PUSH_BATCH_BYTES, 4 * 1_048_576);
    }

    /// Business Logic: incomplete manifest / failed apply 都不得推进 delete ack。
    /// Code Logic: 表驱动断言 decide_acked_delete_epoch 的四组合；max_epoch=0 仍可 Some(0)。
    #[test]
    fn incomplete_manifest_must_not_advance_delete_ack() {
        assert_eq!(decide_acked_delete_epoch(false, true, 9), None);
        assert_eq!(decide_acked_delete_epoch(true, false, 9), None);
        assert_eq!(decide_acked_delete_epoch(false, false, 9), None);
        assert_eq!(decide_acked_delete_epoch(true, true, 9), Some(9));
        // 0 是合法水位底（空域/尚无 tombstone），不是“不 ack”
        assert_eq!(decide_acked_delete_epoch(true, true, 0), Some(0));
    }

    #[test]
    fn max_delete_epoch_from_summaries_tracks_upper_bound() {
        let items: Vec<SyncSummary<String>> = vec![
            SyncSummary {
                id: "a".to_string(),
                vector_clock: clock(1),
                content_hash: "h".to_string(),
                size: 1,
                updated_at: "t".to_string(),
                deleted: true,
                delete_epoch: 3,
            },
            SyncSummary {
                id: "b".to_string(),
                vector_clock: clock(1),
                content_hash: "h".to_string(),
                size: 1,
                updated_at: "t".to_string(),
                deleted: false,
            delete_epoch: 0,
        },
            SyncSummary {
                id: "c".to_string(),
                vector_clock: clock(1),
                content_hash: "h".to_string(),
                size: 1,
                updated_at: "t".to_string(),
                deleted: true,
                delete_epoch: 7,
            },
        ];
        assert_eq!(max_delete_epoch_from_summaries(&items), 7);
        assert_eq!(max_delete_epoch_from_summaries::<String>(&[]), 0);
    }
}
