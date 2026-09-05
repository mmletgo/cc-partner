//! agent_hub/replication/pull/install_target — 安装目标解析与 mutation 能力门禁
//!
//! Business Logic（为什么需要这个模块）:
//!     Pull 安装到目标 Agent 前必须解析目标 scope（user/映射项目）与配置根路径，
//!     且只有目标侧同时具备 inventory mutation Supported 与对应写 capability
//!     （Plugin 还需 ActivatePackage）才允许 InstallToTarget，否则降级 canonical-only。
//!
//! Code Logic（这个模块做什么）:
//!     inventory 查询构造 / destination scope id 解析（要求项目已 opt-in）；
//!     mutation capability 判定、scope 身份冲突识别；
//!     MCP 原生 leaf 渲染与 Claude/OpenCode 配置权威路径、各 Agent 安装根解析。

use super::dto::PortablePullChangeDto;
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::models::ScopeKind;
use crate::agent_hub::portable_inventory::{
    PortableAssetKind, PortableInventoryItemDto, PortableInventoryMutationCapability,
    PortableInventoryQuery, PortableInventorySnapshotDto,
};
use crate::agent_hub::snapshot::portable_builder::PortableSelectionItem;
use crate::agent_hub::support::{CapabilitySupport, EvaluatedTargetSupport, TargetCapability};
use crate::agent_hub::targets::portable::render_mcp_projection;
use crate::error::AppError;
use crate::state::AppState;
use std::path::{Path, PathBuf};

/// 为 Pull 的 user/project 端构造精确 inventory 查询。
///
/// Business Logic: 项目级 Pull 必须始终绑定一个明确的 Workbench project，不能退回全项目扫描。
/// Code Logic: 有 local project id 时固定 project scope；否则固定 user scope，并同时绑定 target。
pub(super) fn portable_pull_inventory_query(
    target: AgentTarget,
    local_project_id: Option<String>,
) -> PortableInventoryQuery {
    PortableInventoryQuery {
        target: Some(target),
        kind: None,
        scope_kind: Some(if local_project_id.is_some() {
            ScopeKind::Project
        } else {
            ScopeKind::User
        }),
        local_project_id,
    }
}

/// 解析 Pull 目标 scope id。
///
/// Business Logic: 远端项目的 Hub id 不能直接复用于本机目标项目；导入/冲突判断必须使用目标机映射。
/// Code Logic: user 返回 `user`；project 通过本机 Workbench id 精确查 mapping，并要求已 opt-in。
pub(super) async fn resolve_destination_scope_id(
    state: &AppState,
    local_project_id: Option<&str>,
) -> Result<String, AppError> {
    let Some(local_project_id) = local_project_id else {
        return Ok("user".to_string());
    };
    let mapping = state
        .agent_hub_repo
        .get_project_mapping_by_local_workbench_id(local_project_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found("PORTABLE_PULL_DESTINATION_PROJECT_MAPPING_NOT_FOUND")
        })?;
    if !mapping.opted_in {
        return Err(AppError::validation(
            "PORTABLE_PULL_DESTINATION_PROJECT_NOT_OPTED_IN",
        ));
    }
    Ok(format!("project:{}", mapping.hub_project_id))
}

/// target 汇总状态 + 实际安装动作 capability 是否共同允许 InstallToTarget。
///
/// 普通 portable 资产写原生文件只认 RenderPortableAssets；Plugin Pull 同时物化并进入
/// 原生 package 目录，因此还必须具备 ActivatePackage。ActivationRequired 不视为可执行。
pub(super) fn mutation_allows_install_to_target(
    aggregate: PortableInventoryMutationCapability,
    evaluated: Option<&EvaluatedTargetSupport>,
    kind: PortableAssetKind,
) -> bool {
    if aggregate != PortableInventoryMutationCapability::Supported {
        return false;
    }
    let Some(evaluated) = evaluated else {
        return false;
    };
    let capability_ready = |capability| {
        matches!(
            evaluated.capability(capability),
            CapabilitySupport::Supported | CapabilitySupport::SupportedAfterRestart
        ) && evaluated.allows_write_capability(capability)
    };
    capability_ready(TargetCapability::RenderPortableAssets)
        && (kind != PortableAssetKind::Plugin
            || capability_ready(TargetCapability::ActivatePackage))
}

/// 从本地 inventory 快照读取 destination target 的 mutation_capability。
pub(super) fn destination_mutation_capability(
    local: &PortableInventorySnapshotDto,
    destination_target: AgentTarget,
) -> PortableInventoryMutationCapability {
    local
        .targets
        .iter()
        .find(|t| t.target == destination_target)
        .map(|t| t.mutation_capability)
        // 无 target 事实 → 诚实 fail-closed（不得假 Supported）
        .unwrap_or(PortableInventoryMutationCapability::Blocked)
}

/// Resolve conflict/rescan scope identity for a local inventory item.
///
/// Business Logic: user vs project same nativeId are distinct assets.
/// Code Logic: prefer non-empty scope_id; else project:<id> / user.
pub(super) fn resolve_inventory_scope_id(item: &PortableInventoryItemDto) -> String {
    let trimmed = item.scope_id.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    if let Some(pid) = item
        .project_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return format!("project:{pid}");
    }
    "user".to_string()
}

/// Whether inventory contains target+kind+nativeId under the resolved scope.
pub(super) fn inventory_has_scoped_item(
    snap: &PortableInventorySnapshotDto,
    target: AgentTarget,
    kind: PortableAssetKind,
    native_id: &str,
    scope_id: &str,
) -> bool {
    let want = scope_id.trim();
    snap.items.iter().any(|i| {
        i.target == target
            && i.kind == kind
            && i.native_id == native_id
            && resolve_inventory_scope_id(i) == want
    })
}

/// MCP 原生 leaf（无 Hub DTO 字段 key/transport/toolAllow）。
pub(super) fn native_mcp_leaf_value(
    target: AgentTarget,
    server: &crate::agent_hub::assets::PortableMcpServer,
) -> Result<serde_json::Value, AppError> {
    let proj = render_mcp_projection(target, server)?;
    let bytes = proj
        .files
        .first()
        .map(|f| f.bytes.as_slice())
        .unwrap_or(b"{}");
    serde_json::from_slice(bytes)
        .map_err(|e| AppError::validation(format!("PORTABLE_PULL_MCP_NATIVE_RENDER:{e}")))
}

/// Claude 用户 MCP 配置权威路径（与 scanner 一致）。
pub(super) fn resolve_claude_mcp_config_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        PathBuf::from(dir).join(".claude.json")
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".claude.json")
    }
}

/// OpenCode MCP 配置权威路径（OPENCODE_CONFIG / jsonc / json）。
pub(super) fn resolve_opencode_mcp_config_path(root: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("OPENCODE_CONFIG") {
        return PathBuf::from(p);
    }
    let jsonc = root.join("opencode.jsonc");
    if jsonc.is_file() {
        return jsonc;
    }
    root.join("opencode.json")
}

/// 解析 InstallToTarget 的 agent 配置根；project scope 走映射项目路径。
pub(super) async fn resolve_install_root(
    state: &AppState,
    item: &PortableSelectionItem,
    change: &PortablePullChangeDto,
) -> Result<PathBuf, AppError> {
    let env_home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    // change.scope_id 是 preview 时解析出的目标 scope；来源 selection.scope_id 属于另一台设备，
    // 项目级 Pull 不能拿来源 Hub id 在目标机查 mapping。
    if change.scope_id.starts_with("project:") {
        let hub_id = change.scope_id.trim_start_matches("project:");
        let mapping = state
            .agent_hub_repo
            .get_project_mapping_by_hub_project_id(hub_id)
            .await?;
        let Some(mapping) = mapping.filter(|m| m.opted_in) else {
            return Err(AppError::validation(
                "PORTABLE_PULL_PROJECT_MAPPING_UNAVAILABLE".to_string(),
            ));
        };
        let project_path = mapping
            .local_absolute_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty());
        let project_path = if let Some(p) = project_path {
            p
        } else if let Some(wb) = mapping.local_workbench_project_id.as_ref() {
            state
                .workbench_project_repo
                .get(wb)
                .await
                .ok()
                .flatten()
                .map(|p| PathBuf::from(p.path))
                .ok_or_else(|| {
                    AppError::validation("PORTABLE_PULL_PROJECT_MAPPING_UNAVAILABLE".to_string())
                })?
        } else {
            return Err(AppError::validation(
                "PORTABLE_PULL_PROJECT_MAPPING_UNAVAILABLE".to_string(),
            ));
        };
        // 项目资产根：Claude `.claude` / Codex 项目 agents / OpenCode 项目 `.opencode`
        return Ok(match item.target {
            AgentTarget::Claude => project_path.join(".claude"),
            AgentTarget::Codex => project_path.join(".agents"),
            AgentTarget::OpenCode => project_path.join(".opencode"),
            AgentTarget::Grok => project_path.join(".grok"),
            AgentTarget::Gemini => project_path.join(".gemini"),
            AgentTarget::Cursor => project_path.join(".cursor"),
            AgentTarget::Pi => project_path.join(".pi"),
        });
    }
    Ok(match item.target {
        AgentTarget::Claude => std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| env_home.join(".claude")),
        AgentTarget::Codex => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| env_home.join(".codex")),
        AgentTarget::OpenCode => std::env::var_os("OPENCODE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(|p| PathBuf::from(p).join("opencode"))
                    .unwrap_or_else(|| env_home.join(".config").join("opencode"))
            }),
        AgentTarget::Grok => std::env::var_os("GROK_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| env_home.join(".grok")),
        AgentTarget::Gemini => std::env::var_os("GEMINI_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| env_home.join(".gemini")),
        AgentTarget::Cursor => std::env::var_os("CURSOR_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| env_home.join(".cursor")),
        AgentTarget::Pi => env_home.join(".pi").join("agent"),
    })
}
