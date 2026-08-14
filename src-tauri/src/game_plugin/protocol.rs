//! `gameplugin://` 协议：只读当前游戏目录内的静态文件。
//!
//! Business Logic（为什么需要这个模块）:
//!     iframe 需要加载游戏 HTML/CSS/JS/图片，但不能让插件读出自己的文件夹。
//!
//! Code Logic（这个模块做什么）:
//!     解析 `/<id>/<rel>`，用 resolve_game_asset 读字节并带 MIME。

use crate::state::AppState;
use std::path::PathBuf;
use tauri::http::{header, HeaderValue, Response, StatusCode, Uri};
use tauri::{AppHandle, Manager};

/// 根据扩展名猜 MIME。
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &raw[i + 1..i + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| raw.to_string())
}

fn mime_for(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => "application/octet-stream",
    }
}

fn not_found() -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Vec::new())
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// 处理 `gameplugin://localhost/<id>/<rel>`。
pub fn respond(app: &AppHandle, uri: &Uri) -> Response<Vec<u8>> {
    let Some(state) = app.try_state::<AppState>() else {
        return not_found();
    };
    let root = PathBuf::from(state.config.read().unwrap().game_plugin_dir.clone());
    let path = uri.path().trim_start_matches('/');
    let mut parts = path.splitn(2, '/');
    let Some(id) = parts.next().filter(|s| !s.is_empty()) else {
        return not_found();
    };
    let rel = parts.next().unwrap_or("index.html");
    let id = percent_decode(id);
    let rel = percent_decode(rel);
    let Ok(file) = super::resolve_game_asset(&root, &id, &rel) else {
        return not_found();
    };
    let Ok(bytes) = std::fs::read(&file) else {
        return not_found();
    };
    let mime = mime_for(&file);
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_str(mime)
                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
        )
        .body(bytes)
        .unwrap_or_else(|_| not_found())
}
