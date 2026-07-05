//! commands/orchestrator_config.rs — Orchestrator 全局自动化配置命令层。
//!
//! Business Logic（为什么需要这个模块）:
//!     Settings 自动化 tab 需要读取当前设备的 Orchestrator 自动化策略、恢复默认值和保存 patch。
//!     这些策略属于本设备运行偏好，持久化在 `AppConfig.orchestrator`，不再作为项目策略写入数据库。
//!
//! Code Logic（这个模块做什么）:
//!     提供三条 Tauri invoke：读取当前配置、读取默认配置、应用 patch 并保存 config.json；
//!     具体校验和归一化委托 `orchestrator::config`。

use crate::error::AppError;
use crate::orchestrator::config::{
    apply_orchestrator_config_patch, default_orchestrator_automation_config,
    OrchestratorAutomationConfigDto, OrchestratorAutomationConfigPatch,
};
use crate::state::AppState;
use tauri::State;

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

/// 更新 Orchestrator 自动化全局配置。
///
/// Business Logic（为什么需要这个函数）:
///     用户保存设置页自动化 tab 后，需要把 patch 归一化、校验并持久化到本设备 config.json。
///
/// Code Logic（这个函数做什么）:
///     在写锁内 clone 当前 orchestrator、应用 patch、替换字段并同步 save；全程不跨 await 持锁。
#[tauri::command]
pub async fn update_orchestrator_config(
    state: State<'_, AppState>,
    patch: OrchestratorAutomationConfigPatch,
) -> Result<OrchestratorAutomationConfigDto, AppError> {
    let updated = {
        let mut cfg = state.config.write().unwrap();
        let next = apply_orchestrator_config_patch(&cfg.orchestrator, patch)?;
        cfg.orchestrator = next.clone();
        cfg.save()?;
        next
    };

    Ok(OrchestratorAutomationConfigDto::from(updated))
}
