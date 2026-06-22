//! net/http_server.rs — axum HTTP server（供对端调用）
//!
//! Business Logic（为什么需要这个模块）:
//!     每个 Claude Partner 实例既是客户端也是服务端，需监听 HTTP 端口接收对端的
//!     同步/传输/健康检查请求。对照 Python `network/server.py`（aiohttp 实现）。
//!     M3 仅注册 `/api/health`；sync/transfer 路由留待 M4/M5 追加到 Router。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_http_server`：构造 axum Router（with_state(AppState)，挂 /api/health），
//!       TcpListener::bind(("0.0.0.0", 0)) 绑定动态端口，取 local_addr 实际端口回填
//!       AppState.actual_http_port（AtomicU16），tokio::spawn(axum::serve)。
//!     - body limit 暂设 2MB（M5 chunk 会调整）。

use crate::net::routes::{health, sync, transfer};
use crate::state::AppState;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;

/// axum body 大小上限（字节）。2MB，容纳 M5 chunk（960KB）+开销，
/// 与 Python `client_max_size=2MB` 一致，确保 Rust↔Python 互通。
const BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// 启动 axum HTTP server，返回实际监听端口。
///
/// Business Logic: 应用启动时调用，绑定动态端口避免冲突；返回端口供 mDNS 注册使用
///     （mDNS 宣告的端口必须是 axum 实际监听端口，对端才能连）。
///
/// Code Logic:
///     1. 构造 Router：/api/health + /api/sync/* + /api/transfer/* → 对应 handler，
///        with_state(AppState)，套 DefaultBodyLimit 限制请求体大小。
///     2. TcpListener::bind(("0.0.0.0", 0)) 绑定动态端口。
///     3. local_addr().port() 取实际端口，回填 AppState.actual_http_port。
///     4. tokio::spawn(axum::serve(listener, app)) 在后台运行（不阻塞 setup）。
pub async fn start_http_server(state: AppState) -> Result<u16, std::io::Error> {
    // axum Router：with_state 注入 AppState，与 invoke 命令层共享同一份 Arc
    let app: Router = Router::new()
        .route("/api/health", get(health::health))
        // P2P 同步协议（M4）：对端调 pull/push，字段对照 Python protocol.py
        .route("/api/sync/pull", post(sync::sync_pull))
        .route("/api/sync/push", post(sync::sync_push))
        // P2P 文件传输协议（M5）：init/chunk/status，字段 + X-Chunk-Offset header 对照 Python
        .route("/api/transfer/init", post(transfer::transfer_init))
        .route("/api/transfer/chunk/:id", post(transfer::transfer_chunk))
        .route("/api/transfer/status/:id", get(transfer::transfer_status))
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
        .with_state(state.clone());

    // 绑定动态端口（0 = 系统分配）
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], 0))).await?;

    // 取实际监听端口并回填 AppState（供 mDNS 注册 + health handler 返回）
    let actual_port = listener.local_addr()?.port();
    state
        .actual_http_port
        .store(actual_port, Ordering::SeqCst);

    // 后台运行 axum serve（serve 持有 listener 与 app 所有权，直到进程退出）
    // axum::serve 返回的 future 为 Send，可直接 spawn 到 tokio runtime。
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("axum HTTP server 异常退出: {e}");
        }
    });

    tracing::info!("axum HTTP server 已启动，监听端口: {actual_port}");
    Ok(actual_port)
}
