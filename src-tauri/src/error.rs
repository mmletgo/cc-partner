//! error.rs — 应用统一错误类型
//!
//! Business Logic（为什么需要这个模块）:
//!     Tauri 命令返回 `Result<T, E>` 时，E 必须实现 `Serialize` 才能跨 IPC 传给前端。
//!     Python 端的 HTTP handler 把异常序列化成 `{"error": "msg"}` 返回 500，
//!     Rust 侧需对齐这个契约，让前端无需改动错误处理逻辑。同时 axum HTTP handler 也
//!     复用此错误类型，需额外实现 `IntoResponse` 以返回 500 + `{"error": "..."}`。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `AppError` 枚举，用 thiserror 派生 `Error`/`Display`，
//!     手动实现 `serde::Serialize`（序列化成 `{"error": "..."}`），
//!     实现 `axum::response::IntoResponse`（HTTP 500 + 同结构 JSON，对照 Python handler），
//!     并为 sqlx::Error / serde_json::Error / io::Error 等实现 `From`，
//!     使命令体与 handler 内都可用 `?` 优雅传播。
//!
//! Finding 3 补丁: 新增 `Remote` variant 携带对端 v1 信封的结构化元数据
//!     （`code`/`status`/`retryable`/`request_id`），让 `classify()` 据此映射稳定分类，
//!     上层重试/退避决策不再依赖人类可读文案。IPC Serialize 仍只输出 `{"error": "..."}`，
//!     前端契约不变；元数据仅由命令层/Rust 内部消费。

/// 远端对端错误信封携带的结构化元数据（Finding 3）。
///
/// Business Logic（为什么需要这个类型）:
///     `PeerCallError::Remote` 已解析出对端信封的 `code`/HTTP `status`/`retryable`/`request_id`，
///     但旧的 `remote_error_to_app_error` 把它们全部丢弃，只保留 `message` 字符串，导致：
///     1. 上层无法用 `classify()` 区分 `unavailable`/`conflict`/`validation`（一律 Internal），
///        重试/退避只能靠字符串匹配；
///     2. `request_id` 丢失，多跳代理链无法关联日志；
///     3. `retryable` 丢失，客户端无法判断是否安全重试。
///     本结构把这些字段原样挂在 `AppError::Remote` 上，由 `classify()` 翻译为稳定分类。
///
/// Code Logic（这个结构做什么）:
///     纯数据载体，所有字段 `pub`；由 `AppError::remote()` 构造，由 `AppError::remote_meta()` 读取。
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteErrorMeta {
    /// 对端信封的稳定 code token（v1 如 `unavailable`/`conflict`；v0 合成 `legacy.remote_error`）。
    pub code: String,
    /// 对端 HTTP 状态码（非 2xx）。
    pub status: u16,
    /// 对端信封声明的 retryable（v0 合成 false）。
    pub retryable: bool,
    /// 对端调用链 request_id（v1 取自信封并校验 header 一致；v0 取自响应 header）。
    pub request_id: String,
    /// 对端信封的结构化 details（含 `domain_code`；v0 合成为空对象）—— Finding 3。
    ///
    /// Business Logic: 多跳代理/客户端据此做细粒度路由（如区分 `transfer.chunk` 与
    ///     `transfer.init` 失败）。保留为 `serde_json::Value` 而非具体类型，因为 details
    ///     是开放对象（各路由可写入自定义字段如 `fields`/`queue`）。
    pub details: serde_json::Value,
}

/// 应用统一错误类型，覆盖数据库、序列化、IO、业务 not-found 等场景。
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// 数据库错误（sqlx）
    #[error("数据库错误: {0}")]
    Db(#[from] sqlx::Error),
    /// JSON 序列化/反序列化错误
    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),
    /// IO 错误（读写配置文件等）
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    /// 业务层 not-found（如 Prompt 不存在）
    #[error("{0}")]
    NotFound(String),
    /// 其他业务错误（参数非法、状态不满足等）
    #[error("{0}")]
    #[allow(dead_code)]
    Bad(String),
    /// Tauri 运行时错误（托盘/菜单/窗口 API 失败）
    #[error("Tauri 错误: {0}")]
    Tauri(#[from] tauri::Error),
    /// 客户端输入校验失败（参数缺失、格式非法、超长等）。
    ///
    /// Business Logic: 为 HTTP/P2P 边界提供稳定的 400 分类；IPC Serialize 输出
    ///     `{"error":"...","code":"validation"}`（R12 M2 稳定 category code）。
    /// Code Logic: Display 直接展示消息，分类时映射到 `AppErrorCategory::Validation`。
    #[error("{0}")]
    Validation(String),
    /// 资源状态冲突（并发覆盖、唯一约束冲突、版本不一致等）。
    ///
    /// Business Logic: 区分于普通 internal，供 HTTP 边界返回 409，调用方可据此决定重试/合并。
    /// Code Logic: Display 直接展示消息，分类时映射到 `AppErrorCategory::Conflict`。
    #[error("{0}")]
    Conflict(String),
    /// 服务暂不可用（依赖未就绪、容量上限、维护中等），通常可重试。
    ///
    /// Business Logic: HTTP 边界返回 503，与 internal(500) 区分以便客户端退避重试。
    /// Code Logic: Display 直接展示消息，分类时映射到 `AppErrorCategory::Unavailable`。
    #[error("{0}")]
    Unavailable(String),
    /// 操作超时（上游/对端在限定时间内未响应）。
    ///
    /// Business Logic: HTTP 边界返回 504，与网络层不可达(503) 区分，便于诊断链路。
    /// Code Logic: Display 直接展示消息，分类时映射到 `AppErrorCategory::Timeout`。
    #[error("{0}")]
    Timeout(String),
    /// 远端对端返回的业务错误（携带 v1 信封结构化元数据）。
    ///
    /// Business Logic（Finding 3）: 远端 client（orchestrator/workbench）经 `parse_peer_response`
    ///     解析对端错误后，把 `PeerCallError::Remote` 的 code/status/retryable/request_id 原样
    ///     存入本 variant，让 `classify()` 据稳定 code（而非人类可读文案）映射分类，
    ///     供上层重试/退避决策使用。Display 只展示 message；IPC Serialize 输出 error+category code。
    #[error("{message}")]
    Remote {
        /// 人类可读错误消息（v1 信封 `error` 字段或 v0 老形态原文）。
        message: String,
        /// 对端信封结构化元数据（code/status/retryable/request_id）。
        meta: RemoteErrorMeta,
    },
}

/// 稳定的内部错误分类，供 HTTP/P2P 边界映射状态码与重试策略。
///
/// Business Logic（为什么需要这个枚举）:
///     `AppError` 本身面向命令层（IPC），变体会随实现演进；HTTP/P2P 边界需要一份稳定的
///     分类视图来决定状态码（400/404/409/503/504/500）与重试策略，二者解耦后边界契约不会
///     因新增 AppError variant 而意外变化。
///
/// Code Logic（这个枚举做什么）:
///     通过 `AppError::classify()` 得到，每个分类对应一个固定的 HTTP 状态码与稳定 code token。
///     新增 variant 默认归入 `Internal`，避免误升 4xx 影响客户端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppErrorCategory {
    /// 客户端输入校验失败（HTTP 400）。
    Validation,
    /// 资源不存在（HTTP 404）。
    NotFound,
    /// 资源状态冲突（HTTP 409）。
    Conflict,
    /// 服务暂不可用（HTTP 503）。
    Unavailable,
    /// 操作超时（HTTP 504）。
    Timeout,
    /// 其他内部错误（HTTP 500）。
    Internal,
}

impl AppError {
    /// 返回该错误的稳定分类，供 HTTP/P2P 边界映射状态码与重试策略。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     边界层不能直接 match AppError 的 variant（variant 会演进），需要一个稳定入口
    ///     把任意 AppError 归入有限分类。新增 variant 默认归入 Internal，保证向后兼容。
    ///
    /// Code Logic（这个函数做什么）:
    ///     显式 variant 映射到对应分类；Db/Json/Io/Tauri/Bad 等兜底归入 Internal。
    ///     `Remote` 变体（Finding 3）依据对端信封的稳定 code token 映射，不再依赖文案：
    ///       - `validation_error`/`unauthorized`/`forbidden`/`payload_too_large`/
    ///         `method_not_allowed` → Validation；
    ///       - `not_found` → NotFound；
    ///       - `conflict` → Conflict；
    ///       - `unavailable` → Unavailable；
    ///       - `timeout` → Timeout；
    ///       - 其余（含 `legacy.remote_error`/`internal_error`/未知 token）→ Internal。
    pub fn classify(&self) -> AppErrorCategory {
        match self {
            AppError::NotFound(_) => AppErrorCategory::NotFound,
            AppError::Validation(_) => AppErrorCategory::Validation,
            AppError::Conflict(_) => AppErrorCategory::Conflict,
            AppError::Unavailable(_) => AppErrorCategory::Unavailable,
            AppError::Timeout(_) => AppErrorCategory::Timeout,
            AppError::Remote { meta, .. } => classify_remote_code(&meta.code),
            // Db/Json/Io/Tauri/Bad 均视为内部错误（500）：
            // 这些 variant 既有调用点大多包裹 IO/进程失败，不应误升 4xx。
            AppError::Db(_)
            | AppError::Json(_)
            | AppError::Io(_)
            | AppError::Bad(_)
            | AppError::Tauri(_) => AppErrorCategory::Internal,
        }
    }

    /// 返回 `Remote` variant 携带的对端元数据（Finding 3）；其它 variant 返回 None。
    ///
    /// Business Logic: 上层重试/退避逻辑可读 `meta.retryable`/`meta.status`/`meta.code`，
    ///     无需把文案当决策依据；调用链也可读 `meta.request_id` 关联多跳日志。
    pub fn remote_meta(&self) -> Option<&RemoteErrorMeta> {
        match self {
            AppError::Remote { meta, .. } => Some(meta),
            _ => None,
        }
    }

    /// 返回稳定业务/分类 code，供测试与调用方程序化分支。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     部分业务错误（如 `review_diff_unavailable`、`runtime_owner_required`）以稳定 token
    ///     作为 Conflict/Validation 消息；调用方需要 `err.code()` 做精确匹配，不能依赖本地化长文案。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `Remote` 返回信封 `meta.code`；其它带消息 variant 原样返回消息字符串；
    ///     Db/Json/Io/Tauri 返回 `internal_error`。
    pub fn code(&self) -> &str {
        match self {
            AppError::Remote { meta, .. } => meta.code.as_str(),
            AppError::Conflict(msg)
            | AppError::Validation(msg)
            | AppError::NotFound(msg)
            | AppError::Unavailable(msg)
            | AppError::Timeout(msg)
            | AppError::Bad(msg) => msg.as_str(),
            AppError::Db(_) | AppError::Json(_) | AppError::Io(_) | AppError::Tauri(_) => {
                "internal_error"
            }
        }
    }
}

/// 把对端信封的稳定 code token 映射为内部稳定分类（Finding 3）。
///
/// Business Logic（为什么独立成函数）:
///     `AppError::classify()` 与远端 client 的错误映射都需要同一套 code→category 规则，
///     集中到一处避免漂移。映射基于稳定 token 字符串，不依赖人类可读文案。
///
/// Code Logic（这个函数做什么）:
///     显式 token → category；未覆盖 token（含 v0 合成的 `legacy.remote_error`、
///     `unknown` 占位、未来新 token）兜底为 Internal，保证向后兼容。
pub fn classify_remote_code(code: &str) -> AppErrorCategory {
    match code {
        "validation_error" | "unauthorized" | "forbidden" | "payload_too_large"
        | "method_not_allowed" => AppErrorCategory::Validation,
        "not_found" => AppErrorCategory::NotFound,
        "conflict" => AppErrorCategory::Conflict,
        "unavailable" => AppErrorCategory::Unavailable,
        "timeout" => AppErrorCategory::Timeout,
        // legacy.remote_error / internal_error / unknown / 未来新 token 兜底。
        _ => AppErrorCategory::Internal,
    }
}

/// AppError 便捷构造器集合（HTTP 边界分类映射）。
///
/// Business Logic: 命令体与路由层用这些构造器书写更自然（如 `ok_or_else(|| AppError::not_found(...))?`），
///     每个构造器对应一个稳定分类，供 HTTP 边界映射状态码。
impl AppError {
    /// not-found 的便捷构造（HTTP 边界映射 404）。
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    /// 通用业务错误便捷构造（HTTP 边界映射 500）。
    pub fn generic(msg: impl Into<String>) -> Self {
        Self::Bad(msg.into())
    }

    /// 校验错误便捷构造（HTTP 边界映射 400）。
    ///
    /// Business Logic: 路由层判定入参非法时使用此构造，分类稳定为 Validation，
    ///     避免与 `generic()`（Internal/500）混淆。
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }

    /// 冲突错误便捷构造（HTTP 边界映射 409）。
    ///
    /// Business Logic: 并发覆盖、唯一约束冲突等状态不一致场景使用，供调用方决定重试/合并。
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    /// 不可用错误便捷构造（HTTP 边界映射 503）。
    ///
    /// Business Logic: 依赖未就绪、容量上限等暂态不可用场景使用，通常可重试。
    pub fn unavailable(msg: impl Into<String>) -> Self {
        Self::Unavailable(msg.into())
    }

    /// 超时错误便捷构造（HTTP 边界映射 504）。
    ///
    /// Business Logic: 上游/对端在限定时间内未响应时使用，区别于网络层不可达(503)。
    pub fn timeout(msg: impl Into<String>) -> Self {
        Self::Timeout(msg.into())
    }

    /// 远端对端业务错误便捷构造（携带 v1 信封元数据，Finding 3）。
    ///
    /// Business Logic: orchestrator/workbench 远端 client 解析 `PeerCallError::Remote` 后
    ///     用此构造把 code/status/retryable/request_id 原样挂入 AppError，让 `classify()`
    ///     据稳定 code 映射分类（而非文案），上层重试决策可读 `remote_meta()`。
    pub fn remote(message: impl Into<String>, meta: RemoteErrorMeta) -> Self {
        Self::Remote {
            message: message.into(),
            meta,
        }
    }
}

impl AppError {
    /// 返回稳定 IPC 分类 code token（R12 M2）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     前端 terminal replay 等路径必须按稳定 code 分类可恢复/永久错误，
    ///     不能再依赖本地化 message 子串（中英文都会漂移）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 `classify()` 映射为固定 `&'static str`：
    ///     validation / not_found / conflict / unavailable / timeout / internal。
    ///     **不是** P2P 信封的 domain.action token（如 validation_error），IPC 边界保持短 token。
    pub fn ipc_category_code(&self) -> &'static str {
        match self.classify() {
            AppErrorCategory::Validation => "validation",
            AppErrorCategory::NotFound => "not_found",
            AppErrorCategory::Conflict => "conflict",
            AppErrorCategory::Unavailable => "unavailable",
            AppErrorCategory::Timeout => "timeout",
            AppErrorCategory::Internal => "internal",
        }
    }
}

/// 让 AppError 可序列化为 `{"error": "<message>", "code": "<category>"}` 给前端。
///
/// Business Logic: Tauri invoke 的 Result Err 分支会把 E 序列化后传给前端 reject。
/// R12 M2 起 IPC 同时输出稳定 category code，供前端分类；仍禁止泄漏 request_id/retryable/details。
/// HTTP `IntoResponse` 与 P2P 信封契约独立，不在此扩大。
impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("error", 2)?;
        // Display 实现已由 thiserror 提供，返回友好的中文消息
        s.serialize_field("error", &self.to_string())?;
        s.serialize_field("code", self.ipc_category_code())?;
        s.end()
    }
}

/// 让 AppError 可作为 axum handler 的返回错误类型（HTTP 500 + `{"error": "..."}`）。
///
/// Business Logic: axum 的 `Result<Json<T>, E>` 要求 E: IntoResponse。sync/transfer 等 P2P
///     handler 复用 AppError，错误响应需与 Python handler 的 `{"error": str(e)}` + 500 一致，
///     以便对端/前端错误处理逻辑通用。
impl axum::response::IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        tracing::error!("HTTP handler 返回错误: {self}");
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": self.to_string() })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个回归测试）:
    ///     R12 M2 要求 Tauri IPC 错误同时携带稳定 category `code`，供前端分类；
    ///     但仍禁止泄漏 request_id/retryable/details（那些属于 P2P 信封）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对每个 variant 序列化，断言 JSON 恰为 `error`+`code` 两字段，
    ///     code 为 validation/not_found/conflict/unavailable/timeout/internal 之一，
    ///     且不含 request_id/retryable/details。
    #[test]
    fn app_error_ipc_serialization_includes_stable_category_code() {
        fn assert_ipc(app: AppError, expected_message: &str, expected_code: &str) {
            let json = serde_json::to_value(&app).expect("AppError 应可序列化");
            assert_eq!(
                json,
                serde_json::json!({ "error": expected_message, "code": expected_code }),
                "IPC 序列化必须输出 {{error, code}} 且 code 为稳定 category token"
            );
            // 显式断言没有泄漏 P2P 信封字段，防止未来误改。
            let obj = json.as_object().expect("应为对象");
            assert_eq!(obj.len(), 2, "IPC 错误对象应只有 error 与 code 两个字段");
            assert!(obj.get("request_id").is_none());
            assert!(obj.get("retryable").is_none());
            assert!(obj.get("details").is_none());
            assert_eq!(app.ipc_category_code(), expected_code);
        }

        assert_ipc(
            AppError::not_found("Prompt 不存在"),
            "Prompt 不存在",
            "not_found",
        );
        assert_ipc(AppError::generic("boom"), "boom", "internal");
        assert_ipc(AppError::validation("参数非法"), "参数非法", "validation");
        assert_ipc(AppError::conflict("状态冲突"), "状态冲突", "conflict");
        assert_ipc(AppError::unavailable("暂不可用"), "暂不可用", "unavailable");
        assert_ipc(AppError::timeout("超时"), "超时", "timeout");
        // 带前缀的既有 variant 也应输出 internal code。
        assert_ipc(
            AppError::Io(std::io::Error::other("disk")),
            "IO 错误: disk",
            "internal",
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     classify() 是 HTTP 边界状态码映射的稳定入口，必须保证每个 variant 映射到约定分类，
    ///     否则 P2P 错误信封会返回错误的状态码。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 Validation/Conflict/Unavailable/Timeout/NotFound 显式 variant 命中对应分类，
    ///     Db/Json/Io/Bad/Tauri 兜底为 Internal。
    #[test]
    fn classify_maps_variants_to_stable_categories() {
        assert_eq!(
            AppError::validation("x").classify(),
            AppErrorCategory::Validation
        );
        assert_eq!(
            AppError::not_found("x").classify(),
            AppErrorCategory::NotFound
        );
        assert_eq!(
            AppError::conflict("x").classify(),
            AppErrorCategory::Conflict
        );
        assert_eq!(
            AppError::unavailable("x").classify(),
            AppErrorCategory::Unavailable
        );
        assert_eq!(AppError::timeout("x").classify(), AppErrorCategory::Timeout);
        assert_eq!(
            AppError::generic("x").classify(),
            AppErrorCategory::Internal
        );
        assert_eq!(
            AppError::Io(std::io::Error::other("x")).classify(),
            AppErrorCategory::Internal
        );
        assert_eq!(
            AppError::Db(sqlx::Error::Configuration("cfg".into())).classify(),
            AppErrorCategory::Internal
        );
    }
}
