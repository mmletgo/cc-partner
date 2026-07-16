//! browser_verification/chromium.rs — managed chrome-headless-shell 定位与 CDP 驱动
//!
//! Business Logic（为什么需要这个模块）:
//!     生产路径需要解析打包资源中的固定 headless-shell；测试与无资源环境必须可降级。
//!     click/fill/console 必须真实执行或明确失败，禁止假成功。
//!
//! Code Logic（这个模块做什么）:
//!     解析 platform 资源路径；`ChromiumEngine` 通过 chromiumoxide 启动 ephemeral browser，
//!     订阅 console、维护 generation 绑定的 Element map，执行 snapshot/click/fill/wait/screenshot。

use super::engine::{
    BrowserVerificationEngine, BrowserVerificationObserver, EngineRunRequest, EngineRunResult,
};
use super::models::{
    redact_console_text, validate_fill_control_kind, validate_fill_value,
    validate_snapshot_byte_budget, validate_snapshot_max_nodes, validate_wait_timeout_ms,
    BrowserCommandResult, BrowserConsoleEntry, BrowserConsoleLevel, BrowserSnapshotNode,
    BrowserSnapshotResult, BrowserVerificationCommand, BrowserVerificationEvidence,
    BrowserWaitCondition, MAX_CONSOLE_ENTRIES, MAX_SNAPSHOT_NODES,
};
use crate::error::AppError;
use crate::workbench::browser::normalize_browser_target_url;
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
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

/// 校验 target 仍为 loopback http(s)+显式 port（每次导航前/后调用）。
///
/// Business Logic（为什么需要这个函数）:
///     redirect 离开 allowlist 必须立即终止（fail-closed）。
///
/// Code Logic（这个函数做什么）:
///     复用 `normalize_browser_target_url`；失败映射 `browser_redirect_escape`。
///     注意：无法在 chromiumoxide 0.7 上可靠挂载 mid-chain navigation listener 时，
///     至少在每次命令前后与 wait 轮询中 revalidate 最终/当前 URL。
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

            match run_chromiumoxide(&request, exe, &target, observer.as_ref(), &cancel).await {
                Ok(result) => Ok(result),
                Err(e) => {
                    let code = e.code();
                    if code == "browser_verification_canceled"
                        || code == "browser_redirect_escape"
                        || code == "resource_limit"
                        || code == "browser_stale_node"
                        || code == "browser_fill_forbidden_control"
                        || code == "browser_wait_timeout"
                        || code == "browser_command_failed"
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

/// 取消感知的异步包装。
///
/// Business Logic（为什么需要这个函数）:
///     cancel 后不能继续 CDP；应尽快返回 canceled。
///
/// Code Logic（这个函数做什么）:
///     `tokio::select!` cancel 与 future。
async fn with_cancel<T, F>(cancel: &CancellationToken, fut: F) -> Result<T, AppError>
where
    F: std::future::Future<Output = Result<T, AppError>>,
{
    tokio::select! {
        _ = cancel.cancelled() => Err(AppError::conflict("browser_verification_canceled")),
        r = fut => r,
    }
}

/// 读取当前 page URL 并 fail-closed revalidate。
///
/// Business Logic（为什么需要这个函数）:
///     导航/redirect 后必须确认仍在 loopback allowlist。
///
/// Code Logic（这个函数做什么）:
///     page.url() → revalidate_loopback_target。
async fn ensure_page_still_loopback(page: &chromiumoxide::Page) -> Result<String, AppError> {
    let current = page
        .url()
        .await
        .map_err(|e| AppError::unavailable(format!("browser_engine_crashed: {e}")))?
        .unwrap_or_default();
    if current.is_empty() || current == "about:blank" {
        return Ok(current);
    }
    revalidate_loopback_target(&current)
}

/// 通过 chromiumoxide 执行命令序列。
///
/// Business Logic（为什么需要这个函数）:
///     真实验证需要导航到 loopback target 并跑 smoke 命令。
///
/// Code Logic（这个函数做什么）:
///     配置 user_data_dir + chrome_executable + headless；订阅 console；维护 node map；
///     真实 click/fill；wait 轮询；snapshot 后校验 2MiB；全程 cancel/revalidate。
async fn run_chromiumoxide(
    request: &EngineRunRequest,
    exe: &Path,
    target: &str,
    observer: &dyn BrowserVerificationObserver,
    cancel: &CancellationToken,
) -> Result<EngineRunResult, AppError> {
    use chromiumoxide::browser::{Browser, BrowserConfig};
    use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
    use chromiumoxide::cdp::js_protocol::runtime::EventConsoleApiCalled;
    use chromiumoxide::Element;

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
        ])
        .build()
        .map_err(|e| AppError::unavailable(format!("browser_engine_unavailable: {e}")))?;

    let (mut browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|e| AppError::unavailable(format!("browser_engine_unavailable: {e}")))?;

    let handler_task = tokio::spawn(async move {
        while let Some(_event) = handler.next().await {}
    });

    let console_entries: Arc<Mutex<Vec<BrowserConsoleEntry>>> =
        Arc::new(Mutex::new(Vec::new()));
    let console_seq = Arc::new(std::sync::atomic::AtomicU64::new(0));

    let run_result = async {
        let page = browser
            .new_page("about:blank")
            .await
            .map_err(|e| AppError::unavailable(format!("browser_engine_crashed: {e}")))?;

        // 订阅 console（Runtime.enable 后 EventConsoleApiCalled）
        let _ = page.enable_runtime().await;
        if let Ok(mut stream) = page.event_listener::<EventConsoleApiCalled>().await {
            let entries = Arc::clone(&console_entries);
            let seq = Arc::clone(&console_seq);
            tokio::spawn(async move {
                while let Some(event) = stream.next().await {
                    let level = map_console_level(&event.r#type);
                    let text = event
                        .args
                        .iter()
                        .map(remote_object_to_text)
                        .collect::<Vec<_>>()
                        .join(" ");
                    let redacted = redact_console_text(&text);
                    let sequence = seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    let mut guard = entries.lock().await;
                    if guard.len() >= MAX_CONSOLE_ENTRIES {
                        continue;
                    }
                    guard.push(BrowserConsoleEntry {
                        sequence,
                        level,
                        text: redacted,
                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                    });
                }
            });
        }

        let target = revalidate_loopback_target(target)?;
        with_cancel(cancel, async {
            page.goto(&target)
                .await
                .map_err(|e| AppError::unavailable(format!("browser_engine_crashed: {e}")))
        })
        .await?;
        ensure_page_still_loopback(&page).await?;

        let mut command_results = Vec::new();
        let mut screenshot_pngs = Vec::new();
        let mut generation: u64 = 1;
        let mut node_map: HashMap<String, Element> = HashMap::new();
        let mut last_snapshot: Option<BrowserSnapshotResult> = None;

        for cmd in &request.commands {
            if cancel.is_cancelled() {
                return Err(AppError::conflict("browser_verification_canceled"));
            }
            // 每条命令前 revalidate（redirect mid-chain 的 fail-closed 加强）
            ensure_page_still_loopback(&page).await?;

            match cmd {
                BrowserVerificationCommand::WaitFor {
                    condition,
                    timeout_ms,
                } => {
                    let timeout_ms = validate_wait_timeout_ms(*timeout_ms)?;
                    wait_for_condition(
                        &page,
                        condition,
                        timeout_ms,
                        cancel,
                        &console_entries,
                    )
                    .await?;
                    ensure_page_still_loopback(&page).await?;
                    command_results.push(BrowserCommandResult::WaitSatisfied { timeout_ms });
                }
                BrowserVerificationCommand::Snapshot { max_nodes } => {
                    let max = validate_snapshot_max_nodes(*max_nodes)?;
                    let (snap, map) =
                        capture_snapshot(&page, generation, max, &target, cancel).await?;
                    validate_snapshot_byte_budget(&snap)?;
                    node_map = map;
                    last_snapshot = Some(snap.clone());
                    command_results.push(BrowserCommandResult::Snapshot(snap));
                    generation = generation.saturating_add(1);
                }
                BrowserVerificationCommand::Click { node_ref } => {
                    let el = node_map.get(node_ref).ok_or_else(|| {
                        AppError::conflict("browser_stale_node")
                    })?;
                    with_cancel(cancel, async {
                        el.click()
                            .await
                            .map_err(|e| AppError::validation(format!("browser_command_failed: click: {e}")))
                    })
                    .await?;
                    // click 可能触发导航
                    let _ = page.wait_for_navigation().await;
                    ensure_page_still_loopback(&page).await?;
                    // 导航后 node map 失效
                    node_map.clear();
                    command_results.push(BrowserCommandResult::clicked(
                        node_ref.clone(),
                        generation.saturating_sub(1),
                        1,
                    ));
                }
                BrowserVerificationCommand::Fill { node_ref, value } => {
                    validate_fill_value(value)?;
                    let el = node_map.get(node_ref).ok_or_else(|| {
                        AppError::conflict("browser_stale_node")
                    })?;
                    let tag = el
                        .property("tagName")
                        .await
                        .ok()
                        .flatten()
                        .and_then(|v| v.as_str().map(|s| s.to_string()));
                    let mut input_type = el.attribute("type").await.ok().flatten();
                    if input_type.is_none() {
                        input_type = el
                            .property("type")
                            .await
                            .ok()
                            .flatten()
                            .and_then(|v| v.as_str().map(|s| s.to_string()));
                    }
                    let hidden_attr = el
                        .attribute("type")
                        .await
                        .ok()
                        .flatten()
                        .map(|t| t.eq_ignore_ascii_case("hidden"))
                        .unwrap_or(false);
                    let is_hidden = hidden_attr
                        || el
                            .property("hidden")
                            .await
                            .ok()
                            .flatten()
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                    validate_fill_control_kind(
                        tag.as_deref(),
                        input_type.as_deref(),
                        is_hidden,
                    )?;
                    with_cancel(cancel, async {
                        // 真实 DOM 写入：先 focus 再 type
                        el.click()
                            .await
                            .map_err(|e| {
                                AppError::validation(format!("browser_command_failed: fill_focus: {e}"))
                            })?;
                        el.type_str(value).await.map_err(|e| {
                            AppError::validation(format!("browser_command_failed: fill: {e}"))
                        })?;
                        Ok::<(), AppError>(())
                    })
                    .await?;
                    // 不把 value 写入任何 result
                    command_results.push(BrowserCommandResult::filled(
                        node_ref.clone(),
                        generation.saturating_sub(1),
                    ));
                }
                BrowserVerificationCommand::Screenshot { full_page } => {
                    let params =
                        chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotParams {
                            format: Some(CaptureScreenshotFormat::Png),
                            quality: None,
                            clip: None,
                            from_surface: None,
                            capture_beyond_viewport: Some(*full_page),
                            optimize_for_speed: None,
                        };
                    let data = with_cancel(cancel, async {
                        page.screenshot(params).await.map_err(|e| {
                            AppError::unavailable(format!("browser_engine_crashed: {e}"))
                        })
                    })
                    .await?;
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
                    let guard = console_entries.lock().await;
                    let entries: Vec<_> = guard
                        .iter()
                        .filter(|e| e.sequence > *after_sequence)
                        .cloned()
                        .collect();
                    let truncated = guard.len() >= MAX_CONSOLE_ENTRIES;
                    drop(guard);
                    command_results.push(BrowserCommandResult::Console {
                        entries,
                        truncated,
                    });
                }
            }
            observer.on_progress(
                &request.run_id,
                serde_json::json!({ "phase": "command", "done": command_results.len() }),
            );
        }

        let console_snapshot = console_entries.lock().await.clone();
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
            console_errors: console_snapshot
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

    // cancel 或完成：强制 close 子进程，再 abort handler
    let _ = browser.close().await;
    // chromiumoxide Browser drop 会尝试 reap；额外 kill 依赖 close
    handler_task.abort();
    // profile 由 runtime 在 join 后删除
    run_result
}

/// 将 CDP console type 映射为业务 level。
///
/// Business Logic（为什么需要这个函数）:
///     evidence 需要稳定的 level 枚举。
///
/// Code Logic（这个函数做什么）:
///     ConsoleApiCalledType → BrowserConsoleLevel。
fn map_console_level(
    t: &chromiumoxide::cdp::js_protocol::runtime::ConsoleApiCalledType,
) -> BrowserConsoleLevel {
    use chromiumoxide::cdp::js_protocol::runtime::ConsoleApiCalledType;
    match t {
        ConsoleApiCalledType::Error | ConsoleApiCalledType::Assert => BrowserConsoleLevel::Error,
        ConsoleApiCalledType::Warning => BrowserConsoleLevel::Warn,
        ConsoleApiCalledType::Info => BrowserConsoleLevel::Info,
        ConsoleApiCalledType::Debug | ConsoleApiCalledType::Trace => BrowserConsoleLevel::Debug,
        _ => BrowserConsoleLevel::Log,
    }
}

/// RemoteObject → 展示文本（仅 value/description，不含 object graph）。
///
/// Business Logic（为什么需要这个函数）:
///     console 参数需拼成可脱敏字符串。
///
/// Code Logic（这个函数做什么）:
///     优先 value 字符串化，否则 description。
fn remote_object_to_text(
    obj: &chromiumoxide::cdp::js_protocol::runtime::RemoteObject,
) -> String {
    if let Some(v) = &obj.value {
        return match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
    }
    obj.description.clone().unwrap_or_default()
}

/// 采集有界 snapshot 并返回 generation 绑定的 Element map。
///
/// Business Logic（为什么需要这个函数）:
///     后续 click/fill 必须能真实定位 DOM 节点；node_ref 仅在当前 generation 有效。
///
/// Code Logic（这个函数做什么）:
///     CSS 查询可交互元素，生成 `g{gen}-n{i}` node_ref，写入 map 与节点列表。
async fn capture_snapshot(
    page: &chromiumoxide::Page,
    generation: u64,
    max: u32,
    fallback_target: &str,
    cancel: &CancellationToken,
) -> Result<(BrowserSnapshotResult, HashMap<String, chromiumoxide::Element>), AppError> {
    if cancel.is_cancelled() {
        return Err(AppError::conflict("browser_verification_canceled"));
    }
    let title = page.get_title().await.ok().flatten();
    let url = page
        .url()
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| fallback_target.to_string());
    revalidate_loopback_target(&url)?;
    let path = url_path_only(&url);

    let mut nodes = vec![BrowserSnapshotNode {
        node_ref: format!("g{generation}-root"),
        role: "RootWebArea".into(),
        name: title.clone().unwrap_or_default(),
        state: None,
        bounds: None,
        source_hint: None,
    }];
    let mut map = HashMap::new();

    let elements = page
        .find_elements("button, a, input, textarea, select, [role]")
        .await
        .map_err(|e| AppError::unavailable(format!("browser_engine_crashed: {e}")))?;

    for (i, el) in elements.into_iter().enumerate() {
        if nodes.len() >= max as usize {
            break;
        }
        if cancel.is_cancelled() {
            return Err(AppError::conflict("browser_verification_canceled"));
        }
        let mut name = el
            .property("innerText")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        if name.is_empty() {
            name = el
                .attribute("aria-label")
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
        }
        if name.is_empty() {
            name = el
                .attribute("placeholder")
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
        }
        if name.is_empty() {
            name = el.attribute("name").await.ok().flatten().unwrap_or_default();
        }
        let role = el
            .property("tagName")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(|s| s.to_lowercase()))
            .unwrap_or_else(|| "generic".into());
        let node_ref = format!("g{generation}-n{i}");
        nodes.push(BrowserSnapshotNode {
            node_ref: node_ref.clone(),
            role,
            name: name.chars().take(200).collect(),
            state: None,
            bounds: None,
            source_hint: None,
        });
        map.insert(node_ref, el);
        if nodes.len() >= max as usize {
            break;
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
    Ok((snap, map))
}

/// 轮询等待结构化条件（带超时与 cancel）。
///
/// Business Logic（为什么需要这个函数）:
///     wait 不得假装满足；超时必须失败。
///
/// Code Logic（这个函数做什么）:
///     按 condition 轮询 page 状态直到满足或 timeout。
async fn wait_for_condition(
    page: &chromiumoxide::Page,
    condition: &BrowserWaitCondition,
    timeout_ms: u64,
    cancel: &CancellationToken,
    console_entries: &Arc<Mutex<Vec<BrowserConsoleEntry>>>,
) -> Result<(), AppError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if cancel.is_cancelled() {
            return Err(AppError::conflict("browser_verification_canceled"));
        }
        // 每轮 revalidate URL（redirect fail-closed）
        ensure_page_still_loopback(page).await?;

        let satisfied = match condition {
            BrowserWaitCondition::DomContentLoaded => {
                // readyState interactive/complete
                match page.evaluate("document.readyState").await {
                    Ok(eval) => {
                        let s = eval
                            .value()
                            .and_then(|v| v.as_str().map(|s| s.to_string()))
                            .unwrap_or_default();
                        s == "interactive" || s == "complete"
                    }
                    Err(_) => false,
                }
            }
            BrowserWaitCondition::UrlPath { path } => {
                let url = page.url().await.ok().flatten().unwrap_or_default();
                url_path_only(&url) == *path
            }
            BrowserWaitCondition::TextPresent { text } => {
                if text.is_empty() {
                    true
                } else {
                    match page.evaluate("document.body ? document.body.innerText : ''").await {
                        Ok(eval) => {
                            let body = eval
                                .value()
                                .and_then(|v| v.as_str().map(|s| s.to_string()))
                                .unwrap_or_default();
                            body.contains(text)
                        }
                        Err(_) => false,
                    }
                }
            }
            BrowserWaitCondition::RoleVisible { role, name } => {
                // 简化：在可交互集合中按 tag/role + name 子串匹配
                match page
                    .find_elements("button, a, input, textarea, select, [role]")
                    .await
                {
                    Ok(els) => {
                        let mut found = false;
                        for el in els.into_iter().take(200) {
                            let tag = el
                                .property("tagName")
                                .await
                                .ok()
                                .flatten()
                                .and_then(|v| v.as_str().map(|s| s.to_lowercase()))
                                .unwrap_or_default();
                            let role_attr = el
                                .attribute("role")
                                .await
                                .ok()
                                .flatten()
                                .unwrap_or_default()
                                .to_lowercase();
                            let text = el
                                .property("innerText")
                                .await
                                .ok()
                                .flatten()
                                .and_then(|v| v.as_str().map(|s| s.to_string()))
                                .unwrap_or_default();
                            let role_l = role.to_lowercase();
                            let name_ok = name.is_empty() || text.contains(name.as_str());
                            let role_ok = tag == role_l
                                || role_attr == role_l
                                || (role_l == "button" && tag == "button")
                                || (role_l == "link" && tag == "a")
                                || (role_l == "textbox" && (tag == "input" || tag == "textarea"));
                            if role_ok && name_ok {
                                found = true;
                                break;
                            }
                        }
                        found
                    }
                    Err(_) => false,
                }
            }
            BrowserWaitCondition::ConsoleErrorCountAtMost { max } => {
                let guard = console_entries.lock().await;
                let errors = guard
                    .iter()
                    .filter(|e| e.level == BrowserConsoleLevel::Error)
                    .count();
                errors as u32 <= *max
            }
        };

        if satisfied {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(AppError::timeout("browser_wait_timeout"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
