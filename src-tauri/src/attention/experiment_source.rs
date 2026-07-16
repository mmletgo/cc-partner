//! Attention 实验组决策投影。
//!
//! Business Logic（为什么需要这个模块）:
//!     只有零合格、并列、judge error 或 confidence 非 high 时，
//!     一个 experiment 只产生一条组级 NeedsDecision Attention。
//!
//! Code Logic（这个模块做什么）:
//!     列出 NeedsDecision 实验，复用 experiment_decision_item_contract 稳定 ID。

use crate::attention::agent_runtime_source::experiment_decision_item_contract;
use crate::attention::models::AttentionItemDto;
use crate::attention::source::AttentionSource;
use crate::error::AppError;
use crate::state::AppState;
use futures_util::future::BoxFuture;
use std::collections::HashMap;

/// 实验组 Attention 投影源。
///
/// Business Logic（为什么需要这个结构体）:
///     聚合器需要独立 source 收集 experiment NeedsDecision，避免污染 per-task 投影。
///
/// Code Logic（这个结构体做什么）:
///     无状态；collect 读 repo.list_experiments_needing_decision。
#[derive(Debug, Default, Clone, Copy)]
pub struct ExperimentAttentionSource;

impl AttentionSource for ExperimentAttentionSource {
    /// Business Logic（为什么需要这个函数）:
    ///     聚合器统一 collect 入口。
    ///
    /// Code Logic（这个函数做什么）:
    ///     投影全部 NeedsDecision 实验。
    fn collect<'a>(
        &'a self,
        state: &'a AppState,
    ) -> BoxFuture<'a, Result<Vec<AttentionItemDto>, AppError>> {
        Box::pin(async move { collect_experiment_attention_items(state).await })
    }
}

/// Business Logic（为什么需要这个函数）:
///     桌面/mobile 共用实验决策投影。
///
/// Code Logic（这个函数做什么）:
///     list NeedsDecision + 项目名映射 + contract helper。
pub async fn collect_experiment_attention_items(
    state: &AppState,
) -> Result<Vec<AttentionItemDto>, AppError> {
    let experiments = state
        .orchestrator_repo
        .list_experiments_needing_decision()
        .await?;
    if experiments.is_empty() {
        return Ok(Vec::new());
    }
    let projects = state.workbench_project_repo.list().await?;
    let names: HashMap<String, String> = projects
        .into_iter()
        .map(|p| (p.id, p.name))
        .collect();
    let mut items = Vec::with_capacity(experiments.len());
    for exp in experiments {
        let name = names
            .get(&exp.project_id)
            .cloned()
            .unwrap_or_else(|| exp.project_id.clone());
        let mut item = experiment_decision_item_contract(
            &exp.project_id,
            &exp.id,
            &name,
            &exp.updated_at,
        );
        // 使用实验标题丰富展示，但不改变稳定 ID
        if !exp.title.is_empty() {
            item.title = format!("实验需要决策：{}", exp.title);
        }
        if let Some(reason) = exp.selection_reason.as_deref() {
            item.summary = reason.to_string();
        }
        items.push(item);
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::models::{AttentionSourceKind, AttentionTargetDto};

    /// Business Logic（为什么需要这个测试）:
    ///     稳定 ID 合同必须保持 experiment:decision:<id>。
    #[test]
    fn contract_id_stable() {
        let item = experiment_decision_item_contract("p", "e9", "demo", "t");
        assert_eq!(item.id, "experiment:decision:e9");
        assert_eq!(
            item.source_kind,
            AttentionSourceKind::ExperimentNeedsDecision
        );
        assert!(matches!(
            item.target,
            AttentionTargetDto::Experiment { experiment_id, .. } if experiment_id == "e9"
        ));
    }
}
