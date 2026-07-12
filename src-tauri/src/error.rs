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
    /// Business Logic: 新增 variant 仅为 HTTP/P2P 边界提供稳定的 400 分类，
    ///     不改变既有 IPC 序列化（Serialize impl 仍统一输出 `{"error": "..."}`）。
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
    pub fn classify(&self) -> AppErrorCategory {
        match self {
            AppError::NotFound(_) => AppErrorCategory::NotFound,
            AppError::Validation(_) => AppErrorCategory::Validation,
            AppError::Conflict(_) => AppErrorCategory::Conflict,
            AppError::Unavailable(_) => AppErrorCategory::Unavailable,
            AppError::Timeout(_) => AppErrorCategory::Timeout,
            // Db/Json/Io/Tauri/Bad 均视为内部错误（500）：
            // 这些 variant 既有调用点大多包裹 IO/进程失败，不应误升 4xx。
            AppError::Db(_)
            | AppError::Json(_)
            | AppError::Io(_)
            | AppError::Bad(_)
            | AppError::Tauri(_) => AppErrorCategory::Internal,
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个回归测试）:
    ///     Task 5 引入了新的 AppError variant（Validation/Conflict/Unavailable/Timeout），
    ///     必须保证 Tauri IPC 序列化仍是老形态 `{"error": "<message>"}`，不能因新增 variant
    ///     漏出 code/request_id 等字段，否则前端老逻辑会解析失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对每个 variant 序列化，断言 JSON 恰为单字段 `error`，且不含 code/request_id/retryable/details。
    #[test]
    fn app_error_ipc_serialization_remains_legacy_form() {
        fn assert_legacy(app: AppError, expected_message: &str) {
            let json = serde_json::to_value(&app).expect("AppError 应可序列化");
            assert_eq!(
                json,
                serde_json::json!({ "error": expected_message }),
                "IPC 序列化必须保持 {{error}} 老形态"
            );
            // 显式断言没有泄漏新字段，防止未来误改。
            let obj = json.as_object().expect("应为对象");
            assert_eq!(obj.len(), 1, "IPC 错误对象应只有 error 一个字段");
            assert!(obj.get("code").is_none());
            assert!(obj.get("request_id").is_none());
            assert!(obj.get("retryable").is_none());
            assert!(obj.get("details").is_none());
        }

        assert_legacy(AppError::not_found("Prompt 不存在"), "Prompt 不存在");
        assert_legacy(AppError::generic("boom"), "boom");
        assert_legacy(AppError::validation("参数非法"), "参数非法");
        assert_legacy(AppError::conflict("状态冲突"), "状态冲突");
        assert_legacy(AppError::unavailable("暂不可用"), "暂不可用");
        assert_legacy(AppError::timeout("超时"), "超时");
        // 带前缀的既有 variant 也应保持老形态。
        assert_legacy(
            AppError::Io(std::io::Error::other("disk")),
            "IO 错误: disk",
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
        assert_eq!(
            AppError::timeout("x").classify(),
            AppErrorCategory::Timeout
        );
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

/// 让 AppError 可序列化为 `{"error": "<message>"}` 给前端。
///
/// Business Logic: Tauri invoke 的 Result Err 分支会把 E 序列化后传给前端 reject，
/// 前端期望 error 字段为字符串消息，与 Python HTTP 500 的 `{"error": str(e)}` 一致。
impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("error", 1)?;
        // Display 实现已由 thiserror 提供，返回友好的中文消息
        s.serialize_field("error", &self.to_string())?;
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
