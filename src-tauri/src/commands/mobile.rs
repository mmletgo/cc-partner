//! commands/mobile.rs — 移动端访问入口 IPC 命令
//!
//! Business Logic:
//!     桌面端 Settings/Workbench 需要展示手机可访问的局域网 `/mobile` 链接和二维码；
//!     Tauri 桌面前端不能依赖同源 HTTP `/api`，必须通过 invoke 获取同一份 access-info。
//!
//! Code Logic:
//!     读取 AppState 中的配置与实际 HTTP 端口，复用 mobile 模块的 URL 过滤/格式化逻辑返回 camelCase DTO。

use crate::mobile::{build_mobile_access_info, MobileAccessInfoDto};
use crate::net::discovery::local_lan_ip;
use crate::state::AppState;
use std::sync::atomic::Ordering;
use tauri::State;

/// 获取移动端局域网访问入口信息。
///
/// Business Logic（为什么需要这个函数）:
///     桌面端访问卡片需要在不走 HTTP `/api` 的情况下获取可复制链接和二维码数据。
///
/// Code Logic（这个函数做什么）:
///     从共享状态复制配置与实际监听端口，探测局域网候选 IP，并调用 build_mobile_access_info 返回 DTO。
#[tauri::command]
pub fn get_mobile_access_info(state: State<'_, AppState>) -> MobileAccessInfoDto {
    let config = state.config.read().expect("config 读锁中毒").clone();
    let port = state.actual_http_port.load(Ordering::SeqCst);
    let candidate_ips = local_lan_ip()
        .map(|ip| vec![ip.to_string()])
        .unwrap_or_default();

    build_mobile_access_info(&config, port, candidate_ips)
}
