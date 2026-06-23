//! sync/engine.rs — 同步引擎：协调 Prompt 在多设备间的双向同步
//!
//! Business Logic（为什么需要这个模块）:
//!     多设备编辑 Prompt 时，需要一个中心协调器管理同步流程：与谁同步、如何处理冲突。
//!     对照 Python `sync/engine.py`。触发机制：用户在 Prompt 管理面板点击"同步"按钮时，
//!     由 `trigger_sync` 命令调用本引擎。
//!
//! Code Logic（这个模块做什么）:
//! trigger_sync(state) 返回 SyncResult：
//! 1. 读 AppState.devices 全部在线对端；
//! 2. 逐个对端执行 sync_with_peer（双向 pull + push）；
//! 3. 任一对端失败不阻断其他对端（try/catch 继续下一个）；
//! 4. 返回 accepted/synced/note（对照 Python /api/sync 返回结构）。
//!
//! sync_with_peer（单对端双向同步，对照 Python 同名方法）：
//! 1. 健康检查，不可达则跳过；
//! 2. 获取本端全部 prompt（含 deleted），投影为 summaries；
//! 3. Pull：发 summaries 给对端，拿回对端认为本端需要的 prompts，逐条 merge_prompt 后落库；
//! 4. Push：重新取本端 summary，找出本端有而对端 pull 未返回的（即对端可能没有的），推送过去。

use crate::models::claude_md::ClaudeMdRow;
use crate::models::prompt::PromptRow;
use crate::state::AppState;
use crate::sync::merger::merge_prompt;
use crate::sync::vector_clock::compare;
use std::collections::HashMap;

/// 触发全局同步的返回结构（字段对照 Python `/api/sync` 的 `{accepted, synced, note}`）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncResult {
    /// 是否已接受同步任务（这里同步是同步执行的，故恒为 true）
    pub accepted: bool,
    /// 实际成功同步的对端设备数量
    pub synced: u64,
    /// 人类可读备注
    pub note: String,
}

/// 触发全局同步：遍历全部在线对端执行双向同步。
///
/// Business Logic: 用户点击"同步"按钮时调用。与所有在线设备同步，任一失败不阻断其他。
///     对照 Python `sync_all` + 各对端 `sync_with_peer`。
///
/// Code Logic: 读 devices RwLock 取快照；无对端直接返回；否则逐个 await sync_with_peer，
///     用 Ok/Err 计数，全做完返回 SyncResult。
pub async fn trigger_sync(state: &AppState) -> SyncResult {
    // 取设备快照（避免长时间持锁）
    let devices: Vec<crate::models::device::Device> = {
        let guard = state.devices.read().expect("devices 读锁中毒");
        guard.values().cloned().collect()
    };

    if devices.is_empty() {
        tracing::debug!("没有在线设备，跳过同步");
        return SyncResult {
            accepted: true,
            synced: 0,
            note: "没有在线设备".to_string(),
        };
    }

    tracing::info!("开始与 {} 个设备同步", devices.len());

    let mut synced_count: u64 = 0;
    for device in devices {
        // 逐对端同步，失败不阻断其他对端（对照 Python sync_all 的 try/except）
        match sync_with_peer(state, &device).await {
            Ok(()) => {
                synced_count += 1;
            }
            Err(e) => {
                tracing::error!("与设备 {} 同步异常: {}", device.name, e);
            }
        }
    }

    SyncResult {
        accepted: true,
        synced: synced_count,
        note: format!("已与 {} 个设备同步", synced_count),
    }
}

/// 将本机 CLAUDE.md 版本推送给所有在线对端，不执行远端 pull。
///
/// Business Logic: CLAUDE.md 页面里的手动按钮语义是"以本机当前配置为准，分发到局域网设备"，
///     不能先拉取远端版本覆盖本机编辑器内容。因此这里只做 health + push，远端收到后仍走
///     既有 merge 规则落库，保持协议兼容。
/// Code Logic: 读取设备快照；逐个健康检查；可达则调用 claude_md_push(row)；请求成功即计入
///     synced，失败仅记录日志并继续其他设备。
pub async fn push_claude_md_to_peers(state: &AppState, row: &ClaudeMdRow) -> SyncResult {
    let devices: Vec<crate::models::device::Device> = {
        let guard = state.devices.read().expect("devices 读锁中毒");
        guard.values().cloned().collect()
    };

    if devices.is_empty() {
        tracing::debug!("没有在线设备，跳过 CLAUDE.md 推送");
        return SyncResult {
            accepted: true,
            synced: 0,
            note: "没有在线设备".to_string(),
        };
    }

    tracing::info!("开始向 {} 个设备推送 CLAUDE.md", devices.len());

    let mut pushed_count: u64 = 0;
    for device in devices {
        let base_url = device.base_url();
        if !state.peer_client.health(&device.host, device.port).await {
            tracing::debug!("设备 {} 不可达，跳过 CLAUDE.md 推送", device.name);
            continue;
        }

        match state.peer_client.claude_md_push(&base_url, row).await {
            Ok(accepted) => {
                pushed_count += 1;
                tracing::info!(
                    "向 {} 推送 CLAUDE.md 完成，accepted={}",
                    device.name,
                    accepted
                );
            }
            Err(e) => {
                tracing::warn!("向 {} 推送 CLAUDE.md 失败: {e}", device.name);
            }
        }
    }

    SyncResult {
        accepted: true,
        synced: pushed_count,
        note: format!("已向 {} 个设备推送 CLAUDE.md", pushed_count),
    }
}

/// 与单个对端执行完整双向同步。
///
/// Business Logic: 确保双方数据一致。对照 Python `sync_with_peer`。
///
/// Code Logic:
///     1. health 检查，不可达跳过；
///     2. 本端 summaries（全部 prompt 含 deleted 的 {id, vector_clock}）；
///     3. Pull：POST sync/pull，拿回对端需要给本端的 prompts；逐条查本地，本地无则直接接收，
///        本地有则 merge_prompt，仅当合并结果与本地有差异时落库；bulk_upsert；
///     4. Push：重新取本端 summary，找出本端有而对端 pull 未返回（即对端可能没有 / 对端落后）
///        的 prompt，POST sync/push 推过去。
async fn sync_with_peer(
    state: &AppState,
    device: &crate::models::device::Device,
) -> Result<(), String> {
    let base_url = device.base_url();
    tracing::info!("开始与设备 {} ({}) 同步", device.name, base_url);

    // 1. 健康检查
    if !state.peer_client.health(&device.host, device.port).await {
        tracing::warn!("设备 {} 不可达，跳过同步", device.name);
        return Ok(());
    }

    // 2. 本端全部 prompt（含 deleted），投影为 summaries {id, vector_clock}
    let local_all = state
        .prompt_repo
        .get_all_for_sync()
        .await
        .map_err(|e| format!("读取本地 prompt 失败: {e}"))?;
    let summary_values: Vec<serde_json::Value> = local_all
        .iter()
        .map(|p| serde_json::json!({ "id": p.id, "vector_clock": p.vector_clock }))
        .collect();

    // 3. Pull：发本端 summaries，拿回对端认为本端需要的 prompts
    let remote_prompts: Vec<PromptRow> =
        state.peer_client.sync_pull(&base_url, summary_values).await;

    let mut prompts_to_upsert: Vec<PromptRow> = Vec::new();
    for remote in &remote_prompts {
        let local_row = state
            .prompt_repo
            .get(&remote.id)
            .await
            .map_err(|e| format!("查询本地 prompt {} 失败: {e}", remote.id))?;
        match local_row {
            None => {
                // 本地没有 → 直接接收
                prompts_to_upsert.push(remote.clone());
            }
            Some(local_row) => {
                // 本地有 → 合并决策
                let merged = merge_prompt(&local_row, remote);
                // 仅当合并结果与本地有差异时才落库
                if merged.vector_clock != local_row.vector_clock
                    || merged.updated_at != local_row.updated_at
                    || merged.content != local_row.content
                    || merged.title != local_row.title
                    || merged.deleted != local_row.deleted
                {
                    prompts_to_upsert.push(merged);
                }
            }
        }
    }

    if !prompts_to_upsert.is_empty() {
        let n = prompts_to_upsert.len();
        state
            .prompt_repo
            .bulk_upsert(&prompts_to_upsert)
            .await
            .map_err(|e| format!("bulk_upsert 失败: {e}"))?;
        tracing::info!("从 {} 拉取并更新了 {} 条 prompt", device.name, n);
    }

    // 4. Push：本端有而对端 pull 未返回的（即对端可能没有 / 对端落后），推送给对端
    //    对端 pull 返回的 id 集合 = 对端已有的（或对端认为本端需要的）；这里取补集策略：
    //    本端独有（对端返回里没有的）+ 本端领先/并发但 pull 未回带的，都推送（对照 Python
    //    sync_with_peer 推送 local_summary 中不在 remote_ids 的逻辑，并扩展到本端领先的对端条目）。
    let remote_ids: std::collections::HashSet<String> =
        remote_prompts.iter().map(|p| p.id.clone()).collect();

    // 重新取本端最新全量（pull 阶段可能已落库更新）
    let local_all_after = state
        .prompt_repo
        .get_all_for_sync()
        .await
        .map_err(|e| format!("重新读取本地 prompt 失败: {e}"))?;

    // 对端返回的 prompt 摘要（用于判断本端是否领先/并发需推送）
    let remote_summary_map: HashMap<String, &HashMap<String, u64>> = remote_prompts
        .iter()
        .map(|p| (p.id.clone(), &p.vector_clock))
        .collect();

    let mut push_prompts: Vec<PromptRow> = Vec::new();
    for p in &local_all_after {
        match remote_summary_map.get(&p.id) {
            None => {
                // 对端没有（pull 未返回此 id）→ 推送
                push_prompts.push(p.clone());
            }
            Some(remote_clock) => {
                // 本端 vs 对端：本端领先或并发 → 推送（对端会做 LWW 合并）
                let relation = compare(&p.vector_clock, remote_clock);
                if matches!(relation, crate::sync::vector_clock::ClockOrder::After)
                    || matches!(relation, crate::sync::vector_clock::ClockOrder::Concurrent)
                {
                    // 仅当不在 remote_ids（避免重复推送 pull 已带走的）时推送
                    if !remote_ids.contains(&p.id) {
                        push_prompts.push(p.clone());
                    }
                }
            }
        }
    }

    if !push_prompts.is_empty() {
        let n = push_prompts.len();
        let success = state.peer_client.sync_push(&base_url, &push_prompts).await;
        if success {
            tracing::info!("向 {} 推送了 {} 条 prompt", device.name, n);
        } else {
            tracing::warn!("向 {} 推送 prompt 失败", device.name);
        }
    }

    tracing::info!("与设备 {} 同步完成", device.name);

    // 同步链路末尾追加 Claude Code 历史同步（独立链路，失败仅 warn 不影响 prompts 同步计数）
    let _ = crate::cc::engine::cc_sync_with_peer(state, device).await;

    // 同步链路末尾追加 SSH 目标同步（独立链路，失败仅 warn 不影响 prompts 同步计数）
    let _ = crate::sync::ssh_target::ssh_target_sync_with_peer(state, device).await;

    // 同步链路末尾追加速记本同步（单例文本；失败仅 warn 不影响 prompts 同步计数）
    let _ = crate::sync::scratchpad::scratchpad_sync_with_peer(state, device).await;

    Ok(())
}
