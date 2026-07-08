//! net/routes — axum HTTP 路由处理器
//!
//! Business Logic: 对照 Python `network/protocol.py` 中供对端调用的 P2P API handler。
//!     已实现 `/api/health`（M3）、`/api/sync/{pull,push}`（M4）、`/api/transfer/*`（M5）；
//!     `/api/cc-history/sync/{pull,push}` 走独立链路同步 Claude Code 历史。
//!
//! Code Logic: 每个 handler 通过 axum `State<AppState>` 取共享依赖，返回 `axum::Json`。
//!     字段命名与对端约定一致（sync/cc-history 用 snake_case 互通，transfer 字段对照 Python）。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

pub mod cc_history;
pub mod claude_code_assets;
pub mod claude_md_sync;
pub mod health;
pub mod mobile;
pub mod orchestrator;
pub mod scratchpad_sync;
pub mod ssh_target_sync;
pub mod sync;
pub mod transfer;
pub mod workbench;

/// HTTP API 错误响应。
///
/// Business Logic（为什么需要这个结构体）:
///     部分 HTTP-only route 需要表达 404/400/502 等状态，不能全部复用 AppError 的 500 响应。
///
/// Code Logic（这个结构体做什么）:
///     保存 HTTP status 与用户可读错误消息，并实现 IntoResponse 输出 `{error}` JSON。
#[derive(Debug, Clone)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    /// 构造 404 API 错误。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     previewId、资源等 HTTP 实体缺失时应向调用方返回 not found 语义。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用传入消息创建 status=404 的 ApiError。
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    /// 构造 400 API 错误。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     请求格式或代理请求体无法读取时，应区分为客户端请求问题。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用传入消息创建 status=400 的 ApiError。
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    /// 构造 502 API 错误。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     preview 上游 dev server 或 owner proxy 不可达时，应表达网关上游失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用传入消息创建 status=502 的 ApiError。
    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: message.into(),
        }
    }

    /// 构造 500 API 错误。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     其他内部错误仍需保留统一 JSON error 响应。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用传入消息创建 status=500 的 ApiError。
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    /// 读取 HTTP 状态码。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单元测试和上层错误处理需要确认具体 HTTP 语义，而不仅是错误文案。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 ApiError 内部保存的 StatusCode。
    #[allow(dead_code)]
    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl From<crate::error::AppError> for ApiError {
    /// 从应用错误转换为 HTTP API 错误。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     HTTP route 复用命令层 helper 时需要用 `?` 传播 AppError。
    ///
    /// Code Logic（这个函数做什么）:
    ///     AppError::NotFound 映射 404，其余业务/IO/DB 错误映射 500 并保留 Display 文案。
    fn from(error: crate::error::AppError) -> Self {
        match error {
            crate::error::AppError::NotFound(message) => ApiError::not_found(message),
            other => ApiError::internal(other.to_string()),
        }
    }
}

impl From<reqwest::Error> for ApiError {
    /// 从 reqwest 错误转换为 HTTP API 错误。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     preview proxy 访问上游 dev server 失败时，本机 HTTP server 应返回网关错误。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 reqwest 网络/协议错误映射为 502，并保留底层错误摘要。
    fn from(error: reqwest::Error) -> Self {
        ApiError::bad_gateway(format!("预览上游请求失败: {error}"))
    }
}

impl IntoResponse for ApiError {
    /// 转换为 axum response。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     browser proxy route 需要稳定返回 `{error}` JSON，供桌面端和移动端统一展示。
    ///
    /// Code Logic（这个函数做什么）:
    ///     使用内部 status 作为 HTTP 状态码，body 序列化为 `{error: message}`。
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}
