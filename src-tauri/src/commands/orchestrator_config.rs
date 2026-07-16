//! commands/orchestrator_config.rs — Orchestrator 全局自动化配置命令层。
//!
//! Business Logic（为什么需要这个模块）:
//!     Settings 自动化 tab 需要读取当前设备的 Orchestrator 自动化策略、恢复默认值和保存 patch。
//!     这些策略属于本设备运行偏好，持久化在 `AppConfig.orchestrator`，不再作为项目策略写入数据库。
//!
//! Code Logic（这个模块做什么）:
//!     提供三条 Tauri invoke：读取当前配置、读取默认配置、应用 patch 并保存 config.json；
//!     具体校验和归一化委托 `orchestrator::config`。

use crate::backend::control_client::BackendControlClient;
#[cfg(test)]
use crate::config_runtime::{update_config_transactionally, ConfigRuntime};
use crate::config_runtime::{OrchestratorRuntimePatch, RuntimeConfigPatch};
use crate::error::AppError;
#[cfg(test)]
use crate::orchestrator::config::apply_orchestrator_config_patch;
use crate::orchestrator::config::{
    default_orchestrator_automation_config, normalize_verification_commands,
    OrchestratorAutomationConfigDto, OrchestratorAutomationConfigPatch,
};
use crate::state::AppState;
use tauri::State;

#[cfg(test)]
/// 应用 Orchestrator 自动化配置 patch 到 AppConfig 并返回 DTO。
///
/// Business Logic（为什么需要这个函数）:
///     update 命令需要先完成 patch 校验和内存配置替换，再保存 config.json 并把最新配置返回前端。
///     将纯同步部分抽出后，命令层行为可以在不构造 Tauri State 的情况下测试。
///
/// Code Logic（这个函数做什么）:
///     调用领域层校验/归一化生成 next；仅成功后替换 `cfg.orchestrator`，并返回同一份配置的 DTO。
fn apply_orchestrator_patch_to_app_config(
    cfg: &mut crate::config::AppConfig,
    patch: OrchestratorAutomationConfigPatch,
) -> Result<OrchestratorAutomationConfigDto, AppError> {
    let next = apply_orchestrator_config_patch(&cfg.orchestrator, patch)?;
    cfg.orchestrator = next.clone();
    Ok(OrchestratorAutomationConfigDto::from(next))
}

/// 读取 Orchestrator 自动化全局配置。
///
/// Business Logic（为什么需要这个函数）:
///     设置页自动化 tab 初始化时需要展示当前设备持久化的 scheduler、验证和 delivery 策略。
///
/// Code Logic（这个函数做什么）:
///     从 `state.config` 读锁克隆 `orchestrator` 字段并转换成 camelCase DTO。
#[tauri::command]
pub async fn get_orchestrator_config(
    state: State<'_, AppState>,
) -> Result<OrchestratorAutomationConfigDto, AppError> {
    let config = state.config.read().unwrap().orchestrator.clone();
    Ok(OrchestratorAutomationConfigDto::from(config))
}

/// 读取 Orchestrator 自动化默认配置。
///
/// Business Logic（为什么需要这个函数）:
///     设置页自动化 tab 的“恢复默认”需要拿到后端定义的统一默认策略。
///
/// Code Logic（这个函数做什么）:
///     不读取或写入当前配置，直接返回 default_orchestrator_automation_config 对应 DTO。
#[tauri::command]
pub async fn get_default_orchestrator_config() -> Result<OrchestratorAutomationConfigDto, AppError>
{
    Ok(OrchestratorAutomationConfigDto::from(
        default_orchestrator_automation_config(),
    ))
}

#[cfg(test)]
/// 在 ConfigRuntime 上应用 Orchestrator 自动化 patch。
///
/// Business Logic（为什么需要这个函数）:
///     自动化 tab 保存必须事务落盘；抽出 helper 便于命令与 save 失败回滚单测。
///
/// Code Logic（这个函数做什么）:
///     经 `update_config_transactionally` 调用 patch helper，返回提交后的 DTO。
pub async fn update_orchestrator_config_for_runtime(
    runtime: &ConfigRuntime,
    patch: OrchestratorAutomationConfigPatch,
) -> Result<OrchestratorAutomationConfigDto, AppError> {
    let (_committed, dto) = update_config_transactionally(runtime, |cfg| {
        apply_orchestrator_patch_to_app_config(cfg, patch)
    })
    .await?;
    Ok(dto)
}

/// 更新 Orchestrator 自动化全局配置。
///
/// Business Logic（为什么需要这个函数）:
///     用户保存设置页自动化 tab 后，需要把 patch 归一化并提交到 sidecar 权威配置。
///
/// Code Logic（这个函数做什么）:
///     归一化 verification_commands 后经 BackendControlClient 提交 OrchestratorRuntimePatch；
///     刷新本地缓存并返回 DTO。
#[tauri::command]
pub async fn update_orchestrator_config(
    state: State<'_, AppState>,
    patch: OrchestratorAutomationConfigPatch,
) -> Result<OrchestratorAutomationConfigDto, AppError> {
    let verification_commands = match patch.verification_commands {
        Some(ref text) => Some(normalize_verification_commands(text)?),
        None => None,
    };
    let client = BackendControlClient::from_control_file()?;
    let resp = client
        .apply_patch(RuntimeConfigPatch {
            orchestrator: Some(OrchestratorRuntimePatch {
                enabled: patch.enabled,
                max_concurrent_tasks: patch.max_concurrent_tasks,
                verification_commands,
                auto_commit: patch.auto_commit,
                auto_push_task_branch: patch.auto_push_task_branch,
                auto_merge_to_main: patch.auto_merge_to_main,
                auto_push_main: patch.auto_push_main,
                notify_human_review: patch.notify_human_review,
                notify_blocked: patch.notify_blocked,
                notify_remote_outbox_failed: patch.notify_remote_outbox_failed,
                notify_task_done: patch.notify_task_done,
            }),
            ..Default::default()
        })
        .await?;
    if let Ok(mut cfg) = state.config.write() {
        resp.snapshot.apply_to_local_config(&mut cfg);
    }
    Ok(OrchestratorAutomationConfigDto::from(
        resp.snapshot.orchestrator,
    ))
}

#[cfg(test)]
mod tests {
    use super::apply_orchestrator_patch_to_app_config;
    use crate::config::{AppConfig, OrchestratorAutomationConfig};
    use crate::orchestrator::config::OrchestratorAutomationConfigPatch;

    /// Business Logic（为什么需要这个函数）:
    ///     命令层单测只关心 AppConfig.orchestrator patch 行为，需要稳定的最小配置样本。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 AppConfig，除 orchestrator 外的字段使用固定默认值，避免测试依赖真实文件系统。
    fn test_app_config(orchestrator: OrchestratorAutomationConfig) -> AppConfig {
        AppConfig {
            device_id: "device-test".to_string(),
            device_name: "test-device".to_string(),
            http_port: 0,
            receive_dir: "/tmp".to_string(),
            db_path: "/tmp/cc-partner.db".to_string(),
            screenshot_hotkey: "<cmd>+<shift>+s".to_string(),
            prompt_optimizer_hotkey: "<ctrl>".to_string(),
            prompt_optimizer_fill_language: "zh".to_string(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: Default::default(),
            orchestrator,
            github_trending: Default::default(),
        }
    }

    /// 验证前端 camelCase patch JSON 可反序列化并应用。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     Tauri invoke 参数来自前端 camelCase payload，字段名不匹配会导致设置页保存静默失效。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 camelCase JSON 反序列化 patch，再应用到 AppConfig，断言 DTO 和内存配置都更新。
    #[test]
    fn camel_case_patch_json_deserializes_and_applies() {
        let mut cfg = test_app_config(OrchestratorAutomationConfig::default());
        let patch: OrchestratorAutomationConfigPatch = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "maxConcurrentTasks": 8,
            "verificationCommands": " cargo test \n\n cargo check ",
            "autoPushMain": false
        }))
        .expect("camelCase patch should deserialize");

        let dto =
            apply_orchestrator_patch_to_app_config(&mut cfg, patch).expect("patch should apply");

        assert!(dto.enabled);
        assert_eq!(dto.max_concurrent_tasks, 8);
        assert_eq!(dto.verification_commands, vec!["cargo test", "cargo check"]);
        assert!(!dto.auto_push_main);
        assert_eq!(cfg.orchestrator.max_concurrent_tasks, 8);
        assert_eq!(
            cfg.orchestrator.verification_commands,
            vec!["cargo test".to_string(), "cargo check".to_string()]
        );
    }

    /// 验证命令层 helper 会替换配置并返回同一份 DTO。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     update 命令需要把校验后的配置写回 AppConfig，并把最新配置返回给前端表单。
    ///
    /// Code Logic（这个测试做什么）:
    ///     应用一个包含多个字段的 patch，断言返回 DTO 与 cfg.orchestrator 保持一致。
    #[test]
    fn apply_helper_updates_app_config_and_returns_matching_dto() {
        let mut cfg = test_app_config(OrchestratorAutomationConfig {
            enabled: false,
            max_concurrent_tasks: 1,
            verification_commands: Vec::new(),
            auto_commit: true,
            auto_push_task_branch: true,
            auto_merge_to_main: true,
            auto_push_main: true,
            notify_human_review: true,
            notify_blocked: true,
            notify_remote_outbox_failed: true,
            notify_task_done: false,
            generic_terminal: None,
        });
        let patch = OrchestratorAutomationConfigPatch {
            enabled: Some(true),
            max_concurrent_tasks: Some(2),
            verification_commands: Some("npm test".to_string()),
            auto_commit: Some(false),
            auto_push_task_branch: None,
            auto_merge_to_main: Some(false),
            auto_push_main: None,
            notify_human_review: None,
            notify_blocked: None,
            notify_remote_outbox_failed: None,
            notify_task_done: None,
        };

        let dto =
            apply_orchestrator_patch_to_app_config(&mut cfg, patch).expect("patch should apply");

        assert_eq!(dto.enabled, cfg.orchestrator.enabled);
        assert_eq!(
            dto.max_concurrent_tasks,
            cfg.orchestrator.max_concurrent_tasks
        );
        assert_eq!(
            dto.verification_commands,
            cfg.orchestrator.verification_commands
        );
        assert_eq!(dto.auto_commit, cfg.orchestrator.auto_commit);
        assert_eq!(dto.auto_merge_to_main, cfg.orchestrator.auto_merge_to_main);
    }

    /// 验证非法 patch 不污染传入的 AppConfig。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     保存配置失败时，设置页不应把内存中的当前配置半写入为非法状态。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对 maxConcurrentTasks=0 断言返回 Err，并确认 cfg.orchestrator 完全保持原值。
    #[test]
    fn invalid_patch_does_not_mutate_app_config() {
        let original = OrchestratorAutomationConfig {
            enabled: true,
            max_concurrent_tasks: 3,
            verification_commands: vec!["cargo test".to_string()],
            auto_commit: false,
            auto_push_task_branch: true,
            auto_merge_to_main: false,
            auto_push_main: true,
            notify_human_review: true,
            notify_blocked: true,
            notify_remote_outbox_failed: true,
            notify_task_done: false,
            generic_terminal: None,
        };
        let mut cfg = test_app_config(original.clone());
        let patch = OrchestratorAutomationConfigPatch {
            max_concurrent_tasks: Some(0),
            enabled: Some(false),
            ..Default::default()
        };

        let err = apply_orchestrator_patch_to_app_config(&mut cfg, patch).unwrap_err();

        assert!(err.to_string().contains("并发"));
        assert_eq!(cfg.orchestrator.enabled, original.enabled);
        assert_eq!(
            cfg.orchestrator.max_concurrent_tasks,
            original.max_concurrent_tasks
        );
        assert_eq!(
            cfg.orchestrator.verification_commands,
            original.verification_commands
        );
        assert_eq!(cfg.orchestrator.auto_commit, original.auto_commit);
    }

    /// 验证 orchestrator 配置 save 失败时回滚。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     自动化 tab 保存失败时不得半提交 enabled。
    ///
    /// Code Logic（这个测试做什么）:
    ///     fail_next_save 后 patch enabled=true，断言 Err 且 snapshot 仍 false。
    #[tokio::test]
    async fn save_failure_rolls_back() {
        use crate::config_runtime::ConfigRuntime;
        use crate::config_store::MemoryConfigStore;
        use std::sync::Arc;

        let initial = test_app_config(OrchestratorAutomationConfig::default());
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        store.fail_next_save();
        let runtime = ConfigRuntime::new(initial, store.clone());
        let patch = OrchestratorAutomationConfigPatch {
            enabled: Some(true),
            ..Default::default()
        };

        let err = super::update_orchestrator_config_for_runtime(&runtime, patch)
            .await
            .expect_err("should fail");
        assert!(err.to_string().contains("注入故障"));
        assert!(!runtime.snapshot().unwrap().orchestrator.enabled);
        assert!(!store.snapshot().unwrap().orchestrator.enabled);
    }
}
