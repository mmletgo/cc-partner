//! net/routes/claude_code_assets.rs — Claude Code assets P2P 路由
//!
//! Business Logic（为什么需要这个模块）:
//!     局域网设备之间需要先展示远端 inventory，再按用户勾选的 items 生成 bundle 拉取。
//!
//! Code Logic（这个模块做什么）:
//!     GET inventory 返回脱敏摘要 DTO；POST bundle 接收 selectors，返回只包含所选 assets 的 zip。

use crate::claude_code_assets::{self, ClaudeCodeAssetSelector};
use crate::error::AppError;
use axum::body::Body;
use axum::extract::Json;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;

/// 返回本机可导出的 Claude Code assets inventory。
pub async fn assets_inventory() -> Result<Json<Vec<crate::claude_code_assets::ClaudeCodeAsset>>, AppError> {
    Ok(Json(claude_code_assets::list_assets().await?))
}

/// 按 selectors 生成 zip bundle。bundle 中 MCP 配置已脱敏。
pub async fn assets_bundle(
    Json(req): Json<AssetsBundleReq>,
) -> Result<Response<Body>, AppError> {
    let bytes = claude_code_assets::build_bundle(req.items).await?;
    let mut resp = Response::new(Body::from(bytes));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    Ok(resp)
}

/// bundle 请求体。
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetsBundleReq {
    pub items: Vec<ClaudeCodeAssetSelector>,
}
