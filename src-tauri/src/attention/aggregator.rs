//! attention/aggregator.rs — Attention 确定性聚合。
//!
//! Business Logic（为什么需要这个模块）:
//!     全局 Inbox 需要把多个 source 的投影合并为一次完整快照；任一 source 失败不得返回
//!     看似完整的部分列表，重复 ID 冲突必须暴露完整性错误而不是静默择优。
//!
//! Code Logic（这个模块做什么）:
//!     顺序收集 source → 稳定 ID 去重（相等保留、冲突报错）→ 分类/时间/ID 排序 → 计数。

use crate::attention::models::{
    AttentionCategory, AttentionCountsDto, AttentionItemDto, AttentionSnapshotDto,
};
use crate::attention::source::AttentionSource;
use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use std::collections::HashMap;

/// 从多个 source 聚合 Attention 快照。
///
/// Business Logic（为什么需要这个函数）:
///     桌面 Tauri 与 Mobile HTTP 共用同一聚合逻辑，保证两端列表顺序与计数一致。
///
/// Code Logic（这个函数做什么）:
///     依次 await 每个 source；任一失败立即返回错误且不生成 generatedAt。
///     成功后按 ID 去重（内容相等则保留首次，冲突返回 integrity 错误），再按
///     category(decision→blocked→environment)、updatedAt 降序、ID 升序排序，最后写 counts。
pub async fn aggregate_attention_sources(
    state: &AppState,
    sources: &[&dyn AttentionSource],
) -> Result<AttentionSnapshotDto, AppError> {
    let mut collected: Vec<AttentionItemDto> = Vec::new();
    for source in sources {
        let items = source.collect(state).await?;
        collected.extend(items);
    }

    // 所有 source 成功后才打 generatedAt，避免失败路径产出半成品时间戳。
    let generated_at = Utc::now().to_rfc3339();
    let mut items = dedupe_and_sort(collected)?;
    // 聚合阶段为每个 item 注入本设备视角的 read_at；
    // 读仓储失败上抛使整次快照失败（与既有 fail-closed 语义一致）。
    let read_set = state
        .attention_read_repo
        .load_read_ids(state.device_id.as_str())
        .await?;
    for item in items.iter_mut() {
        item.read_at = read_set.get(&item.id).cloned();
    }
    let counts = count_items(&items);

    Ok(AttentionSnapshotDto {
        generated_at,
        counts,
        items,
        my_device_id: state.device_id.as_str().to_string(),
    })
}

/// 纯函数聚合入口（测试与内部复用）：在 source 已收集完成后执行去重/排序/计数。
///
/// Business Logic（为什么需要这个函数）:
///     单测需要在不构造完整 AppState 的情况下验证确定性排序与完整性错误策略。
///
/// Code Logic（这个函数做什么）:
///     顺序消费批次：遇到 Err 立即失败；全部 Ok 后生成 generatedAt 并去重排序计数。
pub fn aggregate_attention_item_batches(
    batches: Vec<Result<Vec<AttentionItemDto>, AppError>>,
) -> Result<AttentionSnapshotDto, AppError> {
    let mut collected: Vec<AttentionItemDto> = Vec::new();
    for batch in batches {
        collected.extend(batch?);
    }
    let generated_at = Utc::now().to_rfc3339();
    let items = dedupe_and_sort(collected)?;
    // 测试 / 内部复用：read_at 与 unread_* 计数保持为"全部未读"默认值。
    let counts = count_items(&items);
    Ok(AttentionSnapshotDto {
        generated_at,
        counts,
        items,
        my_device_id: String::new(),
    })
}

/// Business Logic（为什么需要这个函数）:
///     同一权威实体可能被多个 source 同时投影出相同稳定 ID，需要确定性去重并检测冲突。
///
/// Code Logic（这个函数做什么）:
///     按首次出现顺序保留相等重复项；ID 相同但内容不等返回 AppError::generic 完整性错误；
///     随后按分类序、updatedAt 降序、ID 升序排序。
fn dedupe_and_sort(items: Vec<AttentionItemDto>) -> Result<Vec<AttentionItemDto>, AppError> {
    let mut by_id: HashMap<String, AttentionItemDto> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for item in items {
        match by_id.get(&item.id) {
            Some(existing) if existing == &item => {
                // 内容完全相等的重复 ID：稳定保留首次出现，忽略后续。
            }
            Some(_) => {
                return Err(AppError::generic(format!(
                    "attention 聚合完整性错误：重复 ID 内容不一致: {}",
                    item.id
                )));
            }
            None => {
                order.push(item.id.clone());
                by_id.insert(item.id.clone(), item);
            }
        }
    }

    let mut result: Vec<AttentionItemDto> = order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect();

    result.sort_by(|a, b| {
        a.category
            .sort_rank()
            .cmp(&b.category.sort_rank())
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.id.cmp(&b.id))
    });

    Ok(result)
}

/// Business Logic（为什么需要这个函数）:
///     badge 与分组空态依赖 total 与分类计数一致。
///
/// Code Logic（这个函数做什么）:
///     统计三类条目数量，total 等于 items 长度。
fn count_items(items: &[AttentionItemDto]) -> AttentionCountsDto {
    let mut decision = 0u32;
    let mut blocked = 0u32;
    let mut environment = 0u32;
    let mut unread_decision = 0u32;
    let mut unread_blocked = 0u32;
    let mut unread_environment = 0u32;
    let mut unread_total = 0u32;
    for item in items {
        let unread = item.read_at.is_none();
        if unread {
            unread_total += 1;
        }
        match item.category {
            AttentionCategory::Decision => {
                decision += 1;
                if unread {
                    unread_decision += 1;
                }
            }
            AttentionCategory::Blocked => {
                blocked += 1;
                if unread {
                    unread_blocked += 1;
                }
            }
            AttentionCategory::Environment => {
                environment += 1;
                if unread {
                    unread_environment += 1;
                }
            }
        }
    }
    AttentionCountsDto {
        total: items.len() as u32,
        decision,
        blocked,
        environment,
        unread_total,
        unread_decision,
        unread_blocked,
        unread_environment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::models::{
        AttentionDeviceRef, AttentionFreshness, AttentionProjectKind, AttentionProjectRef,
        AttentionSettingsTab, AttentionSourceKind, AttentionTargetDto,
    };

    /// Business Logic: 构造最小条目，仅覆盖排序/去重所需字段。
    /// Code Logic: 按 category/id/updated_at 填默认 target。
    fn item(
        id: &str,
        category: AttentionCategory,
        updated_at: &str,
        title: &str,
    ) -> AttentionItemDto {
        let target = match category {
            AttentionCategory::Environment => AttentionTargetDto::Settings {
                tab: AttentionSettingsTab::Dependencies,
            },
            AttentionCategory::Decision | AttentionCategory::Blocked => {
                AttentionTargetDto::OrchestratorTask {
                    project_id: "p".to_string(),
                    task_id: id.to_string(),
                }
            }
        };
        AttentionItemDto {
            id: id.to_string(),
            category,
            source_kind: match category {
                AttentionCategory::Decision => AttentionSourceKind::OrchestratorHumanReview,
                AttentionCategory::Blocked => AttentionSourceKind::OrchestratorBlocked,
                AttentionCategory::Environment => AttentionSourceKind::WorkbenchDependency,
            },
            title: title.to_string(),
            summary: format!("summary-{title}"),
            updated_at: updated_at.to_string(),
            freshness: AttentionFreshness::Live,
            cached_at: None,
            project: Some(AttentionProjectRef {
                id: "p".to_string(),
                name: "demo".to_string(),
                kind: AttentionProjectKind::Local,
            }),
            device: Some(AttentionDeviceRef {
                id: "d".to_string(),
                name: "local".to_string(),
            }),
            target,
            read_at: None,
        }
    }

    #[test]
    fn concatenates_sources_and_counts_consistently() {
        let a = item(
            "orchestrator:human-review:t1",
            AttentionCategory::Decision,
            "2026-07-11T12:00:00Z",
            "review",
        );
        let b = item(
            "orchestrator:blocked:t2",
            AttentionCategory::Blocked,
            "2026-07-11T11:00:00Z",
            "blocked",
        );
        let c = item(
            "workbench:dependency:tmux",
            AttentionCategory::Environment,
            "2026-07-11T10:00:00Z",
            "tmux",
        );

        let snapshot = aggregate_attention_item_batches(vec![
            Ok(vec![c.clone(), b.clone()]),
            Ok(vec![a.clone()]),
        ])
        .expect("aggregate");

        assert_eq!(snapshot.items.len(), 3);
        assert_eq!(snapshot.counts.total, 3);
        assert_eq!(snapshot.counts.decision, 1);
        assert_eq!(snapshot.counts.blocked, 1);
        assert_eq!(snapshot.counts.environment, 1);
        assert_eq!(
            snapshot.counts.total,
            snapshot.counts.decision + snapshot.counts.blocked + snapshot.counts.environment
        );
        assert_eq!(snapshot.counts.total as usize, snapshot.items.len());
        // 分类顺序 decision → blocked → environment
        assert_eq!(snapshot.items[0].id, a.id);
        assert_eq!(snapshot.items[1].id, b.id);
        assert_eq!(snapshot.items[2].id, c.id);
        assert!(!snapshot.generated_at.is_empty());
    }

    #[test]
    fn stable_id_dedupe_keeps_equal_duplicates() {
        let first = item(
            "orchestrator:blocked:t1",
            AttentionCategory::Blocked,
            "2026-07-11T10:00:00Z",
            "blocked",
        );
        let duplicate = first.clone();
        let snapshot =
            aggregate_attention_item_batches(vec![Ok(vec![first.clone()]), Ok(vec![duplicate])])
                .expect("equal duplicates ok");
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0], first);
        assert_eq!(snapshot.counts.total, 1);
        assert_eq!(snapshot.counts.blocked, 1);
    }

    #[test]
    fn conflicting_duplicate_id_returns_integrity_error() {
        let first = item(
            "orchestrator:human-review:t1",
            AttentionCategory::Decision,
            "2026-07-11T10:00:00Z",
            "review-a",
        );
        let mut conflict = first.clone();
        conflict.title = "review-b".to_string();

        let err = aggregate_attention_item_batches(vec![Ok(vec![first]), Ok(vec![conflict])])
            .expect_err("conflict must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("完整性")
                || msg.contains("不一致")
                || msg.contains("orchestrator:human-review:t1"),
            "错误应标识完整性冲突: {msg}"
        );
    }

    #[test]
    fn sorts_by_category_then_updated_at_desc_then_id() {
        let older_decision = item(
            "orchestrator:human-review:a",
            AttentionCategory::Decision,
            "2026-07-11T09:00:00Z",
            "old-decision",
        );
        let newer_decision = item(
            "orchestrator:human-review:b",
            AttentionCategory::Decision,
            "2026-07-11T12:00:00Z",
            "new-decision",
        );
        let equal_time_z = item(
            "orchestrator:blocked:z",
            AttentionCategory::Blocked,
            "2026-07-11T11:00:00Z",
            "z",
        );
        let equal_time_a = item(
            "orchestrator:blocked:a",
            AttentionCategory::Blocked,
            "2026-07-11T11:00:00Z",
            "a",
        );
        let env = item(
            "workbench:dependency:tmux",
            AttentionCategory::Environment,
            "2026-07-11T13:00:00Z",
            "tmux",
        );

        // 故意乱序输入
        let snapshot = aggregate_attention_item_batches(vec![Ok(vec![
            env,
            equal_time_z,
            older_decision,
            equal_time_a,
            newer_decision,
        ])])
        .expect("sort");

        let ids: Vec<&str> = snapshot.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "orchestrator:human-review:b", // decision, newer first
                "orchestrator:human-review:a", // decision, older
                "orchestrator:blocked:a",      // blocked, equal time → id asc
                "orchestrator:blocked:z",
                "workbench:dependency:tmux", // environment last even if newer
            ]
        );
    }

    #[test]
    fn one_source_error_fails_whole_aggregate() {
        let ok_item = item(
            "orchestrator:human-review:t1",
            AttentionCategory::Decision,
            "2026-07-11T10:00:00Z",
            "review",
        );
        let err = aggregate_attention_item_batches(vec![
            Ok(vec![ok_item]),
            Err(AppError::generic("source boom")),
        ])
        .expect_err("source error must fail aggregate");
        assert!(err.to_string().contains("source boom"));
    }

    #[test]
    fn generated_at_only_after_all_sources_succeed() {
        let fail = aggregate_attention_item_batches(vec![Err(AppError::generic("fail first"))]);
        assert!(fail.is_err());

        let ok = aggregate_attention_item_batches(vec![Ok(vec![item(
            "workbench:dependency:tmux",
            AttentionCategory::Environment,
            "2026-07-11T10:00:00Z",
            "tmux",
        )])])
        .expect("success");
        assert!(!ok.generated_at.is_empty());
        assert!(ok.generated_at.contains('T'));
    }
}
