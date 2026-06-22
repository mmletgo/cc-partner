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

/// not-found 的便捷构造，使命令体书写更自然（如 `ok_or_not_found(...)?`）。
impl AppError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    /// 通用业务错误便捷构造。
    pub fn generic(msg: impl Into<String>) -> Self {
        Self::Bad(msg.into())
    }
}
