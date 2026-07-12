//! attention/source.rs — Attention source trait 与错误策略。
//!
//! Business Logic（为什么需要这个模块）:
//!     Inbox 条目来自 Orchestrator 与 Workbench 多个投影源，必须通过统一接口接入，
//!     禁止在聚合器或页面里散落业务判断；任一 source 失败时整次快照失败。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `AttentionSource` trait：接收 AppState，异步返回 `Vec<AttentionItemDto>`。

use crate::attention::models::AttentionItemDto;
use crate::error::AppError;
use crate::state::AppState;
use futures_util::future::BoxFuture;

/// Attention 条目投影源。
///
/// Business Logic（为什么需要这个 trait）:
///     新 source 必须通过明确接口接入，聚合器只负责合并/去重/排序，不理解具体业务投影。
///
/// Code Logic（这个 trait 做什么）:
///     `collect` 返回该 source 的全部条目；错误向上传播，由聚合器使整次聚合失败。
pub(crate) trait AttentionSource: Send + Sync {
    /// Business Logic（为什么需要这个函数）:
    ///     聚合器按注册顺序调用每个 source 收集待办投影。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取 AppState 中需要的仓储/运行时，产出该 source 的 AttentionItem 列表。
    fn collect<'a>(
        &'a self,
        state: &'a AppState,
    ) -> BoxFuture<'a, Result<Vec<AttentionItemDto>, AppError>>;
}
