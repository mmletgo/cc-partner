//! net/routes/prompts.rs — 移动端 Prompt 库只读 HTTP 路由
//!
//! Business Logic（为什么需要这个模块）:
//!     手机浏览器 `/mobile` 需要同源只读访问本机 Prompt 库（含 favorite 收藏状态），
//!     以便移动端展示与桌面同步的 Prompt 列表。本期移动端只读消费，不暴露 toggle 写路由。
//!
//! Code Logic（这个模块做什么）:
//!     `GET /api/mobile/prompts` 接收可选 query（search/tag/favorite），复用 `PromptRepo::list`
//!     查询并返回 `Vec<PromptDto>`（camelCase，带 favorite）；错误经 P2pError 信封返回。

use crate::models::prompt::PromptDto;
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use axum::extract::{Extension, Query, State};
use axum::Json;
use serde::Deserialize;

/// `GET /api/mobile/prompts` 的 query 参数（全部可选，缺省不过滤）。
///
/// Business Logic: 移动端列表页可按关键词、单标签或收藏状态筛选；不传则返回全部未删除 Prompt。
/// Code Logic: `favorite` 为可选 bool（`true`/`false`），解析失败视为不过滤。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MobilePromptsQuery {
    /// 关键词搜索（title/content LIKE）。
    pub search: Option<String>,
    /// 单标签筛选（json_each 匹配）。
    pub tag: Option<String>,
    /// 收藏过滤：`true` 只返回收藏，`false` 只返回非收藏，缺省不过滤。
    pub favorite: Option<bool>,
}

/// GET /api/mobile/prompts：返回本机 Prompt 列表（含 favorite），移动端只读。
///
/// Business Logic（为什么需要这个函数）:
///     移动端 `/mobile` 浏览器需要只读读取本机 Prompt 库；favorite 作为 PromptRow 元数据
///     随整行同步，移动端只消费不写入。路由经既有的 lan_socket_gate + browser_request_guard。
///
/// Code Logic（这个函数做什么）:
///     解析 query → 委托 `PromptRepo::list(search, tag, favorite)` → 映射为 PromptDto 列表；
///     错误经 P2pError 信封（domain code `prompts.list_mobile`）。
pub async fn list_mobile_prompts(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Query(query): Query<MobilePromptsQuery>,
) -> P2pResult<Json<Vec<PromptDto>>> {
    let rows = state
        .prompt_repo
        .list(
            query.search.as_deref(),
            query.tag.as_deref(),
            query.favorite,
        )
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "prompts.list_mobile"))?;
    let dtos: Vec<PromptDto> = rows.iter().map(|r| r.to_dto()).collect();
    Ok(Json(dtos))
}
