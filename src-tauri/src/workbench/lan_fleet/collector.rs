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
    AgentPhaseCounts, FleetAgentActivityStatus, FleetBrowserState, FleetFreshness, FleetGitState,
    FleetReachability, LanFleetDeviceSummary, LanFleetOwnerBatchReq, LanFleetOwnerBatchResp,
    LanFleetProjectSummary, LanFleetSnapshot, FLEET_DEVICE_TIMEOUT_SECS,
    FLEET_FANOUT_MAX_CONCURRENCY, FLEET_OWNER_BATCH_MAX_PROJECTS, FLEET_SNAPSHOT_MAX_PROJECTS,
};
use crate::error::AppError;
use crate::models::device::Device;
use crate::net::protocol::{
    CAPABILITY_WORKBENCH_AGENT_LEDGER_SUMMARY_V1, CAPABILITY_WORKBENCH_LAN_FLEET_V1,
};
use crate::orchestrator::repo::OrchestratorRepo;
use crate::state::AppState;
use crate::workbench::agent_ledger::aggregation::summarize_window;
use crate::workbench::agent_ledger::models::{
    AgentLedgerSummaryBatchReq, LedgerWindow, AGENT_LEDGER_SUMMARY_MAX_PROJECTS,
};
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
            let active = rows.iter().filter(|r| r.status != "exited").count();
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
    let orchestrator_running = match state.orchestrator_repo.count_active_tasks(project_id).await {
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

    // 7d ledger 聚合：失败只降级 status，不阻断其它 Fleet 字段
    let (agent_activity_status, agent_activity) = match summarize_window(
        &state.agent_ledger_repo,
        LedgerWindow::Days7,
        Some(project_id),
        Utc::now(),
    )
    .await
    {
        Ok(mut summary) => {
            summary.project_id = Some(project.id.clone());
            (FleetAgentActivityStatus::Live, Some(summary))
        }
        Err(_) => (FleetAgentActivityStatus::Unavailable, None),
    };

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
        agent_activity_status,
        agent_activity,
    })
}

/// 构建 owning device 本机 batch 摘要（仅 local projects）。
///
/// Business Logic（为什么需要这个函数）:
///     P2P route 与本机 collector 共用：按请求 id/path 解析本机 local 项目，带 100 上限。
///     失效 shortcut / 缺失 id 必须返回 unavailable 占位（请求顺序一一对应），禁止静默省略，
///     以便控制侧按 path/id 稳定 join，而不是按稀疏结果 index zip。
///
/// Code Logic（这个函数做什么）:
///     校验规模与 remote id → 每个非空请求槽输出一条 summary（命中则 build，失败/缺失则
///     unavailable）→ 填 device-global slots。path 解析只 list 一次 local 项目。
pub async fn build_owner_device_summary(
    state: &AppState,
    req: &LanFleetOwnerBatchReq,
) -> Result<LanFleetOwnerBatchResp, AppError> {
    let total_requested = req
        .project_ids
        .len()
        .saturating_add(req.project_paths.len());
    if total_requested > FLEET_OWNER_BATCH_MAX_PROJECTS {
        return Err(AppError::validation("resource_limit"));
    }

    // path 匹配只拉一次 local 列表，避免 O(paths × list)
    let local_by_path: Vec<WorkbenchProjectRow> = state
        .workbench_project_repo
        .list()
        .await?
        .into_iter()
        .filter(|p| p.kind == "local")
        .collect();

    let mut projects: Vec<LanFleetProjectSummary> = Vec::with_capacity(total_requested);

    for raw_id in &req.project_ids {
        let id = raw_id.trim();
        if id.is_empty() {
            continue;
        }
        if is_remote_id(id) {
            return Err(AppError::validation("local_project_required"));
        }
        match state.workbench_project_repo.get(id).await? {
            Some(row) if row.kind == "local" => {
                projects.push(build_or_unavailable(state, &row).await?);
            }
            Some(_) => {
                // DB 中存在但非 local（如 remote shortcut）→ 整批拒绝，禁止递归
                return Err(AppError::validation("local_project_required"));
            }
            None => {
                // 缺失 id：unavailable 占位，保留请求 id 供导航锚点
                projects.push(unavailable_for_request(id, id));
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
        // 已保存 local project 精确/尾斜杠 path 匹配；不自动创建
        if let Some(row) = local_by_path
            .iter()
            .find(|p| p.path == path || paths_equal(&p.path, path))
        {
            projects.push(build_or_unavailable(state, row).await?);
        } else {
            // shortcut 失效：unavailable；project_id 不得写绝对 path（控制侧按请求 path 槽位 join）
            projects.push(unavailable_for_request(
                FLEET_UNRESOLVED_PROJECT_ID,
                &path_display_name(path),
            ));
        }
    }

    if projects.len() > FLEET_OWNER_BATCH_MAX_PROJECTS {
        return Err(AppError::validation("resource_limit"));
    }

    // 防御：响应不得含绝对 path 形态 id
    for project in &projects {
        if project.project_id.starts_with('/') || project.project_id.contains(":\\") {
            return Err(AppError::generic("fleet_path_leak"));
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

/// Business Logic（为什么需要这个函数）:
///     单项目 build 失败时仍要占位，不能让整批缺槽导致控制侧 join 错位。
///
/// Code Logic（这个函数做什么）:
///     build_local_fleet_project；local_project_required 上抛；其它错误 → unavailable。
async fn build_or_unavailable(
    state: &AppState,
    row: &WorkbenchProjectRow,
) -> Result<LanFleetProjectSummary, AppError> {
    match build_local_fleet_project(state, row).await {
        Ok(summary) => Ok(summary),
        Err(e) if e.code() == "local_project_required" => Err(e),
        Err(_) => Ok(unavailable_project_summary(row)),
    }
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
    let cache_arc = cache.clone();
    let devices_map = state.devices.clone();

    let remote_results: Vec<LanFleetDeviceSummary> = stream::iter(remote_entries)
        .map(|(device_id, device_name, shortcuts)| {
            let semaphore = semaphore.clone();
            // 每个远端设备独立 bind expected_device_id，避免 fan-out 打到端口复用后的错误节点。
            let client = RemoteWorkbenchClient::new().with_expected_device_id(&device_id);
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

                // 按 path 批请求；稳定排序 + 去重；上限 100
                let mut paths: Vec<String> = shortcuts.iter().map(|s| s.path.clone()).collect();
                paths.sort();
                paths.dedup();
                if paths.len() > FLEET_OWNER_BATCH_MAX_PROJECTS {
                    paths.truncate(FLEET_OWNER_BATCH_MAX_PROJECTS);
                }

                // path → control-side shortcut 映射（响应按 path 键 join，禁止泄漏 path）
                let path_to_shortcut: HashMap<String, WorkbenchProjectRow> = shortcuts
                    .iter()
                    .map(|s| (s.path.clone(), s.clone()))
                    .collect();

                let req = LanFleetOwnerBatchReq {
                    project_ids: Vec::new(),
                    project_paths: paths.clone(),
                };

                let fetch = client.lan_fleet_snapshot(&base_url, &req);
                let timed =
                    tokio::time::timeout(Duration::from_secs(FLEET_DEVICE_TIMEOUT_SECS), fetch)
                        .await;

                match timed {
                    Ok(Ok(resp)) => {
                        let mut device = resp.device;
                        // ledger join 必须在 remap 前：owner 返回 local project_id
                        join_remote_agent_activity(&client, &base_url, &mut device.projects).await;
                        // 按请求 path 键 join → 控制侧 remote shortcut id；失效 path 保留 unavailable
                        device.projects = remap_remote_projects(
                            &device_id,
                            device.projects,
                            &paths,
                            &path_to_shortcut,
                        );
                        // activity.project_id 与 project_id 同步为 remote 包装 id
                        for p in device.projects.iter_mut() {
                            if let Some(ref mut activity) = p.agent_activity {
                                activity.project_id = Some(p.project_id.clone());
                            }
                        }
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

/// owner 响应中“未解析 path”占位的稳定 project_id（禁止写绝对 path）。
const FLEET_UNRESOLVED_PROJECT_ID: &str = "__fleet_unresolved__";

/// Fleet 项目 kind：shortcut/path 失效或构建失败时的显式 unavailable。
const FLEET_PROJECT_KIND_UNAVAILABLE: &str = "unavailable";

/// Business Logic（为什么需要这个函数）:
///     单项目构建失败时仍保留导航锚点，避免整 device 空白。
///
/// Code Logic（这个函数做什么）:
///     用 unknown 字段 + kind=unavailable 填充 LanFleetProjectSummary。
fn unavailable_project_summary(row: &WorkbenchProjectRow) -> LanFleetProjectSummary {
    unavailable_for_request(&row.id, &row.name)
}

/// Business Logic（为什么需要这个函数）:
///     请求 id/path 无法解析为 local 项目时必须输出 unavailable 行，禁止静默省略。
///
/// Code Logic（这个函数做什么）:
///     构造 kind=unavailable 的占位 summary；project_id 不得是绝对 path。
fn unavailable_for_request(project_id: &str, display_name: &str) -> LanFleetProjectSummary {
    LanFleetProjectSummary {
        project_id: project_id.to_string(),
        display_name: display_name.to_string(),
        project_kind: FLEET_PROJECT_KIND_UNAVAILABLE.to_string(),
        agent_counts: AgentPhaseCounts::default(),
        attention_count: 0,
        terminal_count: 0,
        git_state: FleetGitState::Unknown,
        browser_state: FleetBrowserState::Unknown,
        orchestrator_running: 0,
        orchestrator_retrying: 0,
        last_activity_at: None,
        agent_activity_status: FleetAgentActivityStatus::Unavailable,
        agent_activity: None,
    }
}

/// 为 remote owner 响应 join 7d ledger 聚合；失败/unsupported 不改写其它 Fleet 字段。
///
/// Business Logic（为什么需要这个函数）:
///     Fleet 第一版不得被 ledger 阻断；旧 peer 显示 unsupported，用量永不伪造为 0。
///
/// Code Logic（这个函数做什么）:
///     capability 门控 → 收集 owner local project_ids → agent_ledger_summary → 按 id 合并。
async fn join_remote_agent_activity(
    client: &RemoteWorkbenchClient,
    base_url: &str,
    projects: &mut [LanFleetProjectSummary],
) {
    let supports = match client
        .peer_supports_capability(base_url, CAPABILITY_WORKBENCH_AGENT_LEDGER_SUMMARY_V1)
        .await
    {
        Ok(v) => v,
        Err(_) => {
            for p in projects.iter_mut() {
                p.agent_activity_status = FleetAgentActivityStatus::Unavailable;
                p.agent_activity = None;
            }
            return;
        }
    };
    if !supports {
        for p in projects.iter_mut() {
            p.agent_activity_status = FleetAgentActivityStatus::Unsupported;
            p.agent_activity = None;
        }
        return;
    }

    let mut project_ids: Vec<String> = projects
        .iter()
        .filter(|p| p.project_kind != FLEET_PROJECT_KIND_UNAVAILABLE)
        .map(|p| p.project_id.clone())
        .filter(|id| !id.is_empty() && id != FLEET_UNRESOLVED_PROJECT_ID)
        .collect();
    project_ids.sort();
    project_ids.dedup();
    if project_ids.len() > AGENT_LEDGER_SUMMARY_MAX_PROJECTS {
        project_ids.truncate(AGENT_LEDGER_SUMMARY_MAX_PROJECTS);
    }
    if project_ids.is_empty() {
        for p in projects.iter_mut() {
            p.agent_activity_status = FleetAgentActivityStatus::Unavailable;
            p.agent_activity = None;
        }
        return;
    }

    let req = AgentLedgerSummaryBatchReq {
        project_ids: project_ids.clone(),
        window: LedgerWindow::Days7.as_str().to_string(),
    };
    let fetch = client.agent_ledger_summary(base_url, &req);
    let timed = tokio::time::timeout(Duration::from_secs(FLEET_DEVICE_TIMEOUT_SECS), fetch).await;
    match timed {
        Ok(Ok(resp)) => {
            let by_id: HashMap<String, _> = resp
                .projects
                .into_iter()
                .filter_map(|s| s.project_id.clone().map(|id| (id, s)))
                .collect();
            for p in projects.iter_mut() {
                if let Some(summary) = by_id.get(&p.project_id) {
                    p.agent_activity_status = FleetAgentActivityStatus::Live;
                    p.agent_activity = Some(summary.clone());
                } else {
                    // project_kind==unavailable 与 batch 缺 summary 结果相同
                    p.agent_activity_status = FleetAgentActivityStatus::Unavailable;
                    p.agent_activity = None;
                }
            }
        }
        _ => {
            for p in projects.iter_mut() {
                p.agent_activity_status = FleetAgentActivityStatus::Unavailable;
                p.agent_activity = None;
            }
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     unavailable 占位需要可读 display_name，但不能把绝对 path 写进 DTO。
///
/// Code Logic（这个函数做什么）:
///     取 path 最后一段；空则回落 "unavailable"。
fn path_display_name(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    let name = trimmed.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if name.is_empty() {
        FLEET_PROJECT_KIND_UNAVAILABLE.to_string()
    } else {
        name.to_string()
    }
}

/// Business Logic（为什么需要这个函数）:
///     判断 owner 摘要是否为失效/失败占位，控制侧 remap 后仍要展示 unavailable。
///
/// Code Logic（这个函数做什么）:
///     project_kind == unavailable 或 unresolved 哨兵 id。
fn is_unavailable_summary(summary: &LanFleetProjectSummary) -> bool {
    summary.project_kind == FLEET_PROJECT_KIND_UNAVAILABLE
        || summary.project_id == FLEET_UNRESOLVED_PROJECT_ID
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
///     且不得把远端绝对 path 写入最终 snapshot。中间 path 缺失时禁止 index zip 错绑邻接项目。
///
/// Code Logic（这个函数做什么）:
///     以请求 `project_paths` 为稳定 join 键：槽位 i 对应 paths[i] → path_to_shortcut 查
///     shortcut id/name；owner 同步按请求顺序返回（含 unavailable 占位）。缺失槽合成
///     unavailable。绝不按 owner 稀疏结果与 shortcut 列表 index zip。
fn remap_remote_projects(
    device_id: &str,
    owner_projects: Vec<LanFleetProjectSummary>,
    requested_paths: &[String],
    path_to_shortcut: &HashMap<String, WorkbenchProjectRow>,
) -> Vec<LanFleetProjectSummary> {
    // path 键 → 规范化 lookup（精确 + 尾斜杠）
    let lookup_shortcut = |path: &str| -> Option<&WorkbenchProjectRow> {
        if let Some(row) = path_to_shortcut.get(path) {
            return Some(row);
        }
        path_to_shortcut
            .iter()
            .find(|(k, _)| paths_equal(k, path))
            .map(|(_, v)| v)
    };

    if !requested_paths.is_empty() {
        let mut out = Vec::with_capacity(requested_paths.len());
        for (idx, path) in requested_paths.iter().enumerate() {
            let shortcut = lookup_shortcut(path);
            let owner_summary = owner_projects.get(idx);
            let unavailable = match owner_summary {
                Some(s) => is_unavailable_summary(s),
                None => true,
            };

            let mut summary = match owner_summary {
                Some(s) => s.clone(),
                None => {
                    unavailable_for_request(FLEET_UNRESOLVED_PROJECT_ID, &path_display_name(path))
                }
            };

            if let Some(sc) = shortcut {
                summary.project_id = sc.id.clone();
                if !sc.name.trim().is_empty() {
                    summary.display_name = sc.name.clone();
                }
            } else {
                // 无 shortcut 记录时用 path 哈希 remote id（稳定、不写绝对 path）
                summary.project_id = remote_project_id(device_id, path);
            }

            summary.project_kind = if unavailable {
                FLEET_PROJECT_KIND_UNAVAILABLE.to_string()
            } else {
                "remote".to_string()
            };

            if summary.project_id.starts_with('/') || summary.project_id.contains(":\\") {
                summary.project_id = remote_project_id(device_id, &summary.project_id);
            }
            let _ = parse_remote_entity_id(&summary.project_id);
            out.push(summary);
        }
        return out;
    }

    // 无 path 列表时（异常/测试）：仅包装 owner local id，不猜 shortcut
    owner_projects
        .into_iter()
        .map(|mut summary| {
            let unavailable = is_unavailable_summary(&summary);
            if !summary.project_id.starts_with("remote:") {
                summary.project_id = format!("remote:{device_id}:{}", summary.project_id);
            }
            summary.project_kind = if unavailable {
                FLEET_PROJECT_KIND_UNAVAILABLE.to_string()
            } else {
                "remote".to_string()
            };
            if summary.project_id.starts_with('/') || summary.project_id.contains(":\\") {
                summary.project_id = remote_project_id(device_id, &summary.project_id);
            }
            summary
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ui::HeadlessBackendUi;
    use crate::config::{
        AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
    };
    use crate::net::peer_client::PeerClient;
    use crate::orchestrator::models::{OrchestratorTaskRow, OrchestratorTaskStatus};
    use crate::orchestrator::repo::OrchestratorRepo;
    use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
    use crate::state::AppState;
    use crate::storage::{
        ClaudeHistoryRepo, ClaudeMdRepo, DatabaseMaintenanceGate, PromptRepo, ScratchpadRepo,
        TransferRepo, WorkbenchAgentSessionRepo, WorkbenchBrowserRepo, WorkbenchProjectRepo,
        WorkbenchSessionRepo, WorkbenchWorkspaceLayoutRepo, WorkbenchWorktreeRepo,
    };
    use crate::transfer::registry::TransferRegistry;
    use crate::workbench::agent_runtime::models::CreateActiveAgentSession;
    use crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry;
    use crate::workbench::lan_fleet::cache::FleetDisplayCache;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use std::sync::atomic::AtomicU16;
    use std::sync::{Arc, Mutex, RwLock};

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
        insert_project_at(pool, id, kind, &format!("/tmp/{id}")).await;
    }

    /// Business Logic（为什么需要这个函数）:
    ///     path 批测试需要可控绝对 path。
    ///
    /// Code Logic（这个函数做什么）:
    ///     INSERT workbench_projects 带自定义 path。
    async fn insert_project_at(pool: &sqlx::SqlitePool, id: &str, kind: &str, path: &str) {
        sqlx::query(
            "INSERT INTO workbench_projects \
             (id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'd1', 'Dev', ?, '2026-07-15T00:00:00Z', '2026-07-15T00:00:00Z', '2026-07-15T00:00:00Z')",
        )
        .bind(id)
        .bind(format!("P-{id}"))
        .bind(kind)
        .bind(path)
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

    /// Business Logic（为什么需要这个函数）:
    ///     owner batch / remap 集成测需要可调用 `build_owner_device_summary` 的真实 AppState。
    ///
    /// Code Logic（这个函数做什么）:
    ///     内存库 + 最小 schema + Headless UI 拼装 AppState。
    async fn fleet_test_state(device_id: &str) -> AppState {
        let pool = setup_pool().await;
        // 其它 repo 需要的空表（get 路径不碰时也需构造）
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS prompts (\
             id TEXT PRIMARY KEY, title TEXT NOT NULL, content TEXT NOT NULL, \
             tags TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, \
             device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, \
             delete_epoch INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS ssh_targets (\
             host TEXT PRIMARY KEY, port INTEGER NOT NULL, username TEXT NOT NULL, \
             label TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, \
             device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, \
             delete_epoch INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS scratchpad (\
             id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '速记本', content TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL, device_id TEXT NOT NULL, \
             vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, \
             delete_epoch INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let config = AppConfig {
            device_id: device_id.to_string(),
            device_name: "fleet-owner".into(),
            http_port: 0,
            receive_dir: "/tmp/cc-partner-fleet-test-recv".into(),
            db_path: ":memory:".into(),
            screenshot_hotkey: "<cmd>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
        };
        let store = Arc::new(crate::config_store::MemoryConfigStore::with_config(
            config.clone(),
        ));
        let config_runtime = Arc::new(crate::config_runtime::ConfigRuntime::new(config, store));
        let config = config_runtime.shared_value();
        let maintenance_gate = Arc::new(DatabaseMaintenanceGate::new());

        AppState {
            config,
            config_runtime,
            db: pool.clone(),
            maintenance_gate: maintenance_gate.clone(),
            prompt_repo: Arc::new(PromptRepo::with_gate(
                pool.clone(),
                maintenance_gate.clone(),
            )),
            transfer_repo: Arc::new(TransferRepo::new(pool.clone())),
            claude_md_repo: Arc::new(ClaudeMdRepo::new(pool.clone())),
            scratchpad_repo: Arc::new(ScratchpadRepo::with_gate(
                pool.clone(),
                maintenance_gate.clone(),
            )),
            device_id: Arc::new(device_id.to_string()),
            devices: Arc::new(RwLock::new(HashMap::new())),
            actual_http_port: Arc::new(AtomicU16::new(0)),
            discovery: Arc::new(Mutex::new(None)),
            peer_client: Arc::new(PeerClient::new()),
            transfers: Arc::new(TransferRegistry::new()),
            ui: Arc::new(HeadlessBackendUi::new(std::path::PathBuf::from("/tmp"))),
            update_runtime: Arc::new(crate::updater::UpdateRuntime::new()),
            cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
            ssh_target_repo: Arc::new(crate::storage::SshTargetRepo::with_gate(
                pool.clone(),
                maintenance_gate.clone(),
            )),
            workbench_project_repo: Arc::new(WorkbenchProjectRepo::new(pool.clone())),
            workbench_session_repo: Arc::new(WorkbenchSessionRepo::new(pool.clone())),
            workbench_agent_session_repo: Arc::new(WorkbenchAgentSessionRepo::new(pool.clone())),
            agent_ledger_repo: Arc::new(crate::storage::AgentLedgerRepo::new(pool.clone())),
            agent_ledger_service: Arc::new(
                crate::workbench::agent_ledger::AgentLedgerService::new(
                    crate::storage::AgentLedgerRepo::new(pool.clone()),
                ),
            ),
            workbench_worktree_repo: Arc::new(WorkbenchWorktreeRepo::new(pool.clone())),
            workbench_browser_repo: Arc::new(WorkbenchBrowserRepo::new(pool.clone())),
            workbench_workspace_layout_repo: Arc::new(WorkbenchWorkspaceLayoutRepo::new(
                pool.clone(),
            )),
            workbench_browser_previews: Arc::new(WorkbenchBrowserPreviewRegistry::new()),
            browser_verification: Arc::new(
                crate::workbench::browser_verification::BrowserVerificationService::new(
                    Arc::new(crate::workbench::browser_verification::FakeEngine::succeeds()),
                    std::env::temp_dir().join("cc-partner-bv-fleet-test"),
                    "test-owner".into(),
                )
                .expect("browser verification test service"),
            ),
            workbench_sessions: Arc::new(
                crate::workbench::sessions::WorkbenchSessionRegistry::new(),
            ),
            workbench_remote_events: {
                let (tx, _) = tokio::sync::broadcast::channel(8);
                tx
            },
            workbench_remote_event_bridges: Arc::new(
                crate::workbench::remote_events::RemoteEventBridgeRegistry::new(),
            ),
            workbench_dependency: Arc::new(
                crate::workbench::dependencies::WorkbenchDependencyInstallRuntime::new(),
            ),
            cc_collector_cancel: Arc::new(Mutex::new(None)),
            cloud_sync_runtime: Arc::new(crate::cloud_sync::CloudSyncRuntime::new()),
            cloud_sync_cancel: Arc::new(Mutex::new(None)),
            health: Arc::new(crate::health::HealthRuntime::new()),
            health_repo: Arc::new(crate::storage::health_repo::HealthRepo::new(pool.clone())),
            health_cancel: Arc::new(Mutex::new(None)),
            orchestrator_repo: Arc::new(OrchestratorRepo::new(pool)),
            orchestrator_scheduler_telemetry: OrchestratorSchedulerTelemetry::new(),
            orchestrator_cancel: Arc::new(Mutex::new(None)),
            orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
            agent_ledger_cancel: Arc::new(Mutex::new(None)),
            workbench_claude_session_indexes: Arc::new(RwLock::new(HashMap::new())),
            workbench_claude_session_watchers: Arc::new(Mutex::new(HashMap::new())),
            workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(
                HashMap::new(),
            )),
            workbench_claude_session_index_dispose_epochs: Arc::new(Mutex::new(HashMap::new())),
            runtime_metrics: Arc::new(crate::backend::runtime_metrics::RuntimeMetrics::new()),
            runtime_role: crate::backend::authority::RuntimeRole::HeadlessOwner,
            event_bus: Arc::new(crate::backend::event_bus::RuntimeEventBus::new(format!(
                "fleet-test-{device_id}"
            ))),
            backend_control_client_runtime: Arc::new(
                crate::backend::control_client::BackendControlClientRuntime::new(),
            ),
            gui_event_relay_cancel: Arc::new(Mutex::new(None)),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     remap 单测需要构造 control 侧 shortcut 行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     最小 WorkbenchProjectRow（kind=remote）。
    fn shortcut_row(id: &str, name: &str, path: &str) -> WorkbenchProjectRow {
        WorkbenchProjectRow {
            id: id.to_string(),
            name: name.to_string(),
            kind: "remote".into(),
            device_id: "owner-1".into(),
            device_name: "Owner".into(),
            path: path.to_string(),
            last_opened_at: "2026-07-15T00:00:00Z".into(),
            created_at: "2026-07-15T00:00:00Z".into(),
            updated_at: "2026-07-15T00:00:00Z".into(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     remap 单测需要模拟 owner 返回的 project 摘要。
    ///
    /// Code Logic（这个函数做什么）:
    ///     最小 LanFleetProjectSummary。
    fn owner_summary(project_id: &str, display_name: &str, kind: &str) -> LanFleetProjectSummary {
        LanFleetProjectSummary {
            project_id: project_id.to_string(),
            display_name: display_name.to_string(),
            project_kind: kind.to_string(),
            agent_counts: AgentPhaseCounts {
                working: if kind == "unavailable" { 0 } else { 1 },
                ..AgentPhaseCounts::default()
            },
            attention_count: 0,
            terminal_count: 0,
            git_state: if kind == "unavailable" {
                FleetGitState::Unknown
            } else {
                FleetGitState::Clean
            },
            browser_state: if kind == "unavailable" {
                FleetBrowserState::Unknown
            } else {
                FleetBrowserState::Absent
            },
            orchestrator_running: 0,
            orchestrator_retrying: 0,
            last_activity_at: None,
            agent_activity_status: FleetAgentActivityStatus::Unavailable,
            agent_activity: None,
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
        repo.create_task(&task_row("t1", "p1", OrchestratorTaskStatus::Running))
            .await
            .unwrap();
        repo.create_task(&task_row("t2", "p2", OrchestratorTaskStatus::Preparing))
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

    /// Business Logic（为什么需要这个测试）:
    ///     中间 path 失效时，剩余摘要必须仍绑定正确 shortcut，不能 index zip 错绑。
    ///
    /// Code Logic（这个测试做什么）:
    ///     请求 paths A/B/C，owner 返回 A、unavailable、C；remap 后 shortcut 顺序与 metrics 对齐。
    #[test]
    fn remap_joins_by_requested_path_not_sparse_index() {
        let path_a = "/tmp/proj-a";
        let path_b = "/tmp/proj-b-missing";
        let path_c = "/tmp/proj-c";
        let sc_a = shortcut_row("remote:owner-1:hash-a", "Shortcut A", path_a);
        let sc_b = shortcut_row("remote:owner-1:hash-b", "Shortcut B", path_b);
        let sc_c = shortcut_row("remote:owner-1:hash-c", "Shortcut C", path_c);
        let mut path_to_shortcut = HashMap::new();
        path_to_shortcut.insert(path_a.to_string(), sc_a);
        path_to_shortcut.insert(path_b.to_string(), sc_b);
        path_to_shortcut.insert(path_c.to_string(), sc_c);

        // owner 按请求顺序返回：A live / B unavailable / C live（不再跳过 B）
        let owner_projects = vec![
            owner_summary("local-a", "P-a", "local"),
            unavailable_for_request(FLEET_UNRESOLVED_PROJECT_ID, "proj-b-missing"),
            owner_summary("local-c", "P-c", "local"),
        ];
        let requested = vec![path_a.to_string(), path_b.to_string(), path_c.to_string()];

        let remapped =
            remap_remote_projects("owner-1", owner_projects, &requested, &path_to_shortcut);

        assert_eq!(remapped.len(), 3);
        assert_eq!(remapped[0].project_id, "remote:owner-1:hash-a");
        assert_eq!(remapped[0].display_name, "Shortcut A");
        assert_eq!(remapped[0].project_kind, "remote");
        assert_eq!(remapped[0].agent_counts.working, 1);

        assert_eq!(remapped[1].project_id, "remote:owner-1:hash-b");
        assert_eq!(remapped[1].display_name, "Shortcut B");
        assert_eq!(remapped[1].project_kind, FLEET_PROJECT_KIND_UNAVAILABLE);
        assert_eq!(remapped[1].agent_counts.working, 0);

        assert_eq!(remapped[2].project_id, "remote:owner-1:hash-c");
        assert_eq!(remapped[2].display_name, "Shortcut C");
        assert_eq!(remapped[2].project_kind, "remote");
        assert_eq!(remapped[2].agent_counts.working, 1);

        // 旧 index-zip 会把 C 的 metrics 错绑到 B；此处明确否定
        assert_ne!(remapped[1].project_id, "remote:owner-1:hash-c");
        for p in &remapped {
            assert!(!p.project_id.starts_with('/'));
            assert!(!p.project_id.contains(":\\"));
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     owner 响应比请求 path 短时，尾部槽必须合成 unavailable，不得越界 panic。
    ///
    /// Code Logic（这个测试做什么）:
    ///     owner 仅 1 条、paths 2 条 → 槽 1 unavailable 且绑定正确 shortcut id。
    #[test]
    fn remap_synthesizes_unavailable_for_missing_owner_slots() {
        let path_a = "/tmp/a";
        let path_b = "/tmp/b";
        let mut map = HashMap::new();
        map.insert(path_a.to_string(), shortcut_row("remote:d:a", "A", path_a));
        map.insert(path_b.to_string(), shortcut_row("remote:d:b", "B", path_b));
        let owner_projects = vec![owner_summary("la", "A", "local")];
        let requested = vec![path_a.to_string(), path_b.to_string()];
        let remapped = remap_remote_projects("d", owner_projects, &requested, &map);
        assert_eq!(remapped.len(), 2);
        assert_eq!(remapped[0].project_id, "remote:d:a");
        assert_eq!(remapped[0].project_kind, "remote");
        assert_eq!(remapped[1].project_id, "remote:d:b");
        assert_eq!(remapped[1].project_kind, FLEET_PROJECT_KIND_UNAVAILABLE);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     owner batch 必须真正拒绝 remote project id，而不是只构造 AppError 常量。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用 build_owner_device_summary → local_project_required。
    #[tokio::test]
    async fn build_owner_rejects_remote_project_ids() {
        let state = fleet_test_state("owner-dev").await;
        let err = build_owner_device_summary(
            &state,
            &LanFleetOwnerBatchReq {
                project_ids: vec!["remote:d:p".into()],
                project_paths: Vec::new(),
            },
        )
        .await
        .expect_err("remote id must fail");
        assert_eq!(err.code(), "local_project_required");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     owner batch >100 必须 resource_limit（真实校验分支）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     101 个 project_ids 调用 build_owner_device_summary。
    #[tokio::test]
    async fn build_owner_caps_projects_at_resource_limit() {
        let state = fleet_test_state("owner-dev").await;
        let ids: Vec<String> = (0..=FLEET_OWNER_BATCH_MAX_PROJECTS)
            .map(|i| format!("p{i}"))
            .collect();
        assert!(ids.len() > FLEET_OWNER_BATCH_MAX_PROJECTS);
        let err = build_owner_device_summary(
            &state,
            &LanFleetOwnerBatchReq {
                project_ids: ids,
                project_paths: Vec::new(),
            },
        )
        .await
        .expect_err("oversize must fail");
        assert_eq!(err.code(), "resource_limit");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     失效 path 必须返回 unavailable 占位且不泄漏绝对 path；命中 path 保留 local id。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 A/C，请求 A/B/C paths → 三条；B unavailable；无绝对 path project_id。
    #[tokio::test]
    async fn build_owner_emits_unavailable_for_missing_paths_in_request_order() {
        let state = fleet_test_state("owner-dev").await;
        insert_project_at(&state.db, "local-a", "local", "/tmp/proj-a").await;
        insert_project_at(&state.db, "local-c", "local", "/tmp/proj-c").await;

        let resp = build_owner_device_summary(
            &state,
            &LanFleetOwnerBatchReq {
                project_ids: Vec::new(),
                project_paths: vec![
                    "/tmp/proj-a".into(),
                    "/tmp/proj-b-missing".into(),
                    "/tmp/proj-c".into(),
                ],
            },
        )
        .await
        .expect("batch ok");

        assert_eq!(resp.device.projects.len(), 3);
        assert_eq!(resp.device.projects[0].project_id, "local-a");
        assert_ne!(
            resp.device.projects[0].project_kind,
            FLEET_PROJECT_KIND_UNAVAILABLE
        );
        assert_eq!(
            resp.device.projects[1].project_kind,
            FLEET_PROJECT_KIND_UNAVAILABLE
        );
        assert_eq!(
            resp.device.projects[1].project_id,
            FLEET_UNRESOLVED_PROJECT_ID
        );
        assert_eq!(resp.device.projects[2].project_id, "local-c");

        for p in &resp.device.projects {
            assert!(
                !p.project_id.starts_with('/'),
                "project_id must not be absolute path: {}",
                p.project_id
            );
            assert!(!p.project_id.contains(":\\"));
        }

        // 端到端：owner 响应 + path 键 remap 后 B 绑定正确 shortcut，C 不被错绑
        let mut map = HashMap::new();
        map.insert(
            "/tmp/proj-a".into(),
            shortcut_row("remote:owner-dev:sa", "SA", "/tmp/proj-a"),
        );
        map.insert(
            "/tmp/proj-b-missing".into(),
            shortcut_row("remote:owner-dev:sb", "SB", "/tmp/proj-b-missing"),
        );
        map.insert(
            "/tmp/proj-c".into(),
            shortcut_row("remote:owner-dev:sc", "SC", "/tmp/proj-c"),
        );
        let paths = vec![
            "/tmp/proj-a".to_string(),
            "/tmp/proj-b-missing".to_string(),
            "/tmp/proj-c".to_string(),
        ];
        let remapped = remap_remote_projects("owner-dev", resp.device.projects, &paths, &map);
        assert_eq!(remapped[0].project_id, "remote:owner-dev:sa");
        assert_eq!(remapped[1].project_id, "remote:owner-dev:sb");
        assert_eq!(remapped[1].project_kind, FLEET_PROJECT_KIND_UNAVAILABLE);
        assert_eq!(remapped[2].project_id, "remote:owner-dev:sc");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     缺失 local project id 也必须 unavailable 占位，不能静默省略。
    ///
    /// Code Logic（这个测试做什么）:
    ///     请求存在 id + 不存在 id → 两条，第二条 unavailable。
    #[tokio::test]
    async fn build_owner_emits_unavailable_for_missing_project_ids() {
        let state = fleet_test_state("owner-dev").await;
        insert_project_at(&state.db, "exists", "local", "/tmp/exists").await;
        let resp = build_owner_device_summary(
            &state,
            &LanFleetOwnerBatchReq {
                project_ids: vec!["exists".into(), "gone".into()],
                project_paths: Vec::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(resp.device.projects.len(), 2);
        assert_eq!(resp.device.projects[0].project_id, "exists");
        assert_eq!(resp.device.projects[1].project_id, "gone");
        assert_eq!(
            resp.device.projects[1].project_kind,
            FLEET_PROJECT_KIND_UNAVAILABLE
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     offline 时 cache 命中必须标 cached，且不清除其它 device 的 live 数据语义。
    ///
    /// Code Logic（这个测试做什么）:
    ///     put cache → offline_or_cached → Offline+Cached+error_code。
    #[test]
    fn offline_or_cached_preserves_last_live_projects() {
        let cache = FleetDisplayCache::new();
        let shared = Arc::new(cache);
        shared.put(LanFleetDeviceSummary {
            device_id: "d1".into(),
            device_name: "One".into(),
            reachability: FleetReachability::Live,
            freshness: FleetFreshness::Live,
            scheduler_slots_used: Some(2),
            scheduler_slots_max: Some(4),
            projects: vec![owner_summary("p1", "P1", "remote")],
            error_code: None,
            captured_at: Some("2026-07-15T00:00:00Z".into()),
        });
        let out = offline_or_cached(&shared, "d1", "One", "timeout");
        assert_eq!(out.reachability, FleetReachability::Offline);
        assert_eq!(out.freshness, FleetFreshness::Cached);
        assert_eq!(out.error_code.as_deref(), Some("timeout"));
        assert_eq!(out.projects.len(), 1);
        assert_eq!(out.projects[0].project_id, "p1");

        let miss = offline_or_cached(&shared, "d2", "Two", "peer_error");
        assert_eq!(miss.reachability, FleetReachability::Offline);
        assert_eq!(miss.freshness, FleetFreshness::Unknown);
        assert!(miss.projects.is_empty());
    }
}
