//! commands/config.rs — 配置读写 + 版本查询命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端设置页通过 invoke 读取/修改应用配置（设备名、接收目录、快捷键、Workbench Prompt 优化偏好），
//!     关于页通过 invoke 获取版本号。对照 Python protocol.py 的
//!     handle_get_config / handle_update_config / handle_version。
//!
//! Code Logic（这个模块做什么）:
//!     - get_config: 读 RwLock 配置，转 ConfigDto（camelCase）。
//!     - get_default_config: 返回环境感知默认偏好，供设置页恢复默认。
//!     - update_config: 经 `ConfigRuntime` 事务路径应用 patch；成功提交后再热更新截图快捷键。
//!     - get_version: 返回 {version, buildDate}，version 取 CARGO_PKG_VERSION。

use crate::config::{
    default_preference_values, normalize_prompt_optimizer_fill_language, AppConfig,
};
use crate::config_runtime::{update_config_transactionally, ConfigRuntime};
use crate::error::AppError;
use crate::hotkey::{
    compensate_screenshot_hotkey_os, replace_screenshot_hotkey_os, TauriGlobalShortcutBackend,
};
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
    pub prompt_optimizer_hotkey: String,
    pub prompt_optimizer_fill_language: String,
    /// HTTP 端口（M1 未实际监听，暂返回配置值；M3 接入真实监听端口后更新）
    pub http_port: i64,
}

/// 将 AppConfig 转为前端 ConfigDto。
///
/// Business Logic（为什么需要这个函数）:
///     读命令与事务更新成功后都要返回同一份 camelCase 结构，避免重复拼装字段。
///
/// Code Logic（这个函数做什么）:
///     从配置克隆可展示字段，并对 Prompt 优化填入语言做归一化。
fn config_to_dto(cfg: &AppConfig) -> ConfigDto {
    ConfigDto {
        device_id: cfg.device_id.clone(),
        device_name: cfg.device_name.clone(),
        receive_dir: cfg.receive_dir.clone(),
        screenshot_hotkey: cfg.screenshot_hotkey.clone(),
        prompt_optimizer_hotkey: cfg.prompt_optimizer_hotkey.clone(),
        prompt_optimizer_fill_language: normalize_prompt_optimizer_fill_language(
            &cfg.prompt_optimizer_fill_language,
        ),
        http_port: cfg.http_port,
    }
}

/// 读取应用配置。
///
/// Business Logic: 前端设置页初始化时展示当前配置。
#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<ConfigDto, AppError> {
    let cfg = state.config.read().unwrap();
    Ok(config_to_dto(&cfg))
}

/// 读取应用偏好的环境默认值。
///
/// Business Logic: 设置页“恢复默认”需要得到 hostname、默认接收目录和平台默认快捷键，
///     不能在前端硬编码或用空字符串代替。
/// Code Logic: 保留当前 device_id/http_port，只替换可编辑偏好字段为默认值后返回 ConfigDto。
#[tauri::command]
pub async fn get_default_config(state: State<'_, AppState>) -> Result<ConfigDto, AppError> {
    let cfg = state.config.read().unwrap();
    let (
        device_name,
        receive_dir,
        screenshot_hotkey,
        prompt_optimizer_hotkey,
        prompt_optimizer_fill_language,
    ) = default_preference_values();
    Ok(ConfigDto {
        device_id: cfg.device_id.clone(),
        device_name,
        receive_dir,
        screenshot_hotkey,
        prompt_optimizer_hotkey,
        prompt_optimizer_fill_language,
        http_port: cfg.http_port,
    })
}

/// 在 ConfigRuntime 上应用基础偏好 patch。
///
/// Business Logic（为什么需要这个函数）:
///     设置页保存配置必须串行事务落盘；抽出 helper 便于命令层与单测共用，无需 Tauri State。
///
/// Code Logic（这个函数做什么）:
///     调用 `update_config_transactionally` 只改 candidate，返回提交后的 ConfigDto。
pub async fn update_config_for_runtime(
    runtime: &ConfigRuntime,
    device_name: Option<String>,
    receive_dir: Option<String>,
    screenshot_hotkey: Option<String>,
    prompt_optimizer_hotkey: Option<String>,
    prompt_optimizer_fill_language: Option<String>,
) -> Result<ConfigDto, AppError> {
    let (_committed, dto) = update_config_transactionally(runtime, |cfg| {
        if let Some(n) = device_name {
            cfg.device_name = n;
        }
        if let Some(d) = receive_dir {
            cfg.receive_dir = d;
        }
        if let Some(h) = screenshot_hotkey {
            cfg.screenshot_hotkey = h;
        }
        if let Some(h) = prompt_optimizer_hotkey {
            cfg.prompt_optimizer_hotkey = h;
        }
        if let Some(language) = prompt_optimizer_fill_language {
            cfg.prompt_optimizer_fill_language =
                normalize_prompt_optimizer_fill_language(&language);
        }
        Ok(config_to_dto(cfg))
    })
    .await?;
    Ok(dto)
}

/// 更新应用配置（基础偏好 + Workbench Prompt 优化偏好），并持久化。
///
/// Business Logic: 用户在设置页保存修改后需落盘，下次启动生效。
/// Code Logic: 经 ConfigRuntime 事务更新；仅提交成功后才热更新截图全局快捷键，并返回 DTO。
#[tauri::command]
pub async fn update_config(
    app: AppHandle,
    state: State<'_, AppState>,
    device_name: Option<String>,
    receive_dir: Option<String>,
    screenshot_hotkey: Option<String>,
    prompt_optimizer_hotkey: Option<String>,
    prompt_optimizer_fill_language: Option<String>,
) -> Result<ConfigDto, AppError> {
    // 截图快捷键：先 OS 切换（注册新→注销旧），再事务持久化；失败则补偿 OS。
    let old_hotkey = state
        .config
        .read()
        .expect("config 读锁中毒")
        .screenshot_hotkey
        .clone();
    let mut backend = TauriGlobalShortcutBackend::new(app.clone());
    let mut need_os_compensate = false;
    if let Some(ref new_hotkey) = screenshot_hotkey {
        if new_hotkey != &old_hotkey {
            replace_screenshot_hotkey_os(&mut backend, &old_hotkey, new_hotkey)?;
            need_os_compensate = true;
        }
    }

    match update_config_for_runtime(
        &state.config_runtime,
        device_name,
        receive_dir,
        screenshot_hotkey.clone(),
        prompt_optimizer_hotkey,
        prompt_optimizer_fill_language,
    )
    .await
    {
        Ok(dto) => Ok(dto),
        Err(e) => {
            if need_os_compensate {
                if let Some(ref new_hotkey) = screenshot_hotkey {
                    compensate_screenshot_hotkey_os(&mut backend, &old_hotkey, new_hotkey)?;
                }
            }
            Err(e)
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig};
    use crate::config_store::MemoryConfigStore;
    use std::sync::Arc;

    fn sample_config() -> AppConfig {
        AppConfig {
            device_id: "dev-cfg-1".into(),
            device_name: "cfg-device".into(),
            http_port: 0,
            receive_dir: "/tmp/recv".into(),
            db_path: "/tmp/db.db".into(),
            screenshot_hotkey: "<ctrl>+<shift>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
        }
    }

    /// 验证 save 失败时命令层 helper 回滚内存与 store。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     设置页保存失败时，用户必须仍看到旧配置，不能半提交。
    ///
    /// Code Logic（这个测试做什么）:
    ///     MemoryConfigStore 注入 fail_next_save，调用 update_config_for_runtime，断言 Err
    ///     且 snapshot/store 保持旧 device_name。
    #[tokio::test]
    async fn save_failure_rolls_back() {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        store.fail_next_save();
        let runtime = ConfigRuntime::new(initial.clone(), store.clone());

        let err = update_config_for_runtime(
            &runtime,
            Some("mutated-name".into()),
            None,
            None,
            None,
            None,
        )
        .await
        .expect_err("save 失败应返回 Err");
        assert!(
            err.to_string().contains("注入故障") || err.to_string().contains("MemoryConfigStore"),
            "应是 store 注入错误: {err}"
        );

        let snap = runtime.snapshot().expect("snapshot");
        assert_eq!(snap.device_name, "cfg-device");
        assert_eq!(
            store.snapshot().unwrap().device_name,
            "cfg-device",
            "store 侧也应保持旧值"
        );
    }

    /// 验证成功 patch 后 DTO 反映提交值。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     设置页依赖返回 DTO 刷新表单，字段必须来自已提交配置。
    ///
    /// Code Logic（这个测试做什么）:
    ///     更新 device_name 与 receive_dir，断言 DTO 与 runtime snapshot 一致。
    #[tokio::test]
    async fn successful_update_returns_committed_dto() {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = ConfigRuntime::new(initial, store);

        let dto = update_config_for_runtime(
            &runtime,
            Some("new-name".into()),
            Some("/tmp/new-recv".into()),
            None,
            None,
            None,
        )
        .await
        .expect("update");

        assert_eq!(dto.device_name, "new-name");
        assert_eq!(dto.receive_dir, "/tmp/new-recv");
        let snap = runtime.snapshot().unwrap();
        assert_eq!(snap.device_name, "new-name");
        assert_eq!(snap.receive_dir, "/tmp/new-recv");
    }
}
