//! Orchestrator Agent adapter catalog 与 local-only downgrade 守卫。
//!
//! Business Logic（为什么需要这个模块）:
//!     Settings/远端需要 owner adapter 可用性；旧 peer 降级前必须 quiesce 非 Claude Runner。
//!
//! Code Logic（这个模块做什么）:
//!     `list_orchestrator_agent_adapters` 返回 redacted catalog；
//!     `prepare_orchestrator_agent_downgrade` 仅本机 invoke，取消 active non-Claude 任务。

use crate::error::AppError;
use crate::orchestrator::agent_adapter::{
    AgentAdapterRegistry, AgentAvailability, AgentProviderId,
};
use crate::orchestrator::models::{OrchestratorTaskStatus};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

/// 对外 adapter catalog 条目（无 path/env/credential）。
///
/// Business Logic（为什么需要这个结构体）:
///     Desktop/remote/mobile 只应看到 provider 可用性与能力标志。
///
/// Code Logic（这个结构体做什么）:
///     camelCase DTO：provider/available/completionContract/supportsResume/supportsUsage/reasonCode。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorAgentAdapterCatalogItem {
    pub provider: String,
    pub available: bool,
    pub completion_contract: String,
    pub supports_resume: bool,
    pub supports_usage: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
}

/// catalog 列表响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorAgentAdapterCatalog {
    pub adapters: Vec<OrchestratorAgentAdapterCatalogItem>,
}

/// 构建 redacted catalog（不含 executable/env）。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri 与 P2P 共享同一 redaction 规则。
///
/// Code Logic（这个函数做什么）:
///     probe 三个内置 adapter，映射到 catalog item。
pub fn build_agent_adapter_catalog(
    registry: &AgentAdapterRegistry,
) -> Result<OrchestratorAgentAdapterCatalog, AppError> {
    let probes = registry.list_probes()?;
    let mut adapters = Vec::with_capacity(probes.len());
    for probe in probes {
        let adapter = registry.get(probe.provider_id)?;
        adapters.push(OrchestratorAgentAdapterCatalogItem {
            provider: probe.provider_id.as_str().to_string(),
            available: probe.availability == AgentAvailability::Available,
            completion_contract: adapter.completion_contract().as_str().to_string(),
            supports_resume: adapter.supports_resume(),
            supports_usage: adapter.supports_usage(),
            reason_code: probe.reason_code,
        });
    }
    Ok(OrchestratorAgentAdapterCatalog { adapters })
}

/// 列出 owner adapter catalog。
///
/// Business Logic（为什么需要这个函数）:
///     Settings 展示 Claude/Codex/generic 可用性；project_id 预留未来 per-project 配置。
///
/// Code Logic（这个函数做什么）:
///     读取 optional generic_terminal 配置构造 registry，返回 redacted catalog。
#[tauri::command]
pub async fn list_orchestrator_agent_adapters(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<OrchestratorAgentAdapterCatalog, AppError> {
    let _ = project_id;
    let generic = state
        .config
        .read()
        .expect("config 读锁中毒")
        .orchestrator
        .generic_terminal
        .clone();
    let registry = AgentAdapterRegistry::new(generic);
    build_agent_adapter_catalog(&registry)
}

/// downgrade 结果摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareAgentDowngradeResult {
    pub canceled_task_ids: Vec<String>,
    pub refused_delivering: Vec<String>,
}

/// 降级前 quiesce：取消 active 非 Claude 任务（保留 worktree/session/evidence）。
///
/// Business Logic（为什么需要这个函数）:
///     旧 peer 不支持 adapter 时不得静默把非 Claude 改成 Claude；须先 drain。
///
/// Code Logic（这个函数做什么）:
///     仅本机 invoke（无 LAN 路由）：扫描 Preparing/Running 且 provider != Claude 的任务，
///     Delivering 拒绝并列入 refused；其余 CAS Abort。
#[tauri::command]
pub async fn prepare_orchestrator_agent_downgrade(
    state: State<'_, AppState>,
) -> Result<PrepareAgentDowngradeResult, AppError> {
    prepare_agent_downgrade_for_state(&state).await
}

/// 可测试的 downgrade 实现。
///
/// Business Logic（为什么需要这个函数）:
///     单测无需 Tauri State。
///
/// Code Logic（这个函数做什么）:
///     见 prepare_orchestrator_agent_downgrade。
pub async fn prepare_agent_downgrade_for_state(
    state: &AppState,
) -> Result<PrepareAgentDowngradeResult, AppError> {
    let tasks = state.orchestrator_repo.list_tasks(None).await?;
    let mut canceled = Vec::new();
    let mut refused = Vec::new();
    for task in tasks {
        let provider = AgentProviderId::parse_legacy(task.runner_provider.as_deref())
            .unwrap_or(AgentProviderId::ClaudeCodeVisible);
        if provider.is_claude() {
            continue;
        }
        match task.status {
            OrchestratorTaskStatus::Delivering => {
                refused.push(task.id);
            }
            OrchestratorTaskStatus::Preparing
            | OrchestratorTaskStatus::Running
            | OrchestratorTaskStatus::Verifying => {
                if let Some(updated) = state
                    .orchestrator_repo
                    .try_transition_task_status(
                        &task.id,
                        task.status,
                        OrchestratorTaskStatus::Aborted,
                        Some("agent_adapter_downgrade_quiesce"),
                    )
                    .await?
                {
                    canceled.push(updated.id);
                }
            }
            _ => {}
        }
    }
    Ok(PrepareAgentDowngradeResult {
        canceled_task_ids: canceled,
        refused_delivering: refused,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::agent_adapter::AgentAdapterRegistry;

    /// Business Logic（为什么需要这个测试）:
    ///     remote DTO 绝不能含 executable/env。
    ///
    /// Code Logic（这个测试做什么）:
    ///     catalog 序列化字符串断言。
    #[test]
    fn remote_adapter_catalog_never_contains_executable_or_environment() {
        let catalog = build_agent_adapter_catalog(&AgentAdapterRegistry::with_defaults()).unwrap();
        let value = serde_json::to_value(&catalog).unwrap();
        let text = value.to_string();
        assert!(!text.contains("executable"));
        assert!(!text.contains("\"env\""));
        assert!(!text.contains("credential"));
        assert!(catalog.adapters.len() >= 3);
        for item in &catalog.adapters {
            assert!(!item.provider.is_empty());
        }
    }
}
