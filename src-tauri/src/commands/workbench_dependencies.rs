//! commands/workbench_dependencies.rs — 工作台运行时依赖命令
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench 页面和设置页需要通过 Tauri invoke 检测、安装、轮询和取消 tmux 依赖；
//!     Attention source 还需要只读读取缓存中的稳定状态变更时间。
//!
//! Code Logic（这个模块做什么）:
//!     暴露 dependency manager 的四个 thin command，状态与任务句柄保存在 AppState；
//!     另提供 state helper 供 Attention 只读缓存状态，不触发探测。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::dependencies::{
    actual_install_command_preview, probe_workbench_dependency, WorkbenchDependencyState,
    WorkbenchDependencyStatusDto,
};
use tauri::State;

/// Business Logic（为什么需要这个函数）:
///     Attention 聚合依赖 tmux 环境条目时，必须读取进程内缓存的稳定 statusChangedAt，
///     且不能在 Inbox 刷新路径上触发探测/安装副作用。
///
/// Code Logic（这个函数做什么）:
///     直接返回 `AppState.workbench_dependency` 的当前 DTO 快照（含 status_changed_at）。
pub fn get_workbench_dependency_status_for_state(state: &AppState) -> WorkbenchDependencyStatusDto {
    state.workbench_dependency.status()
}

/// 检测 Workbench tmux 依赖状态。
///
/// Business Logic（为什么需要这个函数）:
///     进入 Workbench 或设置页时，前端需要知道 tmux 是否可用以及缺失时可执行的安装命令预览。
///
/// Code Logic（这个函数做什么）:
///     运行后端探测并写入共享 dependency runtime；安装中则保留当前安装状态；
///     写入时由 manager 按语义状态差维护 status_changed_at。
#[tauri::command]
pub async fn check_workbench_dependency(
    state: State<'_, AppState>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    Ok(state
        .workbench_dependency
        .set_checked_status(probe_workbench_dependency()))
}

/// 启动 Workbench tmux 依赖安装。
///
/// Business Logic（为什么需要这个函数）:
///     用户确认安装后，后端负责执行平台安装命令并让前端轮询状态；不做静默 sudo 密码注入。
///
/// Code Logic（这个函数做什么）:
///     若 tmux 已可用直接返回 ready；否则按平台预览命令 spawn 后台任务。
#[tauri::command]
pub async fn install_workbench_dependency(
    state: State<'_, AppState>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    let detected = probe_workbench_dependency();
    if detected.status == WorkbenchDependencyState::Ready {
        return Ok(state.workbench_dependency.set_checked_status(detected));
    }

    let command = actual_install_command_preview()
        .ok_or_else(|| AppError::generic("当前平台不支持自动安装 tmux"))?;
    state.workbench_dependency.spawn_install(command)
}

/// 读取 Workbench tmux 依赖安装状态。
///
/// Business Logic（为什么需要这个函数）:
///     安装命令可能运行较久，前端需要轮询当前状态和最近输出摘要。
///
/// Code Logic（这个函数做什么）:
///     返回 AppState 中 dependency runtime 的当前 DTO 快照（含稳定 statusChangedAt）。
#[tauri::command]
pub async fn get_workbench_dependency_install_status(
    state: State<'_, AppState>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    Ok(get_workbench_dependency_status_for_state(&state))
}

/// 取消正在进行的 Workbench tmux 依赖安装。
///
/// Business Logic（为什么需要这个函数）:
///     用户不想继续等待安装时，应能停止后台安装命令并看到取消状态。
///
/// Code Logic（这个函数做什么）:
///     触发 runtime 取消令牌并返回取消后的状态快照。
#[tauri::command]
pub async fn cancel_workbench_dependency_install(
    state: State<'_, AppState>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    Ok(state.workbench_dependency.cancel())
}

#[cfg(test)]
mod tests {
    use crate::workbench::dependencies::{
        WorkbenchDependencyInstallRuntime, WorkbenchDependencyState, WorkbenchDependencyStatusDto,
    };
    use std::sync::Arc;

    /// Business Logic（为什么需要这个测试）:
    ///     Attention 与前端 status 命令必须共享同一份缓存，重复读取不能重置 statusChangedAt。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在 runtime 上写入 ready，连续两次 status 读取，断言 status_changed_at 稳定。
    #[test]
    fn cached_status_read_preserves_status_changed_at() {
        let runtime = Arc::new(WorkbenchDependencyInstallRuntime::new());
        let first = runtime.set_checked_status(WorkbenchDependencyStatusDto {
            status: WorkbenchDependencyState::Ready,
            available: true,
            version: Some("3.4".into()),
            backend: "native".into(),
            path: Some("/usr/bin/tmux".into()),
            installable: false,
            install_command_preview: Vec::new(),
            error: None,
            output: Vec::new(),
            status_changed_at: String::new(),
        });
        let second = runtime.status();
        let third = runtime.status();

        assert_eq!(first.status, WorkbenchDependencyState::Ready);
        assert!(!first.status_changed_at.is_empty());
        assert_eq!(second.status_changed_at, first.status_changed_at);
        assert_eq!(third.status_changed_at, first.status_changed_at);
    }
}
