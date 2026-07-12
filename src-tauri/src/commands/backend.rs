//! commands/backend.rs — GUI 管理独立后端 sidecar 的 Tauri 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面 GUI 现在只作为前端壳启动，不再自己 advertise 或启动 axum server；
//!     因此启动时、关闭时和设置/调试入口都需要能管理 `cc-partner-backend` sidecar。
//!
//! Code Logic（这个模块做什么）:
//!     提供 status/start/stop/exit_gui 四个 invoke 命令；packaged 环境通过 tauri-plugin-shell sidecar
//!     执行 `cc-partner-backend start|stop`，开发环境回退到 target/debug binary 或 cargo run。

use crate::backend::control::{self, BackendStatus, BackendStatusKind};
use crate::error::AppError;
use std::path::PathBuf;
use std::process::Stdio;
use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;
use tokio::process::Command;

const BACKEND_SIDECAR_NAME: &str = "cc-partner-backend";

/// 获取当前独立后端状态。
///
/// Business Logic（为什么需要这个函数）:
///     前端关闭弹窗和调试入口需要展示或判断后台 sidecar 是否仍在运行。
///
/// Code Logic（这个函数做什么）:
///     直接委托 `backend::control::current_status`，返回包含 kind/control/error 的 camelCase DTO。
#[tauri::command]
pub async fn get_backend_status() -> BackendStatus {
    control::current_status().await
}

/// 启动独立后端进程。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 启动时必须确保后台 sidecar 已运行；用户也可能从前端手动恢复已停止的后台后端。
///
/// Code Logic（这个函数做什么）:
///     若状态已 Running 则幂等返回；stale 时先清理控制文件；否则执行 sidecar/dev fallback 的 `start` 并再次读取状态。
#[tauri::command]
pub async fn start_backend_process(app: AppHandle) -> Result<BackendStatus, AppError> {
    ensure_backend_process_for_gui(&app).await
}

/// 停止独立后端进程。
///
/// Business Logic（为什么需要这个函数）:
///     用户选择“前后端都关闭”时，GUI 退出前必须先优雅停止后台 sidecar，避免后台服务继续 advertise。
///
/// Code Logic（这个函数做什么）:
///     stopped 状态幂等返回；error 状态向前端报错；其它状态执行 sidecar/dev fallback 的 `stop` 后读取最终状态。
#[tauri::command]
pub async fn stop_backend_process(app: AppHandle) -> Result<BackendStatus, AppError> {
    let status = control::current_status().await;
    match status.kind {
        BackendStatusKind::Stopped => return Ok(status),
        BackendStatusKind::Error => {
            return Err(AppError::generic(
                status
                    .error
                    .unwrap_or_else(|| "后端状态读取失败".to_string()),
            ));
        }
        BackendStatusKind::Running | BackendStatusKind::Stale => {}
    }

    run_backend_cli_command(&app, "stop").await?;
    Ok(control::current_status().await)
}

/// 退出 GUI 进程。
///
/// Business Logic（为什么需要这个函数）:
///     关闭选择弹窗需要“仅关闭 GUI”和“前后端都关闭”两条路径共用同一个最终退出动作。
///
/// Code Logic（这个函数做什么）:
///     调用 Tauri AppHandle.exit(0) 请求桌面进程退出；命令返回 Ok 仅表示退出请求已发出。
#[tauri::command]
pub fn exit_gui(app: AppHandle) -> Result<(), AppError> {
    app.exit(0);
    Ok(())
}

/// 确保 GUI 可复用的独立后端已运行。
///
/// Business Logic（为什么需要这个函数）:
///     GUI setup 需要在启动 mDNS browse-only 前确认 sidecar 已经负责 HTTP/mDNS advertise 和后台任务。
///
/// Code Logic（这个函数做什么）:
///     先读状态；Running 直接返回，Stale 清控制文件，Error 报错，Stopped 则执行 `start` 并要求最终状态为 Running。
pub async fn ensure_backend_process_for_gui(app: &AppHandle) -> Result<BackendStatus, AppError> {
    let initial = control::current_status().await;
    match initial.kind {
        BackendStatusKind::Running => return Ok(initial),
        BackendStatusKind::Stale => control::remove_control_files()?,
        BackendStatusKind::Error => {
            return Err(AppError::generic(
                initial
                    .error
                    .unwrap_or_else(|| "后端状态读取失败".to_string()),
            ));
        }
        BackendStatusKind::Stopped => {}
    }

    run_backend_cli_command(app, "start").await?;
    let status = control::current_status().await;
    if status.kind == BackendStatusKind::Running {
        Ok(status)
    } else {
        Err(AppError::generic(format!(
            "启动独立后端后状态异常: {:?}{}",
            status.kind,
            status
                .error
                .as_deref()
                .map(|error| format!(" ({error})"))
                .unwrap_or_default()
        )))
    }
}

/// 执行 backend CLI 子命令。
///
/// Business Logic（为什么需要这个函数）:
///     packaged app 必须通过 Tauri sidecar 执行 bundled backend；开发模式还需要在未打包 sidecar 时可启动。
///
/// Code Logic（这个函数做什么）:
///     先尝试 `app.shell().sidecar("cc-partner-backend")`，失败后按 dev binary/cargo fallback 执行同一子命令。
async fn run_backend_cli_command(app: &AppHandle, subcommand: &str) -> Result<(), AppError> {
    match run_packaged_sidecar_command(app, subcommand).await {
        Ok(()) => Ok(()),
        Err(sidecar_error) => {
            tracing::warn!(
                "执行 packaged backend sidecar 失败，尝试 dev fallback: {sidecar_error}"
            );
            run_dev_backend_command(subcommand)
                .await
                .map_err(|dev_error| {
                    AppError::generic(format!(
                    "backend sidecar 执行失败: {sidecar_error}; dev fallback 也失败: {dev_error}"
                ))
                })
        }
    }
}

/// 执行打包后的 sidecar CLI。
///
/// Business Logic（为什么需要这个函数）:
///     正式安装包中 `cc-partner-backend` 由 Tauri externalBin 打包，GUI 只能通过 shell plugin 定位它。
///
/// Code Logic（这个函数做什么）:
///     创建 sidecar command，传入 start/stop 子命令并等待短生命周期 CLI 输出；非零退出转业务错误。
async fn run_packaged_sidecar_command(app: &AppHandle, subcommand: &str) -> Result<(), AppError> {
    let output = app
        .shell()
        .sidecar(BACKEND_SIDECAR_NAME)
        .map_err(|error| AppError::generic(format!("创建 backend sidecar 失败: {error}")))?
        .arg(subcommand)
        .output()
        .await
        .map_err(|error| AppError::generic(format!("执行 backend sidecar 失败: {error}")))?;

    if output.status.success() {
        return Ok(());
    }

    Err(AppError::generic(format!(
        "backend sidecar {subcommand} 退出失败: status={:?}, {}",
        output.status,
        command_output_detail(&output.stdout, &output.stderr)
    )))
}

/// 执行开发环境 backend CLI fallback。
///
/// Business Logic（为什么需要这个函数）:
///     `tauri dev` 时 externalBin 可能尚未准备，开发者仍需要 GUI 自动拉起本地 debug backend。
///
/// Code Logic（这个函数做什么）:
///     优先查找 target/debug/cc-partner-backend；找到但执行失败（例如 build.rs 生成的占位 launcher 被误当成真 binary、
///     或 binary 过期损坏）时不能就此终止，必须继续尝试其它候选并最终回退 `cargo run --bin cc-partner-backend`。
///     只有全部路径都失败才聚合错误返回，避免单个坏 candidate 让 dev 模式直接 panic。
async fn run_dev_backend_command(subcommand: &str) -> Result<(), AppError> {
    let mut candidate_errors = Vec::new();
    for candidate in dev_backend_binary_candidates() {
        if !candidate.exists() {
            continue;
        }
        // 防护：跳过明显不是真二进制的 candidate。build.rs 会在 debug profile 生成约 300 字节的
        // shell launcher 占位（`binaries/cc-partner-backend-<target>`），Tauri externalBin 在 dev 模式
        // 会把它复制到 target/debug/cc-partner-backend 覆盖真 binary。真正的 debug backend binary
        // 至少几十 MB，这里用 1MB 作为最低阈值过滤掉 launcher 脚本，确保 dev fallback 走 cargo run。
        if !looks_like_real_backend_binary(&candidate) {
            tracing::warn!(
                "跳过可疑的 backend binary（体积过小，疑似 build.rs 占位 launcher）: {}",
                candidate.display()
            );
            continue;
        }
        let output = match Command::new(&candidate)
            .arg(subcommand)
            .stdin(Stdio::null())
            .output()
            .await
        {
            Ok(output) => output,
            Err(error) => {
                candidate_errors.push(format!("执行 {} 失败: {error}", candidate.display()));
                continue;
            }
        };
        if output.status.success() {
            return Ok(());
        }
        candidate_errors.push(format!(
            "{} {subcommand} 退出失败: status={:?}, {}",
            candidate.display(),
            output.status,
            command_output_detail(&output.stdout, &output.stderr)
        ));
    }

    // 所有 candidate 都不存在或执行失败，回退到 cargo run 现场构建。
    let cargo_output = Command::new("cargo")
        .args(["run", "--bin", BACKEND_SIDECAR_NAME, "--", subcommand])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdin(Stdio::null())
        .output()
        .await?;
    if cargo_output.status.success() {
        return Ok(());
    }

    let mut detail = format!(
        "cargo run backend {subcommand} 退出失败: status={:?}, {}",
        cargo_output.status,
        command_output_detail(&cargo_output.stdout, &cargo_output.stderr)
    );
    if !candidate_errors.is_empty() {
        detail.push_str(&format!(
            "\n候选 backend binary 也全部失败:\n- {}",
            candidate_errors.join("\n- ")
        ));
    }
    Err(AppError::generic(detail))
}

/// 返回开发环境 backend debug binary 候选路径。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri dev、单元测试和直接 cargo run 的当前 exe 位置可能不同，需要多路径查找提升开发体验。
///
/// Code Logic（这个函数做什么）:
///     基于当前 exe 目录及 Cargo manifest 的 target/debug 目录生成候选路径，Windows 自动追加 `.exe`。
fn dev_backend_binary_candidates() -> Vec<PathBuf> {
    let binary_name = backend_binary_name();
    let mut candidates = Vec::new();
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            candidates.push(dir.join(binary_name));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join(binary_name));
            }
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join(binary_name),
    );
    candidates
}

/// 返回当前平台的 backend binary 文件名。
///
/// Business Logic（为什么需要这个函数）:
///     开发 fallback 需要跨 macOS/Linux/Windows 查找实际可执行文件。
///
/// Code Logic（这个函数做什么）:
///     Windows 返回带 `.exe` 后缀的文件名，其它平台返回裸 binary 名。
fn backend_binary_name() -> &'static str {
    if cfg!(windows) {
        "cc-partner-backend.exe"
    } else {
        BACKEND_SIDECAR_NAME
    }
}

/// 判断 candidate 是否像一个真正的 backend 二进制而不是占位脚本。
///
/// Business Logic（为什么需要这个函数）:
///     `build.rs` 在 debug profile 会生成约 300 字节的 shell launcher 占位（用于通过 Tauri externalBin 构建期校验）。
///     Tauri externalBin 在 dev 模式会把该 launcher 复制到 `target/debug/cc-partner-backend` 覆盖真 binary。
///     dev fallback 若把它当真 binary 执行，会触发 launcher 自身的 cargo run（间接但冗余），甚至在旧版 launcher
///     里无限自递归导致 GUI 启动 panic。真正的 debug backend binary 至少几十 MB，用体积阈值即可可靠区分。
///
/// Code Logic（这个函数做什么）:
///     读取 candidate 文件元数据；文件不存在返回 false；体积小于 1MB 视为占位脚本返回 false，否则返回 true。
fn looks_like_real_backend_binary(path: &std::path::Path) -> bool {
    const MIN_REAL_BINARY_BYTES: u64 = 1_000_000;
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.len() >= MIN_REAL_BINARY_BYTES,
        Err(_) => false,
    }
}

/// 格式化命令输出中的可读错误详情。
///
/// Business Logic（为什么需要这个函数）:
///     sidecar 启动失败时用户需要看到 stderr/stdout 中的真实原因，而不是只有退出码。
///
/// Code Logic（这个函数做什么）:
///     优先返回 stderr，缺失时返回 stdout；两者都为空时返回固定占位。
fn command_output_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr_text = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr_text.is_empty() {
        return stderr_text;
    }
    let stdout_text = String::from_utf8_lossy(stdout).trim().to_string();
    if !stdout_text.is_empty() {
        return stdout_text;
    }
    "无输出".to_string()
}
