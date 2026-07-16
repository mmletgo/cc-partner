//! workbench/lan_fleet/collector — owner-local 与控制设备聚合
//!
//! Business Logic（为什么需要这个模块）:
//!     Fleet 需要从 Agent runtime、Attention、terminal、Git、browser、Orchestrator
//!     读取同一时间窗口摘要；单 field 失败不得拖垮整个 device。
//!
//! Code Logic（这个模块做什么）:
//!     纯函数计数/映射 + 基于 AppState 的异步构建；全局 fan-out 读 shortcut、
//!     capability 门控、semaphore=3、5s timeout、display cache 合并。

use super::cache::SharedFleetDisplayCache;
use super::models::{
    AgentPhaseCounts, FleetBrowserState, FleetFreshness, FleetGitState, FleetReachability,
    LanFleetDeviceSummary, LanFleetOwnerBatchReq, LanFleetOwnerBatchResp, LanFleetProjectSummary,
    LanFleetSnapshot, FLEET_DEVICE_TIMEOUT_SECS, FLEET_FANOUT_MAX_CONCURRENCY,
    FLEET_OWNER_BATCH_MAX_PROJECTS, FLEET_SNAPSHOT_MAX_PROJECTS,
};
use crate::error::AppError;
use crate::models::device::Device;
use crate::net::protocol::CAPABILITY_WORKBENCH_LAN_FLEET_V1;
use crate::orchestrator::repo::OrchestratorRepo;
use crate::state::AppState;
use crate::workbench::agent_runtime::models::{AgentSessionPhase, AgentSessionRuntime};
use crate::workbench::models::{WorkbenchGitStatusDto, WorkbenchProjectRow};
use crate::workbench::remote_client::RemoteWorkbenchClient;
use crate::workbench::remote_ids::{is_remote_id, parse_remote_entity_id, remote_project_id};
use chrono::Utc;
use futures_util::stream::{self, StreamExt};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

/// 从 active Agent sessions 统计 phase 计数。
///
/// Business Logic（为什么需要这个函数）:
///     Fleet 与测试需要同一套 phase 汇总，不依赖 UI 拼装。
///
/// Code Logic（这个函数做什么）:
///     遍历 sessions，按 AgentSessionPhase 递增对应计数。
pub fn count_agent_phases(sessions: &[AgentSessionRuntime]) -> AgentPhaseCounts {
    let mut counts = AgentPhaseCounts::default();
    for session in sessions {
        match session.phase {
            AgentSessionPhase::Launching => counts.launching = counts.launching.saturating_add(1),
            AgentSessionPhase::Working => counts.working = counts.working.saturating_add(1),
            AgentSessionPhase::NeedsInput => {
                counts.needs_input = counts.needs_input.saturating_add(1)
            }
            AgentSessionPhase::Idle => counts.idle = counts.idle.saturating_add(1),
            AgentSessionPhase::Completed => counts.completed = counts.completed.saturating_add(1),
            AgentSessionPhase::Failed => counts.failed = counts.failed.saturating_add(1),
            AgentSessionPhase::Disconnected => {
                counts.disconnected = counts.disconnected.saturating_add(1)
            }
        }
    }
    counts
}

/// 将 Git status 结果映射为 FleetGitState。
///
/// Business Logic（为什么需要这个函数）:
///     Git 子源失败只能 field-level unknown，不能把整个 device 标 offline。
///
/// Code Logic（这个函数做什么）:
///     Err → Unknown；conflicts>0 → Conflict；!clean 或 changed>0 → Dirty；否则 Clean。
pub fn map_git_status(result: Result<WorkbenchGitStatusDto, AppError>) -> FleetGitState {
    match result {
        Err(_) => FleetGitState::Unknown,
        Ok(status) if status.conflicts > 0 => FleetGitState::Conflict,
        Ok(status) if !status.clean || status.changed > 0 => FleetGitState::Dirty,
        Ok(_) => FleetGitState::Clean,
    }
}

/// 将 browser active 探测结果映射为 FleetBrowserState。
///
/// Business Logic（为什么需要这个函数）:
///     无 preview 是正常态（Absent），探测失败才是 Unknown。
///
/// Code Logic（这个函数做什么）:
///     Ok(true)→Active；Ok(false)→Absent；Err→Unknown。
pub fn map_browser_state(result: Result<bool, AppError>) -> FleetBrowserState {
    match result {
        Ok(true) => FleetBrowserState::Active,
        Ok(false) => FleetBrowserState::Absent,
        Err(_) => FleetBrowserState::Unknown,
    }
}

/// 统计本设备全部 local 项目的 active Orchestrator 槽位。
///
/// Business Logic（为什么需要这个函数）:
///     Fleet device header 需要真正的 global slots_used；禁止用当前 project slotsUsed 推导。
///
/// Code Logic（这个函数做什么）:
///     委托 `OrchestratorRepo::count_active_local_tasks`，转 u32。
pub async fn count_active_slots_for_device(repo: &OrchestratorRepo) -> Result<u32, AppError> {
    let n = repo.count_active_local_tasks().await?;
    Ok(n.max(0) as u32)
}

/// 为单个本机 local project 构建 Fleet 项目摘要。
///
/// Business Logic（为什么需要这个函数）:
///     owner batch 与本机聚合都需要字段级摘要；remote absolute path 不得进入 DTO。
///
/// Code Logic（这个函数做什么）:
///     并行读取 agent/attention/terminal/git/browser/orchestrator；field 失败独立降级。
pub async fn build_local_fleet_project(
    state: &AppState,
    project: &WorkbenchProjectRow,
) -> Result<LanFleetProjectSummary, AppError> {
    if project.kind != "local" {
        return Err(AppError::validation("local_project_required"));
    }

    let project_id = project.id.as_str();

    // Agent active sessions → phase counts + last activity
    let agent_sessions = state
        .workbench_agent_session_repo
        .list_active(Some(project_id), 1_000)
        .await
        .unwrap_or_default();
    let agent_counts = count_agent_phases(&agent_sessions);
    let mut last_activity_at = agent_sessions
        .iter()
        .map(|s| s.last_activity_at.as_str())
        .max()
        .map(str::to_string);

    // Attention-relevant (needsInput active + failed)
    let attention_count = match state
        .workbench_agent_session_repo
        .list_attention_relevant(Some(project_id), 1_000)
        .await
    {
        Ok(rows) => rows.len() as u32,
        Err(_) => 0,
    };

    // Terminals: non-exited preferred; fall back to all listed
    let terminal_count = match state.workbench_session_repo.list(Some(project_id)).await {
        Ok(rows) => {
            let active = rows
                .iter()
                .filter(|r| r.status != "exited")
                .count();
            if active > 0 {
                active as u32
            } else {
                rows.len() as u32
            }
        }
        Err(_) => 0,
    };

    // Git read-only status on project path（失败 → unknown）
    let path = project.path.clone();
    let git_state = map_git_status(
        tokio::task::spawn_blocking(move || crate::workbench::git::status(Path::new(&path)))
            .await
            .unwrap_or_else(|e| Err(AppError::generic(format!("git status join: {e}")))),
    );

    // Browser: registry active preview for project
    let browser_state = map_browser_state(Ok(state
        .workbench_browser_previews
        .has_active_for_project(project_id)));

    // Orchestrator project-local running / retrying
    let orchestrator_running = match state
        .orchestrator_repo
        .count_active_tasks(project_id)
        .await
    {
        Ok(n) => n.max(0) as u32,
        Err(_) => 0,
    };
    let orchestrator_retrying = match state
        .orchestrator_repo
        .list_retrying_runtime_tasks_for_project(project_id, 50)
        .await
    {
        Ok(rows) => rows.len() as u32,
        Err(_) => 0,
    };

    // 若无 agent activity，尝试用 project updated_at
    if last_activity_at.is_none() && !project.updated_at.is_empty() {
        last_activity_at = Some(project.updated_at.clone());
    }

    Ok(LanFleetProjectSummary {
        project_id: project.id.clone(),
        display_name: project.name.clone(),
        project_kind: "local".to_string(),
        agent_counts,
        attention_count,
        terminal_count,
        git_state,
        browser_state,
        orchestrator_running,
        orchestrator_retrying,
        last_activity_at,
    })
}

/// 构建 owning device 本机 batch 摘要（仅 local projects）。
///
/// Business Logic（为什么需要这个函数）:
///     P2P route 与本机 collector 共用：按请求 id/path 解析本机 local 项目，带 100 上限。
///
/// Code Logic（这个函数做什么）:
///     校验请求规模 → 解析 id/path → 逐项 build_local_fleet_project → 填 global slots。
pub async fn build_owner_device_summary(
    state: &AppState,
    req: &LanFleetOwnerBatchReq,
) -> Result<LanFleetOwnerBatchResp, AppError> {
    let total_requested = req.project_ids.len().saturating_add(req.project_paths.len());
    if total_requested > FLEET_OWNER_BATCH_MAX_PROJECTS {
        return Err(AppError::validation("resource_limit"));
    }

    let mut resolved: Vec<WorkbenchProjectRow> = Vec::new();
    let mut seen_ids: HashMap<String, ()> = HashMap::new();

    for raw_id in &req.project_ids {
        let id = raw_id.trim();
        if id.is_empty() {
            continue;
        }
        if is_remote_id(id) {
            return Err(AppError::validation("local_project_required"));
        }
        if seen_ids.contains_key(id) {
            continue;
        }
        match state.workbench_project_repo.get(id).await? {
            Some(row) if row.kind == "local" => {
                seen_ids.insert(row.id.clone(), ());
                resolved.push(row);
            }
            Some(_) => {
                return Err(AppError::validation("local_project_required"));
            }
            None => {
                // 缺失项目：跳过，调用方显示 unavailable；不因单 id 失败整批
                continue;
            }
        }
    }

    for raw_path in &req.project_paths {
        let path = raw_path.trim();
        if path.is_empty() {
            continue;
        }
        // 禁止把 remote: 包装当 path
        if is_remote_id(path) {
            return Err(AppError::validation("local_project_required"));
        }
        // 先按已保存 local project 精确 path 匹配（不自动创建新项目，避免 batch 副作用）
        let all = state.workbench_project_repo.list().await?;
        if let Some(row) = all
            .into_iter()
            .find(|p| p.kind == "local" && (p.path == path || paths_equal(&p.path, path)))
        {
            if seen_ids.contains_key(&row.id) {
                continue;
            }
            seen_ids.insert(row.id.clone(), ());
            resolved.push(row);
        }
        // 未找到：跳过（shortcut 失效）
    }

    if resolved.len() > FLEET_OWNER_BATCH_MAX_PROJECTS {
        return Err(AppError::validation("resource_limit"));
    }

    let mut projects = Vec::with_capacity(resolved.len());
    for row in &resolved {
        match build_local_fleet_project(state, row).await {
            Ok(summary) => projects.push(summary),
            Err(e) if e.code() == "local_project_required" => {
                return Err(e);
            }
            Err(_) => {
                // 单项目整体失败：以 unavailable 占位（保留 id/name）
                projects.push(unavailable_project_summary(row));
            }
        }
    }

    let slots_used = count_active_slots_for_device(&state.orchestrator_repo)
        .await
        .ok();
    let slots_max = {
        let cfg = state.config.read().expect("config 读锁中毒");
        Some(cfg.orchestrator.max_concurrent_tasks.max(0) as u32)
    };

    let generated_at = Utc::now().to_rfc3339();
    let device = LanFleetDeviceSummary {
        device_id: state.device_id.as_ref().clone(),
        device_name: state.device_name(),
        reachability: FleetReachability::Live,
        freshness: FleetFreshness::Live,
        scheduler_slots_used: slots_used,
        scheduler_slots_max: slots_max,
        projects,
        error_code: None,
        captured_at: Some(generated_at.clone()),
    };

    Ok(LanFleetOwnerBatchResp {
        generated_at,
        device,
    })
}

/// 控制设备：仅聚合已保存 shortcut，按 device 去重 fan-out（使用全局 display cache）。
///
/// Business Logic（为什么需要这个函数）:
///     桌面/mobile 一次拿到 Fleet snapshot；单 device 失败保留 cache，不阻塞其他 live 结果。
///
/// Code Logic（这个函数做什么）:
///     委托 `collect_lan_fleet_for_state_with_cache` + `global_fleet_display_cache`。
pub async fn collect_lan_fleet_for_state(state: &AppState) -> Result<LanFleetSnapshot, AppError> {
    collect_lan_fleet_for_state_with_cache(state, &super::cache::global_fleet_display_cache()).await
}

/// 控制设备：仅聚合已保存 shortcut，按 device 去重 fan-out。
///
/// Business Logic（为什么需要这个函数）:
///     桌面/mobile 一次拿到 Fleet snapshot；单 device 失败保留 cache，不阻塞其他 live 结果。
///
/// Code Logic（这个函数做什么）:
///     list projects → 分组 local/remote → local 同步构建 → remote semaphore=3 + 5s timeout
///     → cache 合并 → 全局 500 projects 截断。
pub async fn collect_lan_fleet_for_state_with_cache(
    state: &AppState,
    cache: &SharedFleetDisplayCache,
) -> Result<LanFleetSnapshot, AppError> {
    let projects = state.workbench_project_repo.list().await?;
    let local_device_id = state.device_id.as_ref().clone();
    let local_device_name = state.device_name();

    let mut local_projects: Vec<WorkbenchProjectRow> = Vec::new();
    // device_id → (device_name, Vec of remote shortcuts)
    let mut remote_by_device: HashMap<String, (String, Vec<WorkbenchProjectRow>)> = HashMap::new();

    for project in projects {
        if project.kind == "local" {
            local_projects.push(project);
        } else if project.kind == "remote" {
            let entry = remote_by_device
                .entry(project.device_id.clone())
                .or_insert_with(|| (project.device_name.clone(), Vec::new()));
            if entry.0.trim().is_empty() {
                entry.0 = project.device_name.clone();
            }
            entry.1.push(project);
        }
    }

    let mut devices: Vec<LanFleetDeviceSummary> = Vec::new();
    let mut truncated = false;
    let mut total_projects: usize = 0;

    // ---- local device ----
    if !local_projects.is_empty() {
        let ids: Vec<String> = local_projects.iter().map(|p| p.id.clone()).collect();
        // 本地也遵守 100 batch 上限
        let (batch_ids, rest) = if ids.len() > FLEET_OWNER_BATCH_MAX_PROJECTS {
            truncated = true;
            (
                ids[..FLEET_OWNER_BATCH_MAX_PROJECTS].to_vec(),
                ids[FLEET_OWNER_BATCH_MAX_PROJECTS..].len(),
            )
        } else {
            (ids, 0)
        };
        let _ = rest;
        let req = LanFleetOwnerBatchReq {
            project_ids: batch_ids,
            project_paths: Vec::new(),
        };
        match build_owner_device_summary(state, &req).await {
            Ok(resp) => {
                total_projects = total_projects.saturating_add(resp.device.projects.len());
                cache.put(resp.device.clone());
                devices.push(resp.device);
            }
            Err(_) => {
                if let Some(cached) = cache.get(&local_device_id) {
                    let mut d = cached;
                    d.reachability = FleetReachability::Offline;
                    d.freshness = FleetFreshness::Cached;
                    d.error_code = Some("local_collect_failed".into());
                    total_projects = total_projects.saturating_add(d.projects.len());
                    devices.push(d);
                } else {
                    devices.push(empty_device(
                        &local_device_id,
                        &local_device_name,
                        FleetReachability::Offline,
                        FleetFreshness::Unknown,
                        Some("local_collect_failed"),
                    ));
                }
            }
        }
    }

    // ---- remote devices (bounded fan-out) ----
    let remote_entries: Vec<(String, String, Vec<WorkbenchProjectRow>)> = remote_by_device
        .into_iter()
        .map(|(id, (name, projs))| (id, name, projs))
        .collect();

    let semaphore = Arc::new(Semaphore::new(FLEET_FANOUT_MAX_CONCURRENCY));
    let client = RemoteWorkbenchClient::new();
    let cache_arc = cache.clone();
    let devices_map = state.devices.clone();

    let remote_results: Vec<LanFleetDeviceSummary> = stream::iter(remote_entries)
        .map(|(device_id, device_name, shortcuts)| {
            let semaphore = semaphore.clone();
            let client = client.clone();
            let cache_arc = cache_arc.clone();
            let devices_map = devices_map.clone();
            let state_ref_port = state.actual_http_port.clone();
            async move {
                let _permit = match semaphore.acquire().await {
                    Ok(p) => p,
                    Err(_) => {
                        return offline_or_cached(
                            &cache_arc,
                            &device_id,
                            &device_name,
                            "semaphore_closed",
                        );
                    }
                };

                let base_url = match device_base_url_from_map(&devices_map, &device_id) {
                    Some(url) => url,
                    None => {
                        let _ = state_ref_port;
                        return offline_or_cached(
                            &cache_arc,
                            &device_id,
                            &device_name,
                            "device_offline",
                        );
                    }
                };

                // capability gate
                let supports = match client
                    .peer_supports_capability(&base_url, CAPABILITY_WORKBENCH_LAN_FLEET_V1)
                    .await
                {
                    Ok(v) => v,
                    Err(_) => {
                        return offline_or_cached(
                            &cache_arc,
                            &device_id,
                            &device_name,
                            "health_failed",
                        );
                    }
                };
                if !supports {
                    return LanFleetDeviceSummary {
                        device_id: device_id.clone(),
                        device_name: device_name.clone(),
                        reachability: FleetReachability::Unsupported,
                        freshness: FleetFreshness::Unknown,
                        scheduler_slots_used: None,
                        scheduler_slots_max: None,
                        projects: Vec::new(),
                        error_code: Some("capability_unsupported".into()),
                        captured_at: None,
                    };
                }

                // 按 path 批请求；上限 100
                let mut paths: Vec<String> = shortcuts.iter().map(|s| s.path.clone()).collect();
                paths.sort();
                paths.dedup();
                if paths.len() > FLEET_OWNER_BATCH_MAX_PROJECTS {
                    paths.truncate(FLEET_OWNER_BATCH_MAX_PROJECTS);
                }

                // path → control-side shortcut id 映射（响应改写 project_id，禁止泄漏 path）
                let path_to_shortcut: HashMap<String, WorkbenchProjectRow> = shortcuts
                    .iter()
                    .map(|s| (s.path.clone(), s.clone()))
                    .collect();

                let req = LanFleetOwnerBatchReq {
                    project_ids: Vec::new(),
                    project_paths: paths,
                };

                let fetch = client.lan_fleet_snapshot(&base_url, &req);
                let timed = tokio::time::timeout(
                    Duration::from_secs(FLEET_DEVICE_TIMEOUT_SECS),
                    fetch,
                )
                .await;

                match timed {
                    Ok(Ok(resp)) => {
                        let mut device = resp.device;
                        // 改写 project_id 为控制侧 remote shortcut id；kind=remote
                        device.projects = remap_remote_projects(
                            &device_id,
                            device.projects,
                            &path_to_shortcut,
                        );
                        device.device_id = device_id.clone();
                        if device.device_name.trim().is_empty() {
                            device.device_name = device_name.clone();
                        }
                        device.reachability = FleetReachability::Live;
                        device.freshness = FleetFreshness::Live;
                        device.error_code = None;
                        if device.captured_at.is_none() {
                            device.captured_at = Some(resp.generated_at.clone());
                        }
                        cache_arc.put(device.clone());
                        device
                    }
                    Ok(Err(_)) => {
                        offline_or_cached(&cache_arc, &device_id, &device_name, "peer_error")
                    }
                    Err(_) => offline_or_cached(&cache_arc, &device_id, &device_name, "timeout"),
                }
            }
        })
        .buffer_unordered(FLEET_FANOUT_MAX_CONCURRENCY)
        .collect()
        .await;

    for d in remote_results {
        total_projects = total_projects.saturating_add(d.projects.len());
        devices.push(d);
    }

    // 全局 500 projects 稳定截断（按 device 顺序裁剪尾部 projects）
    if total_projects > FLEET_SNAPSHOT_MAX_PROJECTS {
        truncated = true;
        let mut remaining = FLEET_SNAPSHOT_MAX_PROJECTS;
        for device in &mut devices {
            if remaining == 0 {
                device.projects.clear();
                continue;
            }
            if device.projects.len() > remaining {
                device.projects.truncate(remaining);
                remaining = 0;
            } else {
                remaining = remaining.saturating_sub(device.projects.len());
            }
        }
    }

    // 稳定排序：本机优先，其余 device_id 字典序
    devices.sort_by(|a, b| {
        let a_local = a.device_id == local_device_id;
        let b_local = b.device_id == local_device_id;
        b_local
            .cmp(&a_local)
            .then_with(|| a.device_id.cmp(&b.device_id))
    });

    Ok(LanFleetSnapshot {
        generated_at: Utc::now().to_rfc3339(),
        devices,
        truncated,
    })
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Business Logic（为什么需要这个函数）:
///     路径字符串可能带尾斜杠，匹配 shortcut 时需宽松相等。
///
/// Code Logic（这个函数做什么）:
///     trim 尾部 `/` 后比较。
fn paths_equal(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim_end_matches('/').to_string();
    norm(a) == norm(b)
}

/// Business Logic（为什么需要这个函数）:
///     单项目构建失败时仍保留导航锚点，避免整 device 空白。
///
/// Code Logic（这个函数做什么）:
///     用 unknown 字段填充 LanFleetProjectSummary。
fn unavailable_project_summary(row: &WorkbenchProjectRow) -> LanFleetProjectSummary {
    LanFleetProjectSummary {
        project_id: row.id.clone(),
        display_name: row.name.clone(),
        project_kind: row.kind.clone(),
        agent_counts: AgentPhaseCounts::default(),
        attention_count: 0,
        terminal_count: 0,
        git_state: FleetGitState::Unknown,
        browser_state: FleetBrowserState::Unknown,
        orchestrator_running: 0,
        orchestrator_retrying: 0,
        last_activity_at: None,
    }
}

/// Business Logic（为什么需要这个函数）:
///     无 cache 的 offline 设备仍要在 Fleet 列表占位。
///
/// Code Logic（这个函数做什么）:
///     构造空 projects 的 device summary。
fn empty_device(
    device_id: &str,
    device_name: &str,
    reachability: FleetReachability,
    freshness: FleetFreshness,
    error_code: Option<&str>,
) -> LanFleetDeviceSummary {
    LanFleetDeviceSummary {
        device_id: device_id.to_string(),
        device_name: device_name.to_string(),
        reachability,
        freshness,
        scheduler_slots_used: None,
        scheduler_slots_max: None,
        projects: Vec::new(),
        error_code: error_code.map(str::to_string),
        captured_at: None,
    }
}

/// Business Logic（为什么需要这个函数）:
///     offline 优先展示 last live cache 并标 cached，避免清空其他 live 数据时误丢历史。
///
/// Code Logic（这个函数做什么）:
///     cache hit → 改 reachability/freshness/error；miss → empty offline。
fn offline_or_cached(
    cache: &SharedFleetDisplayCache,
    device_id: &str,
    device_name: &str,
    error_code: &str,
) -> LanFleetDeviceSummary {
    if let Some(mut cached) = cache.get(device_id) {
        cached.reachability = FleetReachability::Offline;
        cached.freshness = FleetFreshness::Cached;
        cached.error_code = Some(error_code.to_string());
        cached
    } else {
        empty_device(
            device_id,
            device_name,
            FleetReachability::Offline,
            FleetFreshness::Unknown,
            Some(error_code),
        )
    }
}

/// Business Logic（为什么需要这个函数）:
///     从 mDNS devices 表解析 base_url；离线设备无 URL。
///
/// Code Logic（这个函数做什么）:
///     读锁取 Device.base_url / host:port。
fn device_base_url_from_map(
    devices: &std::sync::Arc<std::sync::RwLock<HashMap<String, Device>>>,
    device_id: &str,
) -> Option<String> {
    let guard = devices.read().ok()?;
    let device = guard.get(device_id)?;
    if !device.online {
        return None;
    }
    Some(device.base_url())
}

/// Business Logic（为什么需要这个函数）:
///     owner 返回的 local project_id 不能直接用于控制设备导航；必须改写为 remote shortcut id。
///     且不得把远端绝对 path 写入最终 snapshot。
///
/// Code Logic（这个函数做什么）:
///     优先用 path→shortcut 映射；否则 remote_project_id(device, owner_id) 不稳定时用
///     remote_entity_id 包装 owner local id；display_name 优先 shortcut.name。
fn remap_remote_projects(
    device_id: &str,
    owner_projects: Vec<LanFleetProjectSummary>,
    path_to_shortcut: &HashMap<String, WorkbenchProjectRow>,
) -> Vec<LanFleetProjectSummary> {
    // owner 响应不含 path；用 display_name/path 无法可靠匹配。
    // 控制侧 shortcut.path 对应 owner 上 local path；open 后 owner id 存在于 shortcut 流程。
    // 策略：若只有一条 shortcut 且一条 project，直接绑定；否则按 remote_project_id(device, path)
    // 预计算的 shortcut id 列表顺序对齐（batch 按 path 排序后 owner 按解析顺序返回）。
    // 更稳妥：用 shortcut 列表建立 owner_local_id 未知时的 fallback——按 path 排序的 shortcuts
    // 与 owner 返回顺序 zip（owner build 按请求 path 顺序）。
    let mut shortcuts_by_path: Vec<&WorkbenchProjectRow> = path_to_shortcut.values().collect();
    shortcuts_by_path.sort_by(|a, b| a.path.cmp(&b.path));

    owner_projects
        .into_iter()
        .enumerate()
        .map(|(idx, mut summary)| {
            if let Some(shortcut) = shortcuts_by_path.get(idx) {
                summary.project_id = shortcut.id.clone();
                if !shortcut.name.trim().is_empty() {
                    summary.display_name = shortcut.name.clone();
                }
            } else {
                // fallback：包装 owner local id（不泄漏 path）
                summary.project_id = format!("remote:{device_id}:{}", summary.project_id);
            }
            summary.project_kind = "remote".to_string();
            // 防御：确保 project_id 不含绝对 path 形态
            if summary.project_id.starts_with('/') || summary.project_id.contains(":\\") {
                summary.project_id = remote_project_id(device_id, &summary.project_id);
            }
            let _ = parse_remote_entity_id(&summary.project_id);
            summary
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::models::{OrchestratorTaskRow, OrchestratorTaskStatus};
    use crate::orchestrator::repo::OrchestratorRepo;
    use crate::storage::{
        DatabaseMaintenanceGate, WorkbenchAgentSessionRepo, WorkbenchProjectRepo,
        WorkbenchSessionRepo,
    };
    use crate::workbench::agent_runtime::models::CreateActiveAgentSession;
    use crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use std::sync::Arc;

    /// Business Logic（为什么需要这个函数）:
    ///     collector 单测需要共享内存库与 schema。
    ///
    /// Code Logic（这个函数做什么）:
    ///     内存 SQLite + orchestrator/agent/projects/sessions schema。
    async fn setup_pool() -> sqlx::SqlitePool {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        OrchestratorRepo::init_schema(&pool).await.unwrap();
        WorkbenchAgentSessionRepo::ensure_schema(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_projects (\
             id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, device_id TEXT NOT NULL, \
             device_name TEXT NOT NULL, path TEXT NOT NULL, last_opened_at TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_sessions (\
             id TEXT PRIMARY KEY, project_id TEXT NOT NULL, worktree_id TEXT, name TEXT NOT NULL, \
             command TEXT, cwd TEXT, status TEXT NOT NULL, cols INTEGER, rows INTEGER, \
             started_at TEXT, exited_at TEXT, exit_code INTEGER, backend TEXT, backend_id TEXT, \
             backend_window_id TEXT, created_at TEXT, updated_at TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    /// Business Logic（为什么需要这个函数）:
    ///     插入 local Workbench 项目供 slot join。
    ///
    /// Code Logic（这个函数做什么）:
    ///     INSERT workbench_projects。
    async fn insert_project(pool: &sqlx::SqlitePool, id: &str, kind: &str) {
        sqlx::query(
            "INSERT INTO workbench_projects \
             (id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'd1', 'Dev', ?, '2026-07-15T00:00:00Z', '2026-07-15T00:00:00Z', '2026-07-15T00:00:00Z')",
        )
        .bind(id)
        .bind(format!("P-{id}"))
        .bind(kind)
        .bind(format!("/tmp/{id}"))
        .execute(pool)
        .await
        .unwrap();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     构造最小任务行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     default_for_status + 覆盖 id/project。
    fn task_row(id: &str, project_id: &str, status: OrchestratorTaskStatus) -> OrchestratorTaskRow {
        OrchestratorTaskRow {
            id: id.to_string(),
            project_id: project_id.to_string(),
            title: format!("T-{id}"),
            goal: "g".into(),
            acceptance_criteria: "a".into(),
            status,
            priority: 0,
            branch_name: None,
            worktree_id: None,
            session_id: None,
            blocked_reason: None,
            attempt: 0,
            created_at: "2026-07-15T00:00:00Z".into(),
            updated_at: "2026-07-15T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
            ..OrchestratorTaskRow::default_for_status(status)
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     device slots 必须统计所有 local 项目 active 任务，不能只看当前 project。
    ///
    /// Code Logic（这个测试做什么）:
    ///     p1/p2 各 1 个 Running；count_active_slots_for_device == 2；
    ///     p1 的 count_active_tasks == 1。
    #[tokio::test]
    async fn device_slots_include_active_tasks_from_other_projects() {
        let pool = setup_pool().await;
        insert_project(&pool, "p1", "local").await;
        insert_project(&pool, "p2", "local").await;
        let repo = OrchestratorRepo::new(pool.clone());
        repo.create_task(&task_row(
            "t1",
            "p1",
            OrchestratorTaskStatus::Running,
        ))
        .await
        .unwrap();
        repo.create_task(&task_row(
            "t2",
            "p2",
            OrchestratorTaskStatus::Preparing,
        ))
        .await
        .unwrap();

        let device_slots = count_active_slots_for_device(&repo).await.unwrap();
        let project_running = repo.count_active_tasks("p1").await.unwrap();

        assert_eq!(device_slots, 2);
        assert_eq!(project_running, 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     phase 计数必须覆盖 needsInput/failed，供 Rail 异常 badge。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造各 phase 的 mock runtime，断言 count_agent_phases。
    #[test]
    fn agent_phase_counts_cover_exceptions() {
        let mk = |phase: AgentSessionPhase| AgentSessionRuntime {
            id: format!("{phase:?}"),
            project_id: "p".into(),
            worktree_id: None,
            terminal_session_id: format!("t-{phase:?}"),
            orchestrator_task_id: None,
            orchestrator_attempt: None,
            provider_id: "claudeCodeVisible".into(),
            native_session_id: None,
            phase,
            version: 1,
            started_at: "2026-07-15T00:00:00Z".into(),
            last_activity_at: "2026-07-15T00:00:00Z".into(),
            ended_at: None,
            outcome_code: None,
            resumed_from_agent_session_id: None,
            is_active: !phase.is_terminal(),
        };
        let sessions = vec![
            mk(AgentSessionPhase::Working),
            mk(AgentSessionPhase::Working),
            mk(AgentSessionPhase::NeedsInput),
            mk(AgentSessionPhase::Failed),
            mk(AgentSessionPhase::Idle),
        ];
        let counts = count_agent_phases(&sessions);
        assert_eq!(counts.working, 2);
        assert_eq!(counts.needs_input, 1);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.exception_count(), 2);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Git 错误只能映射 unknown，不能伪装 clean。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Err → Unknown；conflicts → Conflict；changed → Dirty；clean → Clean。
    #[test]
    fn git_error_maps_to_unknown() {
        assert_eq!(
            map_git_status(Err(AppError::generic("boom"))),
            FleetGitState::Unknown
        );
        assert_eq!(
            map_git_status(Ok(WorkbenchGitStatusDto {
                conflicts: 1,
                clean: false,
                changed: 0,
                ..Default::default()
            })),
            FleetGitState::Conflict
        );
        assert_eq!(
            map_git_status(Ok(WorkbenchGitStatusDto {
                conflicts: 0,
                clean: false,
                changed: 2,
                ..Default::default()
            })),
            FleetGitState::Dirty
        );
        assert_eq!(
            map_git_status(Ok(WorkbenchGitStatusDto {
                conflicts: 0,
                clean: true,
                changed: 0,
                ..Default::default()
            })),
            FleetGitState::Clean
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     无 preview 是 Absent，不是 Unknown。
    ///
    /// Code Logic（这个测试做什么）:
    ///     map_browser_state 三分支。
    #[test]
    fn browser_absent_when_no_preview() {
        assert_eq!(map_browser_state(Ok(false)), FleetBrowserState::Absent);
        assert_eq!(map_browser_state(Ok(true)), FleetBrowserState::Active);
        assert_eq!(
            map_browser_state(Err(AppError::generic("x"))),
            FleetBrowserState::Unknown
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     agent repo 写入后 count_agent_phases 应反映 needsInput。
    ///
    /// Code Logic（这个测试做什么）:
    ///     create NeedsInput session → list_active → count。
    #[tokio::test]
    async fn agent_repo_phase_counts_from_active_sessions() {
        let pool = setup_pool().await;
        let gate = Arc::new(DatabaseMaintenanceGate::new());
        let agent_repo = WorkbenchAgentSessionRepo::with_gate(pool.clone(), gate);
        agent_repo
            .create_active(CreateActiveAgentSession {
                id: None,
                project_id: "p1".into(),
                worktree_id: None,
                terminal_session_id: "term-1".into(),
                orchestrator_task_id: None,
                orchestrator_attempt: None,
                provider_id: "claudeCodeVisible".into(),
                native_session_id: None,
                phase: AgentSessionPhase::NeedsInput,
                started_at: "2026-07-15T00:00:00Z".into(),
                resumed_from_agent_session_id: None,
            })
            .await
            .unwrap();
        let listed = agent_repo.list_active(Some("p1"), 100).await.unwrap();
        let counts = count_agent_phases(&listed);
        assert_eq!(counts.needs_input, 1);
        let _ = WorkbenchProjectRepo::new(pool.clone());
        let _ = WorkbenchSessionRepo::new(pool.clone());
        let _ = WorkbenchBrowserPreviewRegistry::new();
    }
}
