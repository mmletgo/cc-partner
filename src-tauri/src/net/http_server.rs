//! net/http_server.rs — axum HTTP server（供对端调用）
//!
//! Business Logic（为什么需要这个模块）:
//!     每个 cc-partner 实例既是客户端也是服务端，需监听 HTTP 端口接收对端的
//!     同步/传输/健康检查请求。对照 Python `network/server.py`（aiohttp 实现）。
//!     M3 仅注册 `/api/health`；sync/transfer 路由留待 M4/M5 追加到 Router。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_http_server`：构造 axum Router（with_state(AppState)，挂 /api/health），
//!       TcpListener::bind(("0.0.0.0", 0)) 绑定动态端口，取 local_addr 实际端口回填
//!       AppState.actual_http_port（AtomicU16），tokio::spawn(axum::serve)。
//!     - body limit 暂设 2MB（M5 chunk 会调整）。

use crate::net::routes::{
    cc_history, claude_code_assets, claude_md_sync, health, scratchpad_sync, ssh_target_sync, sync,
    transfer, workbench,
};
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
        // P2P CLAUDE.md 主动推送协议（单例 0/1 条；push 覆盖为发送方版本）
        .route(
            "/api/sync/claude_md/pull",
            post(claude_md_sync::claude_md_pull),
        )
        .route(
            "/api/sync/claude_md/push",
            post(claude_md_sync::claude_md_push),
        )
        // P2P 文件传输协议（M5）：init/chunk/status，字段 + X-Chunk-Offset header 对照 Python
        .route("/api/transfer/init", post(transfer::transfer_init))
        .route("/api/transfer/chunk/:id", post(transfer::transfer_chunk))
        .route("/api/transfer/status/:id", get(transfer::transfer_status))
        // Claude Code 历史同步协议（独立链路）：cc-history/sync/{pull,push}，snake_case 互通
        .route("/api/cc-history/sync/pull", post(cc_history::cc_sync_pull))
        .route("/api/cc-history/sync/push", post(cc_history::cc_sync_push))
        // SSH 目标同步协议（独立链路）：ssh-target/sync/{pull,push}，snake_case 互通
        .route(
            "/api/ssh-target/sync/pull",
            post(ssh_target_sync::ssh_target_sync_pull),
        )
        .route(
            "/api/ssh-target/sync/push",
            post(ssh_target_sync::ssh_target_sync_push),
        )
        // 速记本同步协议（单例文本）：scratchpad/sync/{pull,push}
        .route(
            "/api/scratchpad/sync/pull",
            post(scratchpad_sync::scratchpad_pull),
        )
        .route(
            "/api/scratchpad/sync/push",
            post(scratchpad_sync::scratchpad_push),
        )
        // Claude Code assets 选择性拉取：inventory + 按 selectors 生成 zip bundle
        .route(
            "/api/claude-code/assets/inventory",
            get(claude_code_assets::assets_inventory),
        )
        .route(
            "/api/claude-code/assets/bundle",
            post(claude_code_assets::assets_bundle),
        )
        // Workbench 远端目录选择与项目打开：远端设备执行本机 helper，调用方后续再建立 remote shortcut
        .route("/api/workbench/fs/roots", get(workbench::remote_roots))
        .route("/api/workbench/fs/list", post(workbench::remote_list_dir))
        .route("/api/workbench/fs/info", post(workbench::remote_path_info))
        .route(
            "/api/workbench/projects/open",
            post(workbench::open_remote_project),
        )
        .route(
            "/api/workbench/worktrees/list",
            post(workbench::list_worktrees),
        )
        .route(
            "/api/workbench/worktrees/create",
            post(workbench::create_worktree),
        )
        .route(
            "/api/workbench/worktrees/get",
            post(workbench::get_worktree),
        )
        .route(
            "/api/workbench/worktrees/commit",
            post(workbench::commit_worktree),
        )
        .route(
            "/api/workbench/worktrees/push",
            post(workbench::push_worktree),
        )
        .route(
            "/api/workbench/worktrees/merge",
            post(workbench::merge_worktree),
        )
        .route(
            "/api/workbench/worktrees/remove",
            post(workbench::remove_worktree),
        )
        .route(
            "/api/workbench/git/commits",
            post(workbench::list_git_commits),
        )
        .route(
            "/api/workbench/files/list-dir",
            post(workbench::list_workbench_dir),
        )
        .route(
            "/api/workbench/files/info",
            post(workbench::workbench_path_info),
        )
        .route(
            "/api/workbench/files/open",
            post(workbench::open_workbench_file),
        )
        .route(
            "/api/workbench/files/save-text",
            post(workbench::save_workbench_text_file),
        )
        .route(
            "/api/workbench/files/create-file",
            post(workbench::create_workbench_file),
        )
        .route(
            "/api/workbench/files/create-dir",
            post(workbench::create_workbench_dir),
        )
        .route(
            "/api/workbench/files/rename",
            post(workbench::rename_workbench_path),
        )
        .route(
            "/api/workbench/files/delete",
            post(workbench::delete_workbench_path),
        )
        .route("/api/workbench/events", get(workbench::workbench_events))
        .route(
            "/api/workbench/sessions/list",
            post(workbench::list_workbench_sessions),
        )
        .route(
            "/api/workbench/sessions/create",
            post(workbench::create_workbench_session),
        )
        .route(
            "/api/workbench/sessions/write",
            post(workbench::write_workbench_session_input),
        )
        .route(
            "/api/workbench/sessions/resize",
            post(workbench::resize_workbench_session),
        )
        .route(
            "/api/workbench/sessions/focus",
            post(workbench::focus_workbench_session),
        )
        .route(
            "/api/workbench/sessions/focused",
            post(workbench::focused_workbench_session),
        )
        .route(
            "/api/workbench/sessions/split-pane",
            post(workbench::split_workbench_pane),
        )
        .route(
            "/api/workbench/sessions/close-pane",
            post(workbench::close_workbench_pane),
        )
        .route(
            "/api/workbench/sessions/close",
            post(workbench::close_workbench_session),
        )
        .route(
            "/api/workbench/sessions/rename",
            post(workbench::rename_workbench_session),
        )
        .route(
            "/api/workbench/prompt-optimizer/stream-to-session",
            post(workbench::stream_prompt_optimizer_to_session),
        )
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
        .with_state(state.clone());

    // 绑定动态端口（0 = 系统分配）
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], 0))).await?;

    // 取实际监听端口并回填 AppState（供 mDNS 注册 + health handler 返回）
    let actual_port = listener.local_addr()?.port();
    state.actual_http_port.store(actual_port, Ordering::SeqCst);

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
