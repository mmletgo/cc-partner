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

use crate::models::claude_md::{ClaudeMdRow, CLAUDE_MD_ID};
use crate::models::prompt::PromptRow;
use crate::state::AppState;
use crate::sync::merger::merge_prompt;
use crate::sync::vector_clock::{compare, ClockOrder};
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

    // 同步前先把应用外编辑（用户可能在 Claude Code/编辑器里直接改了 ~/.claude/CLAUDE.md）
    // 纳入向量时钟，避免用过期 DB 版本覆盖用户最新改动。失败仅告警，不阻断同步。
    if let Err(e) = crate::sync::claude_md::reconcile_from_file(state).await {
        tracing::warn!("CLAUDE.md 文件对账失败，跳过本次文件纳入: {e}");
    }

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
        // CLAUDE.md 同步挂在与 prompts 同步同一循环里：prompts 同步之后追加，
        // 失败仅告警不阻断其他对端，也不影响 synced 计数（计数语义保持"prompts 同步成功"不变）。
        if let Err(e) = sync_claude_md_with_peer(state, &device).await {
            tracing::warn!("与设备 {} CLAUDE.md 同步异常: {e}", device.name);
        }
    }

    SyncResult {
        accepted: true,
        synced: synced_count,
        note: format!("已与 {} 个设备同步", synced_count),
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

    Ok(())
}

/// 与单个对端执行 CLAUDE.md 双向同步。
///
/// Business Logic: 确保 user 级 CLAUDE.md 双方一致。先 pull 拉回对端版本合并落库，
///     再据比较结果决定是否把本地版本 push 过去。对照 sync_with_peer 的结构，只是单例退化为 0/1 条。
///
/// Code Logic:
///     1. 构造 base_url（与 sync_with_peer 一致：`http://{host}:{port}`）；
///     2. health 检查，不可达记日志后 return Ok(())；
///     3. 读本地 claude_md，None 视为空行（content/updated_at 空串、空向量时钟）；
///     4. pull：拉回对端版本，错误（含 404 旧版本对端无此路由）记 warn 并视 remote=None 继续；
///     5. 若 remote 存在：merge_claude_md 合并，与本地有差异则落库 + 写文件（错误记日志）；
///     6. 重读本地（pull 合并可能已改 DB），None 视空行；
///     7. 决策是否 push：对端无数据且本地非空，或本地相对对端领先/并发 → push（错误记日志）。
async fn sync_claude_md_with_peer(
    state: &AppState,
    device: &crate::models::device::Device,
) -> Result<(), String> {
    let base_url = device.base_url();

    // 1/2. 健康检查（与 sync_with_peer 一致：不可达直接返回，不算同步成功也不算失败）
    if !state.peer_client.health(&device.host, device.port).await {
        tracing::debug!("设备 {} 不可达，跳过 CLAUDE.md 同步", device.name);
        return Ok(());
    }

    // 3. 读本地 CLAUDE.md（None 视为空行）
    let local = state
        .claude_md_repo
        .get()
        .await
        .map_err(|e| format!("读取本地 CLAUDE.md 失败: {e}"))?;
    let local_row = local.unwrap_or_else(|| ClaudeMdRow {
        id: CLAUDE_MD_ID.to_string(),
        content: String::new(),
        updated_at: String::new(),
        device_id: state.device_id.as_str().to_string(),
        vector_clock: HashMap::new(),
    });

    // 4. Pull：拉回对端版本。错误（含 404 旧版本对端未实现此路由）→ warn + 视 None 继续。
    let remote = match state
        .peer_client
        .claude_md_pull(&base_url, &local_row.vector_clock)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "从 {} claude_md_pull 失败（可能是旧版本对端）: {e}",
                device.name
            );
            None
        }
    };

    // 5. 若对端有版本：合并落库 + 写文件（错误记日志不阻断）
    if let Some(remote_row) = &remote {
        let merged = crate::sync::claude_md::merge_claude_md(&local_row, remote_row);
        if merged.content != local_row.content || merged.vector_clock != local_row.vector_clock {
            if let Err(e) = state.claude_md_repo.upsert(&merged).await {
                tracing::warn!("向本地落库合并后的 CLAUDE.md 失败: {e}");
            } else if let Err(e) =
                crate::sync::claude_md::write_file_if_changed(&merged.content).await
            {
                tracing::warn!("写回 CLAUDE.md 文件失败: {e}");
            }
        }
    }

    // 6. 重读本地（pull 合并可能已改 DB）
    let local_after = state
        .claude_md_repo
        .get()
        .await
        .map_err(|e| format!("重读本地 CLAUDE.md 失败: {e}"))?;
    let local_after_row = local_after.unwrap_or_else(|| ClaudeMdRow {
        id: CLAUDE_MD_ID.to_string(),
        content: String::new(),
        updated_at: String::new(),
        device_id: state.device_id.as_str().to_string(),
        vector_clock: HashMap::new(),
    });

    // 7. 决策是否 push：对端无数据且本地非空；或本地相对对端领先/并发。
    let remote_vc = remote
        .as_ref()
        .map(|r| r.vector_clock.clone())
        .unwrap_or_default();
    let relation = compare(&local_after_row.vector_clock, &remote_vc);
    let need_push = (remote.is_none() && !local_after_row.content.is_empty())
        || matches!(relation, ClockOrder::After | ClockOrder::Concurrent);

    if need_push {
        if let Err(e) = state
            .peer_client
            .claude_md_push(&base_url, &local_after_row)
            .await
        {
            tracing::warn!("向 {} claude_md_push 失败: {e}", device.name);
        }
    }

    Ok(())
}
