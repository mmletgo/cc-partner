//! commands/config.rs — 配置读写 + 版本查询命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端设置页通过 invoke 读取/修改应用配置（设备名、接收目录、快捷键、Workbench Prompt 优化偏好），
//!     关于页通过 invoke 获取版本号。对照 Python protocol.py 的
//!     handle_get_config / handle_update_config / handle_version。
//!
//! Code Logic（这个模块做什么）:
//!     - get_config: 优先读 sidecar 权威快照，失败回落本地缓存。
//!     - get_default_config: 返回环境感知默认偏好，供设置页恢复默认。
//!     - update_config: GUI 经 `BackendControlClient` 提交 allowlist patch；截图快捷键走两阶段补偿。
//!     - `*_for_runtime` helper 仍保留给 owner 侧单测（进程内 ConfigRuntime）。
//!     - get_version: 返回 {version, buildDate}，version 取 CARGO_PKG_VERSION。

use crate::backend::control_client::BackendControlClient;
use crate::config::{
    default_preference_values, normalize_prompt_optimizer_fill_language, AppConfig,
};
#[cfg(test)]
use crate::config_runtime::{update_config_transactionally, ConfigRuntime};
use crate::config_runtime::{ConfigSnapshot, ConfigUpdateResponse, RuntimeConfigPatch};
use crate::error::AppError;
#[cfg(test)]
use crate::hotkey::{compensate_screenshot_hotkey_os, replace_screenshot_hotkey_os};
use crate::hotkey::{GlobalShortcutBackend, TauriGlobalShortcutBackend};
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
    /// Prompt 库 Quick Input 面板快捷键（窗口级 keydown，不走 GlobalShortcut）。
    pub prompt_quick_input_hotkey: String,
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
        prompt_quick_input_hotkey: cfg.prompt_quick_input_hotkey.clone(),
        http_port: cfg.http_port,
    }
}

/// 读取应用配置。
///
/// Business Logic: 前端设置页初始化时展示当前配置；优先展示 sidecar 权威值。
/// Code Logic: control client get-config → 刷新本地缓存；失败回落本地 RwLock。
#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<ConfigDto, AppError> {
    if let Ok(client) = BackendControlClient::from_control_file() {
        if let Ok(snap) = client.get_config().await {
            refresh_local_from_snapshot(&state, &snap)?;
            return Ok(snapshot_to_config_dto(&snap, state.device_id.as_str()));
        }
    }
    let cfg = state.config.read().unwrap();
    Ok(config_to_dto(&cfg))
}

/// 将权威快照刷新到 GUI 本地配置缓存。
///
/// Business Logic（为什么需要这个函数）:
///     代理写成功后设置页再读应看到新值，无需重启。
///
/// Code Logic（这个函数做什么）:
///     写锁 `state.config` 并 `apply_to_local_config`。
fn refresh_local_from_snapshot(state: &AppState, snap: &ConfigSnapshot) -> Result<(), AppError> {
    let mut cfg = state
        .config
        .write()
        .map_err(|_| AppError::generic("配置写锁中毒"))?;
    snap.apply_to_local_config(&mut cfg);
    Ok(())
}

/// 快照 → 设置页 ConfigDto。
///
/// Business Logic（为什么需要这个函数）:
///     ConfigSnapshot 无 device_id，需与本机 device_id 拼装前端 DTO。
///
/// Code Logic（这个函数做什么）:
///     投影 allowlist 字段 + 传入 device_id。
fn snapshot_to_config_dto(snap: &ConfigSnapshot, device_id: &str) -> ConfigDto {
    ConfigDto {
        device_id: device_id.to_string(),
        device_name: snap.device_name.clone(),
        receive_dir: snap.receive_dir.clone(),
        screenshot_hotkey: snap.screenshot_hotkey.clone(),
        prompt_optimizer_hotkey: snap.prompt_optimizer_hotkey.clone(),
        prompt_optimizer_fill_language: normalize_prompt_optimizer_fill_language(
            &snap.prompt_optimizer_fill_language,
        ),
        prompt_quick_input_hotkey: snap.prompt_quick_input_hotkey.clone(),
        http_port: snap.http_port,
    }
}

/// 构建设置页基础偏好的 RuntimeConfigPatch。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 只能提交字段级 patch，禁止整份 stale AppConfig。
///
/// Code Logic（这个函数做什么）:
///     Option 字段映射到 RuntimeConfigPatch。
fn build_preference_patch(
    device_name: Option<String>,
    receive_dir: Option<String>,
    screenshot_hotkey: Option<String>,
    prompt_optimizer_hotkey: Option<String>,
    prompt_optimizer_fill_language: Option<String>,
    prompt_quick_input_hotkey: Option<String>,
) -> RuntimeConfigPatch {
    RuntimeConfigPatch {
        device_name,
        receive_dir,
        screenshot_hotkey,
        prompt_optimizer_hotkey,
        prompt_optimizer_fill_language,
        prompt_quick_input_hotkey,
        ..Default::default()
    }
}

/// 经 owner control client 应用基础偏好 patch（无热键 OS 副作用）。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 命令路径统一代理到 sidecar，返回后刷新本地缓存。
///
/// Code Logic（这个函数做什么）:
///     BackendControlClient::apply_patch → refresh_local_from_snapshot → ConfigDto。
async fn update_config_via_owner(
    state: &AppState,
    device_name: Option<String>,
    receive_dir: Option<String>,
    screenshot_hotkey: Option<String>,
    prompt_optimizer_hotkey: Option<String>,
    prompt_optimizer_fill_language: Option<String>,
    prompt_quick_input_hotkey: Option<String>,
) -> Result<ConfigDto, AppError> {
    let client = BackendControlClient::from_control_file()?;
    let resp: ConfigUpdateResponse = client
        .apply_patch(build_preference_patch(
            device_name,
            receive_dir,
            screenshot_hotkey,
            prompt_optimizer_hotkey,
            prompt_optimizer_fill_language,
            prompt_quick_input_hotkey,
        ))
        .await?;
    refresh_local_from_snapshot(state, &resp.snapshot)?;
    Ok(snapshot_to_config_dto(
        &resp.snapshot,
        state.device_id.as_str(),
    ))
}

/// 经 owner 两阶段补偿更新（含截图快捷键 OS）。
///
/// Business Logic（为什么需要这个函数）:
///     热键 OS 在 GUI；权威配置在 sidecar；必须两阶段 + 响应丢失对账。
///
/// Code Logic（这个函数做什么）:
///     control client `update_config_with_hotkey_compensation` → 刷新本地 → DTO。
#[allow(clippy::too_many_arguments)]
async fn update_config_via_owner_with_hotkey(
    state: &AppState,
    backend: &mut dyn GlobalShortcutBackend,
    device_name: Option<String>,
    receive_dir: Option<String>,
    screenshot_hotkey: Option<String>,
    prompt_optimizer_hotkey: Option<String>,
    prompt_optimizer_fill_language: Option<String>,
    prompt_quick_input_hotkey: Option<String>,
) -> Result<ConfigDto, AppError> {
    let client = BackendControlClient::from_control_file()?;
    let resp = client
        .update_config_with_hotkey_compensation(
            backend,
            build_preference_patch(
                device_name,
                receive_dir,
                screenshot_hotkey,
                prompt_optimizer_hotkey,
                prompt_optimizer_fill_language,
                prompt_quick_input_hotkey,
            ),
        )
        .await?;
    refresh_local_from_snapshot(state, &resp.snapshot)?;
    Ok(snapshot_to_config_dto(
        &resp.snapshot,
        state.device_id.as_str(),
    ))
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
        prompt_quick_input_hotkey,
    ) = default_preference_values();
    Ok(ConfigDto {
        device_id: cfg.device_id.clone(),
        device_name,
        receive_dir,
        screenshot_hotkey,
        prompt_optimizer_hotkey,
        prompt_optimizer_fill_language,
        prompt_quick_input_hotkey,
        http_port: cfg.http_port,
    })
}

#[cfg(test)]
/// 应用设置页 patch 字段到 candidate。
///
/// Business Logic（为什么需要这个函数）:
///     普通事务与热键事务共用同一 patch 语义，避免分叉。
///
/// Code Logic（这个函数做什么）:
///     对传入的 Option 字段写 candidate；语言字段做归一化。
fn apply_config_patch(
    cfg: &mut AppConfig,
    device_name: Option<String>,
    receive_dir: Option<String>,
    screenshot_hotkey: Option<String>,
    prompt_optimizer_hotkey: Option<String>,
    prompt_optimizer_fill_language: Option<String>,
    prompt_quick_input_hotkey: Option<String>,
) {
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
        cfg.prompt_optimizer_fill_language = normalize_prompt_optimizer_fill_language(&language);
    }
    if let Some(h) = prompt_quick_input_hotkey {
        cfg.prompt_quick_input_hotkey = h;
    }
}

#[cfg(test)]
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
    prompt_quick_input_hotkey: Option<String>,
) -> Result<ConfigDto, AppError> {
    let (_committed, dto) = update_config_transactionally(runtime, |cfg| {
        apply_config_patch(
            cfg,
            device_name,
            receive_dir,
            screenshot_hotkey,
            prompt_optimizer_hotkey,
            prompt_optimizer_fill_language,
            prompt_quick_input_hotkey,
        );
        Ok(config_to_dto(cfg))
    })
    .await?;
    Ok(dto)
}

#[cfg(test)]
/// 在同一 ConfigRuntime 临界区内完成 OS 热键切换 + 配置事务 + 失败补偿。
///
/// Business Logic（为什么需要这个函数）:
///     并发 `update_config` 若在 writer lock 外先改 OS 注册，会交错 old_hotkey/补偿，
///     导致 OS 与 config 分叉；必须串行化在同一 gate 内。
///
/// Code Logic（这个函数做什么）:
///     持 `lock_for_update` → clone candidate → OS replace → mutate/validate →
///     spawn_blocking save_atomic → 成功 swap 内存；失败同临界区 OS 补偿。
#[allow(clippy::too_many_arguments)]
pub async fn update_config_with_hotkey_backend(
    runtime: &ConfigRuntime,
    backend: &mut dyn GlobalShortcutBackend,
    device_name: Option<String>,
    receive_dir: Option<String>,
    screenshot_hotkey: Option<String>,
    prompt_optimizer_hotkey: Option<String>,
    prompt_optimizer_fill_language: Option<String>,
    prompt_quick_input_hotkey: Option<String>,
) -> Result<ConfigDto, AppError> {
    let _guard = runtime.lock_for_update().await;

    let mut candidate = runtime.snapshot()?;
    let old_hotkey = candidate.screenshot_hotkey.clone();
    let mut need_os_compensate = false;
    let mut new_hotkey_for_compensate: Option<String> = None;

    if let Some(ref new_hotkey) = screenshot_hotkey {
        if new_hotkey != &old_hotkey {
            replace_screenshot_hotkey_os(backend, &old_hotkey, new_hotkey)?;
            need_os_compensate = true;
            new_hotkey_for_compensate = Some(new_hotkey.clone());
        }
    }

    apply_config_patch(
        &mut candidate,
        device_name,
        receive_dir,
        screenshot_hotkey,
        prompt_optimizer_hotkey,
        prompt_optimizer_fill_language,
        prompt_quick_input_hotkey,
    );

    if let Err(e) = candidate.validate() {
        if need_os_compensate {
            if let Some(ref new_hotkey) = new_hotkey_for_compensate {
                compensate_screenshot_hotkey_os(backend, &old_hotkey, new_hotkey)?;
            }
        }
        return Err(e);
    }

    let store = runtime.store_handle();
    let to_save = candidate.clone();
    let save_result = tokio::task::spawn_blocking(move || store.save_atomic(&to_save))
        .await
        .map_err(|e| AppError::generic(format!("配置落盘任务失败: {e}")))?;

    if let Err(e) = save_result {
        if need_os_compensate {
            if let Some(ref new_hotkey) = new_hotkey_for_compensate {
                // 补偿失败优先返回 rollback_failed，便于用户重启恢复
                compensate_screenshot_hotkey_os(backend, &old_hotkey, new_hotkey)?;
            }
        }
        return Err(e);
    }

    runtime.swap_memory(candidate.clone())?;
    Ok(config_to_dto(&candidate))
}

/// 更新应用配置（基础偏好 + Workbench Prompt 优化偏好），并持久化。
///
/// Business Logic: 用户在设置页保存修改后需落到 sidecar 权威配置，下次启动与 LAN 运行态生效。
/// Code Logic: GUI 经 BackendControlClient 提交 allowlist patch；截图快捷键走两阶段补偿；
///     成功后刷新本地缓存 DTO。无本地 ConfigRuntime mutation fallback。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn update_config(
    app: AppHandle,
    state: State<'_, AppState>,
    device_name: Option<String>,
    receive_dir: Option<String>,
    screenshot_hotkey: Option<String>,
    prompt_optimizer_hotkey: Option<String>,
    prompt_optimizer_fill_language: Option<String>,
    prompt_quick_input_hotkey: Option<String>,
) -> Result<ConfigDto, AppError> {
    if screenshot_hotkey.is_none() {
        return update_config_via_owner(
            state.inner(),
            device_name,
            receive_dir,
            None,
            prompt_optimizer_hotkey,
            prompt_optimizer_fill_language,
            prompt_quick_input_hotkey,
        )
        .await;
    }

    let mut backend = TauriGlobalShortcutBackend::new(app);
    update_config_via_owner_with_hotkey(
        state.inner(),
        &mut backend,
        device_name,
        receive_dir,
        screenshot_hotkey,
        prompt_optimizer_hotkey,
        prompt_optimizer_fill_language,
        prompt_quick_input_hotkey,
    )
    .await
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
    use crate::config::{
        BatteryConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
    };
    use crate::config_store::MemoryConfigStore;
    use crate::hotkey::FakeGlobalShortcutBackend;
    use std::sync::Arc;

    fn sample_config() -> AppConfig {
        AppConfig {
            device_id: "dev-cfg-1".into(),
            device_name: "cfg-device".into(),
            http_port: 0,
            receive_dir: "/tmp/recv".into(),
            db_path: "/tmp/db.db".into(),
            screenshot_hotkey: "<ctrl>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            prompt_quick_input_hotkey: "<ctrl>+/".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            battery: BatteryConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            internal_claude: crate::config::InternalClaudeConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
        }
    }

    /// 验证 save 失败时命令层 helper 回滚内存与 store。
    #[tokio::test]
    async fn save_failure_rolls_back() {
        let _data_dir_guard = crate::config::install_data_dir_env(None);
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
    #[tokio::test]
    async fn successful_update_returns_committed_dto() {
        let _data_dir_guard = crate::config::install_data_dir_env(None);
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

    /// H2：热键 OS 切换与 config 事务同锁；save 失败时 OS 补偿回旧值。
    #[tokio::test]
    async fn hotkey_os_compensates_when_config_save_fails() {
        let _data_dir_guard = crate::config::install_data_dir_env(None);
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        store.fail_next_save();
        let runtime = ConfigRuntime::new(initial, store);
        let mut fake = FakeGlobalShortcutBackend {
            registered: vec!["<ctrl>+s".into()],
            ..Default::default()
        };

        let err = update_config_with_hotkey_backend(
            &runtime,
            &mut fake,
            None,
            None,
            Some("<ctrl>+<shift>+s".into()),
            None,
            None,
            None,
        )
        .await
        .expect_err("save 失败");

        assert!(err.to_string().contains("注入故障"));
        assert_eq!(
            fake.registered(),
            vec!["<ctrl>+s".to_string()],
            "OS 应补偿回旧热键"
        );
        assert_eq!(
            runtime.snapshot().unwrap().screenshot_hotkey,
            "<ctrl>+s",
            "内存保持旧热键"
        );
    }

    /// H2：热键变更成功路径 OS 与 config 一致。
    #[tokio::test]
    async fn hotkey_os_and_config_commit_together() {
        let _data_dir_guard = crate::config::install_data_dir_env(None);
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = ConfigRuntime::new(initial, store);
        let mut fake = FakeGlobalShortcutBackend {
            registered: vec!["<ctrl>+s".into()],
            ..Default::default()
        };

        let dto = update_config_with_hotkey_backend(
            &runtime,
            &mut fake,
            Some("after".into()),
            None,
            Some("<ctrl>+<shift>+s".into()),
            None,
            None,
            None,
        )
        .await
        .expect("ok");

        assert_eq!(dto.screenshot_hotkey, "<ctrl>+<shift>+s");
        assert_eq!(dto.device_name, "after");
        assert_eq!(fake.registered(), vec!["<ctrl>+<shift>+s".to_string()]);
        assert_eq!(
            runtime.snapshot().unwrap().screenshot_hotkey,
            "<ctrl>+<shift>+s"
        );
    }
}
