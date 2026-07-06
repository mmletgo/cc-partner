//! net/http_server.rs — axum HTTP server（供对端调用）
//!
//! Business Logic（为什么需要这个模块）:
//!     每个 cc-partner 实例既是客户端也是服务端，需监听 HTTP 端口接收对端的
//!     同步/传输/健康检查请求。对照 Python `network/server.py`（aiohttp 实现）。
//!     同时为手机浏览器提供 `/mobile` 静态 SPA 入口，便于局域网移动端访问工作台。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_http_server`：构造 axum Router（with_state(AppState)，挂载全部 /api 路由），
//!       优先绑定固定 HTTP 端口，冲突时向上递增，取 local_addr 实际端口回填
//!       AppState.actual_http_port（AtomicU16），tokio::spawn(axum::serve)。
//!     - `/mobile` fallback：通过 Tauri asset resolver 读取 frontendDist 嵌入资源，并精确服务 `/assets/*` 构建资源。
//!     - body limit 覆盖文件传输 chunk 和 Workbench 远端文本保存。

use crate::net::routes::{
    cc_history, claude_code_assets, claude_md_sync, health, mobile, orchestrator, scratchpad_sync,
    ssh_target_sync, sync, transfer, workbench,
};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{header, HeaderValue, Request, Response, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use std::convert::Infallible;
use std::io::{Error as IoError, ErrorKind};
use std::net::SocketAddr;
use std::path::{Component, Path};
use std::sync::atomic::Ordering;
use tower::service_fn;

/// axum body 大小上限（字节）。32MB，容纳 M5 chunk（960KB）+ Workbench 远端文本保存（5MB 高转义 JSON）+ 开销。
const BODY_LIMIT_BYTES: usize = 32 * 1024 * 1024;
/// HTTP server 默认首选端口。配置端口为 0 或非法值时使用该端口，冲突则自动向上递增。
const DEFAULT_HTTP_PORT: u16 = 62116;

/// 判断请求路径是否属于移动端静态入口命名空间。
///
/// Business Logic（为什么需要这个函数）:
///     手机端入口只应接管 `/mobile` shell 与 Vite 生成的 `/assets/*` 资源；P2P API、桌面 Workbench 等其它路径必须维持原路由语义。
///
/// Code Logic（这个函数做什么）:
///     对 path 做精确前缀判断：`/mobile`、`/mobile/...` 和 `/assets/...` 返回 true，其它路径返回 false。
fn is_mobile_spa_path(path: &str) -> bool {
    path == "/mobile" || path.starts_with("/mobile/") || path.starts_with("/assets/")
}

/// axum fallback service：按 `/mobile` SPA 规则返回 Tauri 静态资源或 404。
///
/// Business Logic（为什么需要这个函数）:
///     桌面端生成的局域网手机访问 URL 指向 `/mobile`，手机浏览器刷新任意 SPA 子路由时需要回退到
///     `mobile.html`，但未知非移动端路径不能被错误接管。
///
/// Code Logic（这个函数做什么）:
///     非移动端静态路径直接 404；`/mobile`/`/mobile/` 返回 shell；`/assets/*` 只按 exact asset key 读取，
///     资源缺失时直接 404；`/mobile/<rest>` 保留 SPA 子路由回退 shell，shell 缺失时返回纯文本 404。
async fn serve_mobile_spa(
    state: AppState,
    req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
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
        if let Some(response) = mobile_asset_response(&state, "mobile.html").await {
            return Ok(response);
        }
    } else if path.starts_with("/assets/") {
        if let Some(response) = mobile_asset_response(&state, path).await {
            return Ok(response);
        }

        let mut response = Response::new(Body::from("Asset not found"));
        *response.status_mut() = StatusCode::NOT_FOUND;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        return Ok(response);
    } else if let Some(asset_path) = path.strip_prefix("/mobile/") {
        if let Some(response) = mobile_asset_response(&state, asset_path).await {
            return Ok(response);
        }

        if asset_path.starts_with("assets/") {
            let mut response = Response::new(Body::from("Asset not found"));
            *response.status_mut() = StatusCode::NOT_FOUND;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            return Ok(response);
        }

        if let Some(response) = mobile_asset_response(&state, "mobile.html").await {
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

/// 将 `/mobile` 请求路径规范化为 Tauri frontendDist asset key。
///
/// Business Logic（为什么需要这个函数）:
///     `/mobile` 是局域网 HTTP shell 前缀，Vite 多入口产物会引用 frontendDist 根下的 `/assets/*`；
///     后端必须把这两种 HTTP 路径规范化为真实 Tauri asset key。
///
/// Code Logic（这个函数做什么）:
///     接收完整 `/mobile/...`、`/assets/...` 或已剥离的相对路径；用 `Component` 拒绝空路径、绝对路径、
///     父目录和其它非普通路径组件；返回用 `/` 拼接的 asset key。
fn mobile_asset_key(path: &str) -> Option<String> {
    let logical_path = match path {
        "/mobile" | "/mobile/" => "mobile.html".to_string(),
        _ if path.starts_with("/mobile/") => path
            .strip_prefix("/mobile/")
            .unwrap_or_default()
            .to_string(),
        _ if path.starts_with("/assets/") => {
            let asset_path = path.strip_prefix("/assets/").unwrap_or_default();
            if asset_path.is_empty() {
                return None;
            }
            format!("assets/{asset_path}")
        }
        _ if path.starts_with('/') => return None,
        _ => path.to_string(),
    };
    let requested_path = Path::new(&logical_path);

    if logical_path.is_empty() || requested_path.is_absolute() {
        return None;
    }

    let mut safe_segments = Vec::new();
    for component in requested_path.components() {
        match component {
            Component::Normal(segment) => {
                let segment = segment.to_str()?;
                if segment.is_empty() || segment.contains('\\') {
                    return None;
                }
                safe_segments.push(segment);
            }
            _ => return None,
        }
    }

    if safe_segments.is_empty() {
        return None;
    }

    Some(safe_segments.join("/"))
}

/// 读取移动端 SPA 的 Tauri asset 并转换为 HTTP 响应。
///
/// Business Logic（为什么需要这个函数）:
///     手机端页面由前端构建写入 Tauri `frontendDist`，生产打包后源码相对目录不存在，必须从 Tauri
///     asset resolver 读取嵌入资源；开发/测试环境可在 resolver 缺失时读 dist 兜底。
///
/// Code Logic（这个函数做什么）:
///     将请求路径归一化为 asset key；优先调用精确 Tauri asset 查询；命中时用 asset bytes/mime/CSP 生成响应；
///     未命中时再尝试 dev/test filesystem fallback。
async fn mobile_asset_response(state: &AppState, path: &str) -> Option<Response<Body>> {
    let asset_key = mobile_asset_key(path)?;

    if let Some(asset) = mobile_tauri_asset(state, &asset_key) {
        let content_type = mobile_content_type_header(asset.mime_type(), &asset_key);
        let csp_header = asset
            .csp_header()
            .and_then(|value| HeaderValue::from_str(value).ok());
        let mut response = Response::new(Body::from(asset.bytes));
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
        if let Some(csp_header) = csp_header {
            response
                .headers_mut()
                .insert(header::CONTENT_SECURITY_POLICY, csp_header);
        }
        return Some(response);
    }

    mobile_dev_dist_asset_response(&asset_key).await
}

/// 从 Tauri asset resolver 精确读取移动端资源。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri asset resolver 在生产态找不到某个 key 时会自动回退到 `index.html`；`/mobile` 必须由本模块
///     自己回退到 `mobile.html`，否则手机端深层路径可能误拿桌面 shell。
///
/// Code Logic（这个函数做什么）:
///     若当前包内有嵌入资源，则先遍历 resolver asset key，只有 exact match 才调用 `get`；若没有嵌入资源
///     （典型 devUrl 开发态），直接调用 `get` 让 Tauri 按 frontendDist 目录读取。
fn mobile_tauri_asset(state: &AppState, asset_key: &str) -> Option<tauri::Asset> {
    let resolver = state.app_handle.asset_resolver();
    let mut has_embedded_assets = false;
    let mut has_exact_asset = false;

    for (key, _) in resolver.iter() {
        has_embedded_assets = true;
        if key.as_ref().trim_start_matches('/') == asset_key {
            has_exact_asset = true;
            break;
        }
    }

    if has_embedded_assets && !has_exact_asset {
        return None;
    }

    resolver.get(asset_key.to_string())
}

/// 从源码树 dist 目录读取移动端资源作为开发/测试兜底。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri dev 模式通常由 asset resolver 回退读取 `frontendDist`，但部分单测或本地开发环境可能没有完整
///     Tauri webview 上下文；保留文件系统读取只作为额外兜底，不能作为生产路径。
///
/// Code Logic（这个函数做什么）:
///     使用已规范化的 asset key 拼出 `../web/dist` 内路径；读取成功则按扩展名 fallback MIME 返回响应。
async fn mobile_dev_dist_asset_response(asset_key: &str) -> Option<Response<Body>> {
    let dist_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../web/dist")
        .join(asset_key);
    let bytes = tokio::fs::read(dist_path).await.ok()?;

    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        mobile_content_type_header("", asset_key),
    );
    Some(response)
}

/// 生成移动端响应的 content-type header。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri asset resolver 能提供构建期 MIME；若 resolver 未提供或提供非法值，仍要保证浏览器按静态资源类型加载。
///
/// Code Logic（这个函数做什么）:
///     优先把非空 asset MIME 转为 HeaderValue；为空或转换失败时，按 asset key 扩展名使用本地 MIME 映射。
fn mobile_content_type_header(asset_mime: &str, asset_key: &str) -> HeaderValue {
    let asset_mime = asset_mime.trim();
    if !asset_mime.is_empty() {
        if let Ok(value) = HeaderValue::from_str(asset_mime) {
            return value;
        }
    }

    HeaderValue::from_static(mobile_content_type(asset_key))
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
/// Business Logic: 应用启动时调用，尽量绑定稳定端口，便于移动端 Workbench 链接可收藏和复用；
///     若端口已被占用则自动向上递增，返回端口供 mDNS 注册使用（mDNS 宣告的端口必须是 axum 实际监听端口）。
///
/// Code Logic:
///     1. 构造 Router：全部 /api 路由 → 对应 handler，最后挂 `/mobile` SPA fallback，
///        with_state(AppState)，套 DefaultBodyLimit 限制请求体大小。
///     2. 从配置读取首选端口；配置为 0/非法值时用 DEFAULT_HTTP_PORT；端口占用时递增重试。
///     3. local_addr().port() 取实际端口，回填 AppState.actual_http_port。
///     4. tokio::spawn(axum::serve(listener, app)) 在后台运行（不阻塞 setup）。
pub async fn start_http_server(state: AppState) -> Result<u16, std::io::Error> {
    let fallback_state = state.clone();
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
            "/api/workbench/projects/list",
            get(workbench::list_projects),
        )
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
            "/api/workbench/sessions/replay",
            post(workbench::replay_workbench_session),
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
            "/api/workbench/sessions/switch-pane",
            post(workbench::switch_workbench_pane),
        )
        .route(
            "/api/workbench/sessions/zoom-pane",
            post(workbench::zoom_workbench_pane),
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
        // Mobile Workbench 本机入口：手机 → 本机，可继续由本机代理到远端设备 remote shortcut
        .route(
            "/api/mobile/workbench/projects/list",
            get(workbench::mobile_list_projects),
        )
        .route(
            "/api/mobile/workbench/projects/open",
            post(workbench::mobile_open_project),
        )
        .route(
            "/api/mobile/workbench/worktrees/list",
            post(workbench::mobile_list_worktrees),
        )
        .route(
            "/api/mobile/workbench/worktrees/create",
            post(workbench::mobile_create_worktree),
        )
        .route(
            "/api/mobile/workbench/worktrees/commit",
            post(workbench::mobile_commit_worktree),
        )
        .route(
            "/api/mobile/workbench/worktrees/push",
            post(workbench::mobile_push_worktree),
        )
        .route(
            "/api/mobile/workbench/worktrees/merge",
            post(workbench::mobile_merge_worktree),
        )
        .route(
            "/api/mobile/workbench/worktrees/remove",
            post(workbench::mobile_remove_worktree),
        )
        .route(
            "/api/mobile/workbench/git/commits",
            post(workbench::mobile_list_git_commits),
        )
        .route(
            "/api/mobile/workbench/files/list-dir",
            post(workbench::mobile_list_workbench_dir),
        )
        .route(
            "/api/mobile/workbench/files/info",
            post(workbench::mobile_workbench_path_info),
        )
        .route(
            "/api/mobile/workbench/files/open",
            post(workbench::mobile_open_workbench_file),
        )
        .route(
            "/api/mobile/workbench/files/save-text",
            post(workbench::mobile_save_workbench_text_file),
        )
        .route(
            "/api/mobile/workbench/sessions/list",
            post(workbench::mobile_list_workbench_sessions),
        )
        .route(
            "/api/mobile/workbench/sessions/create",
            post(workbench::mobile_create_workbench_session),
        )
        .route(
            "/api/mobile/workbench/sessions/replay",
            post(workbench::mobile_replay_workbench_session),
        )
        .route(
            "/api/mobile/workbench/sessions/write",
            post(workbench::mobile_write_workbench_session_input),
        )
        .route(
            "/api/mobile/workbench/sessions/resize",
            post(workbench::mobile_resize_workbench_session),
        )
        .route(
            "/api/mobile/workbench/sessions/focus",
            post(workbench::mobile_focus_workbench_session),
        )
        .route(
            "/api/mobile/workbench/sessions/focused",
            post(workbench::mobile_focused_workbench_session),
        )
        .route(
            "/api/mobile/workbench/sessions/split-pane",
            post(workbench::mobile_split_workbench_pane),
        )
        .route(
            "/api/mobile/workbench/sessions/switch-pane",
            post(workbench::mobile_switch_workbench_pane),
        )
        .route(
            "/api/mobile/workbench/sessions/zoom-pane",
            post(workbench::mobile_zoom_workbench_pane),
        )
        .route(
            "/api/mobile/workbench/sessions/close-pane",
            post(workbench::mobile_close_workbench_pane),
        )
        .route(
            "/api/mobile/workbench/sessions/close",
            post(workbench::mobile_close_workbench_session),
        )
        .route(
            "/api/mobile/workbench/prompt-optimizer/stream-to-session",
            post(workbench::mobile_stream_prompt_optimizer_to_session),
        )
        // Orchestrator 远端协议：remote shortcut 操作转发到项目所在设备的本机任务队列
        .route(
            "/api/orchestrator/tasks/create",
            post(orchestrator::create_task),
        )
        .route(
            "/api/orchestrator/tasks/complete-prompt",
            post(orchestrator::complete_task_prompt),
        )
        .route(
            "/api/orchestrator/tasks/list",
            post(orchestrator::list_tasks),
        )
        .route(
            "/api/orchestrator/task-views/list",
            post(orchestrator::list_task_views),
        )
        .route(
            "/api/orchestrator/task-views/create",
            post(orchestrator::create_task_view),
        )
        .route(
            "/api/orchestrator/tasks/evidence",
            post(orchestrator::get_evidence),
        )
        .route(
            "/api/orchestrator/tasks/queue",
            post(orchestrator::queue_task),
        )
        .route(
            "/api/orchestrator/tasks/retry",
            post(orchestrator::retry_task),
        )
        .route(
            "/api/orchestrator/tasks/abort",
            post(orchestrator::abort_task),
        )
        .route("/api/orchestrator/config", get(orchestrator::get_config))
        // 移动端 SPA fallback：只服务 /mobile 命名空间；其它未知路径保持 404。
        .fallback_service(service_fn(move |req| {
            serve_mobile_spa(fallback_state.clone(), req)
        }))
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
        .with_state(state.clone());

    let preferred_port = {
        let config = state.config.read().expect("config 读锁中毒");
        preferred_http_port_from_config(config.http_port)
    };
    let listener = bind_preferred_http_listener(preferred_port).await?;

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

/// 解析配置中的 HTTP 首选端口。
///
/// Business Logic（为什么需要这个函数）:
///     旧配置里 http_port 可能是 0，表示历史动态端口；现在移动端链接要求尽量稳定，因此 0/非法值需要
///     统一回落到固定默认端口。
///
/// Code Logic（这个函数做什么）:
///     接收配置中的 i64 端口值；合法的 1..=65535 直接转 u16，否则返回 DEFAULT_HTTP_PORT。
fn preferred_http_port_from_config(config_port: i64) -> u16 {
    if (1..=u16::MAX as i64).contains(&config_port) {
        config_port as u16
    } else {
        DEFAULT_HTTP_PORT
    }
}

/// 绑定首选 HTTP 端口，冲突时自动向上递增。
///
/// Business Logic（为什么需要这个函数）:
///     移动端 Workbench URL 需要尽量固定；当同机已有进程占用首选端口时，用户仍应能启动应用并获得相邻端口。
///
/// Code Logic（这个函数做什么）:
///     从 preferred_port 开始依次尝试绑定 0.0.0.0:<port>；AddrInUse 时继续下一个端口，
///     其它 IO 错误直接返回；全部端口耗尽时返回最后一次 AddrInUse 错误。
async fn bind_preferred_http_listener(
    preferred_port: u16,
) -> Result<tokio::net::TcpListener, std::io::Error> {
    let start_port = if preferred_port == 0 {
        DEFAULT_HTTP_PORT
    } else {
        preferred_port
    };
    let mut last_addr_in_use: Option<IoError> = None;

    for port in start_port..=u16::MAX {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                if port != start_port {
                    tracing::warn!("HTTP 首选端口 {start_port} 被占用，改用 {port}");
                }
                return Ok(listener);
            }
            Err(error) if error.kind() == ErrorKind::AddrInUse => {
                last_addr_in_use = Some(error);
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_addr_in_use
        .unwrap_or_else(|| IoError::new(ErrorKind::AddrInUse, "没有可用的 HTTP 监听端口")))
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
        assert!(is_mobile_spa_path("/assets/mobile.js"));
        assert!(is_mobile_spa_path("/assets/index.css"));

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
    ///     移动端 Workbench 访问链接应尽量稳定，用户收藏或记忆固定端口后不应每次启动随机变化；
    ///     若首选端口被占用，应用应自动尝试下一个端口而不是启动失败或退回随机端口。
    ///
    /// Code Logic（这个测试做什么）:
    ///     先用标准库 listener 占用一个本机端口，再调用 HTTP listener 绑定 helper，断言最终端口不是
    ///     被占端口，且按递增策略落在更高端口上。
    #[tokio::test]
    async fn preferred_http_listener_increments_when_port_is_busy() {
        let occupied = std::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], 0)))
            .expect("测试应能占用一个临时端口");
        let occupied_port = occupied
            .local_addr()
            .expect("测试 listener 应能读取端口")
            .port();
        if occupied_port == u16::MAX {
            drop(occupied);
            return;
        }

        let listener = bind_preferred_http_listener(occupied_port).await.unwrap();
        let actual_port = listener.local_addr().unwrap().port();

        assert_ne!(actual_port, occupied_port);
        assert!(actual_port > occupied_port);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `/mobile` HTTP 路径只是局域网访问前缀，Tauri 打包资源内实际 key 不包含 `/mobile` 前缀。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言移动端入口映射到 `mobile.html`，静态资源映射到前端 dist 相对路径，危险路径返回 None。
    #[test]
    fn mobile_asset_key_normalizes_http_path_without_mobile_prefix() {
        assert_eq!(mobile_asset_key("/mobile").as_deref(), Some("mobile.html"));
        assert_eq!(mobile_asset_key("/mobile/").as_deref(), Some("mobile.html"));
        assert_eq!(
            mobile_asset_key("/mobile/assets/index.js").as_deref(),
            Some("assets/index.js")
        );
        assert_eq!(
            mobile_asset_key("/assets/mobile.js").as_deref(),
            Some("assets/mobile.js")
        );
        assert_eq!(
            mobile_asset_key("/assets/index.css").as_deref(),
            Some("assets/index.css")
        );
        assert_eq!(
            mobile_asset_key("assets/index.css").as_deref(),
            Some("assets/index.css")
        );

        assert_eq!(mobile_asset_key("/"), None);
        assert_eq!(mobile_asset_key("/mobile/../mobile.html"), None);
        assert_eq!(mobile_asset_key("/mobile/assets/../../secret.js"), None);
        assert_eq!(mobile_asset_key("/etc/passwd"), None);
        assert_eq!(mobile_asset_key(""), None);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `/mobile` 静态服务暴露在局域网，必须拒绝目录穿越和绝对路径，避免读取构建目录外文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接调用路径规范化 helper，断言 `..`、绝对路径和嵌套回退穿越都不会产生 asset key。
    #[test]
    fn mobile_asset_key_rejects_unsafe_paths() {
        assert!(mobile_asset_key("../mobile.html").is_none());
        assert!(mobile_asset_key("/etc/passwd").is_none());
        assert!(mobile_asset_key("assets/../../secret.js").is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     axum fallback 只能服务 `/mobile` SPA；桌面路由或未知路径仍应保持不匹配，不应被移动端入口吞掉。
    ///
    /// Code Logic（这个测试做什么）:
    ///     通过 path matching helper 断言非 `/mobile` 路径不进入移动端 SPA 处理。
    #[test]
    fn mobile_spa_fallback_keeps_unknown_paths_not_found() {
        assert!(!is_mobile_spa_path("/workbench"));
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
