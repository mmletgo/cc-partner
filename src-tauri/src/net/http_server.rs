//! net/http_server.rs — axum HTTP server（供对端调用）
//!
//! Business Logic（为什么需要这个模块）:
//!     每个 cc-partner 实例既是客户端也是服务端，需监听 HTTP 端口接收对端的
//!     同步/传输/健康检查请求。对照 Python `network/server.py`（aiohttp 实现）。
//!     同时为手机浏览器提供 `/mobile` 静态 SPA 入口，便于局域网移动端访问工作台。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_http_server`：构造 axum Router（with_state(AppState)，挂载全部 /api 路由），
//!       TcpListener::bind(("0.0.0.0", 0)) 绑定动态端口，取 local_addr 实际端口回填
//!       AppState.actual_http_port（AtomicU16），tokio::spawn(axum::serve)。
//!     - `/mobile` fallback：从 web/dist 读取 mobile.html 与静态资源，非 /mobile 未知路径仍返回 404。
//!     - body limit 覆盖文件传输 chunk 和 Workbench 远端文本保存。

use crate::net::routes::{
    cc_history, claude_code_assets, claude_md_sync, health, mobile, scratchpad_sync,
    ssh_target_sync, sync, transfer, workbench,
};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{header, HeaderValue, Request, Response, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use tower::service_fn;

/// axum body 大小上限（字节）。32MB，容纳 M5 chunk（960KB）+ Workbench 远端文本保存（5MB 高转义 JSON）+ 开销。
const BODY_LIMIT_BYTES: usize = 32 * 1024 * 1024;

/// 判断请求路径是否属于移动端 SPA 命名空间。
///
/// Business Logic（为什么需要这个函数）:
///     手机端入口只应接管 `/mobile` 与其子路径；P2P API、桌面 Workbench 等其它路径必须维持原路由语义。
///
/// Code Logic（这个函数做什么）:
///     对 path 做精确前缀判断：`/mobile` 和 `/mobile/...` 返回 true，其它路径返回 false。
fn is_mobile_spa_path(path: &str) -> bool {
    path == "/mobile" || path.starts_with("/mobile/")
}

/// axum fallback service：按 `/mobile` SPA 规则返回静态资源或 404。
///
/// Business Logic（为什么需要这个函数）:
///     桌面端生成的局域网手机访问 URL 指向 `/mobile`，手机浏览器刷新任意 SPA 子路由时需要回退到
///     `mobile.html`，但未知非移动端路径不能被错误接管。
///
/// Code Logic（这个函数做什么）:
///     非 `/mobile` 路径直接 404；`/mobile`/`/mobile/` 返回 shell；`/mobile/<rest>` 优先读取静态资源，
///     资源缺失时回退 shell；shell 缺失时返回纯文本 404。
async fn serve_mobile_spa(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    let path = req.uri().path();

    if !is_mobile_spa_path(path) {
        let mut response = Response::new(Body::from("Not Found"));
        *response.status_mut() = StatusCode::NOT_FOUND;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        return Ok(response);
    }

    if path == "/mobile" || path == "/mobile/" {
        if let Some(response) = mobile_asset_response("mobile.html").await {
            return Ok(response);
        }
    } else if let Some(asset_path) = path.strip_prefix("/mobile/") {
        if let Some(response) = mobile_asset_response(asset_path).await {
            return Ok(response);
        }

        if let Some(response) = mobile_asset_response("mobile.html").await {
            return Ok(response);
        }
    }

    let mut response = Response::new(Body::from("mobile.html not found"));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(response)
}

/// 读取移动端 SPA 的构建产物并转换为 HTTP 响应。
///
/// Business Logic（为什么需要这个函数）:
///     手机端页面由后续前端构建生成到 `web/dist/mobile.html` 与 `web/dist/assets/*`，后端需要直接服务这些文件。
///     路径来自局域网 HTTP 请求，必须限制在 dist 目录内。
///
/// Code Logic（这个函数做什么）:
///     将 `/mobile/...` 或已剥离的相对路径归一化为 dist 内相对路径；用 `Component` 拒绝绝对路径、父目录
///     和其它非普通路径组件；读取文件成功则带 content-type 返回，失败返回 None。
async fn mobile_asset_response(path: &str) -> Option<Response<Body>> {
    let logical_path = match path {
        "/mobile" | "/mobile/" => "mobile.html",
        _ => path.strip_prefix("/mobile/").unwrap_or(path),
    };
    let requested_path = Path::new(logical_path);

    if logical_path.is_empty() || requested_path.is_absolute() {
        return None;
    }

    let mut safe_path = PathBuf::new();
    for component in requested_path.components() {
        match component {
            Component::Normal(segment) => safe_path.push(segment),
            _ => return None,
        }
    }

    if safe_path.as_os_str().is_empty() {
        return None;
    }

    let dist_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../web/dist")
        .join(&safe_path);
    let bytes = tokio::fs::read(dist_path).await.ok()?;

    let content_type = safe_path
        .to_str()
        .map(mobile_content_type)
        .unwrap_or("application/octet-stream");
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    Some(response)
}

/// 返回移动端静态资源的 content-type。
///
/// Business Logic（为什么需要这个函数）:
///     浏览器会按响应 content-type 执行 JS、应用 CSS 和渲染图片；缺少映射会导致移动端 shell 加载异常。
///
/// Code Logic（这个函数做什么）:
///     根据文件扩展名返回静态 MIME，覆盖 HTML/JS/CSS/SVG/PNG，并补充常见 JSON/WASM/ICO/map 类型。
fn mobile_content_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" | "map" => "application/json; charset=utf-8",
        "wasm" => "application/wasm",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// 启动 axum HTTP server，返回实际监听端口。
///
/// Business Logic: 应用启动时调用，绑定动态端口避免冲突；返回端口供 mDNS 注册使用
///     （mDNS 宣告的端口必须是 axum 实际监听端口，对端才能连）。
///
/// Code Logic:
///     1. 构造 Router：全部 /api 路由 → 对应 handler，最后挂 `/mobile` SPA fallback，
///        with_state(AppState)，套 DefaultBodyLimit 限制请求体大小。
///     2. TcpListener::bind(("0.0.0.0", 0)) 绑定动态端口。
///     3. local_addr().port() 取实际端口，回填 AppState.actual_http_port。
///     4. tokio::spawn(axum::serve(listener, app)) 在后台运行（不阻塞 setup）。
pub async fn start_http_server(state: AppState) -> Result<u16, std::io::Error> {
    // axum Router：with_state 注入 AppState，与 invoke 命令层共享同一份 Arc
    let app: Router = Router::new()
        .route("/api/health", get(health::health))
        // 移动端访问入口：返回手机可访问的局域网 /mobile URL（过滤 localhost/loopback）
        .route("/api/mobile/access-info", get(mobile::access_info))
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
            "/api/workbench/files/preview-sqlite",
            post(workbench::preview_workbench_sqlite),
        )
        .route(
            "/api/workbench/files/preview-html-asset",
            post(workbench::preview_workbench_html_asset),
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
        // 移动端 SPA fallback：只服务 /mobile 命名空间；其它未知路径保持 404。
        .fallback_service(service_fn(serve_mobile_spa))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::file_content::MAX_EDITABLE_TEXT_BYTES;
    use crate::workbench::remote_protocol::RemoteSaveTextReq;

    /// Business Logic（为什么需要这个测试）:
    ///     移动端 SPA 只允许接管 `/mobile` 命名空间，不能抢占 P2P API 与桌面 Workbench 路由。
    ///
    /// Code Logic（这个测试做什么）:
    ///     枚举移动端入口、移动端静态资源、API 路由和普通桌面路径，断言路径匹配 helper 只识别 `/mobile` 前缀。
    #[test]
    fn mobile_spa_path_matching_does_not_capture_api_routes() {
        assert!(is_mobile_spa_path("/mobile"));
        assert!(is_mobile_spa_path("/mobile/"));
        assert!(is_mobile_spa_path("/mobile/assets/index.js"));

        assert!(!is_mobile_spa_path("/api/workbench/events"));
        assert!(!is_mobile_spa_path("/api/mobile/access-info"));
        assert!(!is_mobile_spa_path("/"));
        assert!(!is_mobile_spa_path("/workbench"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     手机浏览器加载 `/mobile` SPA 时需要正确识别常见静态资源类型，避免脚本或样式被浏览器拒绝。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对 HTML、JS、CSS、SVG、PNG 以及未知扩展名分别断言 content-type 映射结果。
    #[test]
    fn mobile_content_type_maps_common_static_assets() {
        assert_eq!(
            mobile_content_type("mobile.html"),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            mobile_content_type("assets/index.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            mobile_content_type("assets/index.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(mobile_content_type("assets/logo.svg"), "image/svg+xml");
        assert_eq!(mobile_content_type("assets/logo.png"), "image/png");
        assert_eq!(
            mobile_content_type("assets/file.unknown"),
            "application/octet-stream"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `/mobile` 静态服务暴露在局域网，必须拒绝目录穿越和绝对路径，避免读取构建目录外文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接调用静态资源 helper，断言 `..`、绝对路径和嵌套回退穿越都不会产生响应。
    #[tokio::test]
    async fn mobile_asset_response_rejects_unsafe_paths() {
        assert!(mobile_asset_response("../mobile.html").await.is_none());
        assert!(mobile_asset_response("/etc/passwd").await.is_none());
        assert!(mobile_asset_response("assets/../../secret.js")
            .await
            .is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     axum fallback 只能服务 `/mobile` SPA；桌面路由或未知路径仍应保持 404，不应被移动端入口吞掉。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造一个非 `/mobile` 请求直接进入 fallback service，断言返回 404。
    #[tokio::test]
    async fn mobile_spa_fallback_keeps_unknown_paths_not_found() {
        let request = Request::builder()
            .uri("/workbench")
            .body(Body::empty())
            .expect("request should build");

        let response = serve_mobile_spa(request)
            .await
            .expect("mobile fallback should not fail");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 Workbench 文本保存走 P2P HTTP JSON body，服务端 body limit 必须覆盖 5MB 高转义文本。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 5MB NUL 文本让 serde_json 产生接近最坏情况的 `\u0000` 转义，断言序列化 body 仍低于 HTTP limit。
    #[test]
    fn body_limit_allows_workbench_remote_text_save_payloads() {
        let escaped_content = "\u{0000}".repeat(MAX_EDITABLE_TEXT_BYTES as usize);
        let body = serde_json::to_vec(&RemoteSaveTextReq {
            project_id: "project-1".to_string(),
            worktree_id: Some("worktree-1".to_string()),
            path: "docs/note.md".to_string(),
            content: escaped_content,
            base_hash: "old-hash".to_string(),
        })
        .expect("remote save-text request should serialize");

        assert!(body.len() < BODY_LIMIT_BYTES);
    }
}
