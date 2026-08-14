//! agent_hub/project_scope — Workbench 项目 opt-in 与 checkout binding
//!
//! Business Logic（为什么需要这个模块）:
//!     Multi-CLI Agent Hub 只能在用户确认后写入项目仓库；opt-in 覆盖主 checkout 与
//!     Workbench 登记的 worktree，未 opt-in 时预览零写入，外部未登记 worktree 永不自动写入。
//!
//! Code Logic（这个模块做什么）:
//!     提供 build_project_enable_preview / enable_project_scope / refresh_checkout_bindings；
//!     读写 agent_hub_project_mappings + agent_hub_checkout_bindings，并挂 Workbench 生命周期钩子。

use crate::agent_hub::models::{NewScopeNode, ScopeKind};
use crate::commands::workbench::ensure_main_worktree;
use crate::error::AppError;
use crate::state::AppState;
use crate::storage::{
    AgentHubCheckoutBindingRow, UpsertAgentHubCheckoutBinding, UpsertAgentHubProjectMapping,
};
use crate::workbench::git as workbench_git;
use crate::workbench::projects::{normalize_git_remote_fingerprint, read_git_remote_url};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// 启用 Agent Hub 项目作用域请求。
///
/// Business Logic（为什么需要这个结构体）:
///     用户必须显式 confirm 才允许创建 mapping/bindings，防止静默 opt-in。
///
/// Code Logic（这个结构体做什么）:
///     camelCase：project_id + confirm。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnableAgentHubProjectRequest {
    /// 本机 Workbench project id
    pub project_id: String,
    /// 必须为 true 才真正启用
    pub confirm: bool,
}

/// 单个 checkout 绑定（含运行时 dirty 统计）。
///
/// Business Logic（为什么需要这个结构体）:
///     前端/status 需要看到 binding 状态、冲突警告与 Git dirty，且不得暴露到 portable asset。
///
/// Code Logic（这个结构体做什么）:
///     camelCase DTO；dirty/changed_files 来自实时 git status。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCheckoutBinding {
    pub id: String,
    pub hub_project_id: String,
    pub workbench_worktree_id: Option<String>,
    /// "main" | "worktree"
    pub checkout_kind: String,
    pub local_absolute_path: String,
    pub relative_root: Option<String>,
    pub enabled: bool,
    /// active | detached | blocked
    pub status: String,
    pub warning: Option<String>,
    pub dirty: bool,
    pub changed_files: usize,
    pub created_at: String,
    pub updated_at: String,
}

/// 启用前预览。
///
/// Business Logic（为什么需要这个结构体）:
///     用户 opt-in 前必须看到将覆盖的 checkout、planned actions 与不 commit 承诺。
///
/// Code Logic（这个结构体做什么）:
///     聚合 project 身份、checkouts、planned_actions、warnings 与固定 notice。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubProjectPreview {
    pub project_id: String,
    pub hub_project_id: Option<String>,
    pub opted_in: bool,
    pub checkouts: Vec<PreviewCheckoutEntry>,
    pub planned_actions: Vec<PreviewPlannedAction>,
    pub warnings: Vec<String>,
    pub no_commit_notice: String,
    pub git_remote_fingerprint: Option<String>,
}

/// 预览中的单个 checkout。
///
/// Business Logic（为什么需要这个结构体）:
///     仅列出 main + 已登记 worktree，并报告 dirty / 预存在 AGENTS.md 冲突。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 条目；would_block 表示冲突 AGENTS.md。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCheckoutEntry {
    pub workbench_worktree_id: Option<String>,
    pub path: String,
    pub is_main: bool,
    pub dirty: bool,
    pub changed: usize,
    pub conflicts: usize,
    pub branch: Option<String>,
    pub would_block: bool,
    pub block_reason: Option<String>,
}

/// 预览中的计划动作（零写入）。
///
/// Business Logic（为什么需要这个结构体）:
///     用户需要知道 opt-in 后各 target 文件是 create/keep/skip。
///
/// Code Logic（这个结构体做什么）:
///     relative_path + action + 可选 target + detail。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPlannedAction {
    pub relative_path: String,
    /// create | modify | keep | skip
    pub action: String,
    pub target: Option<String>,
    pub detail: String,
}

/// 启用后项目状态。
///
/// Business Logic（为什么需要这个结构体）:
///     enable 成功后返回 mapping + bindings + warnings，供 UI 确认。
///
/// Code Logic（这个结构体做什么）:
///     camelCase status 聚合。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubProjectStatus {
    pub project_id: String,
    pub hub_project_id: String,
    pub opted_in: bool,
    pub git_remote_fingerprint: Option<String>,
    pub bindings: Vec<ProjectCheckoutBinding>,
    pub warnings: Vec<String>,
}

const NO_COMMIT_NOTICE: &str = "cc-partner 不会 commit/push 项目仓库";
const AGENTS_CONFLICT_WARNING: &str =
    "检出根目录已存在 AGENTS.md，Hub 不会覆盖该文件；本 checkout 投影标记为 blocked";

/// 构建项目启用预览（零写入）。
///
/// Business Logic（为什么需要这个函数）:
///     用户 opt-in 前必须看到将纳管的 checkout、脏状态与计划动作，且绝不能改项目仓库。
///
/// Code Logic（这个函数做什么）:
///     加载 local 项目、ensure_main、仅列已登记 worktree + main；读 git status 与 AGENTS.md 存在性；
///     生成 planned_actions，不写任何文件。
pub async fn build_project_enable_preview(
    state: &AppState,
    project_id: &str,
) -> Result<AgentHubProjectPreview, AppError> {
    let project = load_local_project(state, project_id).await?;
    let main = ensure_main_worktree(state, &project).await?;
    let registered = state
        .workbench_worktree_repo
        .list_by_project(project_id)
        .await?;

    let mapping = state
        .agent_hub_repo
        .get_project_mapping_by_local_workbench_id(project_id)
        .await?;
    let opted_in = mapping.as_ref().map(|m| m.opted_in).unwrap_or(false);
    let hub_project_id = mapping.as_ref().map(|m| m.hub_project_id.clone());
    let fingerprint = read_git_remote_url(Path::new(&project.path))
        .map(|url| normalize_git_remote_fingerprint(&url));

    let mut checkouts = Vec::new();
    let mut warnings = Vec::new();
    let mut seen_paths = HashSet::new();

    // main first
    push_preview_checkout(
        &mut checkouts,
        &mut warnings,
        &mut seen_paths,
        Some(main.id.as_str()),
        &main.path,
        true,
    )?;
    for row in registered.iter().filter(|r| !r.is_main) {
        push_preview_checkout(
            &mut checkouts,
            &mut warnings,
            &mut seen_paths,
            Some(row.id.as_str()),
            &row.path,
            false,
        )?;
    }

    let planned_actions = build_planned_actions_for_path(Path::new(&project.path));
    Ok(AgentHubProjectPreview {
        project_id: project_id.to_string(),
        hub_project_id,
        opted_in,
        checkouts,
        planned_actions,
        warnings,
        no_commit_notice: NO_COMMIT_NOTICE.to_string(),
        git_remote_fingerprint: fingerprint,
    })
}

/// 确认启用项目 Agent Hub 作用域。
///
/// Business Logic（为什么需要这个函数）:
///     用户 confirm 后创建 portable hubProjectId 映射与 checkout bindings，后续 worktree 继承 opt-in。
///
/// Code Logic（这个函数做什么）:
///     confirm 必须 true；已 opt-in 则 refresh 返回；否则 UUID hub_id + project ScopeNode + mapping + refresh。
pub async fn enable_project_scope(
    state: &AppState,
    request: EnableAgentHubProjectRequest,
) -> Result<AgentHubProjectStatus, AppError> {
    if !request.confirm {
        return Err(AppError::validation(
            "启用 Agent Hub 项目作用域需要 confirm=true",
        ));
    }
    let project = load_local_project(state, &request.project_id).await?;
    let existing = state
        .agent_hub_repo
        .get_project_mapping_by_local_workbench_id(&request.project_id)
        .await?;

    let hub_project_id = if let Some(mapping) = existing {
        if mapping.opted_in {
            let bindings = refresh_checkout_bindings(state, &request.project_id).await?;
            let warnings = collect_binding_warnings(&bindings);
            return Ok(AgentHubProjectStatus {
                project_id: request.project_id,
                hub_project_id: mapping.hub_project_id,
                opted_in: true,
                git_remote_fingerprint: mapping.git_remote_fingerprint,
                bindings,
                warnings,
            });
        }
        mapping.hub_project_id
    } else {
        uuid::Uuid::new_v4().to_string()
    };

    // 幂等创建 project scope（相对路径为空，绝不写绝对路径）
    if state
        .agent_hub_repo
        .get_project_scope_by_hub_project_id(&hub_project_id)
        .await?
        .is_none()
    {
        state
            .agent_hub_repo
            .insert_scope(NewScopeNode {
                id: None,
                kind: ScopeKind::Project,
                hub_project_id: Some(hub_project_id.clone()),
                relative_path: Some(String::new()),
            })
            .await?;
    }

    let fingerprint = read_git_remote_url(Path::new(&project.path))
        .map(|url| normalize_git_remote_fingerprint(&url));
    let mapping = state
        .agent_hub_repo
        .upsert_project_mapping(UpsertAgentHubProjectMapping {
            hub_project_id: hub_project_id.clone(),
            local_workbench_project_id: Some(request.project_id.clone()),
            git_remote_fingerprint: fingerprint.clone(),
            local_absolute_path: Some(project.path.clone()),
            opted_in: true,
        })
        .await?;

    let bindings = refresh_checkout_bindings(state, &request.project_id).await?;
    let warnings = collect_binding_warnings(&bindings);
    if let Err(e) = crate::agent_hub::projection_ops::ensure_agent_hub_enabled(state).await {
        tracing::warn!(error = %e, "agent_hub enable_project_scope ensure enabled failed");
    }
    Ok(AgentHubProjectStatus {
        project_id: request.project_id,
        hub_project_id: mapping.hub_project_id,
        opted_in: true,
        git_remote_fingerprint: mapping.git_remote_fingerprint,
        bindings,
        warnings,
    })
}

/// 刷新项目 checkout bindings（幂等）。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 增删/登记 worktree 后需同步 binding：新 worktree 继承 opt-in，删除标记 detached，
///     冲突 AGENTS.md 标记 blocked；未 opt-in 时不创建任何 binding。
///
/// Code Logic（这个函数做什么）:
///     若未 opt-in 返回空；否则 ensure_main + 登记 worktree upsert active/blocked，
///     缺失 worktree 的 binding mark detached；runtime dirty 填 DTO；
///     已 opt-in 时 best-effort `schedule_project_projections`。
pub async fn refresh_checkout_bindings(
    state: &AppState,
    project_id: &str,
) -> Result<Vec<ProjectCheckoutBinding>, AppError> {
    let mapping = state
        .agent_hub_repo
        .get_project_mapping_by_local_workbench_id(project_id)
        .await?;
    let Some(mapping) = mapping.filter(|m| m.opted_in) else {
        return Ok(Vec::new());
    };

    let project = load_local_project(state, project_id).await?;
    let main = ensure_main_worktree(state, &project).await?;
    let registered = state
        .workbench_worktree_repo
        .list_by_project(project_id)
        .await?;

    let mut active_worktree_ids: HashSet<String> = HashSet::new();
    // main
    upsert_registered_binding(
        state,
        &mapping.hub_project_id,
        Some(main.id.as_str()),
        "main",
        &main.path,
    )
    .await?;
    active_worktree_ids.insert(main.id.clone());

    for row in registered.iter().filter(|r| !r.is_main) {
        upsert_registered_binding(
            state,
            &mapping.hub_project_id,
            Some(row.id.as_str()),
            "worktree",
            &row.path,
        )
        .await?;
        active_worktree_ids.insert(row.id.clone());
    }

    let all = state
        .agent_hub_repo
        .list_checkout_bindings_by_hub_project(&mapping.hub_project_id)
        .await?;
    for row in &all {
        let still_active = row
            .workbench_worktree_id
            .as_ref()
            .map(|id| active_worktree_ids.contains(id))
            .unwrap_or(false);
        if !still_active && row.status != "detached" {
            state
                .agent_hub_repo
                .mark_checkout_binding_detached(&row.id)
                .await?;
        }
    }

    let refreshed = state
        .agent_hub_repo
        .list_checkout_bindings_by_hub_project(&mapping.hub_project_id)
        .await?;
    let dto = refreshed
        .into_iter()
        .map(binding_row_to_dto)
        .collect::<Vec<_>>();

    // 已 opt-in 路径才到达此处；best-effort 调度项目指令投影。
    if let Err(e) =
        crate::agent_hub::projection_ops::schedule_project_projections(state, project_id).await
    {
        tracing::warn!(
            project_id = %project_id,
            error = %e,
            "agent_hub refresh_checkout_bindings schedule projections failed"
        );
    }
    Ok(dto)
}

/// 加载本机 Workbench 项目；remote shortcut 拒绝。
///
/// Business Logic（为什么需要这个函数）:
///     Hub opt-in 只作用于本机项目，remote shortcut 没有本机 checkout 可写。
///
/// Code Logic（这个函数做什么）:
///     get_project；kind==remote 返回 validation 错误。
async fn load_local_project(
    state: &AppState,
    project_id: &str,
) -> Result<crate::workbench::models::WorkbenchProjectRow, AppError> {
    let project = state
        .workbench_project_repo
        .get(project_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台项目不存在"))?;
    if project.kind == "remote" {
        return Err(AppError::validation(
            "Agent Hub 项目 opt-in 仅支持本机 Workbench 项目，不支持 remote shortcut",
        ));
    }
    Ok(project)
}

/// 把一个 checkout 加入 preview 列表。
///
/// Business Logic（为什么需要这个函数）:
///     preview 需要 dirty 与 AGENTS.md 冲突提示，且不得重复列同一路径。
///
/// Code Logic（这个函数做什么）:
///     git status；检测 AGENTS.md；填充 PreviewCheckoutEntry。
fn push_preview_checkout(
    checkouts: &mut Vec<PreviewCheckoutEntry>,
    warnings: &mut Vec<String>,
    seen_paths: &mut HashSet<String>,
    worktree_id: Option<&str>,
    path: &str,
    is_main: bool,
) -> Result<(), AppError> {
    if !seen_paths.insert(path.to_string()) {
        return Ok(());
    }
    let status = workbench_git::status(Path::new(path)).unwrap_or_default();
    let agents = Path::new(path).join("AGENTS.md");
    let would_block = agents.is_file();
    let block_reason = if would_block {
        let reason = AGENTS_CONFLICT_WARNING.to_string();
        warnings.push(format!("{path}: {reason}"));
        Some(reason)
    } else {
        None
    };
    checkouts.push(PreviewCheckoutEntry {
        workbench_worktree_id: worktree_id.map(str::to_string),
        path: path.to_string(),
        is_main,
        dirty: !status.clean || status.changed > 0 || status.conflicts > 0,
        changed: status.changed,
        conflicts: status.conflicts,
        branch: status.branch,
        would_block,
        block_reason,
    });
    Ok(())
}

/// 根据项目根文件存在性生成 planned actions（不写盘）。
///
/// Business Logic（为什么需要这个函数）:
///     preview 需说明 Claude/Codex/OpenCode 目标文件是 create 还是 keep/skip。
///
/// Code Logic（这个函数做什么）:
///     检查 CLAUDE.md / AGENTS.md / AGENTS.override.md 存在性并生成动作。
fn build_planned_actions_for_path(root: &Path) -> Vec<PreviewPlannedAction> {
    use crate::workbench::agent_runtime::opencode_bridge::{
        OpenCodeRuntimeBridge, OPENCODE_RUNTIME_BRIDGE_REL_PATH,
        OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH,
    };

    let targets = [
        ("CLAUDE.md", Some("claude"), "Claude Code 项目指令"),
        ("AGENTS.md", Some("opencode"), "OpenCode/共享 AGENTS.md"),
        (
            "AGENTS.override.md",
            Some("codex"),
            "Codex AGENTS.override.md",
        ),
    ];
    let mut actions = Vec::new();
    for (rel, target, detail_base) in targets {
        let path = root.join(rel);
        if path.is_file() {
            if rel == "AGENTS.md" {
                actions.push(PreviewPlannedAction {
                    relative_path: rel.to_string(),
                    action: "skip".to_string(),
                    target: target.map(str::to_string),
                    detail: format!("{detail_base}：已存在冲突文件，跳过覆盖"),
                });
            } else {
                actions.push(PreviewPlannedAction {
                    relative_path: rel.to_string(),
                    action: "keep".to_string(),
                    target: target.map(str::to_string),
                    detail: format!("{detail_base}：已存在，预览阶段保持不动"),
                });
            }
        } else {
            actions.push(PreviewPlannedAction {
                relative_path: rel.to_string(),
                action: "create".to_string(),
                target: target.map(str::to_string),
                detail: format!("{detail_base}：opt-in 后由 projection 创建（本预览不写盘）"),
            });
        }
    }

    // OpenCode runtime bridge：app-version 派生物，必须出现在 opt-in preview，不进 Snapshot。
    let bridge_path = root.join(OPENCODE_RUNTIME_BRIDGE_REL_PATH);
    if bridge_path.is_file() {
        match std::fs::read(&bridge_path) {
            Ok(bytes) => match OpenCodeRuntimeBridge::classify_reserved_path(
                OPENCODE_RUNTIME_BRIDGE_REL_PATH,
                &bytes,
            ) {
                Some(true) => actions.push(PreviewPlannedAction {
                    relative_path: OPENCODE_RUNTIME_BRIDGE_REL_PATH.to_string(),
                    action: "keep".to_string(),
                    target: Some("opencode".to_string()),
                    detail: format!(
                        "OpenCode runtime bridge：已是当前 app 派生字节（hash={OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH}）"
                    ),
                }),
                Some(false) => actions.push(PreviewPlannedAction {
                    relative_path: OPENCODE_RUNTIME_BRIDGE_REL_PATH.to_string(),
                    action: "skip".to_string(),
                    target: Some("opencode".to_string()),
                    detail: "OpenCode runtime bridge：externalCollision，保留外部文件且不覆盖".to_string(),
                }),
                None => {}
            },
            Err(_) => actions.push(PreviewPlannedAction {
                relative_path: OPENCODE_RUNTIME_BRIDGE_REL_PATH.to_string(),
                action: "skip".to_string(),
                target: Some("opencode".to_string()),
                detail: "OpenCode runtime bridge：路径不可读，跳过".to_string(),
            }),
        }
    } else {
        actions.push(PreviewPlannedAction {
            relative_path: OPENCODE_RUNTIME_BRIDGE_REL_PATH.to_string(),
            action: "create".to_string(),
            target: Some("opencode".to_string()),
            detail: format!(
                "OpenCode runtime bridge：opt-in 后 materialize 派生 Plugin（hash={OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH}；非 canonical Snapshot）"
            ),
        });
    }
    actions
}

/// 对已登记 checkout upsert binding。
///
/// Business Logic（为什么需要这个函数）:
///     opt-in 项目的每个 Workbench checkout 都要有 active/blocked binding，含本地绝对路径。
///
/// Code Logic（这个函数做什么）:
///     检测 AGENTS.md → blocked+warning，否则 active；enabled=true。
async fn upsert_registered_binding(
    state: &AppState,
    hub_project_id: &str,
    worktree_id: Option<&str>,
    checkout_kind: &str,
    absolute_path: &str,
) -> Result<AgentHubCheckoutBindingRow, AppError> {
    let agents = Path::new(absolute_path).join("AGENTS.md");
    let (status, warning) = if agents.is_file() {
        ("blocked", Some(AGENTS_CONFLICT_WARNING.to_string()))
    } else {
        ("active", None)
    };
    state
        .agent_hub_repo
        .upsert_checkout_binding(UpsertAgentHubCheckoutBinding {
            hub_project_id: hub_project_id.to_string(),
            workbench_worktree_id: worktree_id.map(str::to_string),
            checkout_kind: checkout_kind.to_string(),
            relative_root: Some(String::new()),
            local_absolute_path: Some(absolute_path.to_string()),
            enabled: true,
            status: status.to_string(),
            warning,
        })
        .await
}

/// binding 行转 DTO，并填充 runtime dirty。
///
/// Business Logic（为什么需要这个函数）:
///     UI 需要实时 dirty/changed，但不持久化这些瞬时字段。
///
/// Code Logic（这个函数做什么）:
///     对 local_absolute_path 调 git status；失败当 clean。
fn binding_row_to_dto(row: AgentHubCheckoutBindingRow) -> ProjectCheckoutBinding {
    let path = row.local_absolute_path.clone().unwrap_or_default();
    let (dirty, changed_files) = if path.is_empty() {
        (false, 0)
    } else {
        match workbench_git::status(Path::new(&path)) {
            Ok(st) => (!st.clean || st.changed > 0 || st.conflicts > 0, st.changed),
            Err(_) => (false, 0),
        }
    };
    ProjectCheckoutBinding {
        id: row.id,
        hub_project_id: row.hub_project_id,
        workbench_worktree_id: row.workbench_worktree_id,
        checkout_kind: row.checkout_kind,
        local_absolute_path: path,
        relative_root: row.relative_root,
        enabled: row.enabled,
        status: row.status,
        warning: row.warning,
        dirty,
        changed_files,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// 从 bindings 收集 warning 文案。
///
/// Business Logic（为什么需要这个函数）:
///     status 响应需要顶层 warnings 列表方便 UI 提示。
///
/// Code Logic（这个函数做什么）:
///     收集非空 warning 字段。
fn collect_binding_warnings(bindings: &[ProjectCheckoutBinding]) -> Vec<String> {
    bindings
        .iter()
        .filter_map(|b| {
            b.warning
                .as_ref()
                .map(|w| format!("{}: {w}", b.local_absolute_path))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::authority::RuntimeRole;
    use crate::backend::event_bus::RuntimeEventBus;
    use crate::backend::runtime_metrics::RuntimeMetrics;
    use crate::backend::ui::HeadlessBackendUi;
    use crate::cloud_sync::runtime::CloudSyncRuntime;
    use crate::config::{
        AppConfig, BatteryConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
    };
    use crate::config_runtime::ConfigRuntime;
    use crate::config_store::MemoryConfigStore;
    use crate::net::peer_client::PeerClient;
    use crate::orchestrator::repo::OrchestratorRepo;
    use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
    use crate::storage::maintenance_gate::DatabaseMaintenanceGate;
    use crate::storage::{
        AgentHubRepo, ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, ScratchpadRepo, SshTargetRepo,
        TransferRepo, WorkbenchAgentSessionRepo, WorkbenchBrowserRepo, WorkbenchProjectRepo,
        WorkbenchSessionRepo, WorkbenchWorkspaceLayoutRepo, WorkbenchWorktreeRepo,
    };
    use crate::transfer::registry::TransferRegistry;
    use crate::updater::UpdateRuntime;
    use crate::workbench::models::{WorkbenchProjectRow, WorkbenchWorktreeRow};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::str::FromStr;
    use std::sync::atomic::AtomicU16;
    use std::sync::{Arc, Mutex, RwLock};

    /// 创建临时 Git 仓库 fixture。
    ///
    /// Business Logic: 集成测需要真实 git status/worktree。
    /// Code Logic: git init + 初始提交 + origin remote。
    fn init_git_repo(root: &Path, origin: &str) {
        run_ok(root, &["git", "init", "-b", "main"]);
        run_ok(root, &["git", "config", "user.email", "test@example.com"]);
        run_ok(root, &["git", "config", "user.name", "test"]);
        std::fs::write(root.join("README.md"), "hello\n").unwrap();
        run_ok(root, &["git", "add", "README.md"]);
        run_ok(root, &["git", "commit", "-m", "init"]);
        run_ok(root, &["git", "remote", "add", "origin", origin]);
    }

    /// 执行命令并断言成功。
    ///
    /// Business Logic: fixture 构造失败应立刻暴露。
    /// Code Logic: Command status success。
    fn run_ok(cwd: &Path, args: &[&str]) {
        let status = Command::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .status()
            .expect("spawn");
        assert!(status.success(), "command failed: {args:?}");
    }

    /// 命令 stdout 文本。
    ///
    /// Business Logic: 断言 HEAD / porcelain 前后不变。
    /// Code Logic: output utf8 trim。
    fn run_stdout(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .output()
            .expect("spawn");
        assert!(out.status.success(), "failed {:?}", args);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// 构建带 workbench + agent hub schema 的 AppState。
    ///
    /// Business Logic: project_scope 集成测需要完整 AppState 字段。
    /// Code Logic: 复制 build_restore_fail_state 模式 + AgentHubRepo::ensure_schema + tempfile db。
    async fn build_hub_state(db_path: &Path) -> AppState {
        let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
        let options = SqliteConnectOptions::from_str(&db_url)
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();

        for stmt in [
            "CREATE TABLE IF NOT EXISTS workbench_projects (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, device_id TEXT NOT NULL,
                device_name TEXT NOT NULL, path TEXT NOT NULL, last_opened_at TEXT NOT NULL,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS workbench_worktrees (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT NOT NULL, branch TEXT,
                base_branch TEXT, path TEXT NOT NULL, is_main INTEGER NOT NULL,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS workbench_sessions (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL, worktree_id TEXT, name TEXT NOT NULL,
                name_source TEXT NOT NULL DEFAULT 'default', command TEXT NOT NULL, cwd TEXT, status TEXT NOT NULL, cols INTEGER NOT NULL,
                rows INTEGER NOT NULL, started_at TEXT NOT NULL, exited_at TEXT, exit_code INTEGER,
                backend TEXT NOT NULL, backend_id TEXT, backend_window_id TEXT,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS workbench_mutation_operations (
                client_operation_id TEXT PRIMARY KEY NOT NULL,
                kind TEXT NOT NULL,
                payload_hash TEXT NOT NULL,
                intent_json TEXT NOT NULL,
                state TEXT NOT NULL,
                outcome_json TEXT,
                error_message TEXT,
                project_id TEXT,
                worktree_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL)",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        AgentHubRepo::ensure_schema(&pool).await.unwrap();

        let project_repo = WorkbenchProjectRepo::new(pool.clone());
        let worktree_repo = WorkbenchWorktreeRepo::new(pool.clone());
        let session_repo = WorkbenchSessionRepo::new(pool.clone());
        let layout_repo = WorkbenchWorkspaceLayoutRepo::new(pool.clone());
        layout_repo.ensure_schema().await.unwrap();
        let agent_hub = AgentHubRepo::new(pool.clone());

        let config = AppConfig {
            device_id: "d1".to_string(),
            device_name: "test".to_string(),
            http_port: 0,
            receive_dir: "/tmp".to_string(),
            game_plugin_dir: "/tmp/plugins".into(),
            db_path: db_path.to_string_lossy().to_string(),
            screenshot_hotkey: "<cmd>+s".to_string(),
            prompt_optimizer_hotkey: "<ctrl>".to_string(),
            prompt_optimizer_fill_language: "zh".to_string(),
            prompt_quick_input_hotkey: "<ctrl>+/".to_string(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            battery: BatteryConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            internal_claude: crate::config::InternalClaudeConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
        };
        let store = Arc::new(MemoryConfigStore::with_config(config.clone()));
        let config_runtime = Arc::new(ConfigRuntime::new(config, store));
        let config = config_runtime.shared_value();
        let maintenance_gate = Arc::new(DatabaseMaintenanceGate::new());
        let owner = uuid::Uuid::new_v4().to_string();
        let event_bus = Arc::new(RuntimeEventBus::new(owner));

        AppState {
            config,
            config_runtime,
            db: pool.clone(),
            maintenance_gate: maintenance_gate.clone(),
            prompt_repo: Arc::new(PromptRepo::new(pool.clone())),
            transfer_repo: Arc::new(TransferRepo::new(pool.clone())),
            claude_md_repo: Arc::new(ClaudeMdRepo::new(pool.clone())),
            scratchpad_repo: Arc::new(ScratchpadRepo::new(pool.clone())),
            ssh_target_repo: Arc::new(SshTargetRepo::new(pool.clone())),
            device_id: Arc::new("d1".to_string()),
            devices: Arc::new(RwLock::new(std::collections::HashMap::new())),
            actual_http_port: Arc::new(AtomicU16::new(0)),
            discovery: Arc::new(Mutex::new(None)),
            overlay_trusted_ips: Arc::new(RwLock::new(std::collections::HashSet::new())),
            manual_peer_cancel: Arc::new(Mutex::new(None)),
            peer_client: Arc::new(PeerClient::new()),
            transfers: Arc::new(TransferRegistry::new()),
            ui: Arc::new(HeadlessBackendUi::new(PathBuf::from("/tmp"))),
            update_runtime: Arc::new(UpdateRuntime::new()),
            cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
            workbench_project_repo: Arc::new(project_repo),
            workbench_session_repo: Arc::new(session_repo),
            workbench_worktree_repo: Arc::new(worktree_repo),
            workbench_browser_repo: Arc::new(WorkbenchBrowserRepo::new(pool.clone())),
            workbench_agent_session_repo: Arc::new(WorkbenchAgentSessionRepo::new(pool.clone())),
            agent_ledger_repo: Arc::new(crate::storage::AgentLedgerRepo::new(pool.clone())),
            agent_ledger_service: Arc::new(
                crate::workbench::agent_ledger::AgentLedgerService::new(
                    crate::storage::AgentLedgerRepo::new(pool.clone()),
                ),
            ),
            agent_hub_repo: Arc::new(agent_hub),
            workbench_workspace_layout_repo: Arc::new(layout_repo),
            workbench_project_note_repo: Arc::new(crate::storage::WorkbenchProjectNoteRepo::new(
                pool.clone(),
            )),
            browser_verification: Arc::new(
                crate::workbench::browser_verification::BrowserVerificationService::new(
                    Arc::new(crate::workbench::browser_verification::FakeEngine::succeeds()),
                    PathBuf::from("/tmp/browser-verification-hub-t5"),
                    "test-owner".into(),
                )
                .expect("browser verification fixture"),
            ),
            workbench_browser_previews: Arc::new(
                crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
            ),
            workbench_sessions: Arc::new(
                crate::workbench::sessions::WorkbenchSessionRegistry::new(),
            ),
            workbench_remote_events: Arc::new(
                crate::workbench::remote_events::WorkbenchRemoteEventBus::new("test-owner"),
            ),
            workbench_remote_event_bridges: Arc::new(
                crate::workbench::remote_events::RemoteEventBridgeRegistry::new(),
            ),
            workbench_dependency: Arc::new(
                crate::workbench::dependencies::WorkbenchDependencyInstallRuntime::new(),
            ),
            cc_collector_cancel: Arc::new(Mutex::new(None)),
            cloud_sync_runtime: Arc::new(CloudSyncRuntime::new()),
            cloud_sync_cancel: Arc::new(Mutex::new(None)),
            health: Arc::new(crate::health::HealthRuntime::new()),
            health_repo: Arc::new(crate::storage::health_repo::HealthRepo::new(pool.clone())),
            health_cancel: Arc::new(Mutex::new(None)),
            orchestrator_repo: Arc::new(OrchestratorRepo::new(pool.clone())),
            orchestrator_scheduler_telemetry: OrchestratorSchedulerTelemetry::default(),
            orchestrator_cancel: Arc::new(Mutex::new(None)),
            orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
            agent_ledger_cancel: Arc::new(Mutex::new(None)),
            agent_hub_cancel: Arc::new(Mutex::new(None)),
            agent_hub_git_runtime: Arc::new(crate::agent_hub::git::AgentHubGitRuntime::new()),
            agent_hub_git_cancel: Arc::new(Mutex::new(None)),
            workbench_claude_session_indexes: Arc::new(RwLock::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_watchers: Arc::new(Mutex::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_index_dispose_epochs: Arc::new(Mutex::new(
                std::collections::HashMap::new(),
            )),
            runtime_metrics: Arc::new(RuntimeMetrics::new()),
            runtime_role: RuntimeRole::HeadlessOwner,
            event_bus,
            backend_control_client_runtime: Arc::new(
                crate::backend::control_client::BackendControlClientRuntime::new(),
            ),
            gui_event_relay_cancel: Arc::new(Mutex::new(None)),
        }
    }

    /// 注册 Workbench 项目 + main + 一个已登记 worktree。
    ///
    /// Business Logic: preview 必须只列登记 worktree。
    /// Code Logic: upsert project/worktree rows；真实 git worktree add。
    async fn seed_project_with_worktrees(
        state: &AppState,
        project_id: &str,
        repo: &Path,
        registered_wt: &Path,
        external_wt: &Path,
    ) {
        let now = chrono::Utc::now().to_rfc3339();
        let path = repo.canonicalize().unwrap().to_string_lossy().to_string();
        state
            .workbench_project_repo
            .upsert(&WorkbenchProjectRow {
                id: project_id.to_string(),
                name: "hub-demo".to_string(),
                kind: "local".to_string(),
                device_id: "d1".to_string(),
                device_name: "test".to_string(),
                path: path.clone(),
                last_opened_at: now.clone(),
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .await
            .unwrap();

        // registered worktree
        run_ok(
            repo,
            &[
                "git",
                "worktree",
                "add",
                "-b",
                "feature-reg",
                registered_wt.to_str().unwrap(),
            ],
        );
        // external unregistered worktree
        run_ok(
            repo,
            &[
                "git",
                "worktree",
                "add",
                "-b",
                "feature-ext",
                external_wt.to_str().unwrap(),
            ],
        );

        let reg_path = registered_wt
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
        state
            .workbench_worktree_repo
            .upsert(&WorkbenchWorktreeRow {
                id: format!("{project_id}:wt-reg"),
                project_id: project_id.to_string(),
                name: "feature-reg".to_string(),
                branch: Some("feature-reg".to_string()),
                base_branch: Some("main".to_string()),
                path: reg_path,
                is_main: false,
                created_at: now.clone(),
                updated_at: now,
            })
            .await
            .unwrap();
    }

    #[test]
    fn normalize_git_remote_fingerprint_strips_suffix_and_lowercases() {
        assert_eq!(
            normalize_git_remote_fingerprint("https://GitHub.com/Org/Repo.git/"),
            "https://github.com/org/repo"
        );
        assert_eq!(
            normalize_git_remote_fingerprint("git@github.com:Org/Repo.git"),
            "git@github.com:org/repo"
        );
    }

    #[tokio::test]
    async fn preview_lists_registered_only_reports_dirty_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let registered = tmp.path().join("wt-reg");
        let external = tmp.path().join("wt-ext");
        let db = tmp.path().join("data.db");
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo, "https://GitHub.com/Org/Repo.git/");
        // dirty main
        std::fs::write(repo.join("README.md"), "dirty\n").unwrap();
        // conflicting AGENTS.md on main
        std::fs::write(repo.join("AGENTS.md"), "pre-existing agents\n").unwrap();

        let state = build_hub_state(&db).await;
        let project_id = "proj-hub-1";
        seed_project_with_worktrees(&state, project_id, &repo, &registered, &external).await;

        let before_head = run_stdout(&repo, &["git", "rev-parse", "HEAD"]);
        let before_porcelain = run_stdout(&repo, &["git", "status", "--porcelain"]);
        let before_agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();

        let preview = build_project_enable_preview(&state, project_id)
            .await
            .unwrap();
        assert!(!preview.opted_in);
        assert_eq!(
            preview.git_remote_fingerprint.as_deref(),
            Some("https://github.com/org/repo")
        );
        assert_eq!(preview.no_commit_notice, NO_COMMIT_NOTICE);
        // main + registered only
        assert_eq!(preview.checkouts.len(), 2);
        assert!(preview.checkouts.iter().any(|c| c.is_main));
        assert!(preview
            .checkouts
            .iter()
            .any(|c| c.workbench_worktree_id.as_deref() == Some("proj-hub-1:wt-reg")));
        assert!(!preview.checkouts.iter().any(|c| c.path.contains("wt-ext")));
        let main = preview.checkouts.iter().find(|c| c.is_main).unwrap();
        assert!(main.dirty);
        assert!(main.would_block);
        assert!(main.changed >= 1);
        assert!(!preview.planned_actions.is_empty());

        // zero writes
        assert_eq!(
            run_stdout(&repo, &["git", "rev-parse", "HEAD"]),
            before_head
        );
        assert_eq!(
            run_stdout(&repo, &["git", "status", "--porcelain"]),
            before_porcelain
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("AGENTS.md")).unwrap(),
            before_agents
        );
        // scope not created on preview
        assert!(state
            .agent_hub_repo
            .get_project_mapping_by_local_workbench_id(project_id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn enable_creates_bindings_blocks_conflicting_agents_and_inherits_new_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let registered = tmp.path().join("wt-reg");
        let external = tmp.path().join("wt-ext");
        let db = tmp.path().join("data.db");
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo, "https://example.com/a/b.git");
        std::fs::write(repo.join("AGENTS.md"), "keep-me\n").unwrap();

        let state = build_hub_state(&db).await;
        let project_id = "proj-hub-2";
        seed_project_with_worktrees(&state, project_id, &repo, &registered, &external).await;

        let before_agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
        let status = enable_project_scope(
            &state,
            EnableAgentHubProjectRequest {
                project_id: project_id.to_string(),
                confirm: true,
            },
        )
        .await
        .unwrap();
        assert!(status.opted_in);
        assert!(!status.hub_project_id.is_empty());
        assert!(status
            .bindings
            .iter()
            .any(|b| b.checkout_kind == "main" && b.status == "blocked" && b.warning.is_some()));
        assert!(status
            .bindings
            .iter()
            .any(|b| b.workbench_worktree_id.as_deref() == Some("proj-hub-2:wt-reg")));
        // AGENTS.md unchanged
        assert_eq!(
            std::fs::read_to_string(repo.join("AGENTS.md")).unwrap(),
            before_agents
        );
        // portable scope has no absolute path
        let scope = state
            .agent_hub_repo
            .get_project_scope_by_hub_project_id(&status.hub_project_id)
            .await
            .unwrap()
            .expect("project scope");
        assert!(
            scope.relative_path.as_deref().unwrap_or("").is_empty()
                || !scope
                    .relative_path
                    .as_deref()
                    .unwrap_or("")
                    .starts_with('/')
        );
        assert!(!scope
            .relative_path
            .as_deref()
            .unwrap_or("")
            .contains(&repo.to_string_lossy().to_string()));

        // create new workbench worktree → binding before return
        let created = crate::commands::workbench::local_create_workbench_worktree(
            &state,
            project_id.to_string(),
            "feature-new".to_string(),
            Some("main".to_string()),
        )
        .await
        .unwrap();
        let bindings = refresh_checkout_bindings(&state, project_id).await.unwrap();
        // local_create already hooks refresh; assert binding present
        assert!(
            bindings.iter().any(|b| b.workbench_worktree_id.as_deref()
                == Some(created.id.as_str())
                && b.status != "detached"),
            "new worktree binding missing: {bindings:?}"
        );

        // remove worktree → detached, no asset tombstone required (none created)
        let remove = crate::commands::workbench::local_remove_workbench_worktree_with_ledger(
            &state,
            created.id.clone(),
            Some(true),
            format!("op-remove-{}", uuid::Uuid::new_v4()),
        )
        .await
        .unwrap();
        let _ = remove;
        let after_remove = state
            .agent_hub_repo
            .list_checkout_bindings_by_hub_project(&status.hub_project_id)
            .await
            .unwrap();
        let detached = after_remove
            .iter()
            .find(|b| b.workbench_worktree_id.as_deref() == Some(created.id.as_str()))
            .expect("binding retained");
        assert_eq!(detached.status, "detached");
    }

    #[tokio::test]
    async fn refresh_without_opt_in_creates_no_bindings() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let db = tmp.path().join("data.db");
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo, "https://example.com/x.git");
        let state = build_hub_state(&db).await;
        let project_id = "proj-no-opt";
        let now = chrono::Utc::now().to_rfc3339();
        state
            .workbench_project_repo
            .upsert(&WorkbenchProjectRow {
                id: project_id.to_string(),
                name: "n".into(),
                kind: "local".into(),
                device_id: "d1".into(),
                device_name: "t".into(),
                path: repo.canonicalize().unwrap().to_string_lossy().to_string(),
                last_opened_at: now.clone(),
                created_at: now.clone(),
                updated_at: now,
            })
            .await
            .unwrap();
        let bindings = refresh_checkout_bindings(&state, project_id).await.unwrap();
        assert!(bindings.is_empty());
    }
}
