//! net/error_response.rs — P2P/HTTP 边界标准错误信封
//!
//! Business Logic（为什么需要这个模块）:
//!     Tauri IPC（命令层）错误保持老形态 `{"error": "<msg>"}`，前端老逻辑无需改动；而 P2P/HTTP
//!     边界需要更丰富的稳定错误信封（`code`/`request_id`/`retryable`/`details`），让对端/移动端
//!     能据此分类、关联调用链、决定重试。本模块定义边界专用的 `P2pError`/`P2pErrorEnvelope`，
//!     不触碰 `AppError` 的 IPC 序列化，两套契约完全解耦。
//!
//! Code Logic（这个模块做什么）:
//!     - `P2pErrorCode`：稳定 code token（`validation_error`/`not_found`/`conflict`/`unavailable`
//!       /`timeout`/`internal_error`），客户端据此分支处理，不依赖人类文案。
//!     - `P2pErrorEnvelope`：JSON 信封，保留 `error` 字段做 legacy 兼容，附加新字段。
//!     - `P2pError`：携带 envelope + HTTP 状态码 + request_id 的错误值，实现 `IntoResponse`
//!       输出状态码 + `X-CC-Request-Id` header + JSON body。
//!     - `from_app_error`：从命令层 AppError + request 上下文构造边界错误。
//!     - `parse_remote_error`：客户端侧解析对端响应，同时兼容老 `{error}` 与新信封。

// 路由层已切到 P2pError：模块内的 pub API（构造器、Envelope 等）被 axum handler 与
// 客户端 RemoteErrorBody 解析共用；个别仅供路由内部使用的辅助项仍可能未在所有构建中
// 被引用，保留 allow(dead_code) 避免误删稳定 API 表面。
#![allow(dead_code)]

use crate::error::{AppError, AppErrorCategory};
use crate::net::request_context::P2pRequestContext;
use axum::body::Body;
use axum::http::{HeaderValue, Response, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

/// P2P/HTTP 边界 handler 的统一返回类型。
///
/// Business Logic（为什么需要这个别名）:
///     axum handler 的错误返回类型必须实现 `IntoResponse`；用统一别名让所有 P2P 路由
///     在签名上一致（`Result<Json<T>, P2pError>`），便于grep/重构，也避免每个 handler
///     各自写完整错误类型。
///
/// Code Logic（这个别名做什么）:
///     即 `Result<T, P2pError>`；Ok 携带成功 DTO（通常由 handler 包成 `Json<T>`），Err 携带边界错误。
pub type P2pResult<T> = Result<T, P2pError>;

/// P2P 错误 header：回写请求 ID（与 `request_context::REQUEST_ID_HEADER` 同名）。
///
/// Business Logic: 客户端从响应 header 拿到对端最终使用的 request_id，即便 body 被中间代理改写，
///     header 仍是可信的调用链关联。
/// Code Logic: 复用 `crate::net::request_context::REQUEST_ID_HEADER`，避免两处常量漂移。
pub(crate) const REQUEST_ID_HEADER: axum::http::HeaderName =
    crate::net::request_context::REQUEST_ID_HEADER;

/// 稳定的错误 code token，供客户端程序化分支处理。
///
/// Business Logic（为什么需要这个枚举）:
///     人类可读文案（`error` 字段）可能本地化/调整，客户端不能依赖它做分支；code token 是
///     稳定契约，每个 token 对应一个 HTTP 状态码与一类处理策略。
///
/// Code Logic（这个枚举做什么）:
///     serde 序列化为约定的 snake_case 字符串常量（显式 rename 保证 token 形态稳定）；
///     `http_status()` 给出对应状态码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum P2pErrorCode {
    /// 客户端输入校验失败（HTTP 400）。
    #[serde(rename = "validation_error")]
    Validation,
    /// 资源不存在（HTTP 404）。
    #[serde(rename = "not_found")]
    NotFound,
    /// 资源状态冲突（HTTP 409）。
    #[serde(rename = "conflict")]
    Conflict,
    /// 服务暂不可用（HTTP 503）。
    #[serde(rename = "unavailable")]
    Unavailable,
    /// 操作超时（HTTP 504）。
    #[serde(rename = "timeout")]
    Timeout,
    /// 其他内部错误（HTTP 500）。
    #[serde(rename = "internal_error")]
    Internal,
}

impl P2pErrorCode {
    /// 返回该 code token 对应的 HTTP 状态码。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `IntoResponse` 需要根据 code 设置状态码，集中映射避免边界多处重复且漂移。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Validation→400, NotFound→404, Conflict→409, Unavailable→503, Timeout→504, Internal→500。
    pub fn http_status(self) -> StatusCode {
        match self {
            P2pErrorCode::Validation => StatusCode::BAD_REQUEST,
            P2pErrorCode::NotFound => StatusCode::NOT_FOUND,
            P2pErrorCode::Conflict => StatusCode::CONFLICT,
            P2pErrorCode::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            P2pErrorCode::Timeout => StatusCode::GATEWAY_TIMEOUT,
            P2pErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// 从内部稳定分类映射到边界 code token。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `AppErrorCategory` 是命令层稳定分类，`P2pErrorCode` 是边界稳定 token，
    ///     两者一一对应但命名空间隔离；本函数是二者唯一映射点，避免散落 if/match。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 variant 直接映射，分类已穷尽，无需兜底。
    pub fn from_category(category: AppErrorCategory) -> Self {
        match category {
            AppErrorCategory::Validation => P2pErrorCode::Validation,
            AppErrorCategory::NotFound => P2pErrorCode::NotFound,
            AppErrorCategory::Conflict => P2pErrorCode::Conflict,
            AppErrorCategory::Unavailable => P2pErrorCode::Unavailable,
            AppErrorCategory::Timeout => P2pErrorCode::Timeout,
            AppErrorCategory::Internal => P2pErrorCode::Internal,
        }
    }
}

/// 默认 domain code：当路由未提供更细粒度 code 时使用。
///
/// Business Logic: 多数错误只需分类级 code（如 `not_found`），少数路由需要更细（如
///     `sync.push.conflict`）；提供一个稳定默认值，避免路由层强制传 code。
pub const DEFAULT_DOMAIN_CODE: &str = "p2p";

/// P2P/HTTP 边界标准错误信封（JSON body）。
///
/// Business Logic（为什么需要这个结构）:
///     客户端需要一个稳定结构来：1) 用 code 分支处理；2) 用 request_id 关联调用链；
///     3) 用 retryable 决定重试；4) 用 details 拿结构化补充信息（字段错误明细等）。
///     同时为兼容老客户端，必须保留 `error: string` 字段（与老 `{error}` body 等价）。
///
/// Code Logic（这个结构做什么）:
///     - `error`：人类可读消息（legacy 兼容，必填，字符串）。
///     - `code`：稳定 code token（snake_case），客户端分支处理入口。
///     - `request_id`：调用链 ID，与响应 header `X-CC-Request-Id` 完全一致。
///     - `retryable`：是否可安全重试，默认 false（仅幂等且明确的暂态错误才置 true）。
///     - `details`：结构化补充信息，默认空对象 `{}`，避免客户端 null 处理。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct P2pErrorEnvelope {
    /// 人类可读错误消息（legacy 客户端兼容，等同于老 `{error}` body 的字段）。
    pub error: String,
    /// 稳定 code token（snake_case），客户端据此分支处理。
    pub code: String,
    /// 调用链请求 ID，与响应 header `X-CC-Request-Id` 完全一致。
    pub request_id: String,
    /// 是否可安全重试（默认 false；仅幂等且明确的暂态错误置 true）。
    pub retryable: bool,
    /// 结构化补充信息（默认 `{}`）。
    #[serde(default = "default_details")]
    pub details: serde_json::Value,
}

/// `details` 字段默认值工厂：返回空对象 `{}`。
///
/// Business Logic: 信封序列化时若 details 缺省应输出 `{}` 而非 null，避免客户端 null 处理分支。
fn default_details() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl P2pErrorEnvelope {
    /// 构造一个新的错误信封，retryable 默认 false、details 默认 `{}`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `from_app_error` 与路由层手动构造都需要一个统一入口，保证默认值一致
    ///     （retryable=false、details={}），避免每处重复写默认。
    ///
    /// Code Logic（这个函数做什么）:
    ///     接收消息、code token、request_id，retryable 置 false，details 置空对象。
    pub fn new(message: impl Into<String>, code: P2pErrorCode, request_id: impl Into<String>) -> Self {
        Self {
            error: message.into(),
            code: code.to_token_string(),
            request_id: request_id.into(),
            retryable: false,
            details: default_details(),
        }
    }

    /// 显式标记该错误可安全重试（仅在路由幂等策略允许时调用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     retryable 默认 false 是安全默认；只有路由明确知道该操作幂等且该分类属暂态错误
    ///     （如 503 unavailable、504 timeout）才置 true，避免误重试非幂等写操作。
    ///
    /// Code Logic（这个函数做什么）:
    ///     消耗 self 返回新的 self，retryable=true，便于 builder 风格链式调用。
    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// 设置 details 字段（结构化补充信息）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     校验错误需要返回字段级错误明细（如 `{"fields": {"name": "required"}}`），
    ///     提供一个入口避免每处手动构造 serde_json::Value。
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }
}

/// 边界错误值：携带信封 + HTTP 状态码，实现 `IntoResponse`。
///
/// Business Logic（为什么需要这个结构）:
///     axum handler 的错误返回类型需要实现 `IntoResponse`；本结构把信封与状态码绑定，
///     并在响应里回写 `X-CC-Request-Id` header，让客户端从 header 与 body 都能拿到 request_id。
///
/// Code Logic（这个结构做什么）:
///     - `envelope`：JSON 信封（`error`/`code`/`request_id`/`retryable`/`details`）。
///     - `status`：HTTP 状态码（由 code 决定，构造时一次性确定）。
#[derive(Debug, Clone)]
pub struct P2pError {
    /// 错误信封（也是响应 body）。
    envelope: P2pErrorEnvelope,
    /// HTTP 状态码。
    status: StatusCode,
}

impl P2pError {
    /// 从命令层 AppError + 请求上下文构造边界错误。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     P2P handler 复用命令层 helper（返回 `AppError`），需要一个统一入口把它转成边界错误：
    ///     分类映射 code token、状态码，并把 request context 的 ID 写进信封与响应 header。
    ///     retryable 默认 false（保守默认），仅幂等路由显式开启。
    ///
    /// Code Logic（这个函数做什么）:
    ///     1. `AppError::classify()` 得到稳定分类；
    ///     2. `P2pErrorCode::from_category` 映射到 code token；
    ///     3. token.http_status() 得到状态码；
    ///     4. 信封 request_id 取自 context，保证 header/body 一致。
    pub fn from_app_error(
        error: AppError,
        context: &P2pRequestContext,
        domain_code: &str,
    ) -> Self {
        let _ = domain_code; // 保留参数：后续细粒度 code 可能使用，当前用 code token。
        let category = error.classify();
        let code = P2pErrorCode::from_category(category);
        let status = code.http_status();
        let envelope = P2pErrorEnvelope::new(error.to_string(), code, &context.request_id);
        Self { envelope, status }
    }

    /// 从指定 code token + 消息 + 请求上下文构造边界错误。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     部分 HTTP-only 路由需要绕过 `AppError` 直接表达边界语义（如 proxy 的 413/502），
    ///     用本函数显式指定 code token 与消息，避免再经过 AppError 的分类映射。
    ///
    /// Code Logic（这个函数做什么）:
    ///     直接用 code token 决定状态码（`code.http_status()`），消息与 request_id 写入信封。
    pub fn from_code(
        message: impl Into<String>,
        code: P2pErrorCode,
        context: &P2pRequestContext,
    ) -> Self {
        let status = code.http_status();
        let envelope = P2pErrorEnvelope::new(message, code, &context.request_id);
        Self { envelope, status }
    }

    /// 校验错误（HTTP 400）便捷构造。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     路由层判定入参非法时直接构造边界 400，调用方不必先包成 AppError::validation。
    pub fn validation(message: impl Into<String>, context: &P2pRequestContext) -> Self {
        Self::from_code(message, P2pErrorCode::Validation, context)
    }

    /// not-found（HTTP 404）便捷构造。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     previewId、session、project 等路由实体缺失时直接构造边界 404。
    pub fn not_found(message: impl Into<String>, context: &P2pRequestContext) -> Self {
        Self::from_code(message, P2pErrorCode::NotFound, context)
    }

    /// 冲突（HTTP 409）便捷构造。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     状态转换/乐观锁失败时直接构造边界 409。
    pub fn conflict(message: impl Into<String>, context: &P2pRequestContext) -> Self {
        Self::from_code(message, P2pErrorCode::Conflict, context)
    }

    /// 暂不可用（HTTP 503）便捷构造。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     依赖未就绪、容量上限等暂态不可用时直接构造边界 503。
    pub fn unavailable(message: impl Into<String>, context: &P2pRequestContext) -> Self {
        Self::from_code(message, P2pErrorCode::Unavailable, context)
    }

    /// 超时（HTTP 504）便捷构造。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     上游/对端在限定时间内未响应时直接构造边界 504。
    pub fn timeout(message: impl Into<String>, context: &P2pRequestContext) -> Self {
        Self::from_code(message, P2pErrorCode::Timeout, context)
    }

    /// 内部错误（HTTP 500）便捷构造。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     其他未分类错误兜底为边界 500，避免裸 StatusCode 响应破坏信封契约。
    pub fn internal(message: impl Into<String>, context: &P2pRequestContext) -> Self {
        Self::from_code(message, P2pErrorCode::Internal, context)
    }

    /// 返回内部信封引用（测试与日志使用）。
    #[cfg(test)]
    pub(crate) fn envelope(&self) -> &P2pErrorEnvelope {
        &self.envelope
    }

    /// 返回 HTTP 状态码（测试使用）。
    #[cfg(test)]
    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }
}

impl IntoResponse for P2pError {
    /// 转换为 axum response：状态码 + `X-CC-Request-Id` header + JSON 信封 body。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     客户端需要同时从 header 与 body 拿到 request_id（部分代理可能改写 body），
    ///     因此响应必须同时回写两者，且值完全一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 `(status, header, Json(envelope))` 元组转 response；header 值取自 envelope.request_id，
    ///     保证 header/body 一致。
    fn into_response(self) -> Response<Body> {
        let request_id = self.envelope.request_id.clone();
        let mut response = (self.status, Json(self.envelope)).into_response();
        // HeaderValue::from_str：request_id 由 request_context middleware 校验为可打印 ASCII，
        // 理论上不会失败；兜底用静态占位符避免 panic（不可达路径）。
        let header_value = HeaderValue::from_str(&request_id)
            .unwrap_or_else(|_| HeaderValue::from_static("invalid"));
        response
            .headers_mut()
            .insert(REQUEST_ID_HEADER, header_value);
        response
    }
}

/// P2P/HTTP 边界错误解析：客户端解析对端错误响应，兼容老 `{error}` 与新信封。
///
/// Business Logic（为什么需要这个类型）:
///     对端可能是尚未升级到 v1 的旧版本（返回 `{error: "..."}`），也可能是新版本（返回完整信封）。
///     客户端必须用一个统一解析入口同时处理两种形态，避免对端版本探测负担。
///
/// Code Logic（这个类型做什么）:
///     反序列化时：`error` 必填，其余字段 `#[serde(default)]`；并提供 `is_legacy()` 判断是否老形态。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RemoteErrorBody {
    /// 人类可读错误消息（老/新形态都有）。
    pub error: String,
    /// 稳定 code token（仅新形态；老形态缺省为 `unknown`）。
    #[serde(default = "default_remote_code")]
    pub code: String,
    /// 调用链请求 ID（仅新形态；老形态缺省为空串）。
    #[serde(default)]
    pub request_id: String,
    /// 是否可安全重试（仅新形态；老形态缺省为 false）。
    #[serde(default)]
    pub retryable: bool,
    /// 结构化补充信息（仅新形态；老形态或缺省为 `{}`）。
    #[serde(default = "default_details")]
    pub details: serde_json::Value,
}

/// 老形态响应缺少 code 字段时的占位 token。
///
/// Business Logic: 客户端需要 code 做分支，老形态没有 code，用稳定占位 `unknown` 表示"对端未提供"，
///     客户端可据此回落到基于 HTTP 状态码的处理。
fn default_remote_code() -> String {
    "unknown".to_string()
}

impl RemoteErrorBody {
    /// 判断是否为老形态响应（无 code/request_id 等新字段）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     客户端可能需要区分对端版本（如决定是否上报 richer telemetry）；老形态 code 恒为
    ///     `unknown`（缺省值），据此判断。
    ///
    /// Code Logic（这个函数做什么）:
    ///     code == "unknown"（缺省值）即视为老形态。
    pub fn is_legacy(&self) -> bool {
        self.code == "unknown"
    }
}

/// code token 字符串化辅助 trait（仅供内部模块使用）。
///
/// Business Logic: `P2pErrorCode` 的 serde 序列化为 snake_case，但构造信封时需要拿到字符串；
///     通过一个内部方法避免重复 `serde_json::to_string` 调用与引号处理。
impl P2pErrorCode {
    /// 返回 code token 的 snake_case 字符串常量。
    ///
    /// Code Logic（这个函数做什么）:
    ///     显式 match 输出字符串，与 `#[serde(rename = ...)]` 输出保持一致。
    fn to_token_string(self) -> String {
        match self {
            P2pErrorCode::Validation => "validation_error",
            P2pErrorCode::NotFound => "not_found",
            P2pErrorCode::Conflict => "conflict",
            P2pErrorCode::Unavailable => "unavailable",
            P2pErrorCode::Timeout => "timeout",
            P2pErrorCode::Internal => "internal_error",
        }
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::request_context::P2pRequestContext;

    /// 构造测试用 request context（固定 request_id）。
    fn ctx(id: &str) -> P2pRequestContext {
        P2pRequestContext {
            request_id: id.to_string(),
        }
    }

    // ===== Step 1: 状态码/code/retryability 映射表测试 =====

    /// Business Logic（为什么需要这个测试）:
    ///     HTTP 状态码映射是 P2P 错误信封的核心契约，必须覆盖每个 code token 的边界值，
    ///     防止 mapping 漂移导致客户端拿到错误的语义。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言每个 P2pErrorCode 的 http_status() 恰为约定值（400/404/409/503/504/500）。
    #[test]
    fn http_status_mapping_covers_all_codes() {
        assert_eq!(
            P2pErrorCode::Validation.http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(P2pErrorCode::NotFound.http_status(), StatusCode::NOT_FOUND);
        assert_eq!(P2pErrorCode::Conflict.http_status(), StatusCode::CONFLICT);
        assert_eq!(
            P2pErrorCode::Unavailable.http_status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            P2pErrorCode::Timeout.http_status(),
            StatusCode::GATEWAY_TIMEOUT
        );
        assert_eq!(
            P2pErrorCode::Internal.http_status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     从命令层 AppError 到边界 P2pError 的全链路映射必须覆盖每个分类，确保 handler
    ///     用 `?` 传播 AppError 时边界层返回正确状态码与 code token。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对每个 AppError 分类构造 P2pError，断言 status 与 envelope.code 匹配约定。
    #[test]
    fn from_app_error_maps_all_categories() {
        let cases: Vec<(AppError, StatusCode, &str)> = vec![
            (
                AppError::validation("参数非法"),
                StatusCode::BAD_REQUEST,
                "validation_error",
            ),
            (
                AppError::not_found("Prompt 不存在"),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                AppError::conflict("状态冲突"),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                AppError::unavailable("暂不可用"),
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
            ),
            (
                AppError::timeout("超时"),
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
            ),
            (
                AppError::generic("boom"),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            ),
            (
                AppError::Io(std::io::Error::other("disk")),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            ),
        ];

        for (app, expected_status, expected_code) in cases {
            let p2p = P2pError::from_app_error(app, &ctx("req-abc"), DEFAULT_DOMAIN_CODE);
            assert_eq!(
                p2p.status(),
                expected_status,
                "状态码应匹配 code token 约定"
            );
            assert_eq!(p2p.envelope().code, expected_code, "code token 应匹配");
            // request_id 必须与 context 完全一致。
            assert_eq!(p2p.envelope().request_id, "req-abc");
            // retryable 默认 false（保守默认）。
            assert!(
                !p2p.envelope().retryable,
                "retryable 默认必须为 false"
            );
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     信封必须保留 legacy `error: string` 字段，老客户端仅读该字段也能正常工作。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 P2pErrorEnvelope，断言 `error` 字段为字符串消息，且新字段也齐全。
    #[test]
    fn envelope_serialization_keeps_legacy_error_field() {
        let envelope = P2pErrorEnvelope::new("Prompt 不存在", P2pErrorCode::NotFound, "req-1");
        let json = serde_json::to_value(&envelope).expect("envelope 应可序列化");
        let obj = json.as_object().expect("应为对象");
        // legacy 字段：error 为字符串。
        assert_eq!(obj.get("error").unwrap().as_str().unwrap(), "Prompt 不存在");
        // 新字段。
        assert_eq!(obj.get("code").unwrap().as_str().unwrap(), "not_found");
        assert_eq!(obj.get("request_id").unwrap().as_str().unwrap(), "req-1");
        assert_eq!(obj.get("retryable").unwrap().as_bool().unwrap(), false);
        // details 默认 {}。
        assert_eq!(obj.get("details").unwrap().as_object().unwrap().len(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     details 默认必须为 `{}`（空对象），不能是 null，避免客户端 null 处理分支。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造不带 details 的信封，序列化后断言 details == {}（非 null）。
    #[test]
    fn envelope_details_defaults_to_empty_object() {
        let envelope = P2pErrorEnvelope::new("x", P2pErrorCode::Internal, "req");
        let json = serde_json::to_value(&envelope).unwrap();
        assert!(
            json.get("details").unwrap().is_object(),
            "details 默认应为对象"
        );
        assert_eq!(json.get("details").unwrap().as_object().unwrap().len(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     retryable 默认必须为 false（保守默认），只有显式 with_retryable(true) 才置 true。
    ///     这是安全设计：误重试非幂等写操作会造成数据损坏。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造信封，断言默认 retryable=false；with_retryable(true) 后 retryable=true。
    #[test]
    fn retryable_defaults_false_and_can_be_explicitly_enabled() {
        let env = P2pErrorEnvelope::new("x", P2pErrorCode::Internal, "req");
        assert!(!env.retryable);
        let env_retry = env.with_retryable(true);
        assert!(env_retry.retryable);
        // 反向也能关闭。
        let env_off = env_retry.with_retryable(false);
        assert!(!env_off.retryable);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     响应 header 的 request_id 必须与 body envelope.request_id 完全一致，
    ///     客户端从任一处读取都能拿到同一调用链 ID。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 P2pError，转 response，断言 header 与 body 的 request_id 字符串相等。
    #[tokio::test]
    async fn into_response_header_matches_body_request_id() {
        use axum::body::to_bytes;
        let app = AppError::not_found("Prompt 不存在");
        let p2p = P2pError::from_app_error(app, &ctx("req-xyz-789"), DEFAULT_DOMAIN_CODE);
        let response = p2p.into_response();

        let header_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .expect("响应应携带 X-CC-Request-Id header")
            .to_str()
            .unwrap()
            .to_string();

        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: P2pErrorEnvelope =
            serde_json::from_slice(&bytes).expect("body 应可反序列化为信封");

        assert_eq!(header_id, "req-xyz-789");
        assert_eq!(body.request_id, "req-xyz-789");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     IntoResponse 必须设置正确的 HTTP 状态码（与 code token 约定一致），否则客户端
    ///     基于状态码的重试/分支会失效。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对几个代表性分类构造 P2pError，转 response，断言 response.status() 与约定一致。
    #[tokio::test]
    async fn into_response_sets_status_from_code() {
        let cases = vec![
            (AppError::validation("x"), StatusCode::BAD_REQUEST),
            (AppError::not_found("x"), StatusCode::NOT_FOUND),
            (AppError::conflict("x"), StatusCode::CONFLICT),
            (AppError::unavailable("x"), StatusCode::SERVICE_UNAVAILABLE),
            (AppError::timeout("x"), StatusCode::GATEWAY_TIMEOUT),
            (AppError::generic("x"), StatusCode::INTERNAL_SERVER_ERROR),
        ];
        for (app, expected) in cases {
            let p2p = P2pError::from_app_error(app, &ctx("req"), DEFAULT_DOMAIN_CODE);
            let status = p2p.into_response().status();
            assert_eq!(status, expected);
        }
    }

    // ===== Step 2: legacy 兼容解析测试（客户端侧） =====

    /// Business Logic（为什么需要这个测试）:
    ///     对端可能是旧版本（仅返回 `{error: "..."}`），客户端必须能解析这种老形态，
    ///     并把缺失字段填安全默认值，避免因字段缺失报错。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化老形态 `{error: "boom"}`，断言 error 字段正确，code/request_id/retryable/details 为默认。
    #[test]
    fn client_parses_legacy_error_envelope() {
        let body: RemoteErrorBody =
            serde_json::from_str(r#"{"error":"boom"}"#).expect("老形态应可解析");
        assert_eq!(body.error, "boom");
        assert_eq!(body.code, "unknown");
        assert_eq!(body.request_id, "");
        assert!(!body.retryable);
        assert!(body.details.is_object());
        assert!(body.details.as_object().unwrap().is_empty());
        assert!(body.is_legacy());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     对端是新版本（返回完整信封），客户端必须能解析所有字段，与对端序列化保持一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化完整信封 JSON，断言每个字段值匹配。
    #[test]
    fn client_parses_full_envelope() {
        let json = r#"{
            "error": "Prompt 不存在",
            "code": "not_found",
            "request_id": "req-abc",
            "retryable": false,
            "details": {"resource": "prompt", "id": 42}
        }"#;
        let body: RemoteErrorBody = serde_json::from_str(json).expect("完整信封应可解析");
        assert_eq!(body.error, "Prompt 不存在");
        assert_eq!(body.code, "not_found");
        assert_eq!(body.request_id, "req-abc");
        assert!(!body.retryable);
        assert_eq!(body.details.get("resource").unwrap().as_str().unwrap(), "prompt");
        assert_eq!(body.details.get("id").unwrap().as_i64().unwrap(), 42);
        assert!(!body.is_legacy());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧形态 `{error: "..."}` 必须被识别为 legacy（code 缺省为 "unknown"），
    ///     而新形态即使 code 字段值恰好为 "unknown" 也不应误判 —— 但因协议未约定 code=unknown
    ///     为合法新 token，简单约定 code=="unknown" 即视为 legacy 足够实用。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化显式 code=not_found 的信封，断言 is_legacy() == false；
    ///     反序列化无 code 的老形态，断言 is_legacy() == true。
    #[test]
    fn is_legacy_flag_distinguishes_envelope_versions() {
        let new_body: RemoteErrorBody =
            serde_json::from_str(r#"{"error":"x","code":"validation_error"}"#).unwrap();
        assert!(!new_body.is_legacy());

        let old_body: RemoteErrorBody = serde_json::from_str(r#"{"error":"x"}"#).unwrap();
        assert!(old_body.is_legacy());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     信封必须能稳定往返（序列化→反序列化），保证对端序列化与客户端解析一致，
    ///     不丢字段、不改 code token 大小写。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造信封 → 序列化为 JSON → 反序列化回 RemoteErrorBody，断言所有字段一致。
    #[test]
    fn envelope_round_trips_through_json() {
        let envelope = P2pErrorEnvelope::new("超时", P2pErrorCode::Timeout, "req-rt")
            .with_retryable(true)
            .with_details(serde_json::json!({"upstream": "claude-cli"}));
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: RemoteErrorBody = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.error, "超时");
        assert_eq!(parsed.code, "timeout");
        assert_eq!(parsed.request_id, "req-rt");
        assert!(parsed.retryable);
        assert_eq!(parsed.details.get("upstream").unwrap().as_str().unwrap(), "claude-cli");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     details 缺省（新形态但没带 details）必须回落为 `{}`，不能因字段缺失报错或变 null。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化缺 details 的新形态信封，断言 details 为空对象。
    #[test]
    fn client_envelope_details_defaults_to_empty_object_when_missing() {
        let json = r#"{"error":"x","code":"not_found","request_id":"r","retryable":false}"#;
        let body: RemoteErrorBody = serde_json::from_str(json).unwrap();
        assert!(body.details.is_object());
        assert!(body.details.as_object().unwrap().is_empty());
    }

    // ===== Step 4: 安全性 —— 敏感字段不泄漏到 details/debug =====

    /// Business Logic（为什么需要这个测试）:
    ///     错误信封会跨设备传输，绝不能把敏感字段（Authorization、token、Prompt 文本、
    ///     绝对 home 路径）泄漏到 details 或 Display。AppError 的 Display 文案来自构造时的消息，
    ///     因此测试用敏感消息构造，断言信封 details 与序列化结果不含未预期字段。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 AppError（消息本身可能含敏感字面量，这是调用方责任），但 details 显式置为
    ///     不含敏感数据的结构，断言 details 序列化结果只含预期 key，不含 Authorization/token 等。
    #[test]
    fn details_does_not_leak_sensitive_fields() {
        let safe_details = serde_json::json!({
            "field": "name",
            "reason": "required"
        });
        let envelope = P2pErrorEnvelope::new(
            "参数非法",
            P2pErrorCode::Validation,
            "req-secret",
        )
        .with_details(safe_details);

        let json = serde_json::to_string(&envelope).unwrap();
        // 断言 details 序列化结果不含敏感关键字（这些只可能因误传泄漏）。
        assert!(!json.contains("Authorization"));
        assert!(!json.contains("Bearer"));
        assert!(!json.contains("sk-ant-")); // Claude API key 前缀
        assert!(!json.contains("/Users/"));
        assert!(!json.contains("/home/"));
        // request_id 是调用链 ID（非密钥），允许出现在 JSON。
        assert!(json.contains("req-secret"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     AppError 的 Display 文案是构造方传入的消息；P2pErrorEnvelope 只把它放进 error 字段，
    ///     不应额外把 AppError Debug（可能含底层数据库连接串等）写进 body。本测试回归保护
    ///     `from_app_error` 不调用 Debug，只用 Display。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造带 IO 错误的 AppError，转 P2pError，序列化信封，断言 body 不含底层 Debug
    ///     特征（如 `Custom { kind: ...` 等 Rust Debug 残留）。
    #[test]
    fn from_app_error_uses_display_not_debug() {
        let app = AppError::Io(std::io::Error::other("disk full"));
        let p2p = P2pError::from_app_error(app, &ctx("req"), DEFAULT_DOMAIN_CODE);
        let json = serde_json::to_string(p2p.envelope()).unwrap();
        // Display 应输出 "IO 错误: disk full"，不含 Debug 残留。
        assert!(json.contains("IO 错误: disk full"));
        assert!(!json.contains("Custom {"));
        assert!(!json.contains("kind:"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     code token 大小写契约：`#[serde(rename_all = "snake_case")]` 与 `to_token_string()`
    ///     必须输出一致的 snake_case，否则对端 serde 反序列化会失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 P2pErrorCode 每个变体，断言字符串与 to_token_string() 一致。
    #[test]
    fn code_token_serialization_matches_to_token_string() {
        let codes = vec![
            P2pErrorCode::Validation,
            P2pErrorCode::NotFound,
            P2pErrorCode::Conflict,
            P2pErrorCode::Unavailable,
            P2pErrorCode::Timeout,
            P2pErrorCode::Internal,
        ];
        for code in codes {
            let serde_str = serde_json::to_string(&code).unwrap();
            let token_str = format!("\"{}\"", code.to_token_string());
            assert_eq!(
                serde_str, token_str,
                "serde 序列化与 to_token_string 必须一致"
            );
        }
    }
}
