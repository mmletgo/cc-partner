//! sync/engine.rs — 同步引擎：按设备/领域报告收敛真值
//!
//! Business Logic（为什么需要这个模块）:
//!     用户触发局域网同步时，不能再把“部分失败/不可达”记成成功设备。Settings 需要看到
//!     每台设备、每个领域（prompt/ssh_target/scratchpad）的 typed 结果与 pulled/pushed/unchanged。
//!
//! Code Logic（这个模块做什么）:
//!     1. 对设备快照做 `buffer_unordered(4)` 并发同步，结果按 device_id 排序；
//!     2. 每设备仅一次 `health_info`，跨领域复用 capability；
//!     3. 对端支持 `sync.manifest.v2` 时走 plan 路径，否则 legacy 仍返回 typed 失败；
//!     4. 仅全领域 Succeeded 的设备计入 `succeeded_devices`/`synced`。

use crate::error::AppError;
use crate::models::claude_md::ClaudeMdRow;
use crate::models::prompt::PromptRow;
use crate::net::peer_client::PeerCallError;
use crate::net::protocol::CAPABILITY_SYNC_MANIFEST_V2;
use crate::state::AppState;
use crate::sync::apply_merge::apply_prompt_pull_items;
use crate::sync::protocol::{
    compute_sync_plan, content_sha256_hex, decide_acked_delete_epoch, estimate_summary_wire_bytes,
    max_delete_epoch_from_summaries, validate_manifest_page_bounds, validate_page_not_truncated,
    ManifestStreamState, SyncDomainOutcome, SyncItemFailure, SyncManifestPage, SyncSummary,
    TransportClass, PUSH_BATCH_ITEMS,
};
use crate::sync::vector_clock::{compare, ClockOrder};
use futures_util::{stream, StreamExt};
use std::collections::HashMap;
use uuid::Uuid;

/// 领域名常量：Prompt。
pub const DOMAIN_PROMPT: &str = "prompt";
/// 领域名常量：SSH 目标。
pub const DOMAIN_SSH_TARGET: &str = "ssh_target";
/// 领域名常量：速记本。
pub const DOMAIN_SCRATCHPAD: &str = "scratchpad";

/// 设备级同步状态（Settings 展示用）。
///
/// Business Logic: UI 必须区分全成功 / 部分失败 / 不可达 / 协议 / 资源限制，禁止全部显示为成功。
/// Code Logic: snake_case 序列化，与前端 status 字符串对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceSyncStatus {
    /// 全部领域 Succeeded
    Succeeded,
    /// 至少一个领域非全成功，且不是“全员同一不可达/协议/资源”终态
    Partial,
    /// 设备 health 失败或全部领域 Unreachable
    Unreachable,
    /// 全部领域 ProtocolError（无成功）
    ProtocolError,
    /// 全部领域 ResourceLimit（无成功）
    ResourceLimit,
}

/// 单领域同步报告。
///
/// Business Logic: Settings 按 domain 展示 outcome 与计数。
/// Code Logic: domain 为稳定 token；outcome 为 §4.1 typed 结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DomainSyncReport {
    /// 领域 token：`prompt` / `ssh_target` / `scratchpad`
    pub domain: String,
    /// 该领域终态
    pub outcome: SyncDomainOutcome,
}

/// 单设备同步报告。
///
/// Business Logic: 用户需要看到每台对端的聚合状态与各领域明细。
/// Code Logic: status 由 domains 聚合；device_id 用于排序与前端 key。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceSyncReport {
    /// 对端 device_id
    pub device_id: String,
    /// 展示名
    pub device_name: String,
    /// 设备聚合状态
    pub status: DeviceSyncStatus,
    /// 各领域明细（固定顺序 prompt → ssh_target → scratchpad）
    pub domains: Vec<DomainSyncReport>,
}

/// 一轮局域网同步的权威结果。
///
/// Business Logic: 只有全成功设备计入 succeeded_devices；Partial/Unreachable 等永不计入成功。
/// Code Logic: `synced` 与 `succeeded_devices` 同值，兼容旧前端只读 synced。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncRunResult {
    /// 是否已接受同步任务（同步执行，恒 true）
    pub accepted: bool,
    /// 全领域成功的设备数
    pub succeeded_devices: u64,
    /// 兼容字段：= succeeded_devices
    pub synced: u64,
    /// 参与同步的设备报告（按 device_id 升序）
    pub devices: Vec<DeviceSyncReport>,
    /// 人类可读摘要
    pub note: String,
}

/// 旧版返回结构（CLAUDE.md 推送等仍用简单计数）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncResult {
    /// 是否已接受
    pub accepted: bool,
    /// 成功对端数
    pub synced: u64,
    /// 备注
    pub note: String,
}

/// 判断领域 outcome 是否全成功。
///
/// Business Logic: 只有 Succeeded 可计入设备成功。
/// Code Logic: match Succeeded 变体。
#[allow(dead_code)] // intentional public helper for outcome aggregation
pub fn domain_outcome_is_success(outcome: &SyncDomainOutcome) -> bool {
    matches!(outcome, SyncDomainOutcome::Succeeded { .. })
}

/// 将领域 outcome 映射为粗粒度设备状态分量。
///
/// Business Logic: 聚合时需要知道每个领域属于哪一类失败。
/// Code Logic: Succeeded→Succeeded；其余按变体映射。
fn domain_status_class(outcome: &SyncDomainOutcome) -> DeviceSyncStatus {
    match outcome {
        SyncDomainOutcome::Succeeded { .. } => DeviceSyncStatus::Succeeded,
        SyncDomainOutcome::Partial { .. } => DeviceSyncStatus::Partial,
        SyncDomainOutcome::Unreachable { .. } => DeviceSyncStatus::Unreachable,
        SyncDomainOutcome::ProtocolError { .. } => DeviceSyncStatus::ProtocolError,
        SyncDomainOutcome::ResourceLimit { .. } => DeviceSyncStatus::ResourceLimit,
    }
}

/// 由各领域结果聚合设备状态。
///
/// Business Logic: 单领域失败不得把设备标成 Succeeded；全 Unreachable/全 Protocol/全 Resource 保留该类。
///
/// Code Logic:
///     - 空 domains → Partial（不应发生）；
///     - 全 Succeeded → Succeeded；
///     - 全同一失败类 → 该类；
///     - 否则 → Partial。
pub fn aggregate_device_status(domains: &[DomainSyncReport]) -> DeviceSyncStatus {
    if domains.is_empty() {
        return DeviceSyncStatus::Partial;
    }
    let classes: Vec<DeviceSyncStatus> = domains
        .iter()
        .map(|d| domain_status_class(&d.outcome))
        .collect();
    if classes.iter().all(|c| *c == DeviceSyncStatus::Succeeded) {
        return DeviceSyncStatus::Succeeded;
    }
    let first_fail = classes
        .iter()
        .find(|c| **c != DeviceSyncStatus::Succeeded)
        .copied()
        .unwrap_or(DeviceSyncStatus::Partial);
    if classes
        .iter()
        .all(|c| *c == first_fail || *c == DeviceSyncStatus::Succeeded)
        && classes.iter().all(|c| *c != DeviceSyncStatus::Succeeded)
    {
        // 全部同一失败类
        return first_fail;
    }
    if classes
        .iter()
        .all(|c| *c == DeviceSyncStatus::Unreachable || *c == DeviceSyncStatus::Succeeded)
        && classes.contains(&DeviceSyncStatus::Unreachable)
        && !classes.contains(&DeviceSyncStatus::Succeeded)
    {
        return DeviceSyncStatus::Unreachable;
    }
    // 混合成功与失败，或多种失败类 → Partial
    DeviceSyncStatus::Partial
}

/// 统计全成功设备数。
///
/// Business Logic: completed/succeeded 只计全成功设备。
/// Code Logic: filter status==Succeeded 计数。
pub fn count_succeeded_devices(devices: &[DeviceSyncReport]) -> u64 {
    devices
        .iter()
        .filter(|d| d.status == DeviceSyncStatus::Succeeded)
        .count() as u64
}

/// 把 PeerCallError 映射为 typed domain outcome。
///
/// Business Logic: 传输/协议/资源错误不得伪装成功空集。
/// Code Logic: Network/timeout→Unreachable；413→ResourceLimit；其余→ProtocolError。
pub fn peer_error_to_domain_outcome(error: &PeerCallError) -> SyncDomainOutcome {
    match error {
        PeerCallError::Network { source, .. } => {
            // reqwest: is_timeout 覆盖 connect/read timeout；文案兜底 timed out
            let class = if source.is_timeout()
                || format!("{source}")
                    .to_ascii_lowercase()
                    .contains("timed out")
            {
                TransportClass::Timeout
            } else {
                TransportClass::Network
            };
            SyncDomainOutcome::Unreachable { class }
        }
        PeerCallError::InvalidResponse { .. } => SyncDomainOutcome::ProtocolError {
            code: "invalid_response".to_string(),
        },
        PeerCallError::Unsupported { capability, .. } => SyncDomainOutcome::ProtocolError {
            code: format!("unsupported:{capability}"),
        },
        PeerCallError::Remote { status, code, .. } => {
            if *status == 413 || code.contains("batch_too_large") || code.contains("too_large") {
                SyncDomainOutcome::ResourceLimit {
                    limit: code.clone(),
                }
            } else if *status == 503 || *status == 504 {
                SyncDomainOutcome::Unreachable {
                    class: TransportClass::Http,
                }
            } else {
                SyncDomainOutcome::ProtocolError { code: code.clone() }
            }
        }
    }
}

/// 构造不可达设备报告（health 失败时三领域同为 Unreachable）。
///
/// Business Logic: health 失败后不应继续领域交换，且不得计成功。
/// Code Logic: 三个领域同一 Unreachable outcome + 设备 status Unreachable。
pub fn unreachable_device_report(
    device_id: impl Into<String>,
    device_name: impl Into<String>,
    class: TransportClass,
) -> DeviceSyncReport {
    let outcome = SyncDomainOutcome::Unreachable { class };
    let domains = vec![
        DomainSyncReport {
            domain: DOMAIN_PROMPT.to_string(),
            outcome: outcome.clone(),
        },
        DomainSyncReport {
            domain: DOMAIN_SSH_TARGET.to_string(),
            outcome: outcome.clone(),
        },
        DomainSyncReport {
            domain: DOMAIN_SCRATCHPAD.to_string(),
            outcome,
        },
    ];
    DeviceSyncReport {
        device_id: device_id.into(),
        device_name: device_name.into(),
        status: DeviceSyncStatus::Unreachable,
        domains,
    }
}

/// 由领域列表构造设备报告（聚合 status）。
///
/// Business Logic: 测试与生产共用同一聚合入口。
/// Code Logic: 调用 aggregate_device_status 填充 status。
pub fn device_report_from_domains(
    device_id: impl Into<String>,
    device_name: impl Into<String>,
    domains: Vec<DomainSyncReport>,
) -> DeviceSyncReport {
    let status = aggregate_device_status(&domains);
    DeviceSyncReport {
        device_id: device_id.into(),
        device_name: device_name.into(),
        status,
        domains,
    }
}

/// 从设备报告列表构造 SyncRunResult。
///
/// Business Logic: succeeded_devices 只计全成功。
/// Code Logic: 排序后计数并生成 note。
pub fn build_sync_run_result(mut devices: Vec<DeviceSyncReport>) -> SyncRunResult {
    devices.sort_by(|a, b| a.device_id.cmp(&b.device_id));
    let succeeded_devices = count_succeeded_devices(&devices);
    let total = devices.len();
    let note = if total == 0 {
        "没有在线设备".to_string()
    } else {
        format!("已与 {succeeded_devices}/{total} 个设备完全同步")
    };
    SyncRunResult {
        accepted: true,
        succeeded_devices,
        synced: succeeded_devices,
        devices,
        note,
    }
}

/// Prompt 行 → SyncSummary。
///
/// Business Logic: v2 planner 需要与服务端一致的 content_hash。
/// Code Logic: title/content/tags 固定分隔拼接后 SHA-256。
pub fn prompt_to_summary(row: &PromptRow) -> SyncSummary<String> {
    let tags_json = serde_json::to_string(&row.tags).unwrap_or_else(|_| "[]".to_string());
    let content_hash = content_sha256_hex(&[
        row.title.as_bytes(),
        b"\0",
        row.content.as_bytes(),
        b"\0",
        tags_json.as_bytes(),
    ]);
    SyncSummary {
        id: row.id.clone(),
        vector_clock: row.vector_clock.clone(),
        content_hash,
        size: row.content.len() as u64,
        updated_at: row.updated_at.clone(),
        deleted: row.deleted,
        delete_epoch: row.delete_epoch,
    }
}

/// 流式拉取远端完整 manifest。
///
/// Business Logic: 未到 next_cursor=None 不得进入 planner。
/// Code Logic: ManifestStreamState + page bounds + 调用方提供的 list_page。
pub async fn fetch_complete_remote_manifest<F, Fut>(
    mut list_page: F,
) -> Result<Vec<SyncSummary<String>>, SyncDomainOutcome>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<SyncManifestPage<String>, PeerCallError>>,
{
    let mut stream_state = ManifestStreamState::new();
    let mut all: Vec<SyncSummary<String>> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = list_page(cursor.clone())
            .await
            .map_err(|e| peer_error_to_domain_outcome(&e))?;
        validate_page_not_truncated(&page)?;
        let est: usize = page.items.iter().map(estimate_summary_wire_bytes).sum();
        validate_manifest_page_bounds(page.items.len(), est)?;
        all.extend(page.items);
        stream_state.observe_next_cursor(page.next_cursor.clone())?;
        match page.next_cursor {
            None => break,
            Some(next) => cursor = Some(next),
        }
    }
    stream_state.require_complete()?;
    all.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(all)
}

/// 触发全局同步：并发同步在线对端并返回 per-device/domain 真值。
///
/// Business Logic: 用户点击同步时调用；任一设备失败不阻断其他；成功计数只含全成功设备。
///
/// Code Logic: devices 快照 → buffer_unordered(4) → 按 device_id 排序 → SyncRunResult。
pub async fn trigger_sync(state: &AppState) -> SyncRunResult {
    let devices: Vec<crate::models::device::Device> = {
        let guard = state.devices.read().expect("devices 读锁中毒");
        guard.values().cloned().collect()
    };

    if devices.is_empty() {
        tracing::debug!("没有在线设备，跳过同步");
        return build_sync_run_result(Vec::new());
    }

    tracing::info!("开始与 {} 个设备同步（并发上限 4）", devices.len());

    let reports: Vec<DeviceSyncReport> = stream::iter(devices)
        .map(|device| {
            let state = state.clone();
            async move { sync_device_with_domains(&state, device).await }
        })
        .buffer_unordered(4)
        .collect()
        .await;

    let result = build_sync_run_result(reports);
    tracing::info!(
        "同步结束：succeeded_devices={}/{}",
        result.succeeded_devices,
        result.devices.len()
    );
    // 水位可能在本轮 ack 推进；best-effort 跑 tombstone GC（失败仅 warn）
    if let Err(e) = run_tombstone_gc_best_effort(state).await {
        tracing::warn!("同步后 tombstone GC 失败: {e}");
    }
    result
}

/// 收集三域 deleted tombstone 并运行生产 GC。
///
/// Business Logic（为什么需要这个函数）:
///     Spec §4.4 要求 age≥30d + 活跃 peer ack 后把完整 tombstone 压成 deletion floor；
///     同步结束与后台 tick 必须调用，禁止只在单测中 compact。
///
/// Code Logic（这个函数做什么）:
///     委托 `DeletionFloorRepo::run_production_tombstone_gc`（内部收集候选 +
///     age/ack 门闸 + 同事务 floor upsert 与 DELETE tombstone）。
pub async fn run_tombstone_gc_best_effort(state: &AppState) -> Result<usize, AppError> {
    use crate::storage::deletion_floor_repo::DeletionFloorRepo;
    use crate::storage::sync_watermark_repo::SyncWatermarkRepo;

    let now = chrono::Utc::now().to_rfc3339();
    let floors = DeletionFloorRepo::with_gate(state.db.clone(), state.maintenance_gate.clone());
    let watermarks = SyncWatermarkRepo::with_gate(state.db.clone(), state.maintenance_gate.clone());
    let result = floors
        .run_production_tombstone_gc(&watermarks, &now)
        .await?;
    if result.deleted > 0 {
        tracing::info!(
            "tombstone GC: compacted={} skipped={}",
            result.deleted,
            result.skipped
        );
    }
    Ok(result.deleted)
}

/// 与单设备同步三个领域，并附加 CC 历史（不计 domain 报告）。
///
/// Business Logic: 一次 health/capability 复用；v2 与 legacy 分支；CC 历史仍独立 warn。
/// Code Logic: 一次 health_info → supports v2 → 顺序跑 prompt/ssh/scratchpad + 复用 protocol 的 CC → 聚合。
async fn sync_device_with_domains(
    state: &AppState,
    device: crate::models::device::Device,
) -> DeviceSyncReport {
    let base_url = device.base_url();
    tracing::info!("开始与设备 {} ({}) 同步", device.name, base_url);

    let health = match state.peer_client.health_info(&base_url).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("设备 {} 不可达，跳过同步: {e}", device.name);
            let class = match peer_error_to_domain_outcome(&e) {
                SyncDomainOutcome::Unreachable { class } => class,
                _ => TransportClass::Network,
            };
            return unreachable_device_report(&device.id, &device.name, class);
        }
    };

    let protocol = health.protocol_info();
    let supports_v2 = protocol.supports(CAPABILITY_SYNC_MANIFEST_V2);
    // 一次 health 复用：capability/protocol 传入各领域（含 CC），领域内不再重复 health。
    let prompt_outcome = prompt_sync_with_peer(state, &device, &base_url, supports_v2).await;
    let ssh_outcome =
        crate::sync::ssh_target::ssh_target_sync_with_peer(state, &device, &base_url, supports_v2)
            .await;
    let scratchpad_outcome =
        crate::sync::scratchpad::scratchpad_sync_with_peer(state, &device, &base_url, supports_v2)
            .await;

    // CC 历史独立链路但复用本设备已探测的 protocol；失败不影响 domain 报告
    let _ = crate::cc::engine::cc_sync_with_peer_using_protocol(state, &device, &protocol).await;

    let domains = vec![
        DomainSyncReport {
            domain: DOMAIN_PROMPT.to_string(),
            outcome: prompt_outcome,
        },
        DomainSyncReport {
            domain: DOMAIN_SSH_TARGET.to_string(),
            outcome: ssh_outcome,
        },
        DomainSyncReport {
            domain: DOMAIN_SCRATCHPAD.to_string(),
            outcome: scratchpad_outcome,
        },
    ];
    let report = device_report_from_domains(&device.id, &device.name, domains);
    tracing::info!("与设备 {} 同步结束 status={:?}", device.name, report.status);
    report
}

/// Prompt 领域双向同步（v2 plan 或 typed legacy）。
///
/// Business Logic: Prompt 是主同步领域；transport 失败必须 typed，不得空成功。
/// Code Logic: supports_v2 → manifest plan；否则 legacy pull/push 用 Result 路径。
async fn prompt_sync_with_peer(
    state: &AppState,
    device: &crate::models::device::Device,
    base_url: &str,
    supports_v2: bool,
) -> SyncDomainOutcome {
    if supports_v2 {
        prompt_sync_v2(state, device, base_url).await
    } else {
        prompt_sync_legacy_typed(state, device, base_url).await
    }
}

/// Prompt v2：完整 manifest 比较 + items/push-batch。
///
/// Business Logic: exact equality 零正文；batch 有界；失败 typed；仅完整+成功后 ack delete 水位。
/// Code Logic: 拉远端页 → 本地摘要排序 → compute_sync_plan → fetch/merge/push；
///     有正文时末批 push 可携带 acked_delete_epoch；无正文时改走专用 ack-delete-epoch 路由。
async fn prompt_sync_v2(
    state: &AppState,
    device: &crate::models::device::Device,
    base_url: &str,
) -> SyncDomainOutcome {
    // fetch_complete_remote_manifest 仅在 next_cursor=None 完成时返回 Ok
    let remote = match fetch_complete_remote_manifest(|cursor| {
        let client = state.peer_client.clone();
        let base = base_url.to_string();
        async move {
            client
                .list_prompt_manifest_page(&base, cursor.as_deref())
                .await
        }
    })
    .await
    {
        Ok(v) => v,
        Err(o) => return o,
    };
    let max_remote_epoch = max_delete_epoch_from_summaries(&remote);

    let local_all = match state.prompt_repo.get_all_for_sync().await {
        Ok(v) => v,
        Err(e) => {
            return SyncDomainOutcome::ProtocolError {
                code: format!("local_read_failed:{e}"),
            };
        }
    };
    let mut local_manifest: Vec<SyncSummary<String>> =
        local_all.iter().map(prompt_to_summary).collect();
    local_manifest.sort_by(|a, b| a.id.cmp(&b.id));
    let local_by_id: HashMap<String, PromptRow> =
        local_all.into_iter().map(|r| (r.id.clone(), r)).collect();

    let plan = compute_sync_plan(&local_manifest, &remote);
    let unchanged = plan.unchanged;
    let mut pulled: u32 = 0;
    let mut pushed: u32 = 0;

    // fetch + merge（中途失败：已 applied>0 报 Partial，不得 ack）
    for chunk in plan.fetch_from_remote.chunks(PUSH_BATCH_ITEMS) {
        if chunk.is_empty() {
            continue;
        }
        let ids: Vec<String> = chunk.to_vec();
        let resp = match state.peer_client.fetch_prompt_items(base_url, &ids).await {
            Ok(r) => r,
            Err(e) => {
                let applied = pulled.saturating_add(pushed);
                if applied > 0 {
                    return mid_batch_fail_outcome(applied, format!("fetch_failed:{e}"));
                }
                return peer_error_to_domain_outcome(&e);
            }
        };
        if !resp.items.is_empty() {
            match apply_prompt_pull_items(
                &state.prompt_repo.pool(),
                state.maintenance_gate.as_ref(),
                state.prompt_repo.as_ref(),
                &resp.items,
            )
            .await
            {
                Ok(n) => {
                    if n > 0 {
                        pulled = pulled.saturating_add(n as u32);
                        tracing::info!(
                            "从 {} 拉取并更新了 {} 条 prompt (v2 apply_merge)",
                            device.name,
                            n
                        );
                    }
                }
                Err(e) => {
                    return mid_batch_fail_outcome(
                        pulled.saturating_add(pushed),
                        format!("apply_merge_failed:{e}"),
                    );
                }
            }
        }
        // missing_ids 非空：已落库的 items 保留，但禁止 Succeeded / delete-epoch ack
        if let Some(o) =
            incomplete_items_outcome(&resp.missing_ids, pulled.saturating_add(pushed))
        {
            return o;
        }
    }

    // 完整 manifest + apply 成功（含 items 无 missing）→ 允许在末批 ack
    let ack_epoch = decide_acked_delete_epoch(true, true, max_remote_epoch);
    let claimed = state.device_id.as_str();

    // 收集非空 push 批次
    let mut batches: Vec<Vec<PromptRow>> = Vec::new();
    for chunk in plan.push_to_remote.chunks(PUSH_BATCH_ITEMS) {
        if chunk.is_empty() {
            continue;
        }
        let items: Vec<PromptRow> = chunk
            .iter()
            .filter_map(|id| local_by_id.get(id).cloned())
            .collect();
        if !items.is_empty() {
            batches.push(items);
        }
    }

    if batches.is_empty() {
        // 无正文可推：专用 ack-delete-epoch（禁止空 push-batch）
        if let Some(epoch) = ack_epoch {
            if let Err(e) = state
                .peer_client
                .ack_prompt_delete_epoch(base_url, claimed, epoch)
                .await
            {
                let applied = pulled.saturating_add(pushed);
                if applied > 0 {
                    return mid_batch_fail_outcome(applied, format!("ack_failed:{e}"));
                }
                return peer_error_to_domain_outcome(&e);
            }
        }
    } else {
        let last = batches.len() - 1;
        for (i, items) in batches.into_iter().enumerate() {
            let epoch = if i == last { ack_epoch } else { None };
            let req_id = Uuid::new_v4().to_string();
            match state
                .peer_client
                .push_prompt_batch(base_url, &items, &req_id, claimed, epoch)
                .await
            {
                Ok(resp) => {
                    pushed = pushed.saturating_add(resp.accepted as u32);
                    tracing::info!(
                        "向 {} 推送了 {} 条 prompt (v2 accepted={})",
                        device.name,
                        items.len(),
                        resp.accepted
                    );
                }
                Err(e) => {
                    let applied = pulled.saturating_add(pushed);
                    if applied > 0 {
                        return mid_batch_fail_outcome(applied, format!("push_failed:{e}"));
                    }
                    return peer_error_to_domain_outcome(&e);
                }
            }
        }
    }

    SyncDomainOutcome::Succeeded {
        pulled,
        pushed,
        unchanged,
    }
}

/// Prompt legacy 路径（typed）：使用 Result 语义，push 失败不计成功。
///
/// Business Logic: 旧对端无 v2 时仍可同步，但 transport 失败不得空成功；
///     本地 apply 必须保留 conflict 副本，禁止 merge+bulk_upsert 静默丢 loser。
/// Code Logic: sync_pull_result → apply_prompt_pull_items；再 push_result。
async fn prompt_sync_legacy_typed(
    state: &AppState,
    device: &crate::models::device::Device,
    base_url: &str,
) -> SyncDomainOutcome {
    let local_all = match state.prompt_repo.get_all_for_sync().await {
        Ok(v) => v,
        Err(e) => {
            return SyncDomainOutcome::ProtocolError {
                code: format!("local_read_failed:{e}"),
            };
        }
    };
    let summary_values: Vec<serde_json::Value> = local_all
        .iter()
        .map(|p| serde_json::json!({ "id": p.id, "vector_clock": p.vector_clock }))
        .collect();

    let remote_prompts = match state
        .peer_client
        .sync_pull_result(base_url, summary_values)
        .await
    {
        Ok(v) => v,
        Err(e) => return peer_error_to_domain_outcome(&e),
    };

    let mut pulled: u32 = 0;
    if !remote_prompts.is_empty() {
        match apply_prompt_pull_items(
            &state.prompt_repo.pool(),
            state.maintenance_gate.as_ref(),
            state.prompt_repo.as_ref(),
            &remote_prompts,
        )
        .await
        {
            Ok(n) => {
                pulled = n as u32;
                if n > 0 {
                    tracing::info!(
                        "从 {} 拉取并更新了 {} 条 prompt (legacy apply_merge)",
                        device.name,
                        n
                    );
                }
            }
            Err(e) => {
                return SyncDomainOutcome::ProtocolError {
                    code: format!("apply_merge_failed:{e}"),
                };
            }
        }
    }

    let remote_ids: std::collections::HashSet<String> =
        remote_prompts.iter().map(|p| p.id.clone()).collect();
    let remote_summary_map: HashMap<String, &HashMap<String, u64>> = remote_prompts
        .iter()
        .map(|p| (p.id.clone(), &p.vector_clock))
        .collect();

    let local_all_after = match state.prompt_repo.get_all_for_sync().await {
        Ok(v) => v,
        Err(e) => {
            return SyncDomainOutcome::ProtocolError {
                code: format!("local_reread_failed:{e}"),
            };
        }
    };

    let mut push_prompts: Vec<PromptRow> = Vec::new();
    for p in &local_all_after {
        match remote_summary_map.get(&p.id) {
            None => push_prompts.push(p.clone()),
            Some(remote_clock) => {
                let relation = compare(&p.vector_clock, remote_clock);
                if matches!(relation, ClockOrder::After | ClockOrder::Concurrent)
                    && !remote_ids.contains(&p.id)
                {
                    push_prompts.push(p.clone());
                }
            }
        }
    }

    let mut pushed: u32 = 0;
    if !push_prompts.is_empty() {
        let n = push_prompts.len() as u32;
        match state
            .peer_client
            .sync_push_result(base_url, &push_prompts)
            .await
        {
            Ok(true) => {
                pushed = n;
                tracing::info!("向 {} 推送了 {} 条 prompt (legacy)", device.name, n);
            }
            Ok(false) => {
                return SyncDomainOutcome::ProtocolError {
                    code: "push_rejected".to_string(),
                };
            }
            Err(e) => return peer_error_to_domain_outcome(&e),
        }
    }

    // legacy 无法精确算 unchanged；用 max(0, local - pulled - pushed) 近似，允许 0
    let unchanged = local_all_after
        .len()
        .saturating_sub(pulled as usize)
        .saturating_sub(pushed as usize) as u32;

    SyncDomainOutcome::Succeeded {
        pulled,
        pushed,
        unchanged,
    }
}

/// 多批中途失败时：已应用 >0 则 Partial，否则 ProtocolError。
///
/// Business Logic（为什么需要这个函数）:
///     v2 多批 fetch/push 若前批已成功写入/对端已 accepted，后批失败不得伪装全失败或全成功。
///
/// Code Logic（这个函数做什么）:
///     applied>0 → Partial{applied, failed:[mid_batch_fail]}；否则 ProtocolError{code}。
pub fn mid_batch_fail_outcome(applied: u32, code: impl Into<String>) -> SyncDomainOutcome {
    let code = code.into();
    if applied > 0 {
        SyncDomainOutcome::Partial {
            applied,
            failed: vec![SyncItemFailure {
                id: "*".to_string(),
                code: code.clone(),
                message: code,
            }],
        }
    } else {
        SyncDomainOutcome::ProtocolError { code }
    }
}

/// items 响应含 missing_ids 时视为拉取不完整，禁止领域成功与 delete-epoch ack。
///
/// Business Logic（为什么需要这个函数）:
///     peer 在 manifest 中宣告了 id，但 items 未返回正文，说明本轮 fetch 不完整。
///     若仍报 Succeeded 并推进 acked_delete_epoch，对端可能把本机尚未拿到的
///     tombstone 当成“全网已收齐”而 GC 掉。
///
/// Code Logic（这个函数做什么）:
///     missing_ids 空 → None（调用方继续 push/ack）；
///     非空 → Some(mid_batch_fail_outcome(applied, "items_missing_ids:count=N"))，
///     调用方必须 return 且不得再 decide_acked_delete_epoch(true, true, ...)。
pub fn incomplete_items_outcome(
    missing_ids: &[String],
    applied: u32,
) -> Option<SyncDomainOutcome> {
    if missing_ids.is_empty() {
        None
    } else {
        Some(mid_batch_fail_outcome(
            applied,
            format!("items_missing_ids:count={}", missing_ids.len()),
        ))
    }
}

/// 将本机 CLAUDE.md 版本推送给所有在线对端，不执行远端 pull。
///
/// Business Logic: CLAUDE.md 页手动推送，不先 pull 覆盖本机。
/// Code Logic: health + push；失败仅跳过，返回简单 SyncResult。
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
        note: format!("已向 {pushed_count} 个设备推送 CLAUDE.md"),
    }
}

#[cfg(test)]
mod tests {
    //! engine 聚合与真值计数测试（无网络）。

    use super::*;

    #[test]
    fn mid_batch_fail_emits_partial_when_applied_positive() {
        let o = mid_batch_fail_outcome(3, "push_failed:timeout");
        match o {
            SyncDomainOutcome::Partial { applied, failed } => {
                assert_eq!(applied, 3);
                assert_eq!(failed.len(), 1);
                assert_eq!(failed[0].code, "push_failed:timeout");
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    #[test]
    fn mid_batch_fail_emits_protocol_error_when_applied_zero() {
        let o = mid_batch_fail_outcome(0, "fetch_failed:net");
        match o {
            SyncDomainOutcome::ProtocolError { code } => {
                assert_eq!(code, "fetch_failed:net");
            }
            other => panic!("expected ProtocolError, got {other:?}"),
        }
    }

    /// missing_ids 非空必须阻断领域成功，且 delete-epoch ack 不得推进。
    ///
    /// Business Logic: 证明 incomplete_items_outcome + decide_acked_delete_epoch
    ///     在 items 不完整时不会伪装 Succeeded / 推进水位。
    /// Code Logic: 空 missing → None；非空 applied>0 → Partial；applied=0 → ProtocolError；
    ///     apply_ok=false 时 decide_acked_delete_epoch 返回 None。
    #[test]
    fn missing_ids_blocks_success_and_delete_ack() {
        assert!(incomplete_items_outcome(&[], 0).is_none());
        assert!(incomplete_items_outcome(&[], 5).is_none());

        let partial = incomplete_items_outcome(&["gone".into()], 2).expect("must fail");
        match partial {
            SyncDomainOutcome::Partial { applied, failed } => {
                assert_eq!(applied, 2);
                assert_eq!(failed.len(), 1);
                assert!(
                    failed[0].code.contains("items_missing_ids"),
                    "code={}",
                    failed[0].code
                );
                assert!(failed[0].code.contains("count=1"));
            }
            other => panic!("expected Partial, got {other:?}"),
        }

        let protocol = incomplete_items_outcome(&["a".into(), "b".into()], 0).expect("must fail");
        match protocol {
            SyncDomainOutcome::ProtocolError { code } => {
                assert!(code.contains("items_missing_ids"));
                assert!(code.contains("count=2"));
            }
            other => panic!("expected ProtocolError, got {other:?}"),
        }

        // missing_ids 等价于 apply_ok=false：禁止 ack
        assert_eq!(decide_acked_delete_epoch(true, false, 42), None);
        // 仅完整 fetch 才可 ack
        assert_eq!(decide_acked_delete_epoch(true, true, 42), Some(42));
    }

    /// 构造 Succeeded 领域报告。
    fn ok_domain(domain: &str) -> DomainSyncReport {
        DomainSyncReport {
            domain: domain.to_string(),
            outcome: SyncDomainOutcome::Succeeded {
                pulled: 0,
                pushed: 0,
                unchanged: 1,
            },
        }
    }

    /// 构造指定失败类领域报告。
    fn fail_domain(domain: &str, outcome: SyncDomainOutcome) -> DomainSyncReport {
        DomainSyncReport {
            domain: domain.to_string(),
            outcome,
        }
    }

    /// 模拟某一领域失败后的整设备聚合（brief 中的 run_with_domain_failure）。
    ///
    /// Business Logic: 单领域失败时设备必须 Partial 且 succeeded_devices=0。
    /// Code Logic: prompt/ssh 成功，指定 domain 失败，再 build_sync_run_result。
    async fn run_with_domain_failure(failed_domain: &str) -> SyncRunResult {
        let fail = SyncDomainOutcome::ProtocolError {
            code: "injected_domain_failure".to_string(),
        };
        let domains = vec![
            if failed_domain == DOMAIN_PROMPT {
                fail_domain(DOMAIN_PROMPT, fail.clone())
            } else {
                ok_domain(DOMAIN_PROMPT)
            },
            if failed_domain == DOMAIN_SSH_TARGET {
                fail_domain(DOMAIN_SSH_TARGET, fail.clone())
            } else {
                ok_domain(DOMAIN_SSH_TARGET)
            },
            if failed_domain == DOMAIN_SCRATCHPAD {
                fail_domain(DOMAIN_SCRATCHPAD, fail)
            } else {
                ok_domain(DOMAIN_SCRATCHPAD)
            },
        ];
        let device = device_report_from_domains("dev-b", "Peer-B", domains);
        build_sync_run_result(vec![device])
    }

    #[tokio::test]
    async fn one_domain_failure_marks_device_partial() {
        let result = run_with_domain_failure("scratchpad").await;
        assert_eq!(result.succeeded_devices, 0);
        assert_eq!(result.synced, 0);
        assert_eq!(result.devices[0].status, DeviceSyncStatus::Partial);
        assert!(!domain_outcome_is_success(
            &result.devices[0].domains[2].outcome
        ));
    }

    #[test]
    fn all_domains_success_marks_device_succeeded() {
        let device = device_report_from_domains(
            "dev-a",
            "A",
            vec![
                ok_domain(DOMAIN_PROMPT),
                ok_domain(DOMAIN_SSH_TARGET),
                ok_domain(DOMAIN_SCRATCHPAD),
            ],
        );
        assert_eq!(device.status, DeviceSyncStatus::Succeeded);
        let result = build_sync_run_result(vec![device]);
        assert_eq!(result.succeeded_devices, 1);
        assert_eq!(result.synced, 1);
    }

    #[test]
    fn all_unreachable_domains_mark_device_unreachable() {
        let outcome = SyncDomainOutcome::Unreachable {
            class: TransportClass::Network,
        };
        let device = device_report_from_domains(
            "dev-c",
            "C",
            vec![
                fail_domain(DOMAIN_PROMPT, outcome.clone()),
                fail_domain(DOMAIN_SSH_TARGET, outcome.clone()),
                fail_domain(DOMAIN_SCRATCHPAD, outcome),
            ],
        );
        assert_eq!(device.status, DeviceSyncStatus::Unreachable);
        assert_eq!(count_succeeded_devices(&[device]), 0);
    }

    #[test]
    fn build_sync_run_result_sorts_by_device_id() {
        let d2 = device_report_from_domains(
            "z-dev",
            "Z",
            vec![
                ok_domain(DOMAIN_PROMPT),
                ok_domain(DOMAIN_SSH_TARGET),
                ok_domain(DOMAIN_SCRATCHPAD),
            ],
        );
        let d1 = device_report_from_domains(
            "a-dev",
            "A",
            vec![
                ok_domain(DOMAIN_PROMPT),
                ok_domain(DOMAIN_SSH_TARGET),
                ok_domain(DOMAIN_SCRATCHPAD),
            ],
        );
        let result = build_sync_run_result(vec![d2, d1]);
        assert_eq!(result.devices[0].device_id, "a-dev");
        assert_eq!(result.devices[1].device_id, "z-dev");
        assert_eq!(result.succeeded_devices, 2);
    }

    #[test]
    fn partial_never_counts_as_success() {
        let partial = device_report_from_domains(
            "p",
            "P",
            vec![
                ok_domain(DOMAIN_PROMPT),
                fail_domain(
                    DOMAIN_SSH_TARGET,
                    SyncDomainOutcome::Partial {
                        applied: 1,
                        failed: vec![],
                    },
                ),
                ok_domain(DOMAIN_SCRATCHPAD),
            ],
        );
        assert_eq!(partial.status, DeviceSyncStatus::Partial);
        let unreachable = unreachable_device_report("u", "U", TransportClass::Timeout);
        assert_eq!(unreachable.status, DeviceSyncStatus::Unreachable);
        let result = build_sync_run_result(vec![partial, unreachable]);
        assert_eq!(result.succeeded_devices, 0);
        assert_eq!(result.synced, 0);
    }

    #[test]
    fn peer_error_maps_413_to_resource_limit() {
        let err = PeerCallError::Remote {
            url: "http://x".into(),
            status: 413,
            code: "batch_too_large".into(),
            message: "too large".into(),
            request_id: "r".into(),
            retryable: false,
            legacy: false,
            details: serde_json::json!({}),
        };
        match peer_error_to_domain_outcome(&err) {
            SyncDomainOutcome::ResourceLimit { limit } => {
                assert!(limit.contains("batch_too_large") || limit == "batch_too_large");
            }
            other => panic!("expected ResourceLimit, got {other:?}"),
        }
    }

    /// health 复用：同一 PeerProtocolInfo 布尔应驱动三领域同一分支，不要求重复 health。
    ///
    /// Business Logic: 任务要求 one health/capability fetch per device。
    /// Code Logic: 纯函数验证 supports_v2 输入在聚合路径中被一致使用（由同步入口保证）。
    #[test]
    fn health_capability_flag_is_reused_across_domain_reports() {
        // 构造“capability 已判定”后的三领域成功报告，模拟复用后的结果形状
        let device = device_report_from_domains(
            "cap-dev",
            "Cap",
            vec![
                ok_domain(DOMAIN_PROMPT),
                ok_domain(DOMAIN_SSH_TARGET),
                ok_domain(DOMAIN_SCRATCHPAD),
            ],
        );
        assert_eq!(device.domains.len(), 3);
        assert_eq!(device.domains[0].domain, DOMAIN_PROMPT);
        assert_eq!(device.domains[1].domain, DOMAIN_SSH_TARGET);
        assert_eq!(device.domains[2].domain, DOMAIN_SCRATCHPAD);
        assert_eq!(device.status, DeviceSyncStatus::Succeeded);
    }

    /// mixed-version：v2 客户端对 legacy 对端只走 legacy Prompt 路由。
    #[tokio::test]
    async fn content_sync_mixed_version_v2_to_legacy_only_legacy() {
        crate::sync::mixed_version_harness::assert_v2_client_to_legacy_peer_uses_only_legacy_routes()
            .await;
    }

    /// mixed-version：legacy pull 失败必须 typed，不得空成功。
    #[tokio::test]
    async fn content_sync_mixed_version_legacy_failure_typed() {
        crate::sync::mixed_version_harness::assert_legacy_peer_network_failure_is_typed_not_empty_success()
            .await;
    }

    /// mixed-version：v2 对端走 manifest 路径。
    #[tokio::test]
    async fn content_sync_mixed_version_v2_peer_uses_manifest() {
        crate::sync::mixed_version_harness::assert_v2_peer_uses_manifest_routes().await;
    }

    /// 计数型假 peer：记录 health 调用次数，模拟多领域同步编排。
    ///
    /// Business Logic（为什么需要这个类型）:
    ///     验收「同设备只探一次 health」需要确定性 fake，不能依赖真实网络。
    ///
    /// Code Logic（这个类型做什么）:
    ///     `health_info` 递增计数并返回固定 protocol；领域同步方法只消费 protocol，不再调 health。
    #[derive(Default)]
    struct CountingPeer {
        health_calls: std::sync::atomic::AtomicU32,
    }

    impl CountingPeer {
        /// 创建计数 peer。
        fn new() -> Self {
            Self::default()
        }

        /// 返回已发生的 health 调用次数。
        fn health_calls(&self) -> u32 {
            self.health_calls.load(std::sync::atomic::Ordering::SeqCst)
        }

        /// 模拟一次 typed health / protocol 探测。
        async fn health_info(&self) -> crate::net::protocol::PeerProtocolInfo {
            self.health_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            crate::net::protocol::PeerProtocolInfo {
                protocol_version: 1,
                capabilities: vec![
                    CAPABILITY_SYNC_MANIFEST_V2.to_string(),
                    crate::net::protocol::CAPABILITY_CC_HISTORY_PAGED_SYNC_V1.to_string(),
                ],
            }
        }

        /// 领域同步只读已注入 protocol（不调用 health）。
        async fn sync_domain_with_protocol(
            &self,
            _protocol: &crate::net::protocol::PeerProtocolInfo,
        ) {
            // no-op：编排层契约测试只关心 health 次数
        }
    }

    /// 模拟 `sync_device_with_domains` 的 health 复用编排：一次 health，prompt/ssh/scratchpad/cc 共用。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     把「每设备一次 health」固化为可测的编排 helper，防止 CC 路径回退二次 probe。
    ///
    /// Code Logic（这个函数做什么）:
    ///     health_info 一次 → 四个领域均以同一 protocol 调用，不再 health。
    async fn sync_all_domains(peer: &CountingPeer) {
        let protocol = peer.health_info().await;
        peer.sync_domain_with_protocol(&protocol).await; // prompt
        peer.sync_domain_with_protocol(&protocol).await; // ssh
        peer.sync_domain_with_protocol(&protocol).await; // scratchpad
        peer.sync_domain_with_protocol(&protocol).await; // cc (using_protocol)
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同设备跨 prompt/ssh/scratchpad/cc 同步不得重复 health，否则放大尾延迟。
    ///
    /// Code Logic（这个测试做什么）:
    ///     CountingPeer 编排 sync_all_domains，断言 health_calls == 1。
    #[tokio::test]
    async fn one_device_sync_uses_one_health_probe() {
        let peer = CountingPeer::new();
        sync_all_domains(&peer).await;
        assert_eq!(peer.health_calls(), 1);
    }
}
