//! workbench/browser.rs — Workbench 浏览器预览发现
//!
//! Business Logic（为什么需要这个模块）:
//!     用户在 Workbench 启动前端 dev server 后，需要应用自动发现可预览的本机 URL，
//!     并拒绝把浏览器预览变成可访问任意地址的开放代理。
//!
//! Code Logic（这个模块做什么）:
//!     提供 URL 提取、URL 安全归一化、候选排序、项目配置推断和默认端口探测等基础逻辑。

#![allow(dead_code)]

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::browser_models::{
    WorkbenchBrowserDiscovery, WorkbenchBrowserTarget, WorkbenchBrowserTargetSource,
};
use regex::Regex;
use reqwest::Url;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use std::time::Duration;

/**
 * Business Logic（为什么需要这个函数）:
 *   网页浏览是内测功能，默认关闭；入口隐藏后发现/预览/代理也必须 fail-closed。
 *
 * Code Logic（这个函数做什么）:
 *   读 `experimental_features.browser`；缺配置或读锁失败视为关闭。
 */
pub fn experimental_browser_enabled(state: &AppState) -> bool {
    state
        .config
        .read()
        .map(|cfg| cfg.experimental_features.browser)
        .unwrap_or(false)
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Tauri / P2P / mobile 发现与创建 preview 在未 opt-in 时不得扫描端口或建会话。
 *
 * Code Logic（这个函数做什么）:
 *   未开启则返回 validation，文案与游戏/云同步内测门禁一致。
 */
pub fn require_experimental_browser(state: &AppState) -> Result<(), AppError> {
    if experimental_browser_enabled(state) {
        Ok(())
    } else {
        Err(AppError::validation("内测功能「网页浏览」未开启"))
    }
}

static DEV_URL_RE: OnceLock<Regex> = OnceLock::new();
static HOST_PORT_RE: OnceLock<Regex> = OnceLock::new();

/// 端口探测与「上次误开」残留共用的常见 dev server 端口。
///
/// Business Logic（为什么需要这个常量）:
///     本机任意项目都可能占用 3000/5173/8080；不能仅凭 Remembered 或探测命中就把它们当当前项目默认页。
const DEFAULT_PROBE_PORTS: [u16; 8] = [5173, 3000, 4173, 5174, 8080, 8000, 4321, 6006];
const LOOPBACK_PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// 获取 dev server URL 正则。
///
/// Business Logic（为什么需要这个函数）:
///     自动发现需要从 Vite/Next 等终端输出中识别显式 loopback URL。
///
/// Code Logic（这个函数做什么）:
///     懒加载 Regex，仅匹配 http(s) 且 host 为 localhost/127.0.0.1/0.0.0.0/[::1] 的带端口 URL。
fn dev_url_re() -> &'static Regex {
    DEV_URL_RE.get_or_init(|| {
        Regex::new(
            r#"https?://(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\])(?::\d{2,5})(?:/[^\s'"<)]*)?"#,
        )
        .expect("valid dev server url regex")
    })
}

/// 获取 host:port 正则。
///
/// Business Logic（为什么需要这个函数）:
///     部分 dev server 只输出 `0.0.0.0:3000` 这类片段，仍应被发现为本机 URL。
///
/// Code Logic（这个函数做什么）:
///     懒加载 Regex，捕获 loopback host 后的显式端口。
fn host_port_re() -> &'static Regex {
    HOST_PORT_RE.get_or_init(|| {
        Regex::new(r#"(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]):(\d{2,5})"#)
            .expect("valid host port regex")
    })
}

/// 发现 Workbench 浏览器预览候选目标。
///
/// Business Logic（为什么需要这个函数）:
///     用户进入 Browser tab 时，应自动看到上次使用、终端输出、项目配置和可达端口的候选 URL。
///     默认只自动打开上次使用 / 终端输出 / 项目配置；端口探测只列出。
///     Remembered 若落在默认探测端口，必须同时被终端输出或项目配置印证，避免上次误开的 3000 残留继续默认打开。
///     Task 1 发现只能在项目所属设备上的 local project 执行；remote shortcut 必须由后续命令/路由 wrapper 转发到 owning device。
///
/// Code Logic（这个函数做什么）:
///     解析本机 local 项目/worktree 根目录，合并 remembered/terminal/package/probe 四类候选；
///     Remembered 默认探测端口先标不可达，由 PortProbe 合并真实可达性；去重排序后按印证规则写 selected_target_id。
///     非 local 项目由 resolve 阶段拒绝，避免本机直接扫描远端 shortcut 的终端或端口。
pub async fn discover_workbench_browser_targets(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
) -> Result<WorkbenchBrowserDiscovery, AppError> {
    require_experimental_browser(state)?;
    let root = resolve_browser_worktree_root(state, &project_id, worktree_id.as_deref()).await?;
    let probe_client = loopback_probe_client();
    let remembered = remembered_browser_targets(
        state
            .workbench_browser_repo
            .get_target(&project_id, worktree_id.as_deref())
            .await?,
        probe_client.as_ref(),
    )
    .await;
    let terminal = terminal_output_targets(state, &project_id, worktree_id.as_deref()).await?;
    let config = project_config_targets(&root).await?;
    let probed = probe_default_port_targets(state, &root, probe_client.as_ref()).await?;
    let unranked = remembered
        .into_iter()
        .chain(terminal)
        .chain(config)
        .chain(probed)
        .collect::<Vec<_>>();
    let corroborated_urls = corroborated_browser_urls(&unranked);
    let targets = rank_browser_targets(unranked);
    let selected_target_id = select_auto_open_browser_target_id(&targets, &corroborated_urls);
    Ok(WorkbenchBrowserDiscovery {
        project_id,
        worktree_id,
        targets,
        selected_target_id,
    })
}

/// 从终端输出中提取本机 dev server URL。
///
/// Business Logic（为什么需要这个函数）:
///     用户启动 Vite/Next/Astro 等 dev server 后，希望 Workbench 自动发现预览地址，不需要手动输入 URL。
///
/// Code Logic（这个函数做什么）:
///     扫描终端文本中的 loopback http(s) URL 和 host:port 片段，校验后去重返回；显式 localhost URL 保留原展示值。
pub fn extract_dev_server_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for matched in dev_url_re().find_iter(text).map(|m| m.as_str()) {
        if let Ok(url) = terminal_url_candidate(matched) {
            push_unique(&mut urls, url);
        }
    }
    for captures in host_port_re().captures_iter(text) {
        let Some(matched) = captures.get(0) else {
            continue;
        };
        if is_part_of_explicit_url(text, matched.start()) {
            continue;
        }
        if let Some(port) = captures.get(1) {
            let raw = format!("http://127.0.0.1:{}/", port.as_str());
            if let Ok(url) = normalize_browser_target_url(&raw) {
                push_unique(&mut urls, url);
            }
        }
    }
    urls
}

/// 规范化并校验浏览器预览目标 URL。
///
/// Business Logic（为什么需要这个函数）:
///     Browser preview 代理不能成为开放代理，只允许访问项目所在设备上的本机 dev server。
///
/// Code Logic（这个函数做什么）:
///     补齐 scheme，拒绝非 http(s)，把 localhost/0.0.0.0/[::1] 归一化到 127.0.0.1，并要求显式端口。
pub fn normalize_browser_target_url(raw: &str) -> Result<String, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::generic("预览地址不能为空"));
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = Url::parse(&with_scheme).map_err(|_| AppError::generic("预览地址格式无效"))?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err(AppError::generic("预览地址只支持 http 或 https")),
    }
    let host = url
        .host_str()
        .ok_or_else(|| AppError::generic("预览地址缺少 host"))?
        .to_ascii_lowercase();
    let allowed = matches!(host.as_str(), "localhost" | "127.0.0.1" | "0.0.0.0" | "::1");
    if !allowed {
        return Err(AppError::generic(
            "预览地址必须指向项目所在设备的本机 dev server",
        ));
    }
    if url.port().is_none() {
        return Err(AppError::generic("预览地址必须包含端口"));
    }
    url.set_host(Some("127.0.0.1"))
        .map_err(|_| AppError::generic("预览地址 host 无法归一化"))?;
    if url.path().is_empty() {
        url.set_path("/");
    }
    Ok(url.to_string())
}

/// 解析浏览器发现的 worktree 根目录。
///
/// Business Logic（为什么需要这个函数）:
///     浏览器预览发现必须运行在项目所在设备的本机项目/worktree 根上。
///     本机看到的 remote shortcut 只是入口，必须由后续 commands wrapper/P2P route 转发到 owning device 后再调用本函数。
///
/// Code Logic（这个函数做什么）:
///     读取 project row 并要求 kind=local；无 worktree_id 返回项目路径，有 worktree_id 时校验归属并返回 worktree 路径。
///     该函数不要直接暴露给 remote shortcut，否则会错误扫描当前设备的终端 replay 与 loopback 端口。
async fn resolve_browser_worktree_root(
    state: &AppState,
    project_id: &str,
    worktree_id: Option<&str>,
) -> Result<PathBuf, AppError> {
    let project = state
        .workbench_project_repo
        .get(project_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台项目不存在"))?;
    if project.kind != "local" {
        return Err(AppError::generic("浏览器预览发现只支持本机项目"));
    }
    let Some(worktree_id) = worktree_id else {
        return Ok(PathBuf::from(project.path));
    };
    let worktree = state
        .workbench_worktree_repo
        .get(worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
    if worktree.project_id != project_id {
        return Err(AppError::generic("工作台 worktree 不属于当前项目"));
    }
    Ok(PathBuf::from(worktree.path))
}

/// 从终端 replay 输出生成候选。
///
/// Business Logic（为什么需要这个函数）:
///     dev server 地址通常只出现在终端输出中，自动发现应优先复用当前项目/worktree 的终端历史。
///     项目级发现只应读取主工作区相关 session，不能把同项目 feature worktree 的 dev server 混入主项目预览。
///
/// Code Logic（这个函数做什么）:
///     读取持久化会话 id 与运行期会话 replay buffer，按 worktree scope 过滤后提取 URL 并映射为 TerminalOutput 候选。
async fn terminal_output_targets(
    state: &AppState,
    project_id: &str,
    worktree_id: Option<&str>,
) -> Result<Vec<WorkbenchBrowserTarget>, AppError> {
    let mut session_ids = Vec::new();
    let (target_is_main, main_worktree_ids) = if let Some(worktree_id) = worktree_id {
        (
            state
                .workbench_worktree_repo
                .get(worktree_id)
                .await?
                .map(|worktree| worktree.is_main)
                .unwrap_or(false),
            HashSet::new(),
        )
    } else {
        (
            false,
            project_level_browser_main_worktree_ids(state, project_id).await?,
        )
    };
    let rows = state.workbench_session_repo.list(Some(project_id)).await?;
    for row in rows {
        if session_matches_browser_worktree_scope(
            row.worktree_id.as_deref(),
            worktree_id,
            target_is_main,
            &main_worktree_ids,
        ) {
            push_unique(&mut session_ids, row.id);
        }
    }
    for live in state.workbench_sessions.list(Some(project_id)) {
        if session_matches_browser_worktree_scope(
            live.worktree_id.as_deref(),
            worktree_id,
            target_is_main,
            &main_worktree_ids,
        ) {
            push_unique(&mut session_ids, live.id);
        }
    }

    let mut targets = Vec::new();
    for session_id in session_ids {
        let replay = state.workbench_sessions.replay(&session_id);
        for url in extract_dev_server_urls(&replay.buffer) {
            if let Ok(target) =
                browser_target_from_url(&url, WorkbenchBrowserTargetSource::TerminalOutput, true)
            {
                targets.push(target);
            }
        }
    }
    Ok(targets)
}

/// 获取 project-level 浏览器发现允许读取的主 worktree id。
///
/// Business Logic（为什么需要这个函数）:
///     主项目预览应兼容当前主 worktree session 和旧版无 worktree session，但不能扫描同项目 feature worktree。
///
/// Code Logic（这个函数做什么）:
///     从 workbench_worktrees 查询 is_main=true 的记录，并加入主 worktree 的确定性 id 兜底。
async fn project_level_browser_main_worktree_ids(
    state: &AppState,
    project_id: &str,
) -> Result<HashSet<String>, AppError> {
    let mut ids = HashSet::from([format!("{project_id}:main")]);
    for worktree in state
        .workbench_worktree_repo
        .list_by_project(project_id)
        .await?
    {
        if worktree.is_main {
            ids.insert(worktree.id);
        }
    }
    Ok(ids)
}

/// 判断终端会话是否属于当前浏览器发现范围。
///
/// Business Logic（为什么需要这个函数）:
///     浏览器预览应只复用当前项目根或当前 worktree 的终端输出，避免主项目预览误选 feature worktree 的 dev server。
///
/// Code Logic（这个函数做什么）:
///     对显式 worktree_id 做精确匹配；主 worktree 兼容旧的 project-level session；
///     project-level 请求只接受无 worktree session 和已知主 worktree id。
fn session_matches_browser_worktree_scope(
    session_worktree_id: Option<&str>,
    requested_worktree_id: Option<&str>,
    requested_worktree_is_main: bool,
    main_worktree_ids: &HashSet<String>,
) -> bool {
    match requested_worktree_id {
        Some(id) => {
            session_worktree_id == Some(id)
                || (requested_worktree_is_main && session_worktree_id.is_none())
        }
        None => session_worktree_id
            .map(|id| main_worktree_ids.contains(id))
            .unwrap_or(true),
    }
}

/// 从 package.json 推断候选端口。
///
/// Business Logic（为什么需要这个函数）:
///     项目还没输出终端地址时，也应根据常见前端框架给用户可尝试的预览候选。
///
/// Code Logic（这个函数做什么）:
///     读取根目录 package.json，根据 scripts/dependencies 中的框架关键词映射默认端口。
async fn project_config_targets(root: &Path) -> Result<Vec<WorkbenchBrowserTarget>, AppError> {
    let package_json = match tokio::fs::read_to_string(root.join("package.json")).await {
        Ok(content) => content,
        Err(_) => return Ok(Vec::new()),
    };
    let package: Value = match serde_json::from_str(&package_json) {
        Ok(value) => value,
        Err(_) => return Ok(Vec::new()),
    };
    let mut ports = Vec::new();
    if package_has_any(&package, &["vite", "nuxt", "sveltekit", "@sveltejs/kit"]) {
        add_unique_port(&mut ports, 5173);
    }
    if package_has_any(&package, &["next", "nextjs", "remix", "@remix-run"]) {
        add_unique_port(&mut ports, 3000);
    }
    if package_has_any(&package, &["astro"]) {
        add_unique_port(&mut ports, 4321);
    }
    if package_has_any(&package, &["storybook", "@storybook/"]) {
        add_unique_port(&mut ports, 6006);
    }
    let mut targets = Vec::new();
    for port in ports {
        let raw = format!("http://127.0.0.1:{port}/");
        targets.push(browser_target_from_url(
            &raw,
            WorkbenchBrowserTargetSource::ProjectConfig,
            false,
        )?);
    }
    Ok(targets)
}

/// 探测常见 dev server 端口。
///
/// Business Logic（为什么需要这个函数）:
///     用户已启动服务但终端 replay 不完整时，Workbench 仍应能发现常见端口上的本机 dev server。
///
/// Code Logic（这个函数做什么）:
///     用禁用系统代理的短超时客户端对默认端口发起 GET，排除 cc-partner 自身 HTTP 端口，只返回成功建立响应的端口。
async fn probe_default_port_targets(
    state: &AppState,
    _root: &Path,
    client: Option<&reqwest::Client>,
) -> Result<Vec<WorkbenchBrowserTarget>, AppError> {
    let Some(client) = client else {
        return Ok(Vec::new());
    };
    let actual_http_port = state.actual_http_port.load(Ordering::Relaxed);
    let mut targets = Vec::new();
    for port in DEFAULT_PROBE_PORTS {
        if actual_http_port != 0 && port == actual_http_port {
            continue;
        }
        let raw = format!("http://127.0.0.1:{port}/");
        if probe_loopback_url(client, &raw).await {
            targets.push(browser_target_from_url(
                &raw,
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            )?);
        }
    }
    Ok(targets)
}

/// 构造禁用系统代理的本机探测客户端。
///
/// Business Logic（为什么需要这个函数）:
///     HTTP_PROXY 会让 127.0.0.1:3000 看起来可达或把请求打到无关上游，不能用来判断本机 dev server。
///
/// Code Logic（这个函数做什么）:
///     `no_proxy()` + 300ms timeout；构建失败返回 None，发现路径降级为不探测。
fn loopback_probe_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(LOOPBACK_PROBE_TIMEOUT)
        .build()
        .ok()
}

/// 探测本机 loopback URL 是否能建立 HTTP 响应。
///
/// Business Logic（为什么需要这个函数）:
///     Remembered / PortProbe 必须用同一套直连探测，避免系统代理造成假可达。
///
/// Code Logic（这个函数做什么）:
///     对 url 发 GET，任意成功建立的响应都算可达。
async fn probe_loopback_url(client: &reqwest::Client, url: &str) -> bool {
    client.get(url).send().await.is_ok()
}

/// 把已保存的上次预览地址转成 Remembered 候选。
///
/// Business Logic（为什么需要这个函数）:
///     上次预览可能是误开的 3000；默认探测端口不能无条件标可达，否则会再次被自动打开。
///
/// Code Logic（这个函数做什么）:
///     默认探测端口先标不可达，交给后续 PortProbe 合并真实可达性；自定义端口用 no_proxy 客户端实测。
async fn remembered_browser_targets(
    urls: impl IntoIterator<Item = String>,
    client: Option<&reqwest::Client>,
) -> Vec<WorkbenchBrowserTarget> {
    let mut targets = Vec::new();
    for url in urls {
        let Ok(normalized) = normalize_browser_target_url(&url) else {
            continue;
        };
        let reachable = if is_default_probe_url(&normalized) {
            false
        } else if let Some(client) = client {
            probe_loopback_url(client, &normalized).await
        } else {
            false
        };
        if let Ok(target) = browser_target_from_url(
            &normalized,
            WorkbenchBrowserTargetSource::Remembered,
            reachable,
        ) {
            targets.push(target);
        }
    }
    targets
}

/// 构造浏览器候选目标。
///
/// Business Logic（为什么需要这个函数）:
///     不同发现来源应生成一致的目标结构，便于去重、排序和前端展示。
///
/// Code Logic（这个函数做什么）:
///     规范化 URL，生成 `{source}:{normalized_url}` id，展示 URL 使用 localhost，label 只保存稳定来源 key。
fn browser_target_from_url(
    raw: &str,
    source: WorkbenchBrowserTargetSource,
    reachable: bool,
) -> Result<WorkbenchBrowserTarget, AppError> {
    let normalized_url = normalize_browser_target_url(raw)?;
    Ok(WorkbenchBrowserTarget {
        id: format!("{source:?}:{normalized_url}"),
        display_url: display_url_for_normalized(&normalized_url),
        url: normalized_url,
        label: source_label_key(&source).to_string(),
        source,
        reachable,
    })
}

/// 对浏览器候选做去重和排序。
///
/// Business Logic（为什么需要这个函数）:
///     用户应先看到最可信的候选：上次使用、终端输出、项目配置、端口探测，最后才是手动来源。
///
/// Code Logic（这个函数做什么）:
///     按规范化 URL 去重，保留优先级更高的来源；同 URL 的 PortProbe 可达性合并到更高优先级项，再按来源优先级和 URL 稳定排序。
fn rank_browser_targets(targets: Vec<WorkbenchBrowserTarget>) -> Vec<WorkbenchBrowserTarget> {
    let mut deduped: Vec<WorkbenchBrowserTarget> = Vec::new();
    for mut target in targets {
        let normalized = normalize_browser_target_url(&target.url)
            .unwrap_or_else(|_| target.url.trim().to_string());
        target.url = normalized.clone();
        target.display_url = display_url_for_normalized(&normalized);
        target.id = format!("{:?}:{normalized}", target.source);
        if let Some(existing) = deduped.iter_mut().find(|item| item.url == normalized) {
            let merged_reachable = existing.reachable || target.reachable;
            let existing_priority = source_priority(&existing.source);
            let target_priority = source_priority(&target.source);
            if target_priority < existing_priority
                || (target_priority == existing_priority && target.reachable && !existing.reachable)
            {
                *existing = target;
            }
            existing.reachable = merged_reachable;
        } else {
            deduped.push(target);
        }
    }
    deduped.sort_by(|left, right| {
        source_priority(&left.source)
            .cmp(&source_priority(&right.source))
            .then_with(|| left.url.cmp(&right.url))
    });
    deduped
}

/// 判断候选是否允许进入 Browser tab 后自动打开。
///
/// Business Logic（为什么需要这个函数）:
///     端口探测会命中本机任意常见 dev 端口，不能当作当前项目默认网页。
///
/// Code Logic（这个函数做什么）:
///     仅 Remembered / TerminalOutput / ProjectConfig 返回 true。
fn is_auto_open_browser_source(source: &WorkbenchBrowserTargetSource) -> bool {
    matches!(
        source,
        WorkbenchBrowserTargetSource::Remembered
            | WorkbenchBrowserTargetSource::TerminalOutput
            | WorkbenchBrowserTargetSource::ProjectConfig
    )
}

/// 判断 URL 是否落在默认端口探测列表。
///
/// Business Logic（为什么需要这个函数）:
///     上次误开的 3000/5173/8080 会以 Remembered 落库，必须与用户自定义端口区分。
///
/// Code Logic（这个函数做什么）:
///     解析端口并对照 DEFAULT_PROBE_PORTS。
fn is_default_probe_url(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.port())
        .is_some_and(|port| DEFAULT_PROBE_PORTS.contains(&port))
}

/// 收集可印证自动打开的 URL（终端输出或项目配置）。
///
/// Business Logic（为什么需要这个函数）:
///     rank 会把同 URL 的 ProjectConfig/Terminal 合并进 Remembered，选默认页前必须先记下印证来源。
///
/// Code Logic（这个函数做什么）:
///     归一化 TerminalOutput / ProjectConfig 的 URL 放进 HashSet。
fn corroborated_browser_urls(targets: &[WorkbenchBrowserTarget]) -> HashSet<String> {
    targets
        .iter()
        .filter(|target| {
            matches!(
                target.source,
                WorkbenchBrowserTargetSource::TerminalOutput
                    | WorkbenchBrowserTargetSource::ProjectConfig
            )
        })
        .map(|target| {
            normalize_browser_target_url(&target.url)
                .unwrap_or_else(|_| target.url.trim().to_string())
        })
        .collect()
}

/// 判断已排序候选能否作为默认自动打开目标。
///
/// Business Logic（为什么需要这个函数）:
///     Remembered 默认探测端口只在终端或 package.json 也指向同一 URL 时才代表当前项目。
///
/// Code Logic（这个函数做什么）:
///     要求 reachable 且来源允许自动打开；Remembered 默认探测端口还须出现在印证集合中。
fn can_auto_open_browser_target(
    target: &WorkbenchBrowserTarget,
    corroborated_urls: &HashSet<String>,
) -> bool {
    if !target.reachable || !is_auto_open_browser_source(&target.source) {
        return false;
    }
    if target.source != WorkbenchBrowserTargetSource::Remembered {
        return true;
    }
    !is_default_probe_url(&target.url) || corroborated_urls.contains(&target.url)
}

/// 选出默认自动打开的候选 id。
///
/// Business Logic（为什么需要这个函数）:
///     前端 discover 后会按 selectedTargetId 建 preview；必须排除 PortProbe / Manual，以及未印证的默认探测端口 Remembered。
///
/// Code Logic（这个函数做什么）:
///     在已排序列表中取第一个通过 can_auto_open_browser_target 的项。
fn select_auto_open_browser_target_id(
    targets: &[WorkbenchBrowserTarget],
    corroborated_urls: &HashSet<String>,
) -> Option<String> {
    targets
        .iter()
        .find(|target| can_auto_open_browser_target(target, corroborated_urls))
        .map(|target| target.id.clone())
}

/// 测试入口：对候选目标做确定性排序。
///
/// Business Logic（为什么需要这个函数）:
///     自动发现会混合多个来源，测试需要隔离排序逻辑以避免依赖真实端口探测。
///
/// Code Logic（这个函数做什么）:
///     调用生产排序函数，供单元测试直接验证来源优先级。
#[cfg(test)]
fn rank_browser_targets_for_test(
    targets: Vec<WorkbenchBrowserTarget>,
) -> Vec<WorkbenchBrowserTarget> {
    rank_browser_targets(targets)
}

/// 提取显式终端 URL 候选。
///
/// Business Logic（为什么需要这个函数）:
///     终端输出里的 `localhost` 更符合用户看到的原文，应保留为发现结果展示，同时仍需安全校验。
///
/// Code Logic（这个函数做什么）:
///     先调用 normalize 校验安全边界；localhost/127 显式 URL 返回解析后的原 host URL，其余返回归一化 URL。
fn terminal_url_candidate(raw: &str) -> Result<String, AppError> {
    let normalized = normalize_browser_target_url(raw)?;
    let parsed = Url::parse(raw).map_err(|_| AppError::generic("预览地址格式无效"))?;
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    if matches!(host.as_str(), "localhost" | "127.0.0.1") {
        Ok(parsed.to_string())
    } else {
        Ok(normalized)
    }
}

/// 判断 host:port 匹配是否已经属于显式 URL。
///
/// Business Logic（为什么需要这个函数）:
///     同一终端 URL 不应同时作为显式 URL 和 host:port 片段重复出现。
///
/// Code Logic（这个函数做什么）:
///     检查匹配起点前三个 ASCII 字节是否为 `://`，是则说明它已经被 dev_url_re 覆盖。
fn is_part_of_explicit_url(text: &str, start: usize) -> bool {
    start >= 3 && text.as_bytes().get(start - 3..start) == Some(b"://")
}

/// 去重追加字符串。
///
/// Business Logic（为什么需要这个函数）:
///     自动发现结果应保持首次出现顺序且避免重复候选干扰用户选择。
///
/// Code Logic（这个函数做什么）:
///     Vec 中不存在相同字符串时才 push。
fn push_unique(items: &mut Vec<String>, value: String) {
    if !items.contains(&value) {
        items.push(value);
    }
}

/// 为规范化 URL 生成用户展示 URL。
///
/// Business Logic（为什么需要这个函数）:
///     前端展示 `localhost` 比 `127.0.0.1` 更贴近用户启动 dev server 时看到的地址。
///
/// Code Logic（这个函数做什么）:
///     解析 URL 后把 host 改为 localhost；解析失败时回退原字符串。
fn display_url_for_normalized(normalized_url: &str) -> String {
    let Ok(mut url) = Url::parse(normalized_url) else {
        return normalized_url.to_string();
    };
    if url.host_str() == Some("127.0.0.1") {
        let _ = url.set_host(Some("localhost"));
    }
    url.to_string()
}

/// 获取目标来源稳定 key。
///
/// Business Logic（为什么需要这个函数）:
///     前端候选列表需要解释每个 URL 为什么出现，但用户可见文案必须由前端 i18n 负责，不能由后端返回中文。
///
/// Code Logic（这个函数做什么）:
///     将枚举来源映射为稳定 key，供 DTO label 字段兼容旧结构；前端实际展示应优先使用 target.source。
fn source_label_key(source: &WorkbenchBrowserTargetSource) -> &'static str {
    match source {
        WorkbenchBrowserTargetSource::Remembered => "remembered",
        WorkbenchBrowserTargetSource::TerminalOutput => "terminalOutput",
        WorkbenchBrowserTargetSource::ProjectConfig => "projectConfig",
        WorkbenchBrowserTargetSource::PortProbe => "portProbe",
        WorkbenchBrowserTargetSource::Manual => "manual",
    }
}

/// 获取来源排序优先级。
///
/// Business Logic（为什么需要这个函数）:
///     自动发现需要固定可信度顺序，避免候选在多次刷新之间跳动。
///
/// Code Logic（这个函数做什么）:
///     数字越小优先级越高，排序时再用 URL 字典序作稳定 tie-break。
fn source_priority(source: &WorkbenchBrowserTargetSource) -> u8 {
    match source {
        WorkbenchBrowserTargetSource::Remembered => 0,
        WorkbenchBrowserTargetSource::TerminalOutput => 1,
        WorkbenchBrowserTargetSource::ProjectConfig => 2,
        WorkbenchBrowserTargetSource::PortProbe => 3,
        WorkbenchBrowserTargetSource::Manual => 4,
    }
}

/// 判断 package.json 是否包含任一框架关键词。
///
/// Business Logic（为什么需要这个函数）:
///     不同前端框架对应不同默认端口，自动发现需要从 scripts 和 dependencies 中推断。
///
/// Code Logic（这个函数做什么）:
///     检查 scripts 字符串和 dependency key，命中任一小写 token 即返回 true。
fn package_has_any(package: &Value, tokens: &[&str]) -> bool {
    let normalized_tokens: Vec<String> = tokens
        .iter()
        .map(|token| token.to_ascii_lowercase())
        .collect();
    package_script_values(package).iter().any(|script| {
        let lower = script.to_ascii_lowercase();
        normalized_tokens.iter().any(|token| lower.contains(token))
    }) || package_dependency_keys(package).iter().any(|key| {
        let lower = key.to_ascii_lowercase();
        normalized_tokens.iter().any(|token| lower.contains(token))
    })
}

/// 提取 package.json scripts 字符串。
///
/// Business Logic（为什么需要这个函数）:
///     很多项目没有直接依赖名但 scripts 会包含 `vite`、`next dev` 等启动命令。
///
/// Code Logic（这个函数做什么）:
///     从 scripts object 中收集所有字符串 value。
fn package_script_values(package: &Value) -> Vec<String> {
    package
        .get("scripts")
        .and_then(Value::as_object)
        .map(|scripts| {
            scripts
                .values()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// 提取 package.json dependency key。
///
/// Business Logic（为什么需要这个函数）:
///     默认端口推断应覆盖 dependencies/devDependencies/peerDependencies/optionalDependencies。
///
/// Code Logic（这个函数做什么）:
///     遍历常见依赖字段，收集所有依赖包名。
fn package_dependency_keys(package: &Value) -> Vec<String> {
    let mut keys = HashSet::new();
    for field in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        if let Some(deps) = package.get(field).and_then(Value::as_object) {
            for key in deps.keys() {
                keys.insert(key.clone());
            }
        }
    }
    keys.into_iter().collect()
}

/// 去重追加端口。
///
/// Business Logic（为什么需要这个函数）:
///     一个项目可能同时命中多个关键词但端口相同，候选列表不应重复。
///
/// Code Logic（这个函数做什么）:
///     Vec 中不存在该端口时才 push。
fn add_unique_port(ports: &mut Vec<u16>, port: u16) {
    if !ports.contains(&port) {
        ports.push(port);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::browser_models::{WorkbenchBrowserTarget, WorkbenchBrowserTargetSource};

    /// Business Logic（为什么需要这个测试）:
    ///     用户启动 dev server 后，终端输出中的本机地址应自动出现在浏览器预览候选中。
    ///
    /// Code Logic（这个测试做什么）:
    ///     输入包含 localhost、局域网地址和 0.0.0.0 片段的输出，断言只保留安全 loopback 候选。
    #[test]
    fn extracts_local_dev_server_urls_from_terminal_output() {
        let output = r#"
          VITE v6.0.0 ready
          Local:   http://localhost:5173/
          Network: http://192.168.1.23:5173/
          ready started server on 0.0.0.0:3000
        "#;

        let urls = extract_dev_server_urls(output);

        assert!(urls.contains(&"http://localhost:5173/".to_string()));
        assert!(urls.contains(&"http://127.0.0.1:3000/".to_string()));
        assert!(!urls.iter().any(|url| url.contains("192.168.1.23")));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户手动输入常见 loopback 地址时，浏览器预览应接受并统一保存成安全目标。
    ///
    /// Code Logic（这个测试做什么）:
    ///     覆盖无 scheme、0.0.0.0 path 和 https localhost 三类允许输入的归一化结果。
    #[test]
    fn normalizes_allowed_loopback_targets() {
        assert_eq!(
            normalize_browser_target_url("localhost:5173").unwrap(),
            "http://127.0.0.1:5173/".to_string(),
        );
        assert_eq!(
            normalize_browser_target_url("http://0.0.0.0:3000/app").unwrap(),
            "http://127.0.0.1:3000/app".to_string(),
        );
        assert_eq!(
            normalize_browser_target_url("https://localhost:3443").unwrap(),
            "https://127.0.0.1:3443/".to_string(),
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Browser preview 不能成为开放代理或本地文件读取入口。
    ///
    /// Code Logic（这个测试做什么）:
    ///     输入公网 URL、file URL 和云元数据地址，断言安全校验拒绝。
    #[test]
    fn rejects_open_proxy_targets() {
        assert!(normalize_browser_target_url("https://example.com").is_err());
        assert!(normalize_browser_target_url("file:///etc/passwd").is_err());
        assert!(normalize_browser_target_url("http://169.254.169.254/latest").is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     后端 DTO 不应携带中文来源标签，否则英文界面会直接混入中文。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 terminalOutput 候选，断言 label 字段是稳定 key 而不是用户可见文案。
    #[test]
    fn browser_target_label_is_stable_key_not_localized_text() {
        let target = browser_target_from_url(
            "http://127.0.0.1:5173/",
            WorkbenchBrowserTargetSource::TerminalOutput,
            true,
        )
        .unwrap();

        assert_eq!(target.label, "terminalOutput");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     主项目浏览器预览不应扫描同一项目下 feature worktree 的终端输出，避免误选别的 dev server。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 project-level 发现范围，断言旧 project-level session 和主 worktree session 命中，feature worktree session 不命中。
    #[test]
    fn project_level_browser_scope_excludes_feature_worktree_sessions() {
        let main_worktree_ids = std::collections::HashSet::from(["project-1:main".to_string()]);

        assert!(session_matches_browser_worktree_scope(
            None,
            None,
            false,
            &main_worktree_ids,
        ));
        assert!(session_matches_browser_worktree_scope(
            Some("project-1:main"),
            None,
            false,
            &main_worktree_ids,
        ));
        assert!(!session_matches_browser_worktree_scope(
            Some("feature-worktree"),
            None,
            false,
            &main_worktree_ids,
        ));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     多来源候选同时存在时，用户应先看到可信度最高的地址。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 remembered/terminal/config/probe 四类候选，断言排序按来源优先级稳定输出。
    #[test]
    fn ranks_remembered_then_terminal_then_config_then_probe() {
        let ranked = rank_browser_targets_for_test(vec![
            target_for_test(
                "http://127.0.0.1:8080/",
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            ),
            target_for_test(
                "http://127.0.0.1:5173/",
                WorkbenchBrowserTargetSource::TerminalOutput,
                true,
            ),
            target_for_test(
                "http://127.0.0.1:3000/",
                WorkbenchBrowserTargetSource::Remembered,
                true,
            ),
            target_for_test(
                "http://127.0.0.1:4321/",
                WorkbenchBrowserTargetSource::ProjectConfig,
                true,
            ),
        ]);

        assert_eq!(ranked[0].url, "http://127.0.0.1:3000/");
        assert_eq!(ranked[1].url, "http://127.0.0.1:5173/");
        assert_eq!(ranked[2].url, "http://127.0.0.1:4321/");
        assert_eq!(ranked[3].url, "http://127.0.0.1:8080/");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机常见端口上的无关 HTTP 服务不能在进入网页浏览时被默认打开。
    ///
    /// Code Logic（这个测试做什么）:
    ///     只有 PortProbe 候选时断言 selected_target_id 为空。
    #[test]
    fn does_not_auto_open_port_probe_only_targets() {
        let ranked = rank_browser_targets_for_test(vec![
            target_for_test(
                "http://127.0.0.1:3000/",
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            ),
            target_for_test(
                "http://127.0.0.1:5173/",
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            ),
            target_for_test(
                "http://127.0.0.1:8080/",
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            ),
        ]);

        assert_eq!(select_from(ranked), None);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     终端里已经打出 dev server 地址时，应优先自动打开该地址而不是探测到的其它端口。
    ///
    /// Code Logic（这个测试做什么）:
    ///     同时存在 TerminalOutput 与 PortProbe，断言默认选中终端地址。
    #[test]
    fn auto_opens_terminal_over_port_probe() {
        let selected = select_from(vec![
            target_for_test(
                "http://127.0.0.1:8080/",
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            ),
            target_for_test(
                "http://127.0.0.1:5173/",
                WorkbenchBrowserTargetSource::TerminalOutput,
                true,
            ),
        ])
        .expect("terminal should auto-open");
        assert!(selected.contains("5173"));
        assert!(selected.contains("TerminalOutput"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     package.json 推断的端口只有真正可达时才应自动打开，并保留 ProjectConfig 来源。
    ///
    /// Code Logic（这个测试做什么）:
    ///     同 URL 的不可达 ProjectConfig 与可达 PortProbe 去重后，来源仍是 ProjectConfig 且 reachable，并被选中。
    #[test]
    fn merges_port_probe_reachability_onto_project_config() {
        let ranked = rank_browser_targets_for_test(vec![
            target_for_test(
                "http://127.0.0.1:5173/",
                WorkbenchBrowserTargetSource::ProjectConfig,
                false,
            ),
            target_for_test(
                "http://127.0.0.1:5173/",
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            ),
            target_for_test(
                "http://127.0.0.1:3000/",
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            ),
        ]);

        let config = ranked
            .iter()
            .find(|target| target.url.contains(":5173"))
            .expect("config candidate");
        assert_eq!(config.source, WorkbenchBrowserTargetSource::ProjectConfig);
        assert!(config.reachable);
        let selected = select_from(vec![
            target_for_test(
                "http://127.0.0.1:5173/",
                WorkbenchBrowserTargetSource::ProjectConfig,
                false,
            ),
            target_for_test(
                "http://127.0.0.1:5173/",
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            ),
            target_for_test(
                "http://127.0.0.1:3000/",
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            ),
        ])
        .expect("config should auto-open");
        assert!(selected.contains("5173"));
        assert!(selected.contains("ProjectConfig"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户上次打开过的自定义端口应作为默认预览，即使本机还有其它探测端口。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Remembered :3456 与 PortProbe 并存时断言默认选中 Remembered。
    #[test]
    fn auto_opens_remembered_custom_port() {
        let selected = select_from(vec![
            target_for_test(
                "http://127.0.0.1:3456/",
                WorkbenchBrowserTargetSource::Remembered,
                true,
            ),
            target_for_test(
                "http://127.0.0.1:5173/",
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            ),
        ])
        .expect("remembered custom port should auto-open");
        assert!(selected.contains("3456"));
        assert!(selected.contains("Remembered"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     上次误开的 3000 会以 Remembered 落库，不能在没有终端/配置印证时继续默认打开。
    ///
    /// Code Logic（这个测试做什么）:
    ///     可达 Remembered :3000 仅伴随 PortProbe 时断言不自动打开。
    #[test]
    fn does_not_auto_open_remembered_default_probe_port_without_corroboration() {
        assert_eq!(
            select_from(vec![
                target_for_test(
                    "http://127.0.0.1:3000/",
                    WorkbenchBrowserTargetSource::Remembered,
                    true,
                ),
                target_for_test(
                    "http://127.0.0.1:5173/",
                    WorkbenchBrowserTargetSource::PortProbe,
                    true,
                ),
            ]),
            None
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Next/Remix 等项目的 3000 若同时出现在 package.json 推断中，仍应自动打开。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Remembered :3000 与同 URL ProjectConfig 并存且可达时断言选中 Remembered。
    #[test]
    fn auto_opens_remembered_default_probe_port_when_project_config_corroborates() {
        let selected = select_from(vec![
            target_for_test(
                "http://127.0.0.1:3000/",
                WorkbenchBrowserTargetSource::Remembered,
                true,
            ),
            target_for_test(
                "http://127.0.0.1:3000/",
                WorkbenchBrowserTargetSource::ProjectConfig,
                false,
            ),
            target_for_test(
                "http://127.0.0.1:3000/",
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            ),
        ])
        .expect("corroborated remembered 3000 should auto-open");
        assert!(selected.contains("3000"));
        assert!(selected.contains("Remembered"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     终端已经打出 :3000 时，Remembered 同地址应继续作为默认预览。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Remembered 与 TerminalOutput 同为 :3000 时断言自动打开。
    #[test]
    fn auto_opens_remembered_default_probe_port_when_terminal_corroborates() {
        let selected = select_from(vec![
            target_for_test(
                "http://127.0.0.1:3000/",
                WorkbenchBrowserTargetSource::Remembered,
                true,
            ),
            target_for_test(
                "http://127.0.0.1:3000/",
                WorkbenchBrowserTargetSource::TerminalOutput,
                true,
            ),
        ])
        .expect("terminal-corroborated remembered 3000 should auto-open");
        assert!(selected.contains("3000"));
        assert!(selected.contains("Remembered"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     默认探测端口判定必须覆盖 3000/5173/8080，且不能误伤自定义端口。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对典型 URL 断言 is_default_probe_url。
    #[test]
    fn default_probe_url_covers_common_dev_ports_only() {
        assert!(is_default_probe_url("http://127.0.0.1:3000/"));
        assert!(is_default_probe_url("http://127.0.0.1:5173/"));
        assert!(is_default_probe_url("http://127.0.0.1:8080/"));
        assert!(!is_default_probe_url("http://127.0.0.1:3456/"));
    }

    /// 按生产路径选出默认自动打开项。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单测必须覆盖 rank 合并后丢失 ProjectConfig/Terminal 来源的情况。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先从原始列表收集印证 URL，再 rank，再调用 select_auto_open_browser_target_id。
    fn select_from(targets: Vec<WorkbenchBrowserTarget>) -> Option<String> {
        let corroborated = corroborated_browser_urls(&targets);
        let ranked = rank_browser_targets(targets);
        select_auto_open_browser_target_id(&ranked, &corroborated)
    }

    /// 构造测试用浏览器目标。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     排序测试只关心 URL、来源和 reachable，使用 helper 可让断言更聚焦。
    ///
    /// Code Logic（这个函数做什么）:
    ///     根据入参生成 WorkbenchBrowserTarget，其余展示字段使用稳定占位值。
    fn target_for_test(
        url: &str,
        source: WorkbenchBrowserTargetSource,
        reachable: bool,
    ) -> WorkbenchBrowserTarget {
        WorkbenchBrowserTarget {
            id: format!("{source:?}:{url}"),
            url: url.to_string(),
            display_url: url.to_string(),
            source,
            label: "test".to_string(),
            reachable,
        }
    }
}
