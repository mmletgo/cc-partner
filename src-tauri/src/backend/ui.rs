//! backend/ui.rs — 后端运行时 UI 适配层。
//!
//! Business Logic（为什么需要这个模块）:
//!     GUI 模式和独立 headless 后端都需要复用同一套后端运行时；运行时不能直接依赖 Tauri AppHandle，
//!     因此需要一个 UI 适配边界来统一事件发送与移动端静态资源读取。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `BackendUi` trait，以及 Tauri GUI 和 headless 两种适配器。

use crate::backend::control_client::BackendControlClient;
use crate::backend::event_bus::{GapResyncOutcome, GuiEventRelayState, RelayClientAction};
use crate::error::AppError;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

/// GUI 收到 owner event Gap 后的 Tauri 事件名（触发前端 runtime snapshot 刷新）。
pub const BACKEND_RUNTIME_GAP_EVENT: &str = "backend:runtime-gap";
/// Gap resync 后终端 buffer 全量重置事件（sessionId + buffer）。
pub const WORKBENCH_TERMINAL_RESYNC_EVENT: &str = "workbench:terminal-resync";

/// 后端 UI 静态资源载荷。
///
/// Business Logic（为什么需要这个结构）:
///     `/mobile` 静态入口既可能来自 Tauri 嵌入资源，也可能来自 headless 的 dist 目录；HTTP 层需要统一读取字节、
///     MIME 和可选 CSP，而不关心资源来源。
///
/// Code Logic（这个结构做什么）:
///     保存资源 bytes、MIME 字符串和可选 Content-Security-Policy header；Tauri adapter 会从 `tauri::Asset`
///     转换，headless adapter 会从文件系统读取并按扩展名推导 MIME。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendAsset {
    /// 静态资源字节内容。
    pub bytes: Vec<u8>,
    /// 静态资源 MIME 类型。
    pub mime_type: String,
    /// 可选 CSP header；headless 文件系统资源通常为空。
    pub csp_header: Option<String>,
}

impl BackendAsset {
    /// 构造后端 UI 静态资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     adapter 需要用统一入口创建资源载荷，避免 HTTP 层绑定 Tauri 或文件系统资源类型。
    ///
    /// Code Logic（这个函数做什么）:
    ///     接收资源 bytes、MIME 与可选 CSP，返回 `BackendAsset`。
    pub fn new(bytes: Vec<u8>, mime_type: String, csp_header: Option<String>) -> Self {
        Self {
            bytes,
            mime_type,
            csp_header,
        }
    }

    /// 从 Tauri asset 转换为后端资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 模式仍由 Tauri asset resolver 提供生产包内资源，但运行时其它层不应直接依赖 `tauri::Asset`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     移动 `tauri::Asset` 的 bytes、mime_type 和 csp_header 字段，构造通用 `BackendAsset`。
    pub fn from_tauri_asset(asset: tauri::Asset) -> Self {
        Self::new(asset.bytes, asset.mime_type, asset.csp_header)
    }

    /// 读取资源 MIME 类型。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     HTTP 响应需要设置 content-type，调用方应通过方法读取而不是假设字段语义。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 MIME 字符串切片。
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// 读取可选 CSP header。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Tauri 嵌入 HTML 资源可能携带 CSP，HTTP 层需要原样转发给手机浏览器。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将内部 `Option<String>` 转为 `Option<&str>`，避免调用方 clone。
    pub fn csp_header(&self) -> Option<&str> {
        self.csp_header.as_deref()
    }
}

/// 运行 GUI 进程对本机 sidecar 的事件 relay 循环。
///
/// Business Logic（为什么需要这个函数）:
///     sidecar 是唯一事件源；GUI 必须用 afterSequence 消费 owner 事件，Gap 时先 terminal/runtime
///     真实恢复再 attach latest，禁止 silent loss。
///
/// Code Logic（这个函数做什么）:
///     循环：from_control_file → events_catch_up(after cursor) → GuiEventRelayState 处理；
///     Deliver 转发原始 event；RequestResync → perform_gap_resync（sessions.list/replay +
///     发 `backend:runtime-gap`）→ attach_at(latest)；cancel 或持续短暂退避。
pub async fn run_gui_owner_event_relay(ui: Arc<dyn BackendUi>, cancel: CancellationToken) {
    let mut relay_state = GuiEventRelayState::default();
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let client = match BackendControlClient::from_control_file() {
            Ok(client) => client,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        let after = relay_state.cursor();
        match client.events_catch_up(after.as_ref()).await {
            Ok(batch) => {
                for message in batch.messages {
                    match relay_state.on_message(message) {
                        RelayClientAction::Deliver { event, payload } => {
                            ui.emit(&event, payload);
                        }
                        RelayClientAction::DropDuplicate => {}
                        RelayClientAction::RequestResync {
                            owner_instance_id,
                            oldest_available,
                            latest,
                        } => {
                            let outcome = resync_after_gap(
                                &client,
                                ui.as_ref(),
                                &owner_instance_id,
                                oldest_available,
                                latest,
                            )
                            .await;
                            tracing::info!(
                                terminal_replay_count = outcome.terminal_replay_count,
                                runtime_snapshot_refresh_count =
                                    outcome.runtime_snapshot_refresh_count,
                                "GUI owner event relay gap resync completed"
                            );
                            // 真实 resync 完成后再 attach latest，避免后续 DropDuplicate 掩盖丢更新。
                            relay_state.attach_at(batch.latest.clone());
                        }
                    }
                }
                if relay_state.cursor().is_none() && batch.latest.sequence > 0 {
                    relay_state.attach_at(batch.latest);
                }
            }
            Err(error) => {
                tracing::debug!("GUI owner event relay catch-up 失败: {error}");
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}

/// Gap 后执行 terminal replay + runtime snapshot 恢复（可观测）。
///
/// Business Logic（为什么需要这个函数）:
///     RequestResync 不能只发空事件；必须调用既有 sessions.list/replay 与 runtime 刷新路径。
///
/// Code Logic（这个函数做什么）:
///     先 `resync_terminals_via_control`，再 emit `backend:runtime-gap`；
///     计数汇总为 `GapResyncOutcome`（与 `perform_gap_resync` 语义一致，避免引用生命周期问题）。
pub async fn resync_after_gap(
    client: &BackendControlClient,
    ui: &dyn BackendUi,
    owner_instance_id: &str,
    oldest_available: u64,
    latest: u64,
) -> GapResyncOutcome {
    // 顺序恢复：terminal 先于 runtime 通知，保证前端 snapshot 刷新时 buffer 已开始重置。
    let terminal_replay_count = match resync_terminals_via_control(client, ui).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("gap resync: terminal replay 失败: {e}");
            0
        }
    };
    ui.emit(
        BACKEND_RUNTIME_GAP_EVENT,
        json!({
            "ownerInstanceId": owner_instance_id,
            "oldestAvailable": oldest_available,
            "latest": latest,
            "resyncTerminal": true,
            "resyncRuntime": true,
        }),
    );
    GapResyncOutcome {
        terminal_replay_count,
        runtime_snapshot_refresh_count: 1,
    }
}

/// 经 control workbench 列出 session 并 replay，向前端发出 terminal-resync。
///
/// Business Logic（为什么需要这个函数）:
///     Gap 后 GUI 本地 terminal buffer 可能缺中间输出；必须从 owner registry 拉 replay buffer。
///
/// Code Logic（这个函数做什么）:
///     `sessions.list` → 每个 id `sessions.replay` → emit `workbench:terminal-resync`；
///     单 session 失败跳过，返回成功次数。
async fn resync_terminals_via_control(
    client: &BackendControlClient,
    ui: &dyn BackendUi,
) -> Result<u64, AppError> {
    let sessions: Vec<serde_json::Value> = client
        .workbench_op("sessions.list", json!({}))
        .await
        .unwrap_or_default();
    let mut count = 0u64;
    for session in sessions {
        let Some(session_id) = session
            .get("id")
            .or_else(|| session.get("sessionId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        else {
            continue;
        };
        match client
            .workbench_op::<serde_json::Value>(
                "sessions.replay",
                json!({ "sessionId": session_id }),
            )
            .await
        {
            Ok(replay) => {
                ui.emit(WORKBENCH_TERMINAL_RESYNC_EVENT, replay);
                count = count.saturating_add(1);
            }
            Err(e) => {
                tracing::debug!("gap resync: session replay 失败: {e}");
            }
        }
    }
    Ok(count)
}

/// 后端运行时访问 UI 能力的抽象边界。
///
/// Business Logic（为什么需要这个 trait）:
///     GUI 和 headless 后端共享业务运行时，但前者可以向 Tauri 前端发事件和读嵌入资源，后者只能无界面运行并读本地 dist。
///
/// Code Logic（这个 trait 做什么）:
///     定义线程安全 trait object，暴露事件发送与静态资源读取两个能力；payload 统一用 `serde_json::Value`。
pub trait BackendUi: Send + Sync {
    /// 发送 UI 事件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     后端任务会报告终端输出、传输进度等事件；GUI 可接收，headless 模式则应安全忽略。
    ///
    /// Code Logic（这个函数做什么）:
    ///     接收事件名和 JSON payload，具体 adapter 决定 emit 或 no-op。
    fn emit(&self, event: &str, payload: Value);

    /// 读取移动端静态资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `/mobile` HTTP fallback 需要从当前运行模式对应的 UI 资源来源读取构建产物。
    ///
    /// Code Logic（这个函数做什么）:
    ///     接收规范化后的 asset key，命中时返回 `BackendAsset`，缺失或非法时返回 None。
    fn asset(&self, asset_key: &str) -> Option<BackendAsset>;
}

/// Tauri GUI 模式 UI adapter。
///
/// Business Logic（为什么需要这个结构）:
///     桌面 GUI 模式需要继续使用 Tauri AppHandle 向前端窗口广播事件，并读取生产包内嵌入的 frontendDist 资源。
///
/// Code Logic（这个结构做什么）:
///     封装 `AppHandle`，把 emit 委托给 `Emitter::emit`，把 asset 查询委托给 Tauri asset resolver。
#[derive(Clone)]
pub struct TauriBackendUi {
    app_handle: AppHandle,
}

impl TauriBackendUi {
    /// 创建 Tauri UI adapter。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Tauri setup 阶段拿到 `AppHandle` 后，需要把 GUI 能力注入共享 `AppState`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 `AppHandle` clone，供 trait 方法后续使用。
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }
}

impl BackendUi for TauriBackendUi {
    /// 发送 Tauri UI 事件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 模式需要把后台运行时事件转发给前端窗口，供终端输出、进度状态等界面实时更新。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 `AppHandle::emit` 广播 JSON payload；失败时仅记录 warn，不打断业务流程。
    fn emit(&self, event: &str, payload: Value) {
        if let Err(error) = self.app_handle.emit(event, payload) {
            tracing::warn!("发送 UI 事件 {event} 失败: {error}");
        }
    }

    /// 读取 Tauri 嵌入静态资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 生产包内的移动端资源由 Tauri asset resolver 管理，同时必须避免 resolver 缺省回退到桌面 shell。
    ///
    /// Code Logic（这个函数做什么）:
    ///     遍历 resolver 判断是否存在嵌入资源和 exact key；有嵌入资源但 key 不精确匹配时返回 None，否则读取并转换为 `BackendAsset`。
    fn asset(&self, asset_key: &str) -> Option<BackendAsset> {
        let resolver = self.app_handle.asset_resolver();
        let mut has_embedded_assets = false;
        let mut has_exact_asset = false;

        for (key, _) in resolver.iter() {
            has_embedded_assets = true;
            if key.as_ref().trim_start_matches('/') == asset_key {
                has_exact_asset = true;
                break;
            }
        }

        if has_embedded_assets && !has_exact_asset {
            return None;
        }

        resolver
            .get(asset_key.to_string())
            .map(BackendAsset::from_tauri_asset)
    }
}

/// Headless 后端 UI adapter。
///
/// Business Logic（为什么需要这个结构）:
///     独立后端进程没有 Tauri 窗口，但仍需服务 `/mobile` 页面给手机浏览器；资源来自显式传入的 dist 目录。
///
/// Code Logic（这个结构做什么）:
///     保存 dist 根目录；emit 为 no-op；asset 会先校验相对路径安全性，再读取 dist 下对应文件。
#[derive(Debug, Clone)]
pub struct HeadlessBackendUi {
    dist_dir: PathBuf,
}

impl HeadlessBackendUi {
    /// 创建 headless UI adapter。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     CLI/headless runtime 启动时需要指定 web dist 目录作为手机端静态资源来源。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存调用方传入的 dist 目录路径，读取时再与规范化 asset key 拼接。
    pub fn new(dist_dir: PathBuf) -> Self {
        Self { dist_dir }
    }

    /// 校验并规范化资源 key。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     headless 文件系统读取不能接受绝对路径、父目录、Windows prefix 或反斜杠，否则可能越界读取。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 `Path::components` 只接受 Normal 组件，并额外拒绝空路径、反斜杠、Windows 盘符前缀和非 UTF-8 组件。
    fn normalized_asset_path(asset_key: &str) -> Option<PathBuf> {
        if asset_key.is_empty() || asset_key.contains('\\') {
            return None;
        }

        let requested_path = Path::new(asset_key);
        if requested_path.is_absolute() {
            return None;
        }

        let mut safe_path = PathBuf::new();
        for (index, component) in requested_path.components().enumerate() {
            match component {
                Component::Normal(segment) => {
                    let segment = segment.to_str()?;
                    if segment.is_empty()
                        || segment.contains('\\')
                        || (index == 0 && is_windows_prefix_segment(segment))
                    {
                        return None;
                    }
                    safe_path.push(segment);
                }
                Component::Prefix(_)
                | Component::RootDir
                | Component::CurDir
                | Component::ParentDir => {
                    return None;
                }
            }
        }

        if safe_path.as_os_str().is_empty() {
            return None;
        }

        Some(safe_path)
    }
}

impl BackendUi for HeadlessBackendUi {
    /// 忽略 headless UI 事件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     headless 后端没有本地前端窗口，但业务运行时仍可统一调用事件接口。
    ///
    /// Code Logic（这个函数做什么）:
    ///     不读取参数、不产生副作用，直接返回。
    fn emit(&self, _event: &str, _payload: Value) {}

    /// 读取 headless dist 静态资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     独立后端进程仍要通过局域网 HTTP 为手机浏览器提供 `/mobile` 页面。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先校验 asset key 为安全相对路径，再读取 dist 目录下对应文件并按扩展名填充 MIME。
    fn asset(&self, asset_key: &str) -> Option<BackendAsset> {
        let relative_path = Self::normalized_asset_path(asset_key)?;
        let bytes = std::fs::read(self.dist_dir.join(relative_path)).ok()?;
        Some(BackendAsset::new(
            bytes,
            asset_mime_type(asset_key).to_string(),
            None,
        ))
    }
}

/// 判断路径首段是否是 Windows 盘符前缀。
///
/// Business Logic（为什么需要这个函数）:
///     headless 后端可能在 Unix 上解析来自 HTTP 的资源 key；`C:/...` 在 Unix 会被视作普通相对路径，但语义上是 Windows prefix。
///
/// Code Logic（这个函数做什么）:
///     检查首段是否形如 ASCII 字母 + 冒号，例如 `C:` 或 `z:`。
fn is_windows_prefix_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// 根据 asset key 推导 MIME 类型。
///
/// Business Logic（为什么需要这个函数）:
///     headless 文件系统资源没有 Tauri resolver 提供的 MIME，仍需让手机浏览器按正确类型加载 HTML/JS/CSS 等产物。
///
/// Code Logic（这个函数做什么）:
///     按扩展名返回常见移动端静态资源 MIME；未知扩展名返回 `application/octet-stream`。
fn asset_mime_type(asset_key: &str) -> &'static str {
    match Path::new(asset_key)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" | "map" => "application/json; charset=utf-8",
        "wasm" => "application/wasm",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// 将任意可序列化 payload 转为 UI 事件 JSON 值。
///
/// Business Logic（为什么需要这个函数）:
///     `AppState::emit_event` 需要在进入 trait object 前把业务 DTO 统一转为 JSON，并在失败时记录日志而不打断业务流程。
///
/// Code Logic（这个函数做什么）:
///     调用 `serde_json::to_value`，成功返回 Value，失败返回 serde_json error。
pub fn serialize_event_payload<T>(payload: T) -> Result<Value, serde_json::Error>
where
    T: Serialize,
{
    serde_json::to_value(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 headless 静态资源拒绝父目录路径。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     headless 后端会从本地 dist 目录读取手机端静态资源，必须拒绝 `..` 越界，避免暴露项目外文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造不存在的 dist 目录适配器，请求 `../mobile.html`，断言不会返回资源。
    #[test]
    fn headless_asset_rejects_parent_paths() {
        let ui = HeadlessBackendUi::new(std::path::PathBuf::from("/tmp/missing"));
        assert!(ui.asset("../mobile.html").is_none());
    }

    /// 验证 headless 静态资源拒绝 Windows prefix 路径。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     Unix 文件系统可创建 `C:mobile.html` 这类文件名，但该输入在跨平台语义上是 Windows drive-relative prefix，
    ///     不应被 headless 资源读取接受。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在临时 dist 目录写入 `C:mobile.html`，断言 headless adapter 仍拒绝该 asset key。
    #[test]
    fn headless_asset_rejects_windows_prefix_paths() {
        let dist = tempfile::tempdir().expect("创建临时 dist 目录");
        std::fs::write(dist.path().join("C:mobile.html"), b"secret").expect("写入测试资源");
        let ui = HeadlessBackendUi::new(dist.path().to_path_buf());

        assert!(ui.asset("C:mobile.html").is_none());
    }

    /// 验证 headless 事件发送是 no-op。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     CLI/headless 模式没有前端窗口可接收事件，但后台任务仍会调用事件接口；这里不能 panic。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用 headless adapter 的 emit 方法并确认函数正常返回。
    #[test]
    fn headless_emit_is_noop() {
        let ui = HeadlessBackendUi::new(std::path::PathBuf::from("/tmp/missing"));
        ui.emit("workbench:terminal-output", serde_json::json!({"ok": true}));
    }
}
