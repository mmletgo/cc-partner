//! cc/engine.rs — Claude Code 历史跨设备同步引擎
//!
//! Business Logic（为什么需要这个模块）:
//!     采集到的 Claude Code 历史需要在用户的各设备间同步（在 A 设备问的 prompt，B 设备也能查到）。
//!     复用现有向量时钟基础设施，但走独立同步链路（`/api/cc-history/sync/*`），与 prompts 同步
//!     解耦——cc 同步失败不影响 prompts 同步计数。由 `sync/engine.rs::sync_with_peer` 末尾调用。
//!
//! Code Logic（这个模块做什么）:
//!     `cc_sync_with_peer(state, device)`：
//!     1. health_info 取对端 `PeerProtocolInfo`；不可达则跳过；
//!     2. 若 `supports(cc-history.paged-sync.v1)` → 有界分页路径（manifest-page/items/push-batch）；
//!     3. 否则 → legacy pull/push 路径（兼容旧对端）；
//!     4. 分页路径错误/取消结束本轮并上抛；下轮从零开始（不持久化 remote cursor）。
//!     全程对 legacy 路径失败仅 tracing::warn 不阻断（保持旧行为）。

use crate::cc::merger::merge_cc_history;
use crate::cc::models::{CcSyncSummary, ClaudeHistoryRow};
use crate::net::peer_client::PeerCallError;
use crate::net::protocol::CAPABILITY_CC_HISTORY_PAGED_SYNC_V1;
use crate::net::routes::cc_history::{
    estimate_row_bytes, CC_BATCH_MAX_ESTIMATED_BYTES, CC_ITEM_BATCH_LIMIT,
    CC_MANIFEST_PAGE_LIMIT_DEFAULT, CODE_BATCH_TOO_LARGE, CODE_ITEM_TOO_LARGE,
};
use crate::state::AppState;
use crate::sync::vector_clock::{compare, ClockOrder};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// 同步批次条数指标名（固定标签，不含用户/路径数据）。
const METRIC_SYNC_BATCH_ITEMS: &str = "cc_history.sync_batch.items";
/// 同步批次估算字节指标名。
const METRIC_SYNC_BATCH_BYTES: &str = "cc_history.sync_batch.estimated_bytes";
/// 同步批次耗时指标名。
const METRIC_SYNC_BATCH_MS: &str = "cc_history.sync_batch.ms";
/// 同步整轮耗时指标名。
const METRIC_SYNC_ROUND_MS: &str = "cc_history.sync_round_ms";
/// 本轮因单条 item_too_large 隔离毒丸的次数（脱敏计数，不含 id/正文）。
const METRIC_ITEM_TOO_LARGE_ISOLATED: &str = "cc_history.item_too_large_isolated";

/// 与单个对端执行 Claude Code 历史的双向同步。
///
/// Business Logic: 确保双方 cc 历史一致。对端声明 `cc-history.paged-sync.v1` 时走有界分页协议，
///     否则回退 legacy pull/push。分页路径上的协议/业务错误结束本轮并返回 Err（禁止伪装成
///     成功的零条同步）；legacy 路径失败仍仅 warn 不阻断，保持一代兼容行为。
///
/// Code Logic:
///     1. health_info；网络失败 → warn 并 Ok 跳过；
///     2. protocol supports paged → `cc_sync_paged_with_peer`，否则 `cc_sync_legacy_with_peer`；
///     3. 记录 `cc_history.sync_round_ms`。
pub async fn cc_sync_with_peer(
    state: &AppState,
    device: &crate::models::device::Device,
) -> Result<(), String> {
    let base_url = device.base_url();
    // paged 可观测面不写 device_name；legacy 仍可诊断对端名（一代兼容路径）。
    tracing::info!("开始 CC 历史同步");
    let round_start = Instant::now();

    let health = match state.peer_client.health_info(&base_url).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("对端不可达，跳过 CC 历史同步: {e}");
            return Ok(());
        }
    };
    let protocol = health.protocol_info();
    let result = if protocol.supports(CAPABILITY_CC_HISTORY_PAGED_SYNC_V1) {
        cc_sync_paged_with_peer(state, &base_url).await
    } else {
        cc_sync_legacy_with_peer(state, &base_url, &device.name).await
    };

    state
        .runtime_metrics
        .record_duration(METRIC_SYNC_ROUND_MS, round_start.elapsed());

    match &result {
        Ok(()) => tracing::info!("CC 历史同步完成"),
        Err(e) => tracing::warn!("CC 历史同步失败: {e}"),
    }
    result
}

/// legacy 全量 pull/push 路径（仅当对端无 paged capability）。
///
/// Business Logic（为什么需要这个函数）:
///     旧版本对端只挂载 `/pull|/push`；新客户端必须无 capability 时回退，行为与升级前一致。
///
/// Code Logic（这个函数做什么）:
///     取本地全量摘要 → `cc_sync_pull`（失败折叠为空）→ 逐条 merge + bulk_upsert →
///     算补集 → `cc_sync_push`（bool）。失败仅 warn。
async fn cc_sync_legacy_with_peer(
    state: &AppState,
    base_url: &str,
    device_name: &str,
) -> Result<(), String> {
    let local_all = state
        .cc_history_repo
        .get_all_for_sync()
        .await
        .map_err(|e| format!("读取本地 CC 历史失败: {e}"))?;
    let summary_values: Vec<serde_json::Value> = local_all
        .iter()
        .map(|p| serde_json::json!({ "id": p.id, "vector_clock": p.vector_clock }))
        .collect();

    let remote_items: Vec<ClaudeHistoryRow> = state
        .peer_client
        .cc_sync_pull(base_url, summary_values)
        .await;

    let mut to_upsert: Vec<ClaudeHistoryRow> = Vec::new();
    for remote in &remote_items {
        let local_row = state
            .cc_history_repo
            .get(&remote.id)
            .await
            .map_err(|e| format!("查询本地 CC 历史 {} 失败: {e}", remote.id))?;
        match local_row {
            None => to_upsert.push(remote.clone()),
            Some(local_row) => {
                let merged = merge_cc_history(&local_row, remote);
                if merged.vector_clock != local_row.vector_clock
                    || merged.updated_at != local_row.updated_at
                    || merged.content != local_row.content
                    || merged.deleted != local_row.deleted
                {
                    to_upsert.push(merged);
                }
            }
        }
    }

    if !to_upsert.is_empty() {
        let n = to_upsert.len();
        state
            .cc_history_repo
            .bulk_upsert(&to_upsert)
            .await
            .map_err(|e| format!("CC 历史 bulk_upsert 失败: {e}"))?;
        tracing::info!("从 {device_name} 拉取并更新了 {n} 条 CC 历史 (legacy)");
    }

    let remote_ids: HashSet<String> = remote_items.iter().map(|p| p.id.clone()).collect();
    let remote_clock_map: HashMap<String, &HashMap<String, u64>> = remote_items
        .iter()
        .map(|p| (p.id.clone(), &p.vector_clock))
        .collect();

    let local_all_after = state
        .cc_history_repo
        .get_all_for_sync()
        .await
        .map_err(|e| format!("重新读取本地 CC 历史失败: {e}"))?;

    let mut push_items: Vec<ClaudeHistoryRow> = Vec::new();
    for p in &local_all_after {
        match remote_clock_map.get(&p.id) {
            None => push_items.push(p.clone()),
            Some(remote_clock) => {
                let relation = compare(&p.vector_clock, remote_clock);
                if matches!(relation, ClockOrder::After | ClockOrder::Concurrent)
                    && !remote_ids.contains(&p.id)
                {
                    push_items.push(p.clone());
                }
            }
        }
    }

    if !push_items.is_empty() {
        let n = push_items.len();
        let success = state.peer_client.cc_sync_push(base_url, &push_items).await;
        if success {
            tracing::info!("向 {device_name} 推送了 {n} 条 CC 历史 (legacy)");
        } else {
            tracing::warn!("向 {device_name} 推送 CC 历史失败 (legacy)");
        }
    }
    Ok(())
}

/// 有界分页双向同步路径。
///
/// Business Logic（为什么需要这个函数）:
///     10k+ 行时 legacy 全量 body 会撑爆内存与请求；分页协议只保留摘要映射与 ≤128 正文批，
///     并用事务 upsert，错误不留下半完成批次。
///
/// Code Logic（这个函数做什么）:
///     1. 分页拉 remote manifest 至 done，拒绝不前进 cursor；
///     2. 按批 get 本地行比较 vector clock，收集 need_pull / need_push；
///     3. items 批拉（≤128，遇 batch_too_large **或** 多 ID item_too_large 对半拆到 1；
///        隔离到单条 item_too_large 时记录毒丸后继续完成本批好数据，再结束本轮）；
///     4. merge 后 `upsert_merged_batch`；
///     5. 分页本端 manifest，对 need_push 批取正文并 push-batch（同样拆批语义）；
///     6. 每批记录 items/estimated_bytes/duration 固定名指标（无设备名/正文）。
async fn cc_sync_paged_with_peer(state: &AppState, base_url: &str) -> Result<(), String> {
    let remote_manifest = fetch_all_remote_manifest_pages(state, base_url).await?;
    let remote_clocks: HashMap<String, HashMap<String, u64>> = remote_manifest
        .iter()
        .map(|s| (s.id.clone(), s.vector_clock.clone()))
        .collect();

    let mut need_pull: Vec<String> = Vec::new();
    let mut need_push_ids: HashSet<String> = HashSet::new();

    let remote_ids: Vec<String> = remote_manifest.iter().map(|s| s.id.clone()).collect();
    for chunk in remote_ids.chunks(CC_ITEM_BATCH_LIMIT) {
        let ids: Vec<String> = chunk.to_vec();
        let local_map = state
            .cc_history_repo
            .get_many_for_sync(&ids)
            .await
            .map_err(|e| format!("批量读本地 CC 历史失败: {e}"))?;
        for id in &ids {
            let remote_vc = remote_clocks.get(id).expect("remote id from same list");
            match local_map.get(id) {
                None => need_pull.push(id.clone()),
                Some(local) => {
                    let rel = compare(remote_vc, &local.vector_clock);
                    match rel {
                        ClockOrder::After | ClockOrder::Concurrent => need_pull.push(id.clone()),
                        ClockOrder::Before => {
                            need_push_ids.insert(id.clone());
                        }
                        ClockOrder::Equal => {}
                    }
                    if matches!(rel, ClockOrder::Concurrent) {
                        need_push_ids.insert(id.clone());
                    }
                }
            }
        }
    }

    let mut local_cursor: Option<String> = None;
    loop {
        let page = state
            .cc_history_repo
            .list_sync_manifest_page(local_cursor.as_deref(), CC_MANIFEST_PAGE_LIMIT_DEFAULT)
            .await
            .map_err(|e| format!("分页读本地 manifest 失败: {e}"))?;
        if page.is_empty() {
            break;
        }
        for s in &page {
            if !remote_clocks.contains_key(&s.id) {
                need_push_ids.insert(s.id.clone());
            }
        }
        let last_id = page.last().map(|s| s.id.clone());
        if page.len() < CC_MANIFEST_PAGE_LIMIT_DEFAULT as usize {
            break;
        }
        if last_id == local_cursor {
            return Err("本地 manifest 分页 cursor 未前进".to_string());
        }
        local_cursor = last_id;
    }

    let mut pulled = 0usize;
    let mut isolated_poison = false;
    for chunk in need_pull.chunks(CC_ITEM_BATCH_LIMIT) {
        let ids = chunk.to_vec();
        let (items, poison) = fetch_items_with_halving(state, base_url, ids).await?;
        if !items.is_empty() {
            let batch_start = Instant::now();
            let est: usize = items.iter().map(estimate_row_bytes).sum();
            let n = items.len();

            let mut to_upsert: Vec<ClaudeHistoryRow> = Vec::new();
            let item_ids: Vec<String> = items.iter().map(|r| r.id.clone()).collect();
            let local_map = state
                .cc_history_repo
                .get_many_for_sync(&item_ids)
                .await
                .map_err(|e| format!("pull 后批量读本地失败: {e}"))?;
            for remote in &items {
                match local_map.get(&remote.id) {
                    None => to_upsert.push(remote.clone()),
                    Some(local_row) => {
                        let merged = merge_cc_history(local_row, remote);
                        if merged.vector_clock != local_row.vector_clock
                            || merged.updated_at != local_row.updated_at
                            || merged.content != local_row.content
                            || merged.deleted != local_row.deleted
                        {
                            to_upsert.push(merged);
                        }
                    }
                }
            }
            if !to_upsert.is_empty() {
                let tx_start = Instant::now();
                let written = state
                    .cc_history_repo
                    .upsert_merged_batch(&to_upsert)
                    .await
                    .map_err(|e| format!("CC 历史 upsert_merged_batch 失败: {e}"))?;
                state
                    .runtime_metrics
                    .measure_db_transaction(tx_start.elapsed());
                pulled += written;
            }
            record_batch_metrics(state, n as u64, est as u64, batch_start.elapsed());
        }
        if poison {
            // 好数据已落库；结束本轮，禁止把毒丸伪装成空成功继续扫后续批。
            isolated_poison = true;
            break;
        }
    }
    if pulled > 0 {
        tracing::info!("分页拉取并更新了 {pulled} 条 CC 历史");
    }
    if isolated_poison {
        state
            .runtime_metrics
            .record_count(METRIC_ITEM_TOO_LARGE_ISOLATED, 1);
        return Err("单条 item_too_large，已隔离毒丸并结束本轮".to_string());
    }

    let push_id_list: Vec<String> = need_push_ids.into_iter().collect();
    let mut pushed = 0usize;
    for chunk in push_id_list.chunks(CC_ITEM_BATCH_LIMIT) {
        let ids = chunk.to_vec();
        let local_map = state
            .cc_history_repo
            .get_many_for_sync(&ids)
            .await
            .map_err(|e| format!("push 前批量读本地失败: {e}"))?;
        let mut rows: Vec<ClaudeHistoryRow> = ids
            .iter()
            .filter_map(|id| local_map.get(id).cloned())
            .collect();
        if rows.is_empty() {
            continue;
        }
        while !rows.is_empty() {
            let batch = take_push_batch(&mut rows);
            let (accepted, poison) = push_batch_with_halving(state, base_url, batch).await?;
            pushed += accepted;
            if poison {
                if pushed > 0 {
                    tracing::info!("分页推送了 {pushed} 条 CC 历史");
                }
                state
                    .runtime_metrics
                    .record_count(METRIC_ITEM_TOO_LARGE_ISOLATED, 1);
                return Err("push 单条 item_too_large，已隔离毒丸并结束本轮".to_string());
            }
        }
    }
    if pushed > 0 {
        tracing::info!("分页推送了 {pushed} 条 CC 历史");
    }
    Ok(())
}

/// 分页拉取对端全部 manifest 摘要。
///
/// Business Logic（为什么需要这个函数）:
///     客户端必须先掌握对端完整摘要再决定 items/push；cursor 不前进视为协议故障。
///
/// Code Logic（这个函数做什么）:
///     循环 `cc_sync_manifest_page` 直至 done；比较 next_cursor 与上一 cursor，相同则 Err。
async fn fetch_all_remote_manifest_pages(
    state: &AppState,
    base_url: &str,
) -> Result<Vec<CcSyncSummary>, String> {
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    let mut prev_cursor: Option<String> = None;
    loop {
        let batch_start = Instant::now();
        let page = state
            .peer_client
            .cc_sync_manifest_page(
                base_url,
                cursor.as_deref(),
                Some(CC_MANIFEST_PAGE_LIMIT_DEFAULT),
            )
            .await
            .map_err(|e| format!("manifest-page 失败: {e}"))?;
        let n = page.summaries.len();
        let est = page
            .summaries
            .iter()
            .map(|s| s.id.len() + s.vector_clock.len() * 8)
            .sum::<usize>();
        record_batch_metrics(state, n as u64, est as u64, batch_start.elapsed());
        all.extend(page.summaries);

        if page.done {
            break;
        }
        let next = page.next_cursor.clone();
        if next.is_none() {
            return Err("manifest-page done=false 但 next_cursor 为空".to_string());
        }
        if next == prev_cursor || next == cursor {
            return Err("manifest-page cursor 未前进".to_string());
        }
        prev_cursor = cursor;
        cursor = next;
    }
    Ok(all)
}

/// 拉取 items；遇 `batch_too_large` **或** 多 ID `item_too_large` 对半拆分直至 1。
///
/// Business Logic（为什么需要这个函数）:
///     对端以 8MiB 估算拒绝大包时，客户端必须缩小批次继续；多 ID 批中只要混入一条
///     content>1MiB 的毒丸，服务端会整批 422——若不拆批，好 ID 永远无法同步。
///     拆到单条仍 422 时隔离毒丸（记脱敏计数），继续完成本批其余 ID，再由调用方结束本轮。
///
/// Code Logic（这个函数做什么）:
///     栈式拆分 ids；成功合并 items；`batch_too_large|item_too_large && len>1` → 对半入栈；
///     `item_too_large && len==1` → 丢弃该 id、记 isolated=true、不写 content/id；
///     返回 `(items, isolated_poison)`。
async fn fetch_items_with_halving(
    state: &AppState,
    base_url: &str,
    ids: Vec<String>,
) -> Result<(Vec<ClaudeHistoryRow>, bool), String> {
    // 迭代拆批：async 递归需 Box::pin，这里用栈避免无限尺寸 future。
    let mut stack: Vec<Vec<String>> = vec![ids];
    let mut out = Vec::new();
    let mut isolated_poison = false;
    while let Some(chunk) = stack.pop() {
        if chunk.is_empty() {
            continue;
        }
        let batch_start = Instant::now();
        match state.peer_client.cc_sync_items(base_url, &chunk).await {
            Ok(resp) => {
                let est: usize = resp.items.iter().map(estimate_row_bytes).sum();
                record_batch_metrics(
                    state,
                    resp.items.len() as u64,
                    est as u64,
                    batch_start.elapsed(),
                );
                out.extend(resp.items);
            }
            Err(e)
                if (is_batch_too_large(&e) || is_item_too_large(&e)) && chunk.len() > 1 =>
            {
                let mid = chunk.len() / 2;
                stack.push(chunk[mid..].to_vec());
                stack.push(chunk[..mid].to_vec());
            }
            Err(e) if is_item_too_large(&e) && chunk.len() == 1 => {
                // 隔离毒丸：不把 id/content 写入日志或指标；继续处理栈内其余好 ID。
                isolated_poison = true;
                tracing::warn!("items 遇到单条 item_too_large，已隔离毒丸");
            }
            Err(e) if is_batch_too_large(&e) && chunk.len() == 1 => {
                return Err("单条 batch_too_large，结束本轮".to_string());
            }
            Err(e) => return Err(format!("items 失败: {e}")),
        }
    }
    Ok((out, isolated_poison))
}

/// push-batch；遇 `batch_too_large` **或** 多 ID `item_too_large` 对半拆分直至 1。
///
/// Business Logic（为什么需要这个函数）:
///     与 items 对称：合法 413/422 多 ID 拆批继续；单条超限隔离毒丸后结束本轮，禁止静默跳过。
///
/// Code Logic（这个函数做什么）:
///     调用 `cc_sync_push_batch`；过大则拆分；单条 item_too_large → isolated 标志；
///     返回 `(accepted, isolated_poison)`。
async fn push_batch_with_halving(
    state: &AppState,
    base_url: &str,
    items: Vec<ClaudeHistoryRow>,
) -> Result<(usize, bool), String> {
    let mut stack: Vec<Vec<ClaudeHistoryRow>> = vec![items];
    let mut accepted_total = 0usize;
    let mut isolated_poison = false;
    while let Some(chunk) = stack.pop() {
        if chunk.is_empty() {
            continue;
        }
        let batch_start = Instant::now();
        let n = chunk.len();
        let est: usize = chunk.iter().map(estimate_row_bytes).sum();
        match state.peer_client.cc_sync_push_batch(base_url, &chunk).await {
            Ok(resp) => {
                record_batch_metrics(state, n as u64, est as u64, batch_start.elapsed());
                accepted_total += resp.accepted;
            }
            Err(e)
                if (is_batch_too_large(&e) || is_item_too_large(&e)) && chunk.len() > 1 =>
            {
                let mid = chunk.len() / 2;
                stack.push(chunk[mid..].to_vec());
                stack.push(chunk[..mid].to_vec());
            }
            Err(e) if is_item_too_large(&e) && chunk.len() == 1 => {
                isolated_poison = true;
                tracing::warn!("push-batch 遇到单条 item_too_large，已隔离毒丸");
            }
            Err(e) if is_batch_too_large(&e) && chunk.len() == 1 => {
                return Err("push 单条 batch_too_large，结束本轮".to_string());
            }
            Err(e) => return Err(format!("push-batch 失败: {e}")),
        }
    }
    Ok((accepted_total, isolated_poison))
}

/// 从待 push 队列取出不超过 128 条且估算 ≤8MiB 的一批。
///
/// Business Logic（为什么需要这个函数）:
///     主动控制批大小，减少对端 413 往返。
///
/// Code Logic（这个函数做什么）:
///     从 `rows` 前端弹出，累计 estimate_row_bytes，触顶则把最后一条放回并停止。
fn take_push_batch(rows: &mut Vec<ClaudeHistoryRow>) -> Vec<ClaudeHistoryRow> {
    let mut batch = Vec::new();
    let mut est = 0usize;
    while !rows.is_empty() && batch.len() < CC_ITEM_BATCH_LIMIT {
        let next_est = estimate_row_bytes(&rows[0]);
        if !batch.is_empty() && est.saturating_add(next_est) > CC_BATCH_MAX_ESTIMATED_BYTES {
            break;
        }
        let row = rows.remove(0);
        est = est.saturating_add(next_est);
        batch.push(row);
    }
    batch
}

/// 记录同步批次固定名指标。
///
/// Business Logic（为什么需要这个函数）:
///     本地诊断需要 items/估算字节/耗时，且不得写入正文、路径或设备名。
///
/// Code Logic（这个函数做什么）:
///     `record_count` items 与 estimated_bytes；`record_duration` batch ms。
fn record_batch_metrics(
    state: &AppState,
    items: u64,
    estimated_bytes: u64,
    duration: std::time::Duration,
) {
    state
        .runtime_metrics
        .record_count(METRIC_SYNC_BATCH_ITEMS, items);
    state
        .runtime_metrics
        .record_count(METRIC_SYNC_BATCH_BYTES, estimated_bytes);
    state
        .runtime_metrics
        .record_duration(METRIC_SYNC_BATCH_MS, duration);
}

/// 判定是否 `cc_history.batch_too_large`。
fn is_batch_too_large(err: &PeerCallError) -> bool {
    err.code() == Some(CODE_BATCH_TOO_LARGE)
}

/// 判定是否 `cc_history.item_too_large`。
fn is_item_too_large(err: &PeerCallError) -> bool {
    err.code() == Some(CODE_ITEM_TOO_LARGE)
}

#[cfg(test)]
mod tests {
    /// 委托 mixed_version_harness，避免与 integration test 场景漂移。
    #[test]
    fn cc_history_mixed_version_new_to_new_uses_only_paged_routes() {
        crate::cc::mixed_version_harness::assert_new_to_new_uses_only_paged_routes();
    }

    #[test]
    fn cc_history_mixed_version_new_to_legacy_uses_only_legacy_routes() {
        crate::cc::mixed_version_harness::assert_new_to_legacy_uses_only_legacy_routes();
    }

    #[test]
    fn cc_history_mixed_version_malformed_paged_fails_round_not_empty_success() {
        crate::cc::mixed_version_harness::assert_malformed_paged_fails_round_not_empty_success();
    }

    #[test]
    fn cc_history_mixed_version_item_too_large_halves_and_isolates_poison() {
        crate::cc::mixed_version_harness::assert_item_too_large_halves_and_isolates_poison();
    }

    #[test]
    fn cc_history_mixed_version_batch_too_large_halves_until_success() {
        crate::cc::mixed_version_harness::assert_batch_too_large_halves_until_success();
    }

    #[test]
    fn cc_history_mixed_version_concurrent_vector_clock_merges() {
        crate::cc::mixed_version_harness::assert_concurrent_vector_clock_merges();
    }

    #[test]
    fn cc_history_mixed_version_legacy_bodies_work_against_new_server() {
        crate::cc::mixed_version_harness::assert_legacy_bodies_work_against_new_server();
    }
}
