//! net/peer_error.rs — 客户端侧统一的 P2P 响应解析与错误分类
//!
//! Business Logic（为什么需要这个模块）:
//!     对端可能是尚未升级到 v1 的旧版本（返回老 `{error: "..."}`），也可能是新版本（返回完整
//!     `P2pErrorEnvelope` 信封）。客户端此前的错误处理散落在 `peer_client` 与 orchestrator
//!     `remote_client` 两处，且都依赖人类可读文案做分支，既不稳健也无法区分"离线/不支持/业务失败/
//!     响应非法"四类情况。本模块提供单一解析入口 `parse_peer_response_parts`，一次消费
//!     (HTTP 状态、响应 header 的 request_id、响应 body 字节)，产出统一的 `PeerCallError`，
//!     让调用方用 `code` + HTTP 状态做业务决策，而不是字符串匹配。
//!
//! Code Logic（这个模块做什么）:
//!     - `PeerCallError`：客户端对端调用的统一错误枚举（`Network`/`Unsupported`/`InvalidResponse`/
//!       `Remote`）。`Remote` 同时承载 v1 信封字段与 v0 合成字段（code=`legacy.remote_error`）。
//!     - `parse_peer_response_parts`：纯函数解析入口，接收 (status, header request_id, body bytes,
//!       url)，返回 `Result<T, PeerCallError>`。成功响应反序列化为 T；非 2xx 解析为 `Remote`
//!       （v1 校验 header/body request_id 一致，否则 `InvalidResponse`）；无法解析的 body/空 body
//!       归为 `InvalidResponse`，与业务错误明确区分。
//!     - `parse_peer_response`：异步包装，从 `reqwest::Response` 抽出三要素后委托纯函数版本。
//!
//! 兼容性说明: v0 老形态 `{error: "..."}` 被识别为 legacy（code 缺省为 "unknown"）后合成为
//!     `PeerCallError::Remote { code: "legacy.remote_error", legacy: true, ... }`，**不再**用作
//!     字符串业务决策；调用方若需区分新旧对端可读 `legacy` 字段或 `code`。

use crate::net::error_response::RemoteErrorBody;
use crate::net::request_context::REQUEST_ID_HEADER;
use serde::de::DeserializeOwned;

/// v0 老形态响应合成的稳定 code token。
///
/// Business Logic: 老形态没有真实 code，但客户端错误枚举的 `code` 字段必须有稳定值供分支；
///     用 `legacy.remote_error` 明确标识"来自旧版对端的远端错误"，避免与 v1 token 混淆。
pub const LEGACY_REMOTE_ERROR_CODE: &str = "legacy.remote_error";

/// 客户端侧 P2P 调用的统一错误分类。
///
/// Business Logic（为什么需要这个枚举）:
///     调用方需要明确区分四类失败并采取不同策略：
///     1. `Network` —— 对端离线/不可达/DNS 失败，可重试或回退到下一对端；
///     2. `Unsupported` —— 对端在线但不具备所需能力（capability gate 拦截），不应重试同一对端；
///     3. `InvalidResponse` —— 对端响应非法（request_id 不一致、body 无法解析、空 body 配错误状态），
///        属协议违例，应告警而非当业务错误处理；
///     4. `Remote` —— 对端返回的业务错误（v1 信封或 v0 老形态），用 `code` + `status` 做决策。
///
/// Code Logic（这个枚举做什么）:
///     - `Network` 携带 url 与原始 reqwest 错误。
///     - `Unsupported` 携带 url 与缺失的 capability token。
///     - `InvalidResponse` 携带 url 与原因说明（用于日志）。
///     - `Remote` 携带 url/状态码/code/message/request_id/retryable/legacy；`details` 通过
///       `remote_details()` 访问（多数调用方不需要，避免枚举变量过宽）。
#[derive(Debug, thiserror::Error)]
pub enum PeerCallError {
    /// 网络/连接层失败（reqwest send 或 body 读取返回 Err）。
    #[error("对端调用网络失败 ({url}): {source}")]
    Network {
        /// 对端 URL，便于日志定位是哪个 peer。
        url: String,
        /// 原始 reqwest 错误。
        #[source]
        source: reqwest::Error,
    },

    /// 对端在线但不支持所需能力（capability gate 在调用新路由前拦截）。
    #[error("对端 ({url}) 不支持能力 {capability}")]
    Unsupported {
        /// 对端 URL。
        url: String,
        /// 缺失的能力 token（如 `errors.envelope.v1`）。
        capability: &'static str,
    },

    /// 对端响应非法：request_id 不一致、body 无法解析、或错误状态配空/非 JSON body。
    #[error("对端响应无法解析 ({url}): {reason}")]
    InvalidResponse {
        /// 对端 URL。
        url: String,
        /// 非法原因（用于日志，不依赖对端文案做分支）。
        reason: String,
    },

    /// 对端返回的业务错误（v1 信封或 v0 老形态合成）。
    ///
    /// Business Logic: 调用方应优先用 `code` + `status` 做分支（如 `unavailable`/503 触发重试），
    ///     `message` 仅用于日志/展示；`legacy` 标识来自旧版对端，便于遥测区分。
    #[error("对端业务错误 ({url}): HTTP {status} [{code}] {message}")]
    Remote {
        /// 对端 URL。
        url: String,
        /// HTTP 状态码（非 2xx）。
        status: u16,
        /// 稳定 code token；v0 老形态合成为 `legacy.remote_error`。
        code: String,
        /// 人类可读错误消息（v0 即 `error` 字段原文）。
        message: String,
        /// 调用链请求 ID（v1 取自信封并校验 header 一致；v0 取自响应 header 或空）。
        request_id: String,
        /// 是否可安全重试（仅 v1 信封携带；v0 合成为 false）。
        retryable: bool,
        /// 是否来自旧版 v0 对端（code 为合成值）。
        legacy: bool,
    },
}

impl PeerCallError {
    /// 返回 `Remote` 变体的 code（业务决策入口）；其它变体返回 None。
    ///
    /// Business Logic: 调用方常需 `if err.code() == Some("unavailable")` 做重试判断，
    ///     提供访问器避免重复 match。
    pub fn code(&self) -> Option<&str> {
        match self {
            PeerCallError::Remote { code, .. } => Some(code),
            _ => None,
        }
    }

    /// 返回 `Remote` 变体的 HTTP 状态码；其它变体返回 None。
    pub fn status(&self) -> Option<u16> {
        match self {
            PeerCallError::Remote { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// 返回 `Remote` 变体的 request_id（调用链关联）；其它变体返回 None。
    pub fn request_id(&self) -> Option<&str> {
        match self {
            PeerCallError::Remote { request_id, .. } => Some(request_id),
            _ => None,
        }
    }

    /// 返回是否来自旧版 v0 对端（仅 `Remote { legacy: true }` 为真）。
    pub fn is_legacy(&self) -> bool {
        matches!(self, PeerCallError::Remote { legacy: true, .. })
    }
}

/// 纯函数解析入口：消费 (status, header request_id, body bytes, url) 一次，产出统一结果。
///
/// Business Logic（为什么需要这个函数）:
///     客户端两处调用点（peer_client、orchestrator remote_client）此前各自字符串解析对端错误，
///     行为分叉。集中到一个纯函数便于用 fixture（不依赖网络）穷举 v1 信封/v0 老形态/非法 JSON/
///     空 body/request_id 不匹配等契约场景，保证两处调用方看到完全一致的错误分类。
///
/// Code Logic（这个函数做什么）:
///     - 2xx：把 body 反序列化为 T；失败 → `InvalidResponse`（不掩盖为业务错误）。
///     - 非 2xx 且 body 可解析为 `RemoteErrorBody`：
///       - v1（非 legacy）：校验 header/body request_id 一致（二者皆非空且不等 → `InvalidResponse`），
///         否则产出 `Remote { code, legacy: false, ... }`。
///       - v0（legacy）：合成 code=`legacy.remote_error`、retryable=false、request_id 取自 header。
///     - 非 2xx 且 body 无法解析（含空 body）：→ `InvalidResponse`，与业务错误明确区分。
pub fn parse_peer_response_parts<T: DeserializeOwned>(
    status: u16,
    header_request_id: Option<&str>,
    body: &[u8],
    url: &str,
) -> Result<T, PeerCallError> {
    if is_success_status(status) {
        return serde_json::from_slice::<T>(body).map_err(|e| PeerCallError::InvalidResponse {
            url: url.to_string(),
            reason: format!("成功响应反序列化失败 (HTTP {status}): {e}"),
        });
    }

    // 非 2xx：尝试解析为错误 body（v1 信封或 v0 老形态共用 RemoteErrorBody）。
    let parsed: RemoteErrorBody = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(_) => {
            return Err(PeerCallError::InvalidResponse {
                url: url.to_string(),
                reason: format!(
                    "非 2xx 响应 body 无法解析为错误信封 (HTTP {status}, {} 字节)",
                    body.len()
                ),
            });
        }
    };

    if parsed.is_legacy() {
        // v0 老形态：合成稳定 code，request_id 取自 header（body 里没有）。
        return Err(PeerCallError::Remote {
            url: url.to_string(),
            status,
            code: LEGACY_REMOTE_ERROR_CODE.to_string(),
            message: parsed.error,
            request_id: header_request_id.unwrap_or("").to_string(),
            retryable: false,
            legacy: true,
        });
    }

    // v1 信封：header/body request_id 都非空且不一致 → 协议违例。
    let body_id = parsed.request_id.as_str();
    if let Some(header_id) = header_request_id {
        if !body_id.is_empty() && header_id != body_id {
            return Err(PeerCallError::InvalidResponse {
                url: url.to_string(),
                reason: format!(
                    "request_id 不一致: header={header_id} body={body_id}"
                ),
            });
        }
    }
    let request_id = if !body_id.is_empty() {
        body_id.to_string()
    } else {
        header_request_id.unwrap_or("").to_string()
    };

    Err(PeerCallError::Remote {
        url: url.to_string(),
        status,
        code: parsed.code,
        message: parsed.error,
        request_id,
        retryable: parsed.retryable,
        legacy: false,
    })
}

/// 异步包装：从 `reqwest::Response` 抽出 status/header request_id/body bytes 后委托纯函数。
///
/// Business Logic: 调用方（peer_client、orchestrator remote_client）拿到的是 reqwest::Response，
///     集中抽取避免每处重复 headers().get + bytes().await。
///
/// Code Logic: body 读取失败归为 `Network`（传输层中断，与 send 失败同语义）。
pub async fn parse_peer_response<T: DeserializeOwned>(
    response: reqwest::Response,
    url: &str,
) -> Result<T, PeerCallError> {
    let status = response.status().as_u16();
    let header_request_id = response
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let url_owned = url.to_string();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| PeerCallError::Network {
            url: url_owned,
            source: e,
        })?;
    parse_peer_response_parts::<T>(status, header_request_id.as_deref(), &bytes, url)
}

/// 判定 HTTP 状态码是否为成功（2xx）。
///
/// Code Logic: 与 reqwest::StatusCode::is_success 一致，但接收裸 u16 便于纯函数测试。
fn is_success_status(status: u16) -> bool {
    (200..300).contains(&status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    /// 测试用成功 DTO（仅用于泛型反序列化断言）。
    #[derive(Debug, Deserialize, PartialEq)]
    struct SampleDto {
        ok: bool,
        device_id: String,
    }

    /// 测试常量：被测对端 URL。
    const URL: &str = "http://192.168.1.5:8765/api/probe";

    // ===== Step 1 契约: 完整 v1 信封 =====

    /// Business Logic（为什么需要这个测试）:
    ///     v1 对端返回完整错误信封时，客户端必须解析出所有结构化字段，且 code/status 可用于
    ///     程序化分支（不依赖文案）。这是新协议的核心契约。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 503 + 完整 v1 信封 body（含 code/request_id/retryable/details），
    ///     header 与 body request_id 一致，断言 PeerCallError::Remote 字段全匹配且 legacy=false。
    #[test]
    fn parses_full_v1_envelope_with_matching_request_id() {
        let body = r#"{
            "error": "对端忙",
            "code": "unavailable",
            "request_id": "req-abc",
            "retryable": true,
            "details": {"queue": "sync"}
        }"#;
        let err = parse_peer_response_parts::<SampleDto>(503, Some("req-abc"), body.as_bytes(), URL)
            .expect_err("503 应为错误");

        match &err {
            PeerCallError::Remote {
                status,
                code,
                message,
                request_id,
                retryable,
                legacy,
                ..
            } => {
                assert_eq!(*status, 503);
                assert_eq!(*code, "unavailable");
                assert_eq!(*message, "对端忙");
                assert_eq!(*request_id, "req-abc");
                assert!(*retryable);
                assert!(!*legacy, "v1 信封不应标记为 legacy");
            }
            other => panic!("应为 Remote，实际: {other:?}"),
        }
        assert_eq!(err.code(), Some("unavailable"));
        assert_eq!(err.status(), Some(503));
        assert!(!err.is_legacy());
    }

    // ===== Step 1 契约: legacy `{ "error": "..." }` =====

    /// Business Logic（为什么需要这个测试）:
    ///     旧版对端返回 `{error: "旧错误"}` 时，客户端必须识别为 legacy 并合成稳定 code，
    ///     **不**把文案当业务决策依据；调用方若误用文案匹配会在对端本地化文案后失效。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 409 + 老形态 body（无 code/request_id），断言 Remote.code == legacy.remote_error、
    ///     legacy==true、request_id 取自 header、retryable==false。
    #[test]
    fn parses_legacy_error_body_as_synthesized_remote() {
        let body = r#"{"error":"旧错误"}"#;
        let err = parse_peer_response_parts::<SampleDto>(409, Some("hdr-1"), body.as_bytes(), URL)
            .expect_err("409 应为错误");

        match &err {
            PeerCallError::Remote {
                status,
                code,
                message,
                request_id,
                retryable,
                legacy,
                ..
            } => {
                assert_eq!(*status, 409);
                assert_eq!(*code, LEGACY_REMOTE_ERROR_CODE);
                assert_eq!(*message, "旧错误");
                assert_eq!(*request_id, "hdr-1", "legacy 应保留 header request_id");
                assert!(!*retryable);
                assert!(*legacy, "老形态应标记为 legacy");
            }
            other => panic!("应为 Remote，实际: {other:?}"),
        }
        assert!(err.is_legacy());
        assert_eq!(err.code(), Some(LEGACY_REMOTE_ERROR_CODE));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     老 legacy body 的文案不能被当业务决策依据——即使文案里出现 "conflict" 字样，
    ///     合成 code 仍是 `legacy.remote_error`，调用方必须用 code 而非文案分支。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造文案含 "conflict" 的老形态 body，断言 code 仍为 legacy 合成值（不被文案污染）。
    #[test]
    fn legacy_body_does_not_derive_business_code_from_text() {
        let body = br#"{"error":"state conflict happened"}"#;
        let err = parse_peer_response_parts::<SampleDto>(409, None, body, URL)
            .expect_err("409 应为错误");
        // 关键断言：code 不是 "conflict"，而是合成的 legacy token。
        assert_eq!(err.code(), Some(LEGACY_REMOTE_ERROR_CODE));
    }

    // ===== Step 1 契约: invalid JSON =====

    /// Business Logic（为什么需要这个测试）:
    ///     对端代理/崩溃可能返回非 JSON body（HTML、截断堆栈）；客户端必须归为
    ///     `InvalidResponse` 而非误判为业务错误，避免把代理 500 当成对端业务失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 502 + 非 JSON body，断言 `InvalidResponse`（非 Remote）。
    #[test]
    fn invalid_json_body_becomes_invalid_response() {
        let body = b"<html>502 Bad Gateway</html>";
        let err = parse_peer_response_parts::<SampleDto>(502, None, body, URL)
            .expect_err("502 应为错误");
        match &err {
            PeerCallError::InvalidResponse { reason, .. } => {
                assert!(reason.contains("无法解析"), "原因应说明无法解析: {reason}");
            }
            other => panic!("应为 InvalidResponse，实际: {other:?}"),
        }
        assert_eq!(err.code(), None, "InvalidResponse 不应携带 code");
    }

    // ===== Step 1 契约: 错误状态 + 空 body =====

    /// Business Logic（为什么需要这个测试）:
    ///     某些对端/代理在错误时返回空 body；客户端必须归为 `InvalidResponse`（无法提取业务错误），
    ///     与有信封的 `Remote` 明确区分，让调用方知道"对端没给可用的错误信息"。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 500 + 空 body，断言 `InvalidResponse`。
    #[test]
    fn empty_body_with_error_status_becomes_invalid_response() {
        let err = parse_peer_response_parts::<SampleDto>(500, Some("r"), b"", URL)
            .expect_err("500 空 body 应为错误");
        assert!(matches!(err, PeerCallError::InvalidResponse { .. }));
    }

    // ===== Step 1 契约: v1 header/body request_id 不一致 =====

    /// Business Logic（为什么需要这个测试）:
    ///     v1 信封要求 header 与 body 的 request_id 一致；不一致说明中间链路（代理/对端 bug）
    ///     篡改了调用链 ID，客户端必须拒绝并归为 `InvalidResponse`，不能信任任何一侧。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 v1 信封 body.request_id="body-id"、header="header-id"，断言 `InvalidResponse`
    ///     且原因含两侧 ID。
    #[test]
    fn v1_envelope_request_id_mismatch_becomes_invalid_response() {
        let body = br#"{
            "error": "x",
            "code": "internal_error",
            "request_id": "body-id",
            "retryable": false
        }"#;
        let err = parse_peer_response_parts::<SampleDto>(500, Some("header-id"), body, URL)
            .expect_err("500 应为错误");
        match err {
            PeerCallError::InvalidResponse { reason, .. } => {
                assert!(reason.contains("header-id"), "原因应含 header ID: {reason}");
                assert!(reason.contains("body-id"), "原因应含 body ID: {reason}");
            }
            other => panic!("应为 InvalidResponse，实际: {other:?}"),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     v1 信封若 body 未带 request_id 但 header 有，应信任 header 并正常产出 Remote
    ///     （不强制要求 body 必须带 ID，兼容只回写 header 的对端）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 v1 信封 body 无 request_id、header="hdr"，断言 Remote.request_id=="hdr"。
    #[test]
    fn v1_envelope_without_body_request_id_uses_header() {
        let body = br#"{"error":"x","code":"not_found","retryable":false}"#;
        let err = parse_peer_response_parts::<SampleDto>(404, Some("hdr-9"), body, URL)
            .expect_err("404 应为错误");
        match err {
            PeerCallError::Remote { request_id, code, .. } => {
                assert_eq!(request_id, "hdr-9");
                assert_eq!(code, "not_found");
            }
            other => panic!("应为 Remote，实际: {other:?}"),
        }
    }

    // ===== Step 1 契约: 成功响应解析 =====

    /// Business Logic（为什么需要这个测试）:
    ///     2xx 成功响应必须正常反序列化为目标 DTO；解析失败（DTO 不匹配）归为
    ///     `InvalidResponse`，不被误当业务错误。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 200 + 合法 DTO body，断言 Ok；再构造 200 + 字段缺失 body，断言 InvalidResponse。
    #[test]
    fn success_response_decodes_to_target_dto() {
        let body = br#"{"ok":true,"device_id":"dev-1"}"#;
        let dto =
            parse_peer_response_parts::<SampleDto>(200, Some("r"), body, URL).expect("应解析成功");
        assert!(dto.ok);
        assert_eq!(dto.device_id, "dev-1");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     2xx 但 body 不是预期 DTO（字段类型不匹配/缺失必填）应归为 `InvalidResponse`，
    ///     与对端业务错误区分——这是协议契约违例，不是对端拒绝。
    #[test]
    fn success_response_with_wrong_dto_becomes_invalid_response() {
        // 缺 device_id 字段。
        let body = br#"{"ok":true}"#;
        let err = parse_peer_response_parts::<SampleDto>(200, None, body, URL)
            .expect_err("字段缺失应失败");
        assert!(matches!(err, PeerCallError::InvalidResponse { .. }));
    }

    // ===== 辅助函数与 Display 烟测 =====

    /// Business Logic（为什么需要这个测试）:
    ///     `is_success_status` 是解析分支的核心判定，必须正确覆盖 2xx/非 2xx 边界。
    #[test]
    fn is_success_status_covers_2xx_range() {
        assert!(is_success_status(200));
        assert!(is_success_status(204));
        assert!(is_success_status(299));
        assert!(!is_success_status(199));
        assert!(!is_success_status(300));
        assert!(!is_success_status(404));
        assert!(!is_success_status(503));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     各错误变体的 Display 必须含 url 上下文，便于日志 grep 定位对端。
    #[test]
    fn error_variants_display_url_context() {
        let invalid = format!(
            "{}",
            PeerCallError::InvalidResponse {
                url: "http://1.2.3.4:8765/x".to_string(),
                reason: "bad".to_string(),
            }
        );
        assert!(invalid.contains("1.2.3.4:8765"));

        let unsupported = format!(
            "{}",
            PeerCallError::Unsupported {
                url: "http://1.2.3.4:8765/x".to_string(),
                capability: "errors.envelope.v1",
            }
        );
        assert!(unsupported.contains("1.2.3.4:8765"));
        assert!(unsupported.contains("errors.envelope.v1"));

        let remote = format!(
            "{}",
            PeerCallError::Remote {
                url: "http://1.2.3.4:8765/x".to_string(),
                status: 503,
                code: "unavailable".to_string(),
                message: "busy".to_string(),
                request_id: "r".to_string(),
                retryable: true,
                legacy: false,
            }
        );
        assert!(remote.contains("503"));
        assert!(remote.contains("unavailable"));
    }
}
