//! browser_verification/engine.rs — Engine 缝合点与请求/结果类型
//!
//! Business Logic（为什么需要这个模块）:
//!     运行时服务需要可替换的 engine 实现（测试 FakeEngine / 生产 Chromium），
//!     统一命令执行、观察者事件与取消令牌契约。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `BrowserVerificationEngine` trait、`EngineRunRequest`/`EngineRunResult`、
//!     观察者 trait 与 no-op 实现。

use super::models::{
    BrowserCommandResult, BrowserVerificationCommand, BrowserVerificationEvidence,
};
use crate::error::AppError;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Engine 单次运行请求（target 已由服务层从 live preview 解析）。
///
/// Business Logic（为什么需要这个结构体）:
///     Engine 不得自行接受调用方 target URL；服务层注入已校验 loopback URL。
///
/// Code Logic（这个结构体做什么）:
///     携带 run_id、规范化 target_url、命令列表与可选 profile 目录。
#[derive(Debug, Clone)]
pub struct EngineRunRequest {
    pub run_id: String,
    /// 已由服务层从 preview registry 解析并 revalidate 的 loopback URL。
    pub target_url: String,
    pub commands: Vec<BrowserVerificationCommand>,
    /// 临时 profile 目录（由 runtime 分配）。
    pub profile_dir: std::path::PathBuf,
    /// 托管 chrome-headless-shell 可执行文件路径。
    pub chrome_executable: Option<std::path::PathBuf>,
}

/// Engine 运行结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineRunResult {
    pub command_results: Vec<BrowserCommandResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<BrowserVerificationEvidence>,
    /// 截图原始 PNG（若有），由 artifact store 落盘后不进入长期 DTO。
    #[serde(skip)]
    pub screenshot_pngs: Vec<(String, Vec<u8>)>,
}

/// 验证过程观察者（进度事件）。
///
/// Business Logic（为什么需要这个 trait）:
///     UI 需要 `workbench:browser-verification` 进度；测试可断言事件序列。
///
/// Code Logic（这个 trait 做什么）:
///     接收 run_id 与可序列化进度 payload。
pub trait BrowserVerificationObserver: Send + Sync {
    /// 推送进度事件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     长耗时 smoke 需要中间状态，避免 UI 假死。
    ///
    /// Code Logic（这个函数做什么）:
    ///     实现方可 emit Tauri 事件或 no-op。
    fn on_progress(&self, run_id: &str, payload: serde_json::Value);
}

/// 无操作观察者。
pub struct NoopObserver;

impl BrowserVerificationObserver for NoopObserver {
    /// 忽略进度。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单元测试与 headless 路径不需要 UI 事件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     空实现。
    fn on_progress(&self, _run_id: &str, _payload: serde_json::Value) {}
}

/// 浏览器验证引擎缝合点。
///
/// Business Logic（为什么需要这个 trait）:
///     生产使用 managed Chromium；测试使用 FakeEngine，避免依赖真实 Chrome 二进制。
///
/// Code Logic（这个 trait 做什么）:
///     异步执行一组有界命令，支持取消与观察者。
pub trait BrowserVerificationEngine: Send + Sync {
    /// 执行一次验证运行。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     runtime 在创建 ephemeral profile 后把已校验 target 交给 engine 驱动。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 BoxFuture，产出 `EngineRunResult` 或 `AppError`；取消时尽快返回。
    fn execute<'a>(
        &'a self,
        request: EngineRunRequest,
        observer: Arc<dyn BrowserVerificationObserver>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<EngineRunResult, AppError>>;
}
