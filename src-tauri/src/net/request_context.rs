//! net/request_context.rs — P2P 请求 ID 边界（请求追踪）
//!
//! Business Logic（为什么需要这个模块）:
//!     P2P 局域网调用链横跨多个 cc-partner 实例（A 调 B、B 反向调 A、第三方观察），
//!     一旦日志/错误散落在不同设备，缺少稳定 ID 就无法把它们串成同一条调用链。
//!     客户端通过 `X-CC-Request-Id` header 携带本端生成的 ID；服务端在缺失/非法时生成新 UUID，
//!     在响应 header 与 tracing span 字段里回传同一值，供对端与本端日志互相对齐。
//!
//! Code Logic（这个模块做什么）:
//!     - 常量 `REQUEST_ID_HEADER` 定义 P2P 协议层请求 ID header 名（`X-CC-Request-Id`）。
//!     - `P2pRequestContext` 携带请求级 ID，作为 axum 提取器在 handler 内访问同一 ID。
//!     - `request_id_middleware` 从入站 header 校验/生成 ID，写入 request extensions，
//!       打开带 `request_id` 字段的 tracing span，并把完全相同的 ID 写入响应 header。
//!     - `new_request_id()`：生成新的 UUID v4 字符串，供客户端与服务端共用。

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Response};
use axum::middleware::Next;
use tracing::Instrument;

/// P2P 请求 ID header 名。客户端发送、服务端回传的统一 key。
///
/// Business Logic: 选 `X-CC-Request-Id` 与产品命名（cc-partner）对齐，便于人工 grep 日志。
/// Code Logic: 静态 HeaderName，被 middleware 与客户端注入共用，避免拼写漂移。
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-cc-request-id");

/// 客户端可发送的请求 ID 最大字节数。
///
/// Business Logic: 防止恶意/异常客户端塞入超长 header 撑爆日志或下游解析；
///     128 字节远超 UUID、ULID、Snowflake 等常见 ID 长度（UUID v4 仅 36 字节）。
const REQUEST_ID_MAX_BYTES: usize = 128;

/// P2P 请求上下文：携带本次请求的稳定 ID，供 handler 与日志共用。
///
/// Business Logic（为什么需要这个结构）:
///     handler 内常需要把当前请求 ID 写入业务日志或下游调用（如反查 Workbench、orchestrator），
///     从 request extensions 取 `P2pRequestContext` 比再读 header 更稳妥（middleware 已规范化）。
///
/// Code Logic（这个结构做什么）:
///     - `request_id`：由 middleware 校验/生成后写入 extensions 的最终 ID 字符串；
///     - 实现 `Clone` 便于 handler 把它带入 spawn 的异步任务；不需要 `Copy`（含 String）。
#[derive(Debug, Clone)]
pub struct P2pRequestContext {
    /// 本次请求的稳定 ID（客户端带入或服务端生成的 UUID v4）。
    pub request_id: String,
}

impl P2pRequestContext {
    /// 返回请求 ID 字符串引用。
    ///
    /// Business Logic: handler 与日志调用方普遍需要 `&str` 而非 `&String`，统一暴露引用。
    #[allow(dead_code)]
    pub fn id(&self) -> &str {
        &self.request_id
    }
}

/// 生成新的请求 ID（UUID v4）。
///
/// Business Logic（为什么需要这个函数）:
///     客户端在发起 P2P 请求前需要先生成 ID；服务端在 header 缺失/非法时也需要生成兜底 ID。
///     集中在一个工厂函数，避免两处分别调用 uuid 导致格式漂移（例如未来改成 ULID）。
///
/// Code Logic（这个函数做什么）:
///     调用 `uuid::Uuid::new_v4()` 并返回字符串形式（如 `550e8400-e29b-41d4-a716-446655440000`）。
#[allow(dead_code)]
pub fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 校验客户端传入的请求 ID 是否可被服务端接受。
///
/// Business Logic（为什么需要这个函数）:
///     服务端不能无条件信任 header 里的任意字符串：控制字符会破坏日志聚合、超长字符串撑爆存储、
///     空字符串无法唯一标识请求。需要一个集中入口做最小语义校验，不通过则改用服务端生成的 UUID。
///
/// Code Logic（这个函数做什么）:
///     要求非空、字节数 <= `REQUEST_ID_MAX_BYTES`、且每个字节都是可打印 ASCII
///     （0x20..=0x7E，覆盖字母/数字/常见符号/连字符，排除换行、控制字符与非 ASCII）。
fn is_valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= REQUEST_ID_MAX_BYTES
        && value.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

/// 解析入站请求的请求 ID：合法则保留，否则生成新 UUID。
///
/// Business Logic（为什么需要这个函数）:
///     middleware 入口逻辑：客户端带了合法 ID 就尊重它（保证调用链贯穿对端与本端），
///     否则必须自生成 ID 而不是丢弃（丢失 ID 等于丢失调用链）。
///
/// Code Logic（这个函数做什么）:
///     读取 `X-CC-Request-Id` header，转 `&str` 后过 `is_valid_request_id`；
///     通过则 clone 该值，否则调用 `new_request_id()` 生成兜底。
fn resolve_request_id(headers: &axum::http::HeaderMap) -> String {
    if let Some(value) = headers.get(&REQUEST_ID_HEADER).and_then(|v| v.to_str().ok()) {
        if is_valid_request_id(value) {
            return value.to_string();
        }
    }
    new_request_id()
}

/// P2P 请求 ID 中间件：注入 context、打 tracing span、回写响应 header。
///
/// Business Logic（为什么需要这个函数）:
///     所有 `/api/*` P2P/mobile 路由都应被同一份追踪逻辑覆盖，handler 不应再各自处理 ID。
///     middleware 在请求进入业务 handler 前规范化 ID，在响应返回时把同一 ID 回写到 header，
///     保证客户端能从响应里拿到服务端最终使用的 ID（即使客户端没传，对端也回传生成的 UUID）。
///
/// Code Logic（这个函数做什么）:
///     1. 从入站 header 解析/生成 `request_id`；
///     2. 构造 `P2pRequestContext` 并写入 request extensions，handler 可用提取器访问；
///     3. 打开一个 tracing span（字段 `request_id`），handler 与下游日志会自动落到该 span；
///     4. 运行 next，在返回的 response 上 insert `X-CC-Request-Id` header，值与 context 完全一致。
pub async fn request_id_middleware(mut request: Request<Body>, next: Next) -> Response<Body> {
    let request_id = resolve_request_id(request.headers());
    request.extensions_mut().insert(P2pRequestContext {
        request_id: request_id.clone(),
    });

    let span = tracing::info_span!("p2p_request", request_id = %request_id);
    let response = next.run(request).instrument(span).await;

    // 把与 context 完全相同的 ID 回写到响应 header。
    // HeaderValue::from_str 在 `is_valid_request_id` 已保证可打印 ASCII 下不会失败；
    // 兜底用静态占位符避免极小概率 panic，但这种情况理论上不可达。
    let header_value = HeaderValue::from_str(&request_id)
        .unwrap_or_else(|_| HeaderValue::from_static("invalid"));
    let mut response = response;
    response
        .headers_mut()
        .insert(REQUEST_ID_HEADER, header_value);
    response
}

/// 异步助手：对给定 router 一次性发起请求，返回响应（仅测试用）。
///
/// Business Logic（为什么需要这个函数）:
///     middleware 测试需要构造最小 axum router、注入测试 handler、然后用 `oneshot` 发起请求并取响应，
///     多个测试样本（带/不带 header、不同 ID 形态）共享同一构造逻辑更易读。
///
/// Code Logic（这个函数做什么）:
///     接收测试 router（已套 middleware）和请求，调用 `tower::ServiceExt::oneshot` 发送并返回响应。
#[cfg(test)]
async fn send_request(router: axum::Router, request: Request<Body>) -> Response<Body> {
    use tower::ServiceExt;
    // axum Router 的 Service::Error 是 Infallible，所以 unwrap 不会失败。
    router.oneshot(request).await.expect("router 不可失败")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::routing::get;
    use axum::Router;
    use axum::{extract::Extension, http::StatusCode};

    /// 测试 handler：把 context 里的 ID 写到响应 body，便于断言 handler 看到的 ID 与 header 一致。
    async fn echo_context_handler(
        Extension(ctx): Extension<P2pRequestContext>,
    ) -> (StatusCode, String) {
        (StatusCode::OK, ctx.request_id.clone())
    }

    /// 构造一个最小测试 router：套上 middleware，挂一个返回 context ID 的 handler。
    fn test_router() -> Router {
        Router::new()
            .route("/probe", get(echo_context_handler))
            .layer(axum::middleware::from_fn(request_id_middleware))
    }

    /// 把响应 body 收成字符串（usize::MAX 上限适合测试小响应）。
    async fn body_string(body: Body) -> String {
        let bytes = to_bytes(body, usize::MAX).await.expect("应能读取响应 body");
        String::from_utf8(bytes.to_vec()).expect("测试 handler 返回 UTF-8 字符串")
    }

    /// Business Logic（为什么需要这个测试）:
    ///     客户端带合法 ID 时，服务端必须尊重并使用该 ID，保证 A→B 的调用链在同一条 ID 上贯穿。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造带 `X-CC-Request-Id: abc-123` 的请求，断言响应 header 与 handler body 都看到 `abc-123`。
    #[tokio::test]
    async fn preserves_client_supplied_request_id() {
        let router = test_router();
        let request = Request::builder()
            .uri("/probe")
            .header(REQUEST_ID_HEADER, "abc-123")
            .body(Body::empty())
            .unwrap();
        let response = send_request(router, request).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(REQUEST_ID_HEADER).unwrap(),
            "abc-123"
        );
        assert_eq!(body_string(response.into_body()).await, "abc-123");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     客户端没传 ID 时服务端必须自生成 UUID 而不是丢弃；否则对端无法把响应与日志对齐。
    ///
    /// Code Logic（这个测试做什么）:
    ///     不带 header 发请求，断言响应 header 与 handler body 都返回非空 UUID v4（36 字符含 4 个连字符）。
    #[tokio::test]
    async fn generates_uuid_when_header_missing() {
        let router = test_router();
        let request = Request::builder()
            .uri("/probe")
            .body(Body::empty())
            .unwrap();
        let response = send_request(router, request).await;

        assert_eq!(response.status(), StatusCode::OK);
        let header_id_str = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .expect("missing header should still produce one")
            .to_str()
            .unwrap()
            .to_string();
        assert!(!header_id_str.is_empty());
        // UUID v4 形态：8-4-4-4-12 共 36 字符（不强约束 version 位以兼容未来格式）。
        assert_eq!(header_id_str.len(), 36);
        assert_eq!(header_id_str.matches('-').count(), 4);
        let body = body_string(response.into_body()).await;
        assert_eq!(body, header_id_str);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     空 header 值与缺失等价（无法唯一标识请求），服务端必须替换为 UUID。
    ///
    /// Code Logic（这个测试做什么）:
    ///     发送空 `X-CC-Request-Id`，断言响应 header 不为空且与原值不同（被替换为 UUID）。
    #[tokio::test]
    async fn generates_uuid_when_header_blank() {
        let router = test_router();
        let request = Request::builder()
            .uri("/probe")
            .header(REQUEST_ID_HEADER, "")
            .body(Body::empty())
            .unwrap();
        let response = send_request(router, request).await;

        let header_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(!header_id.is_empty());
        assert_eq!(header_id.len(), 36);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     含控制字符/非 ASCII 字节的 ID 会污染日志聚合或破坏下游解析，服务端必须拒绝并改用 UUID。
    ///
    /// Code Logic（这个测试做什么）:
    ///     客户端经 `HeaderValue::from_str`/`from_bytes` 已无法构造换行等 CTL header，但服务端 middleware
    ///     仍须独立防御。用 `from_bytes` 构造含 0xFF（高字节，`from_bytes` 接受但 `to_str()` 失败）的值，
    ///     断言 `resolve_request_id` 走兜底分支生成 UUID 而非原值。
    #[tokio::test]
    async fn generates_uuid_when_header_invalid() {
        let mut headers = axum::http::HeaderMap::new();
        // 高字节 0xFF 不是合法 ASCII；`from_bytes` 接受，但 `to_str()` 会失败 → 触发 fallback。
        let raw = HeaderValue::from_bytes(b"bad\xff").expect("from_bytes 允许高字节");
        headers.insert(REQUEST_ID_HEADER, raw);

        let resolved = resolve_request_id(&headers);
        assert_eq!(resolved.len(), 36);
        assert!(resolved.is_ascii());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     非 ASCII（如中文、emoji）ID 同样会被部分日志后端拒绝或破坏，应替换为 UUID。
    ///
    /// Code Logic（这个测试做什么）:
    ///     发送包含中文的 ID，断言响应 header 不含非 ASCII 且为新生成的 UUID。
    #[tokio::test]
    async fn generates_uuid_when_header_non_ascii() {
        let router = test_router();
        let request = Request::builder()
            .uri("/probe")
            .header(REQUEST_ID_HEADER, "请求-1")
            .body(Body::empty())
            .unwrap();
        let response = send_request(router, request).await;

        let header_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(header_id.is_ascii());
        assert_eq!(header_id.len(), 36);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     超长 ID 即使是可打印 ASCII 也会撑爆日志存储，必须替换为 UUID。
    ///
    /// Code Logic（这个测试做什么）:
    ///     发送超过 `REQUEST_ID_MAX_BYTES`（128）的纯字母 ID，断言响应 header 是 36 字符 UUID。
    #[tokio::test]
    async fn generates_uuid_when_header_too_long() {
        let router = test_router();
        let too_long = "a".repeat(REQUEST_ID_MAX_BYTES + 1);
        let request = Request::builder()
            .uri("/probe")
            .header(REQUEST_ID_HEADER, &too_long)
            .body(Body::empty())
            .unwrap();
        let response = send_request(router, request).await;

        let header_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(header_id.len(), 36);
        assert_ne!(header_id, too_long);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     handler extensions 里的 context 必须与响应 header 看到同一 ID，否则两者会对不上。
    ///
    /// Code Logic（这个测试做什么）:
    ///     发送合法 ID，断言 handler body（来自 context）与响应 header 字符串完全相等。
    #[tokio::test]
    async fn extension_and_header_see_same_id() {
        let router = test_router();
        let known_id = "req-xyz-789";
        let request = Request::builder()
            .uri("/probe")
            .header(REQUEST_ID_HEADER, known_id)
            .body(Body::empty())
            .unwrap();
        let response = send_request(router, request).await;

        let header_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let body = body_string(response.into_body()).await;
        assert_eq!(header_id, known_id);
        assert_eq!(body, known_id);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     并发请求不能共享同一 ID，否则不同调用会被错误串成一条链。
    ///
    /// Code Logic（这个测试做什么）:
    ///     并发发送两个无 header 请求，收集它们的 handler body（即各自 context ID），断言二者不同。
    #[tokio::test]
    async fn parallel_requests_do_not_share_ids() {
        // Router 在 oneshot 后会被消耗，因此每次构造一个新 router 以保持并发独立。
        let probe = || async {
            let router = test_router();
            let request = Request::builder()
                .uri("/probe")
                .body(Body::empty())
                .unwrap();
            let response = send_request(router, request).await;
            body_string(response.into_body()).await
        };

        let (id_a, id_b) = tokio::join!(probe(), probe());
        assert_ne!(id_a, id_b);
        assert_eq!(id_a.len(), 36);
        assert_eq!(id_b.len(), 36);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `is_valid_request_id` 是 middleware 的核心校验入口，必须严格匹配可打印 ASCII 且拒绝边界。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言合法 UUID、含连字符 ID 通过；空字符串、超长、换行、TAB、非 ASCII 均被拒绝。
    #[test]
    fn is_valid_request_id_accepts_printable_ascii_only() {
        assert!(is_valid_request_id(
            "550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(is_valid_request_id("abc-123"));
        assert!(is_valid_request_id("a")); // 单字符也合法
        assert!(is_valid_request_id("request-1"));
        assert!(is_valid_request_id(&"a".repeat(REQUEST_ID_MAX_BYTES))); // 上界

        assert!(!is_valid_request_id(""));
        assert!(!is_valid_request_id(&"a".repeat(REQUEST_ID_MAX_BYTES + 1))); // 超长
        assert!(!is_valid_request_id("bad\nid"));
        assert!(!is_valid_request_id("tab\tid"));
        // 控制字符边界：0x1F 应拒绝，0x20（空格）应通过。
        assert!(!is_valid_request_id("\u{1F}"));
        assert!(is_valid_request_id("with space"));
        // 0x7F（DEL）应拒绝；0x7E（`~`）应通过。
        assert!(!is_valid_request_id("\u{7F}"));
        assert!(is_valid_request_id("tilde~"));
        // 非 ASCII 一律拒绝。
        assert!(!is_valid_request_id("请求"));
    }
}
