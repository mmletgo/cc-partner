//! commands/config.rs — 配置读写 + 版本查询命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端设置页通过 invoke 读取/修改应用配置（设备名、接收目录、快捷键），
//!     关于页通过 invoke 获取版本号。对照 Python protocol.py 的
//!     handle_get_config / handle_update_config / handle_version。
//!
//! Code Logic（这个模块做什么）:
//!     - get_config: 读 RwLock 配置，转 ConfigDto（camelCase）。
//!     - get_default_config: 返回环境感知默认偏好，供设置页恢复默认。
//!     - update_config: 应用 deviceName/receiveDir/screenshotHotkey patch 后 save() 回 config.json。
//!     - get_version: 返回 {version, buildDate}，version 取 CARGO_PKG_VERSION。

use crate::config::default_preference_values;
use crate::error::AppError;
use crate::hotkey::{register_screenshot_hotkey, screenshot_handler};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

/// 配置前端 DTO（camelCase，对照 Python _get_config 返回结构）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigDto {
    pub device_id: String,
    pub device_name: String,
    pub receive_dir: String,
    pub screenshot_hotkey: String,
    /// HTTP 端口（M1 未实际监听，暂返回配置值；M3 接入真实监听端口后更新）
    pub http_port: i64,
}

/// 读取应用配置。
///
/// Business Logic: 前端设置页初始化时展示当前配置。
#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<ConfigDto, AppError> {
    let cfg = state.config.read().unwrap();
    Ok(ConfigDto {
        device_id: cfg.device_id.clone(),
        device_name: cfg.device_name.clone(),
        receive_dir: cfg.receive_dir.clone(),
        screenshot_hotkey: cfg.screenshot_hotkey.clone(),
        http_port: cfg.http_port,
    })
}

/// 读取应用偏好的环境默认值。
///
/// Business Logic: 设置页“恢复默认”需要得到 hostname、默认接收目录和平台默认快捷键，
///     不能在前端硬编码或用空字符串代替。
/// Code Logic: 保留当前 device_id/http_port，只替换可编辑偏好字段为默认值后返回 ConfigDto。
#[tauri::command]
pub async fn get_default_config(state: State<'_, AppState>) -> Result<ConfigDto, AppError> {
    let cfg = state.config.read().unwrap();
    let (device_name, receive_dir, screenshot_hotkey) = default_preference_values();
    Ok(ConfigDto {
        device_id: cfg.device_id.clone(),
        device_name,
        receive_dir,
        screenshot_hotkey,
        http_port: cfg.http_port,
    })
}

/// 更新应用配置（仅 deviceName/receiveDir/screenshotHotkey 可写），并持久化。
///
/// Business Logic: 用户在设置页保存修改后需落盘，下次启动生效。
/// Code Logic: 取写锁应用 patch → save() → 返回最新配置 DTO。
#[tauri::command]
pub async fn update_config(
    app: AppHandle,
    state: State<'_, AppState>,
    device_name: Option<String>,
    receive_dir: Option<String>,
    screenshot_hotkey: Option<String>,
) -> Result<ConfigDto, AppError> {
    let hotkey_changed = screenshot_hotkey.is_some();
    {
        let mut cfg = state.config.write().unwrap();
        if let Some(n) = device_name {
            cfg.device_name = n;
        }
        if let Some(d) = receive_dir {
            cfg.receive_dir = d;
        }
        if let Some(h) = screenshot_hotkey {
            cfg.screenshot_hotkey = h;
        }
        cfg.save()?;
    }
    // screenshotHotkey 变更时热更新全局快捷键（unregister 旧 + register 新）
    if hotkey_changed {
        let new_hotkey = state.config.read().unwrap().screenshot_hotkey.clone();
        register_screenshot_hotkey(&app, &new_hotkey, screenshot_handler);
    }
    // 复用 get_config 逻辑返回最新 DTO（避免重复构造）
    get_config(state).await
}

/// 版本信息查询。
///
/// Business Logic: 前端关于页/设置页展示当前版本号。
/// Code Logic: version 取编译期 CARGO_PKG_VERSION；buildDate 暂返回 null（M8 接入打包日期后补）。
#[tauri::command]
pub fn get_version() -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "buildDate": serde_json::Value::Null,
    })
}

/// 打开原生目录选择对话框，返回用户选中的接收目录路径。
///
/// Business Logic: 前端设置页"选择接收目录"按钮点击后调用，让用户在系统文件选择器中
///     选定一个目录作为文件接收保存路径。
/// Code Logic: 通过 tauri-plugin-dialog 的 DialogExt 弹出文件夹选择框；
///     blocking_pick_folder 阻塞至用户确认/取消，确认返回 Some(path)，取消返回 None。
#[tauri::command]
pub async fn choose_dir(app: AppHandle) -> Result<Option<String>, AppError> {
    let picked = app
        .dialog()
        .file()
        .set_title("选择接收目录")
        .blocking_pick_folder();
    Ok(picked.map(|p| p.to_string()))
}
