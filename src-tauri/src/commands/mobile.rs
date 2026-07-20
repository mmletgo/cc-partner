//! commands/mobile.rs — 移动端访问入口 IPC 命令
//!
//! Business Logic:
//!     桌面端 Settings/Workbench 需要展示手机可访问的局域网 `/mobile` 链接和二维码；
//!     Tauri 桌面前端不能依赖同源 HTTP `/api`，必须通过 invoke 获取同一份 access-info。
//!     多网段机器会返回结构化 `entries`，供前端芯片切换 URL / 复制 / 二维码。
//!
//! Code Logic:
//!     读取 AppState 中的配置与实际 HTTP 端口，委托 `mobile_access_info_from_state`
//!     完成网卡枚举、角色映射、空列表 fallback 与 DTO 组装（与 HTTP route 共用）。

use crate::mobile::{mobile_access_info_from_state, MobileAccessInfoDto};
use crate::state::AppState;
use std::sync::atomic::Ordering;
use tauri::State;

/// 获取移动端局域网访问入口信息。
///
/// Business Logic（为什么需要这个函数）:
///     桌面端访问卡片需要在不走 HTTP `/api` 的情况下获取可复制链接、二维码数据与多网段 entries。
///
/// Code Logic（这个函数做什么）:
///     从共享状态复制配置与实际监听端口，调用 `mobile_access_info_from_state` 返回多网段 DTO。
#[tauri::command]
pub fn get_mobile_access_info(state: State<'_, AppState>) -> MobileAccessInfoDto {
    let config = state.config.read().expect("config 读锁中毒").clone();
    let port = state.actual_http_port.load(Ordering::SeqCst);
    mobile_access_info_from_state(&config, port)
}
