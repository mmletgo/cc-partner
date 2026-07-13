//! commands/updater.rs — 自动更新命令层（M8）
//!
//! Business Logic（为什么需要这个模块）:
//!     用户需在应用内检查/下载/安装新版本。原 Python 自研 checker/downloader/installer
//!     合计 829 行 + 三平台替换脚本，现由 tauri-plugin-updater 统一接管：
//!     check（拉 latest.json + 签名校验 + 版本比较）→ download（流式 + 进度）→
//!     install（macOS tar.gz / Windows nsis+msi / Linux appimage/deb/rpm 自带替换重启）。
//!     本模块仅暴露 5 个 invoke 命令，返回类型严格对齐前端 `web/src/lib/types.ts`
//!     的 UpdateCheckResult / UpdateDownloadStatus（camelCase，可空性与前端一致）。
//!
//! Code Logic（这个模块做什么）:
//!     - check_update：begin_check → app.updater()?.check() → finish_check
//!     - download_update：begin_download → spawn download → record_progress / finish_download
//!     - get_download_status：读 UpdateRuntime.status
//!     - cancel_download：cancel 取出 token/handle 后锁外 cancel/abort
//!     - install_update：begin_install → spawn_blocking install → finish_install
//!     生命周期与 generation 守卫集中在 `crate::updater::UpdateRuntime`。

use crate::error::AppError;
use crate::state::AppState;
use crate::updater::InstallOutcome;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::UpdaterExt;

/// 更新检查结果，对齐前端 `UpdateCheckResult`（camelCase，字段可空）。
///
/// Business Logic: 前端设置页「检查更新」按钮需展示是否有新版本及版本号/说明。
/// Code Logic: hasUpdate=false 时其余字段给 None（serde skip_serializing_if 由前端可选接受）；
///     hasUpdate=true 时填 version/body/downloadUrl/filename/size。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    /// 是否有新版本
    pub has_update: bool,
    /// 新版本号（如 "0.6.0"），无更新时为 None
    pub version: Option<String>,
    /// Release notes（Markdown 原文），无更新或无说明时为 None
    pub body: Option<String>,
    /// 当前平台安装包下载地址（来自 latest.json 的 url），无更新时为 None
    pub download_url: Option<String>,
    /// 安装包文件名（从 download_url 路径末段解析），无更新时为 None
    pub filename: Option<String>,
    /// 安装包字节数；check 阶段未知（latest.json 不含大小），统一给 0，对齐前端「无匹配资源时为 0」语义
    pub size: Option<u64>,
    /// 检查失败时的错误信息（hasUpdate=false 且检查异常时填）
    pub error: Option<String>,
}

/// 更新下载状态值，对齐前端 `UpdateDownloadStatusValue`（lowercase）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UpdateStatusValue {
    /// 空闲（未开始 / 已重置）
    Idle,
    /// 下载中
    Downloading,
    /// 下载完成，可安装
    Completed,
    /// 下载失败（网络/IO/签名校验）
    Failed,
    /// 用户主动取消
    Cancelled,
}

/// 更新下载状态机，对齐前端 `UpdateDownloadStatus`（camelCase，字段全非可选）。
///
/// Business Logic: 前端进度条轮询此结构展示进度百分比/状态/错误。
/// Code Logic: 字段全部非可选（前端 types.ts 定义 error/filePath/url/filename 为 string、
///     size 为 number），故用 String/u64 而非 Option，初始空串/0。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDownloadStatus {
    /// 状态机当前值
    pub status: UpdateStatusValue,
    /// 下载进度 0.0 ~ 1.0
    pub progress: f64,
    /// 错误信息（status=failed 时填，否则空串）
    pub error: String,
    /// 已下载文件路径（tauri-plugin-updater 下载到内存，故恒为空串；保留字段对齐前端）
    pub file_path: String,
    /// 下载地址
    pub url: String,
    /// 安装包文件名
    pub filename: String,
    /// 安装包字节数（content_length，未知为 0）
    pub size: u64,
}

impl Default for UpdateDownloadStatus {
    fn default() -> Self {
        Self {
            status: UpdateStatusValue::Idle,
            progress: 0.0,
            error: String::new(),
            file_path: String::new(),
            url: String::new(),
            filename: String::new(),
            size: 0,
        }
    }
}

/// 从下载 URL 解析文件名（取 path 末段，URL decode）。
///
/// Business Logic: 前端需展示安装包文件名，latest.json 的 url 是直链，文件名在其路径末段。
/// Code Logic: split('/').last() 取末段；percent-encoding 的空格等不处理（文件名通常无特殊字符）。
fn filename_from_url(url: &str) -> String {
    url.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// 检查更新。
///
/// Business Logic: 前端「检查更新」按钮触发；从配置的 endpoint（latest.json）拉取新版本信息，
/// tauri-plugin-updater 内部做版本比较 + 签名预校验。命中则缓存 Update 供后续 download/install。
///
/// Code Logic: begin_check 递增 generation；await plugin check 后 finish_check 回写；
///     下载/安装中 begin_check 返回 Conflict。
#[tauri::command]
pub async fn check_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<UpdateCheckResult, AppError> {
    let generation = state.update_runtime.begin_check()?;

    let updater = app
        .updater()
        .map_err(|e| AppError::generic(format!("更新器初始化失败: {e}")))?;
    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            let body = update.body.clone();
            let download_url = update.download_url.to_string();
            let filename = filename_from_url(&download_url);
            tracing::info!("发现新版本: {} ({})", version, filename);
            let applied = state
                .update_runtime
                .finish_check(generation, Ok(Some(update)))?;
            if !applied {
                // 已被更新一代 check 取代
                return Ok(UpdateCheckResult {
                    has_update: false,
                    version: None,
                    body: None,
                    download_url: None,
                    filename: None,
                    size: None,
                    error: Some("检查结果已过期，请重试".to_string()),
                });
            }
            Ok(UpdateCheckResult {
                has_update: true,
                version: Some(version),
                body,
                download_url: Some(download_url),
                filename: Some(filename),
                size: Some(0),
                error: None,
            })
        }
        Ok(None) => {
            tracing::info!("已是最新版本");
            let _ = state.update_runtime.finish_check(generation, Ok(None))?;
            Ok(UpdateCheckResult {
                has_update: false,
                version: None,
                body: None,
                download_url: None,
                filename: None,
                size: None,
                error: None,
            })
        }
        Err(e) => {
            tracing::error!("更新检查失败: {e}");
            let msg = format!("更新检查失败: {e}");
            let _ = state
                .update_runtime
                .finish_check(generation, Err(msg.clone()))?;
            Ok(UpdateCheckResult {
                has_update: false,
                version: None,
                body: None,
                download_url: None,
                filename: None,
                size: None,
                error: Some(msg),
            })
        }
    }
}

/// 启动更新包下载（异步，立即返回；进度经 update:download-progress 事件 + get_download_status 轮询）。
///
/// Business Logic: 前端发现新版本后「下载更新」，需后台流式下载并实时报告进度。
///
/// Code Logic: begin_download 返回 lease；spawn 任务在锁外 download，回调 record_progress /
///     finish_download（带 generation 守卫）；attach_download_task 登记句柄供 cancel。
#[tauri::command]
#[allow(unused_variables)]
pub async fn download_update(
    app: AppHandle,
    state: State<'_, AppState>,
    url: Option<String>,
    filename: Option<String>,
) -> Result<serde_json::Value, AppError> {
    let lease = state.update_runtime.begin_download()?;
    let generation = lease.generation;
    let update = lease.update;
    let cancel_for_check = lease.cancel;
    let runtime = state.update_runtime.clone();
    let app_handle = app.clone();
    let url_for_event = update.download_url.to_string();

    let handle = tauri::async_runtime::spawn(async move {
        let mut downloaded: u64 = 0;
        let mut total: Option<u64> = None;
        let download_result = update
            .download(
                |chunk_len, content_length| {
                    if cancel_for_check.is_cancelled() {
                        return;
                    }
                    if let Some(cl) = content_length {
                        total = Some(cl);
                    }
                    downloaded = downloaded.saturating_add(chunk_len as u64);
                    let progress = total
                        .filter(|&t| t > 0)
                        .map(|t| (downloaded as f64) / (t as f64))
                        .unwrap_or(0.0);
                    let _ = runtime.record_progress(generation, progress, total);
                    let _ = app_handle.emit(
                        "update:download-progress",
                        serde_json::json!({
                            "progress": progress.min(1.0),
                            "downloaded": downloaded,
                            "total": total.unwrap_or(0),
                        }),
                    );
                },
                || {},
            )
            .await;

        match download_result {
            Ok(bytes) => {
                if runtime.finish_download(generation, Ok(bytes), false) {
                    tracing::info!("更新下载完成: {} ({} bytes)", url_for_event, downloaded);
                }
            }
            Err(e) => {
                let cancelled = cancel_for_check.is_cancelled();
                if runtime.finish_download(generation, Err(format!("下载失败: {e}")), cancelled) {
                    if cancelled {
                        tracing::info!("更新下载已取消: {}", url_for_event);
                    } else {
                        tracing::error!("更新下载失败: {e}");
                    }
                }
            }
        }
    });

    state
        .update_runtime
        .attach_download_task(generation, handle);

    Ok(serde_json::json!({ "ok": true }))
}

/// 查询下载状态（前端进度条轮询）。
#[tauri::command]
pub async fn get_download_status(
    state: State<'_, AppState>,
) -> Result<UpdateDownloadStatus, AppError> {
    Ok(state.update_runtime.status())
}

/// 取消正在进行的下载。
///
/// Business Logic: 用户下载过程中改变主意，需中止下载。
/// Code Logic: cancel 锁内原子取出 token/handle 并置 Cancelled；锁外 cancel/abort。
#[tauri::command]
pub async fn cancel_download(state: State<'_, AppState>) -> Result<serde_json::Value, AppError> {
    let lease = state.update_runtime.cancel();
    let had_work = lease.cancel.is_some() || lease.task.is_some();
    if let Some(token) = lease.cancel {
        token.cancel();
    }
    if let Some(handle) = lease.task {
        handle.abort();
    }
    if had_work {
        Ok(serde_json::json!({ "ok": true }))
    } else {
        Ok(serde_json::json!({ "ok": false, "error": "无下载任务" }))
    }
}

/// 安装已下载的更新包并重启。
///
/// Business Logic: 下载完成后用户「安装并重启」，用新版本替换当前应用并重启进程。
///
/// Code Logic: begin_install 克隆 bytes（不 take）；spawn_blocking install；
///     finish_install 失败回到 Downloaded 保留 bytes，成功请求重启。
#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, AppError> {
    let lease = state.update_runtime.begin_install()?;
    let generation = lease.generation;
    let update = lease.update;
    let bytes = lease.bytes;

    tracing::info!("开始安装更新并重启...");
    let install_result = tauri::async_runtime::spawn_blocking(move || update.install(bytes.as_ref()))
        .await
        .map_err(|e| AppError::generic(format!("安装任务执行失败: {e}")))?;

    match install_result {
        Ok(()) => {
            match state.update_runtime.finish_install(generation, Ok(())) {
                InstallOutcome::RestartRequested | InstallOutcome::Stale => {
                    // Stale 也尝试重启（本代已成功安装）
                    tracing::info!("安装完成，请求重启应用");
                    app.request_restart();
                    Ok(serde_json::json!({ "ok": true }))
                }
                InstallOutcome::FailedRetained => {
                    // 不应出现：Ok(()) 路径
                    Ok(serde_json::json!({ "ok": true }))
                }
            }
        }
        Err(e) => {
            let msg = format!("安装失败: {e}");
            let outcome = state
                .update_runtime
                .finish_install(generation, Err(msg.clone()));
            match outcome {
                InstallOutcome::FailedRetained | InstallOutcome::Stale => {
                    Err(AppError::generic(msg))
                }
                InstallOutcome::RestartRequested => {
                    // 不应出现
                    Err(AppError::generic(msg))
                }
            }
        }
    }
}
