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

pub mod agent_hub;
pub mod attention;
pub mod browser_verification;
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
pub mod workbench_project_order_sync;

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

    /// 构造 403 API 错误。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     preview proxy 会话层对跨站 Origin 做防御纵深拒绝时需要明确 403，而不是 400/404。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用传入消息创建 status=403 的 ApiError。
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
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

    /// 构造 413 API 错误。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     preview proxy 等 HTTP route 需要在请求体超过安全上限时明确拒绝，避免继续读取或转发大 body。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用传入消息创建 status=413 的 ApiError。
    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
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
    ///     单元测试和上层错误处理需要确认具体 HTTP 语义，而不仅是错误文案；
    ///     preview proxy 错误透传到 axum 边界时也需要据此反查 P2pErrorCode。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 ApiError 内部保存的 StatusCode。
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// 消费 self 取出错误消息（供边界信封转换使用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     preview proxy 内部用 ApiError 表达具体 HTTP 语义，handler 把它转成统一信封时
    ///     需要把人类可读消息原样写入 `P2pErrorEnvelope.error`，避免丢失上下文。
    ///
    /// Code Logic（这个函数做什么）:
    ///     消费 self 返回内部 message 字符串。
    pub(crate) fn into_message(self) -> String {
        self.message
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

/// 把 `ApiError`（仍被 preview proxy 内部使用）映射为边界 `P2pError`。
///
/// Business Logic（为什么需要这个函数）:
///     `browser_proxy.rs` 内部用 `ApiError` 表达 413/502/400 等具体 HTTP 语义；当 proxy
///     handler 把错误透传到 axum 边界时，必须把它转成统一信封 `P2pError`，否则前端/对端会
///     同时看到老形态 `{error}` 与新信封两种 body，破坏契约。
///
/// Code Logic（这个函数做什么）:
///     按 `ApiError` 携带的 `StatusCode` 反查最贴近的 `P2pErrorCode`：
///     - 400/413/422 → Validation
///     - 404 → NotFound
///     - 409 → Conflict
///     - 503 → Unavailable
///     - 504 → Timeout
///     - 其它（含 502/500/5xx）→ Internal
///     再用 `P2pError::from_code` 写入消息与 request_id。
pub(crate) fn api_error_to_p2p(
    error: ApiError,
    context: &crate::net::request_context::P2pRequestContext,
) -> crate::net::error_response::P2pError {
    use crate::net::error_response::{P2pError, P2pErrorCode};
    let code = match error.status() {
        StatusCode::BAD_REQUEST
        | StatusCode::PAYLOAD_TOO_LARGE
        | StatusCode::UNPROCESSABLE_ENTITY => P2pErrorCode::Validation,
        StatusCode::FORBIDDEN => P2pErrorCode::Forbidden,
        StatusCode::NOT_FOUND => P2pErrorCode::NotFound,
        StatusCode::CONFLICT => P2pErrorCode::Conflict,
        StatusCode::SERVICE_UNAVAILABLE => P2pErrorCode::Unavailable,
        StatusCode::GATEWAY_TIMEOUT => P2pErrorCode::Timeout,
        _ => P2pErrorCode::Internal,
    };
    P2pError::from_code(error.into_message(), code, context)
}

#[cfg(test)]
mod envelope_contract_tests {
    use super::*;
    use crate::net::error_response::{P2pError, P2pErrorEnvelope};
    use crate::net::request_context::{
        request_id_middleware, P2pRequestContext, REQUEST_ID_HEADER,
    };
    use axum::body::{to_bytes, Body};
    use axum::extract::Extension;
    use axum::http::{Method, Request, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::Router;
    use serde_json::Value;

    /// 固定 request_id 用作所有状态类测试的调用链 ID。
    const TEST_REQUEST_ID: &str = "req-envelope-123";

    /// 构造测试用 request context（固定 request_id）。
    fn ctx() -> P2pRequestContext {
        P2pRequestContext {
            request_id: TEST_REQUEST_ID.to_string(),
        }
    }

    /// 测试 handler：根据 body 字符串返回对应分类的 P2pError，覆盖各状态类。
    ///
    /// Code Logic: 读取 body 中的分类名（"validation"/"not_found"/"conflict"/
    ///   "unavailable"/"internal"），用对应便捷构造器返回 P2pError。
    async fn classify_handler(
        Extension(ctx): Extension<P2pRequestContext>,
        body: String,
    ) -> Result<String, P2pError> {
        let error = match body.as_str() {
            "validation" => P2pError::validation("参数非法", &ctx),
            "not_found" => P2pError::not_found("实体不存在", &ctx),
            "conflict" => P2pError::conflict("状态冲突", &ctx),
            "unavailable" => P2pError::unavailable("暂不可用", &ctx),
            "internal" => P2pError::internal("内部错误", &ctx),
            other => P2pError::internal(format!("未知分类: {other}"), &ctx),
        };
        Err(error)
    }

    /// 把 classify_handler 包成带 request_id middleware 的最小测试 router。
    fn envelope_router() -> Router {
        Router::new()
            .route("/probe", post(classify_handler))
            .layer(axum::middleware::from_fn(request_id_middleware))
    }

    /// 用 oneshot 发送 probe 请求并解析响应。
    async fn send_probe(category: &str) -> (StatusCode, Value, String) {
        use tower::ServiceExt;
        let router = envelope_router();
        let request = Request::builder()
            .method(Method::POST)
            .uri("/probe")
            .header(REQUEST_ID_HEADER, TEST_REQUEST_ID)
            .body(Body::from(category.to_string()))
            .unwrap();
        let response = router.oneshot(request).await.expect("router 不可失败");

        let status = response.status();
        let header_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .expect("响应应携带 X-CC-Request-Id")
            .to_str()
            .unwrap()
            .to_string();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).expect("body 应为 JSON 信封");
        (status, body, header_id)
    }

    /// Business Logic（为什么需要这个测试）:
    ///     校验错误必须返回 400 + `validation_error` code，并携带与 header 一致的 request_id；
    ///     客户端据此分支到"参数问题"提示而非重试。
    ///
    /// Code Logic（这个测试做什么）:
    ///     POST `validation`，断言 status=400、code=validation_error、header/body request_id 一致。
    #[tokio::test]
    async fn validation_route_returns_400_envelope() {
        let (status, body, header_id) = send_probe("validation").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "validation_error");
        assert_eq!(body["error"], "参数非法");
        assert_eq!(body["request_id"], TEST_REQUEST_ID);
        assert_eq!(
            body["request_id"], header_id,
            "header/body request_id 必须一致"
        );
        assert_eq!(body["retryable"], false);
        assert!(body["details"].is_object(), "details 默认应为对象");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     缺失实体必须返回 404 + `not_found` code，让客户端区分"资源不存在"与"内部错误"。
    #[tokio::test]
    async fn not_found_route_returns_404_envelope() {
        let (status, body, header_id) = send_probe("not_found").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "not_found");
        assert_eq!(body["error"], "实体不存在");
        assert_eq!(body["request_id"], TEST_REQUEST_ID);
        assert_eq!(body["request_id"], header_id);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     状态冲突（409）必须显式区分于内部错误，调用方据此决定合并/重试。
    #[tokio::test]
    async fn conflict_route_returns_409_envelope() {
        let (status, body, header_id) = send_probe("conflict").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "conflict");
        assert_eq!(body["error"], "状态冲突");
        assert_eq!(body["request_id"], TEST_REQUEST_ID);
        assert_eq!(body["request_id"], header_id);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     暂态不可用（503）必须返回 `unavailable` code，客户端可据此退避重试。
    #[tokio::test]
    async fn unavailable_route_returns_503_envelope() {
        let (status, body, header_id) = send_probe("unavailable").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "unavailable");
        assert_eq!(body["error"], "暂不可用");
        assert_eq!(body["request_id"], TEST_REQUEST_ID);
        assert_eq!(body["request_id"], header_id);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     内部错误（500）必须返回 `internal_error` code，且 retryable=false（保守默认），
    ///     避免客户端盲目重试非幂等操作。
    #[tokio::test]
    async fn internal_route_returns_500_envelope() {
        let (status, body, header_id) = send_probe("internal").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["code"], "internal_error");
        assert_eq!(body["error"], "内部错误");
        assert_eq!(body["request_id"], TEST_REQUEST_ID);
        assert_eq!(body["request_id"], header_id);
        assert_eq!(body["retryable"], false);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `P2pError::from_app_error` 是 handler `?` 传播的核心入口：每个 AppError 分类必须
    ///     映射到约定的 code token 与状态码，并写入 context request_id。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对 validation/not_found/conflict/unavailable/timeout/generic 六类构造 AppError，
    ///     断言 from_app_error 的 status/code/request_id 与约定一致；
    ///     并断言 retryable 仅对 unavailable/timeout 置 true（Finding 2）。
    #[test]
    fn from_app_error_covers_all_status_classes_with_matching_request_id() {
        use crate::error::AppError;
        let context = ctx();
        let cases: Vec<(AppError, StatusCode, &str, bool)> = vec![
            (
                AppError::validation("参数非法"),
                StatusCode::BAD_REQUEST,
                "validation_error",
                false,
            ),
            (
                AppError::not_found("Prompt 不存在"),
                StatusCode::NOT_FOUND,
                "not_found",
                false,
            ),
            (
                AppError::conflict("冲突"),
                StatusCode::CONFLICT,
                "conflict",
                false,
            ),
            (
                AppError::unavailable("不可用"),
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                true,
            ),
            (
                AppError::timeout("超时"),
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                true,
            ),
            (
                AppError::generic("boom"),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                false,
            ),
        ];
        for (app, expected_status, expected_code, expected_retryable) in cases {
            let p2p = P2pError::from_app_error(app, &context, "test");
            assert_eq!(p2p.status(), expected_status, "状态码应匹配 code 约定");
            assert_eq!(p2p.envelope().code, expected_code, "code token 应匹配");
            assert_eq!(
                p2p.envelope().request_id,
                TEST_REQUEST_ID,
                "request_id 必须取自 context"
            );
            assert_eq!(
                p2p.envelope().retryable,
                expected_retryable,
                "retryable 应匹配分类策略（unavailable/timeout=true）"
            );
            // domain_code="test" 应写入 details.domain_code（Finding 2）。
            assert_eq!(
                p2p.envelope()
                    .details
                    .get("domain_code")
                    .and_then(|v| v.as_str()),
                Some("test"),
                "domain_code 应写入 details.domain_code"
            );
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `IntoResponse` 必须同时回写 header 与 body 的 request_id 且二者相等，
    ///     否则客户端无法可靠关联调用链（部分代理可能改写 body）。
    #[tokio::test]
    async fn into_response_header_and_body_request_id_match() {
        let context = ctx();
        let p2p = P2pError::not_found("Preview 不存在", &context);
        let response = p2p.into_response();

        let header_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .expect("响应应携带 X-CC-Request-Id header")
            .to_str()
            .unwrap()
            .to_string();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let envelope: P2pErrorEnvelope =
            serde_json::from_slice(&bytes).expect("body 应可反序列化为信封");

        assert_eq!(header_id, TEST_REQUEST_ID);
        assert_eq!(envelope.request_id, TEST_REQUEST_ID);
        assert_eq!(envelope.code, "not_found");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     仓储/IO 错误（如 sqlite 故障）通过 `from_app_error` 必须落到 500 internal_error，
    ///     不能误升 4xx 让客户端以为是自己请求问题。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 IO 错误的 AppError，断言 status=500、code=internal_error。
    #[test]
    fn repository_failure_maps_to_500_internal() {
        use crate::error::AppError;
        let context = ctx();
        let app = AppError::Io(std::io::Error::other("disk full"));
        let p2p = P2pError::from_app_error(app, &context, "repo");
        assert_eq!(p2p.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(p2p.envelope().code, "internal_error");
        assert_eq!(p2p.envelope().request_id, TEST_REQUEST_ID);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `api_error_to_p2p` 是 preview proxy 错误（413/502/404）映射到信封的边界；
    ///     必须把 413 映射为 Validation(400)、502 映射为 Internal(500)、404 映射为 NotFound(404)。
    #[test]
    fn api_error_to_p2p_preserves_status_class() {
        let context = ctx();

        let payload_too_large = api_error_to_p2p(ApiError::payload_too_large("too big"), &context);
        assert_eq!(payload_too_large.status(), StatusCode::BAD_REQUEST);
        assert_eq!(payload_too_large.envelope().code, "validation_error");

        let bad_gateway = api_error_to_p2p(ApiError::bad_gateway("upstream down"), &context);
        assert_eq!(bad_gateway.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(bad_gateway.envelope().code, "internal_error");

        let not_found = api_error_to_p2p(ApiError::not_found("preview missing"), &context);
        assert_eq!(not_found.status(), StatusCode::NOT_FOUND);
        assert_eq!(not_found.envelope().code, "not_found");

        // request_id 必须与 context 一致。
        assert_eq!(not_found.envelope().request_id, TEST_REQUEST_ID);
    }
}
