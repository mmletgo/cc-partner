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
//!     - check_update：app.updater()?.check()，命中则缓存 Update + 写状态，返回元数据
//!     - download_update：spawn 异步任务跑 update.download(on_chunk)，进度写状态 + emit 事件，
//!       完成存 bytes 供 install；JoinHandle 存 AppState，cancel 时 abort 强制中断 reqwest 流
//!     - get_download_status：读状态机
//!     - cancel_download：abort 下载任务句柄 + 置 cancelled
//!     - install_update：取出 bytes + Update，update.install(&bytes) 后 app.request_restart()
//!     生命周期处理：Update 是 owned（derive Clone，无生命周期参数），check 后存入
//!     AppState.update_pending，download/install 时 clone 取出，避免跨命令重复请求 endpoint

use crate::error::AppError;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::UpdaterExt;
use tokio_util::sync::CancellationToken;

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
/// Code Logic: app.updater()?.check().await 返回 Option<Update>：
///     - Some(update)：hasUpdate=true，缓存 update 到 update_pending，填 version/body/downloadUrl/filename
///     - None：hasUpdate=false（已是最新）
///     - Err：hasUpdate=false + error 字段（不 panic，返回给前端展示）
#[tauri::command]
pub async fn check_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<UpdateCheckResult, AppError> {
    // 用 updater_builder 而非 updater()，便于未来按需注入 headers/target；当前用默认配置
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
            // 缓存 Update 供 download/install 命令复用（clone，owned 无生命周期问题）
            *state.update_pending.lock().expect("update_pending 锁中毒") = Some(update);
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
            Ok(UpdateCheckResult {
                has_update: false,
                version: None,
                body: None,
                download_url: None,
                filename: None,
                size: None,
                error: Some(format!("更新检查失败: {e}")),
            })
        }
    }
}

/// 启动更新包下载（异步，立即返回；进度经 update:download-progress 事件 + get_download_status 轮询）。
///
/// Business Logic: 前端发现新版本后「下载更新」，需后台流式下载并实时报告进度。
///
/// Code Logic: tauri-plugin-updater 的 download 接口无原生取消参数，on_chunk 是 FnMut 不可中断 reqwest 流，
///     故把 download 放进 tokio::spawn，存 JoinHandle，cancel 时 task.abort() 强制中断整个 future 树。
///     - 从 update_pending 取 Update clone（若无则用入参 url 构造降级错误）
///     - 重置状态为 downloading（保留 url/filename）
///     - spawn 任务：update.download(on_chunk, on_finish)，on_chunk 累计 downloaded + 写状态 + emit 进度事件；
///       完成存 bytes 到 update_bytes + 置 completed；失败置 failed；被 abort 置 cancelled（由 cancel_download 兜底）
#[tauri::command]
#[allow(unused_variables)]
pub async fn download_update(
    app: AppHandle,
    state: State<'_, AppState>,
    url: Option<String>,
    filename: Option<String>,
) -> Result<serde_json::Value, AppError> {
    // 取出缓存的 Update clone（download/install 基于 check 阶段的同一对象）
    let update_opt = state
        .update_pending
        .lock()
        .expect("update_pending 锁中毒")
        .clone();
    let update = update_opt.ok_or_else(|| {
        AppError::generic("尚未检查到可用更新，请先调用 check_update".to_string())
    })?;

    // 拒绝重复触发：已有下载在进行
    {
        let status = state.update_status.read().expect("update_status 读锁中毒");
        if status.status == UpdateStatusValue::Downloading {
            return Err(AppError::generic("已有下载任务进行中".to_string()));
        }
    }

    let download_url = update.download_url.to_string();
    let download_filename = filename_from_url(&download_url);

    // 重置状态为 downloading（url/filename 来自缓存的 Update；入参 url/filename 仅作兼容透传，不覆盖）
    {
        let mut status = state.update_status.write().expect("update_status 写锁中毒");
        status.status = UpdateStatusValue::Downloading;
        status.progress = 0.0;
        status.error.clear();
        status.file_path.clear();
        status.url = download_url.clone();
        status.filename = download_filename.clone();
        status.size = 0;
    }

    // 取消令牌：cancel_download 调 token.cancel()，spawn 体内 is_cancelled 判定为 Cancelled；
    // 同时 JoinHandle::abort() 兜底强制中断 reqwest 流
    let cancel_token = CancellationToken::new();
    let cancel_for_check = cancel_token.clone();
    // 存取消令牌到 AppState，cancel_download 取出 cancel()
    *state
        .update_cancel_token
        .lock()
        .expect("update_cancel_token 锁中毒") = Some(cancel_token);

    let status_arc = state.update_status.clone();
    let bytes_arc = state.update_bytes.clone();
    let app_handle = app.clone();
    let url_for_event = download_url.clone();

    // spawn 下载任务：JoinHandle 存 AppState，cancel 时 abort
    let handle: JoinHandle<()> = tauri::async_runtime::spawn(async move {
        let mut downloaded: u64 = 0;
        let mut total: Option<u64> = None;
        // on_chunk(chunk_len, content_length)：content_length 仅首个 chunk 非 None
        let download_result = update
            .download(
                |chunk_len, content_length| {
                    // 软取消：cancel_token 置位后，停止累计（reqwest 流仍会继续，由 abort 兜底）
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
                    // 写状态
                    if let Ok(mut s) = status_arc.write() {
                        s.progress = progress.min(1.0);
                        if let Some(t) = total {
                            s.size = t;
                        }
                    }
                    // emit 进度事件（前端可选 listen）
                    let _ = app_handle.emit(
                        "update:download-progress",
                        serde_json::json!({
                            "progress": progress.min(1.0),
                            "downloaded": downloaded,
                            "total": total.unwrap_or(0),
                        }),
                    );
                },
                || {
                    // on_download_finish：download 流结束（但签名校验在 download 内部最后执行，
                    // 校验失败会让 download 返回 Err，走下面的 Err 分支）
                },
            )
            .await;

        match download_result {
            Ok(bytes) => {
                // 下载 + 签名校验通过，缓存 bytes 供 install
                *bytes_arc.lock().expect("update_bytes 锁中毒") = Some(bytes);
                let mut s = status_arc.write().expect("update_status 写锁中毒");
                s.status = UpdateStatusValue::Completed;
                s.progress = 1.0;
                tracing::info!("更新下载完成: {} ({} bytes)", url_for_event, downloaded);
            }
            Err(e) => {
                // 区分取消 vs 失败：cancel_token 置位视为取消
                let cancelled = cancel_for_check.is_cancelled();
                let mut s = status_arc.write().expect("update_status 写锁中毒");
                if cancelled {
                    s.status = UpdateStatusValue::Cancelled;
                    tracing::info!("更新下载已取消: {}", url_for_event);
                } else {
                    s.status = UpdateStatusValue::Failed;
                    s.error = format!("下载失败: {e}");
                    tracing::error!("更新下载失败: {e}");
                }
            }
        }
    });

    // 存 JoinHandle（覆盖旧句柄——旧任务应已完成或被 abort）
    *state
        .update_download_task
        .lock()
        .expect("update_download_task 锁中毒") = Some(handle);

    Ok(serde_json::json!({ "ok": true }))
}

/// 查询下载状态（前端进度条轮询）。
#[tauri::command]
pub async fn get_download_status(
    state: State<'_, AppState>,
) -> Result<UpdateDownloadStatus, AppError> {
    let status = state.update_status.read().expect("update_status 读锁中毒");
    Ok(status.clone())
}

/// 取消正在进行的下载。
///
/// Business Logic: 用户下载过程中改变主意，需中止下载。
/// Code Logic: abort 下载任务 JoinHandle 强制中断 reqwest 流；同时置 cancelled 状态兜底
///     （abort 后 spawn 体可能不再执行到 match，故主动置位）。
#[tauri::command]
pub async fn cancel_download(state: State<'_, AppState>) -> Result<serde_json::Value, AppError> {
    // 先触发软取消令牌（spawn 体内 is_cancelled 判定为 Cancelled）
    let token = state
        .update_cancel_token
        .lock()
        .expect("update_cancel_token 锁中毒")
        .take();
    if let Some(t) = token {
        t.cancel();
    }
    // 再 abort 任务句柄强制中断 reqwest 流
    let mut task_guard = state
        .update_download_task
        .lock()
        .expect("update_download_task 锁中毒");
    if let Some(handle) = task_guard.take() {
        handle.abort();
        // 主动置 cancelled 兜底（abort 后 spawn 体未必执行 match 分支）
        let mut s = state.update_status.write().expect("update_status 写锁中毒");
        s.status = UpdateStatusValue::Cancelled;
        Ok(serde_json::json!({ "ok": true }))
    } else {
        Ok(serde_json::json!({ "ok": false, "error": "无下载任务" }))
    }
}

/// 安装已下载的更新包并重启。
///
/// Business Logic: 下载完成后用户「安装并重启」，用新版本替换当前应用并重启进程。
///
/// Code Logic: 从 update_bytes 取下载的字节 + 从 update_pending 取 Update，调 update.install(&bytes)
///     （macOS 解 tar.gz 替换 .app / Windows 起 nsis/msi / Linux 替换 appimage/deb/rpm，均自带），
///     随后 app.request_restart() 重启。install 失败返回错误给前端。
#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, AppError> {
    // 校验下载已完成
    {
        let status = state.update_status.read().expect("update_status 读锁中毒");
        if status.status != UpdateStatusValue::Completed {
            return Err(AppError::generic("安装包未就绪，请先完成下载".to_string()));
        }
    }

    let bytes_opt = state
        .update_bytes
        .lock()
        .expect("update_bytes 锁中毒")
        .take();
    let bytes =
        bytes_opt.ok_or_else(|| AppError::generic("安装包数据缺失，请重新下载".to_string()))?;

    let update_opt = state
        .update_pending
        .lock()
        .expect("update_pending 锁中毒")
        .clone();
    let update = update_opt
        .ok_or_else(|| AppError::generic("更新元数据缺失，请重新检查更新".to_string()))?;

    tracing::info!("开始安装更新并重启...");
    // install 是同步阻塞调用（内部 fs 操作 + 可能起外部安装进程），用 spawn_blocking 避免阻塞 async 运行时
    let install_result = tauri::async_runtime::spawn_blocking(move || update.install(&bytes))
        .await
        .map_err(|e| AppError::generic(format!("安装任务执行失败: {e}")))?;
    install_result.map_err(|e| AppError::generic(format!("安装失败: {e}")))?;

    // 安装成功，请求重启（tauri Manager 自带，配合 tauri-plugin-process 的 restart 命令同效）
    tracing::info!("安装完成，请求重启应用");
    app.request_restart();
    Ok(serde_json::json!({ "ok": true }))
}
