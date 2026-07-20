//! net/routes/mobile.rs — 移动端局域网访问入口信息 API
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端需要从后端获取当前设备在局域网内的 `/mobile` 可访问地址，用于生成链接和二维码。
//!     多网段机器返回结构化 `entries`（wifi/wired 角色 + isDefault），与 invoke 路径语义一致。
//!
//! Code Logic（这个模块做什么）:
//!     GET /api/mobile/access-info 从 AppState 读取设备配置和实际 HTTP 端口，
//!     委托 `mobile_access_info_from_state` 完成枚举/fallback/组装后以 JSON 返回。

use crate::mobile::{mobile_access_info_from_state, MobileAccessInfoDto};
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use std::sync::atomic::Ordering;

/// GET /api/mobile/access-info：返回移动端局域网访问入口信息。
///
/// Business Logic（为什么需要这个函数）:
///     前端需要一个可信后端 API 获取手机可访问 URL 与多网段 entries，
///     避免前端误用 localhost 或浏览器地址栏端口。
///
/// Code Logic（这个函数做什么）:
///     从共享状态复制配置和实际监听端口，调用 `mobile_access_info_from_state`
///     统一过滤并生成 DTO 后以 JSON 返回（与 Tauri command 共用组装路径）。
pub async fn access_info(State(state): State<AppState>) -> Json<MobileAccessInfoDto> {
    let config = state.config.read().expect("config 读锁中毒").clone();
    let port = state.actual_http_port.load(Ordering::SeqCst);
    Json(mobile_access_info_from_state(&config, port))
}
