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

static DEV_URL_RE: OnceLock<Regex> = OnceLock::new();
static HOST_PORT_RE: OnceLock<Regex> = OnceLock::new();

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
///
/// Code Logic（这个函数做什么）:
///     解析项目/worktree 根目录，合并 remembered/terminal/package/probe 四类候选，去重排序后返回默认可达项。
pub async fn discover_workbench_browser_targets(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
) -> Result<WorkbenchBrowserDiscovery, AppError> {
    let root = resolve_browser_worktree_root(state, &project_id, worktree_id.as_deref()).await?;
    let remembered = state
        .workbench_browser_repo
        .get_target(&project_id, worktree_id.as_deref())
        .await?
        .into_iter()
        .filter_map(|url| {
            browser_target_from_url(&url, WorkbenchBrowserTargetSource::Remembered, true).ok()
        });
    let terminal = terminal_output_targets(state, &project_id, worktree_id.as_deref()).await?;
    let config = project_config_targets(&root).await?;
    let probed = probe_default_port_targets(state, &root).await?;
    let targets = rank_browser_targets(
        remembered
            .chain(terminal)
            .chain(config)
            .chain(probed)
            .collect::<Vec<_>>(),
    );
    let selected_target_id = targets
        .iter()
        .find(|target| target.reachable)
        .map(|target| target.id.clone());
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
///     浏览器预览必须运行在项目所在设备的本机项目/worktree 根上，远端 shortcut 需要由后续 route 代理处理。
///
/// Code Logic（这个函数做什么）:
///     读取 project row；无 worktree_id 返回项目路径，有 worktree_id 时校验归属并返回 worktree 路径。
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
///
/// Code Logic（这个函数做什么）:
///     读取持久化会话 id 与运行期会话 replay buffer，提取 URL 后映射为 TerminalOutput 候选。
async fn terminal_output_targets(
    state: &AppState,
    project_id: &str,
    worktree_id: Option<&str>,
) -> Result<Vec<WorkbenchBrowserTarget>, AppError> {
    let mut session_ids = Vec::new();
    let target_is_main = if let Some(worktree_id) = worktree_id {
        state
            .workbench_worktree_repo
            .get(worktree_id)
            .await?
            .map(|worktree| worktree.is_main)
            .unwrap_or(false)
    } else {
        false
    };
    let rows = state.workbench_session_repo.list(Some(project_id)).await?;
    for row in rows {
        if worktree_id
            .map(|id| {
                row.worktree_id.as_deref() == Some(id)
                    || (target_is_main && row.worktree_id.is_none())
            })
            .unwrap_or(true)
        {
            push_unique(&mut session_ids, row.id);
        }
    }
    for live in state.workbench_sessions.list(Some(project_id)) {
        if worktree_id
            .map(|id| {
                live.worktree_id.as_deref() == Some(id)
                    || (target_is_main && live.worktree_id.is_none())
            })
            .unwrap_or(true)
        {
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
///     对默认端口发起 300ms HTTP GET，排除 cc-partner 自身 HTTP 端口，只返回成功建立响应的端口。
async fn probe_default_port_targets(
    state: &AppState,
    _root: &Path,
) -> Result<Vec<WorkbenchBrowserTarget>, AppError> {
    let actual_http_port = state.actual_http_port.load(Ordering::Relaxed);
    let client = reqwest::Client::new();
    let mut targets = Vec::new();
    for port in [5173_u16, 3000, 4173, 5174, 8080, 8000, 4321, 6006] {
        if actual_http_port != 0 && port == actual_http_port {
            continue;
        }
        let raw = format!("http://127.0.0.1:{port}/");
        let reachable = client
            .get(&raw)
            .timeout(Duration::from_millis(300))
            .send()
            .await
            .is_ok();
        if reachable {
            targets.push(browser_target_from_url(
                &raw,
                WorkbenchBrowserTargetSource::PortProbe,
                true,
            )?);
        }
    }
    Ok(targets)
}

/// 构造浏览器候选目标。
///
/// Business Logic（为什么需要这个函数）:
///     不同发现来源应生成一致的目标结构，便于去重、排序和前端展示。
///
/// Code Logic（这个函数做什么）:
///     规范化 URL，生成 `{source}:{normalized_url}` id，展示 URL 使用 localhost，label 来自来源。
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
        label: source_label(&source).to_string(),
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
///     按规范化 URL 去重，保留优先级更高或同优先级可达的项，再按来源优先级和 URL 稳定排序。
fn rank_browser_targets(targets: Vec<WorkbenchBrowserTarget>) -> Vec<WorkbenchBrowserTarget> {
    let mut deduped: Vec<WorkbenchBrowserTarget> = Vec::new();
    for mut target in targets {
        let normalized = normalize_browser_target_url(&target.url)
            .unwrap_or_else(|_| target.url.trim().to_string());
        target.url = normalized.clone();
        target.display_url = display_url_for_normalized(&normalized);
        target.id = format!("{:?}:{normalized}", target.source);
        if let Some(existing) = deduped.iter_mut().find(|item| item.url == normalized) {
            let existing_priority = source_priority(&existing.source);
            let target_priority = source_priority(&target.source);
            if target_priority < existing_priority
                || (target_priority == existing_priority && target.reachable && !existing.reachable)
            {
                *existing = target;
            }
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

/// 获取目标来源展示标签。
///
/// Business Logic（为什么需要这个函数）:
///     前端候选列表需要解释每个 URL 为什么出现，降低误选成本。
///
/// Code Logic（这个函数做什么）:
///     将枚举来源映射为稳定中文标签。
fn source_label(source: &WorkbenchBrowserTargetSource) -> &'static str {
    match source {
        WorkbenchBrowserTargetSource::Remembered => "上次使用",
        WorkbenchBrowserTargetSource::TerminalOutput => "终端输出",
        WorkbenchBrowserTargetSource::ProjectConfig => "项目配置",
        WorkbenchBrowserTargetSource::PortProbe => "端口探测",
        WorkbenchBrowserTargetSource::Manual => "手动输入",
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

    #[test]
    fn rejects_open_proxy_targets() {
        assert!(normalize_browser_target_url("https://example.com").is_err());
        assert!(normalize_browser_target_url("file:///etc/passwd").is_err());
        assert!(normalize_browser_target_url("http://169.254.169.254/latest").is_err());
    }

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
