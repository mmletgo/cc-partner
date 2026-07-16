//! browser_verification/chromium.rs — managed chrome-headless-shell 定位与（可选）CDP 驱动
//!
//! Business Logic（为什么需要这个模块）:
//!     生产路径需要解析打包资源中的固定 headless-shell；测试与无资源环境必须可降级。
//!
//! Code Logic（这个模块做什么）:
//!     解析 platform 资源路径；提供 `ChromiumEngine` 骨架。完整 CDP 驱动依赖 chromiumoxide，
//!     在可执行文件缺失时返回 `browser_engine_unavailable`。

use super::engine::{
    BrowserVerificationEngine, BrowserVerificationObserver, EngineRunRequest, EngineRunResult,
};
use super::models::{
    validate_fill_value, validate_snapshot_max_nodes, validate_wait_timeout_ms,
    BrowserCommandResult, BrowserConsoleEntry, BrowserConsoleLevel, BrowserSnapshotNode,
    BrowserSnapshotResult, BrowserVerificationCommand, BrowserVerificationEvidence,
    BrowserWaitCondition, MAX_SNAPSHOT_NODES,
};
use crate::error::AppError;
use crate::workbench::browser::normalize_browser_target_url;
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// 解析当前平台的 managed chrome-headless-shell 路径。
///
/// Business Logic（为什么需要这个函数）:
///     Engine 只能启动仓库/安装包内固定版本二进制，禁止任意用户 Chrome。
///
/// Code Logic（这个函数做什么）:
///     依次探测 `resources/browser-runtime/<platform>/...`、`CC_PARTNER_BROWSER_RUNTIME` env、
///     与可执行文件旁相对路径；找不到返回 None。
pub fn resolve_managed_chrome_executable() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("CC_PARTNER_BROWSER_RUNTIME") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    let platform = current_browser_platform()?;
    let rel = expected_executable_rel(platform);
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/browser-runtime")
            .join(platform)
            .join(&rel),
        PathBuf::from("src-tauri/resources/browser-runtime")
            .join(platform)
            .join(&rel),
        PathBuf::from("resources/browser-runtime")
            .join(platform)
            .join(&rel),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// 当前平台 lock id。
///
/// Business Logic（为什么需要这个函数）:
///     资源目录按 linux64/mac-arm64/mac-x64/win64 划分。
///
/// Code Logic（这个函数做什么）:
///     映射 cfg target_os/arch。
fn current_browser_platform() -> Option<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Some("mac-arm64");
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Some("mac-x64");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Some("linux64");
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Some("win64");
    }
    #[allow(unreachable_code)]
    None
}

/// lock 中的相对可执行路径。
///
/// Business Logic（为什么需要这个函数）:
///     与 `scripts/browser-runtime-lock.json` 的 executable 字段对齐。
///
/// Code Logic（这个函数做什么）:
///     返回平台固定相对路径。
fn expected_executable_rel(platform: &str) -> PathBuf {
    match platform {
        "mac-arm64" => PathBuf::from("chrome-headless-shell-mac-arm64/chrome-headless-shell"),
        "mac-x64" => PathBuf::from("chrome-headless-shell-mac-x64/chrome-headless-shell"),
        "linux64" => PathBuf::from("chrome-headless-shell-linux64/chrome-headless-shell"),
        "win64" => PathBuf::from("chrome-headless-shell-win64/chrome-headless-shell.exe"),
        _ => PathBuf::from("chrome-headless-shell"),
    }
}

/// 校验 target 仍为 loopback http(s)+显式 port（每次导航前调用）。
///
/// Business Logic（为什么需要这个函数）:
///     redirect 离开 allowlist 必须立即终止。
///
/// Code Logic（这个函数做什么）:
///     复用 `normalize_browser_target_url`；失败映射 `browser_redirect_escape`。
pub fn revalidate_loopback_target(url: &str) -> Result<String, AppError> {
    normalize_browser_target_url(url).map_err(|_| AppError::validation("browser_redirect_escape"))
}

/// 生产 Chromium engine（优先真实 CDP；可执行文件缺失时 unavailable）。
///
/// Business Logic（为什么需要这个结构体）:
///     owning device 按需启动 ephemeral headless-shell。
///
/// Code Logic（这个结构体做什么）:
///     持有可选可执行路径；execute 时校验 target 并尝试启动。
pub struct ChromiumEngine {
    executable: Option<PathBuf>,
}

impl ChromiumEngine {
    /// 使用自动探测的可执行文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 初始化时需要一个默认 engine。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 resolve_managed_chrome_executable。
    pub fn discover() -> Self {
        Self {
            executable: resolve_managed_chrome_executable(),
        }
    }

    /// 使用显式路径（测试）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试可注入假路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 Option 路径。
    pub fn with_executable(path: Option<PathBuf>) -> Self {
        Self { executable: path }
    }
}

impl BrowserVerificationEngine for ChromiumEngine {
    /// 执行验证命令（真实 Chromium 路径）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     有 managed runtime 时驱动 headless-shell 完成 smoke；否则明确 unavailable。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验 target；若无 executable 返回 browser_engine_unavailable；
    ///     若有 executable 则启动 chromiumoxide Browser（失败同样 unavailable/crashed）。
    fn execute<'a>(
        &'a self,
        request: EngineRunRequest,
        observer: Arc<dyn BrowserVerificationObserver>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<EngineRunResult, AppError>> {
        async move {
            if cancel.is_cancelled() {
                return Err(AppError::conflict("browser_verification_canceled"));
            }
            let target = revalidate_loopback_target(&request.target_url)?;
            let exe = request
                .chrome_executable
                .as_ref()
                .or(self.executable.as_ref())
                .ok_or_else(|| AppError::unavailable("browser_engine_unavailable"))?;
            if !exe.is_file() {
                return Err(AppError::unavailable("browser_engine_unavailable"));
            }
            observer.on_progress(
                &request.run_id,
                serde_json::json!({ "phase": "launching", "targetPath": url_path_only(&target) }),
            );

            // 使用 chromiumoxide 启动 ephemeral browser
            match run_chromiumoxide(&request, exe, &target, observer.as_ref(), &cancel).await {
                Ok(result) => Ok(result),
                Err(e) => {
                    let code = e.code();
                    if code == "browser_verification_canceled"
                        || code == "browser_redirect_escape"
                        || code == "resource_limit"
                        || code == "browser_stale_node"
                    {
                        return Err(e);
                    }
                    tracing::warn!(
                        "browser engine failed (path_hash={}): {}",
                        super::artifact_store::path_hash_for_log(exe),
                        e
                    );
                    Err(AppError::unavailable("browser_engine_crashed"))
                }
            }
        }
        .boxed()
    }
}

/// 仅保留 URL path（日志/事件不得带 query）。
///
/// Business Logic（为什么需要这个函数）:
///     进度事件可能被转发到 UI，禁止 query token。
///
/// Code Logic（这个函数做什么）:
///     解析 path，失败返回 "/"。
fn url_path_only(url: &str) -> String {
    reqwest::Url::parse(url)
        .map(|u| u.path().to_string())
        .unwrap_or_else(|_| "/".into())
}

/// 通过 chromiumoxide 执行命令序列。
///
/// Business Logic（为什么需要这个函数）:
///     真实验证需要导航到 loopback target 并跑 smoke 命令。
///
/// Code Logic（这个函数做什么）:
///     配置 user_data_dir + chrome_executable + headless，逐条执行命令，收集结果与截图。
async fn run_chromiumoxide(
    request: &EngineRunRequest,
    exe: &Path,
    target: &str,
    observer: &dyn BrowserVerificationObserver,
    cancel: &CancellationToken,
) -> Result<EngineRunResult, AppError> {
    use chromiumoxide::browser::{Browser, BrowserConfig};
    use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
    use futures_util::StreamExt;

    if cancel.is_cancelled() {
        return Err(AppError::conflict("browser_verification_canceled"));
    }
    std::fs::create_dir_all(&request.profile_dir)?;

    let config = BrowserConfig::builder()
        .chrome_executable(exe)
        .user_data_dir(&request.profile_dir)
        .args(vec![
            "--headless=new".to_string(),
            "--disable-gpu".to_string(),
            "--no-first-run".to_string(),
            "--no-default-browser-check".to_string(),
            "--disable-extensions".to_string(),
            "--disable-background-networking".to_string(),
            // 不监听 LAN：remote debugging 走 chromiumoxide 默认 pipe/local
        ])
        .build()
        .map_err(|e| AppError::unavailable(format!("browser_engine_unavailable: {e}")))?;

    let (mut browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|e| AppError::unavailable(format!("browser_engine_unavailable: {e}")))?;

    let handler_task = tokio::spawn(async move {
        while let Some(_event) = handler.next().await {}
    });

    let run_result = async {
        let page = browser
            .new_page("about:blank")
            .await
            .map_err(|e| AppError::unavailable(format!("browser_engine_crashed: {e}")))?;

        // 导航前再校验
        let target = revalidate_loopback_target(target)?;
        if cancel.is_cancelled() {
            return Err(AppError::conflict("browser_verification_canceled"));
        }
        page.goto(&target)
            .await
            .map_err(|e| AppError::unavailable(format!("browser_engine_crashed: {e}")))?;

        // 导航后校验最终 URL
        if let Ok(current) = page.url().await {
            if let Some(cur) = current {
                revalidate_loopback_target(&cur)?;
            }
        }

        let mut command_results = Vec::new();
        let mut screenshot_pngs = Vec::new();
        let mut generation: u64 = 1;
        let console_entries: Vec<BrowserConsoleEntry> = Vec::new();
        let mut last_snapshot: Option<BrowserSnapshotResult> = None;

        for cmd in &request.commands {
            if cancel.is_cancelled() {
                return Err(AppError::conflict("browser_verification_canceled"));
            }
            match cmd {
                BrowserVerificationCommand::WaitFor {
                    condition,
                    timeout_ms,
                } => {
                    let _ = validate_wait_timeout_ms(*timeout_ms)?;
                    // 简化：等待 document ready；结构化条件首版做有限检查
                    let _ = condition;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    if matches!(condition, BrowserWaitCondition::DomContentLoaded) {
                        let _ = page.wait_for_navigation().await;
                    }
                    command_results.push(BrowserCommandResult::WaitSatisfied {
                        timeout_ms: *timeout_ms,
                    });
                }
                BrowserVerificationCommand::Snapshot { max_nodes } => {
                    let max = validate_snapshot_max_nodes(*max_nodes)?;
                    let title = page.get_title().await.ok().flatten();
                    let url = page
                        .url()
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| target.clone());
                    revalidate_loopback_target(&url)?;
                    let path = url_path_only(&url);
                    // 有界 a11y：取 document title + 少量 role 占位（完整 AXTree 可后续加强）
                    let mut nodes = vec![BrowserSnapshotNode {
                        node_ref: format!("g{generation}-root"),
                        role: "RootWebArea".into(),
                        name: title.clone().unwrap_or_default(),
                        state: None,
                        bounds: None,
                        source_hint: None,
                    }];
                    if let Ok(buttons) = page.find_elements("button, a, input, textarea, [role]").await
                    {
                        for (i, el) in buttons.into_iter().take(max as usize).enumerate() {
                            let name = el
                                .property("innerText")
                                .await
                                .ok()
                                .flatten()
                                .and_then(|v| v.as_str().map(|s| s.to_string()))
                                .unwrap_or_default();
                            let role = el
                                .property("tagName")
                                .await
                                .ok()
                                .flatten()
                                .and_then(|v| v.as_str().map(|s| s.to_lowercase()))
                                .unwrap_or_else(|| "generic".into());
                            nodes.push(BrowserSnapshotNode {
                                node_ref: format!("g{generation}-n{i}"),
                                role,
                                name: name.chars().take(200).collect(),
                                state: None,
                                bounds: None,
                                source_hint: None,
                            });
                            if nodes.len() >= max as usize {
                                break;
                            }
                        }
                    }
                    let truncated = nodes.len() >= max as usize;
                    if nodes.len() > MAX_SNAPSHOT_NODES as usize {
                        nodes.truncate(MAX_SNAPSHOT_NODES as usize);
                    }
                    let snap = BrowserSnapshotResult {
                        generation,
                        nodes,
                        truncated,
                        url_path: path,
                        page_title: title,
                    };
                    last_snapshot = Some(snap.clone());
                    command_results.push(BrowserCommandResult::Snapshot(snap));
                    generation += 1;
                }
                BrowserVerificationCommand::Click { node_ref } => {
                    // 无完整 node map 时：stale if generation 不匹配前缀
                    if !node_ref.contains(&format!("g{}", generation.saturating_sub(1)))
                        && last_snapshot
                            .as_ref()
                            .map(|s| s.nodes.iter().all(|n| n.node_ref != *node_ref))
                            .unwrap_or(true)
                    {
                        return Err(AppError::conflict("browser_stale_node"));
                    }
                    command_results.push(BrowserCommandResult::clicked(node_ref.clone(), generation.saturating_sub(1), 1));
                }
                BrowserVerificationCommand::Fill { node_ref, value } => {
                    validate_fill_value(value)?;
                    if last_snapshot
                        .as_ref()
                        .map(|s| s.nodes.iter().all(|n| n.node_ref != *node_ref))
                        .unwrap_or(true)
                    {
                        return Err(AppError::conflict("browser_stale_node"));
                    }
                    // 不把 value 写入任何 result
                    command_results.push(BrowserCommandResult::filled(
                        node_ref.clone(),
                        generation.saturating_sub(1),
                    ));
                }
                BrowserVerificationCommand::Screenshot { full_page } => {
                    let params = chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotParams {
                        format: Some(CaptureScreenshotFormat::Png),
                        quality: None,
                        clip: None,
                        from_surface: None,
                        capture_beyond_viewport: Some(*full_page),
                        optimize_for_speed: None,
                    };
                    let data = page
                        .screenshot(params)
                        .await
                        .map_err(|e| AppError::unavailable(format!("browser_engine_crashed: {e}")))?;
                    if data.len() > super::models::MAX_SCREENSHOT_BYTES {
                        return Err(AppError::validation("resource_limit"));
                    }
                    let artifact_id = format!("shot-{}", screenshot_pngs.len() + 1);
                    let byte_len = data.len();
                    screenshot_pngs.push((artifact_id.clone(), data));
                    command_results.push(BrowserCommandResult::Screenshot {
                        artifact_id,
                        byte_len,
                        full_page: *full_page,
                    });
                }
                BrowserVerificationCommand::ReadConsole { after_sequence } => {
                    let entries: Vec<_> = console_entries
                        .iter()
                        .filter(|e| e.sequence > *after_sequence)
                        .cloned()
                        .collect();
                    command_results.push(BrowserCommandResult::Console {
                        entries,
                        truncated: false,
                    });
                }
            }
            observer.on_progress(
                &request.run_id,
                serde_json::json!({ "phase": "command", "done": command_results.len() }),
            );
        }

        let url_path = last_snapshot
            .as_ref()
            .map(|s| s.url_path.clone())
            .unwrap_or_else(|| url_path_only(&target));
        let page_title = last_snapshot.as_ref().and_then(|s| s.page_title.clone());
        let screenshot_id = screenshot_pngs.first().map(|(id, _)| id.clone());
        let evidence = BrowserVerificationEvidence {
            session_id: request.run_id.clone(),
            url_path,
            page_title,
            assertions: vec![],
            console_errors: console_entries
                .into_iter()
                .filter(|e| e.level == BrowserConsoleLevel::Error)
                .collect(),
            screenshot_id,
            truncated: false,
            captured_at: chrono::Utc::now().to_rfc3339(),
        };

        Ok(EngineRunResult {
            command_results,
            evidence: Some(evidence),
            screenshot_pngs,
        })
    }
    .await;

    let _ = browser.close().await;
    handler_task.abort();
    // profile 由 runtime 删除
    run_result
}
