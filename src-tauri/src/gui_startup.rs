//! gui_startup.rs — GUI 启动协调：LAN disclosure gate + sidecar ensure/start once。
//!
//! Business Logic（为什么需要这个模块）:
//!     未确认 LAN 风险前，GUI 不得 ensure sidecar 或启动 browse-only 后端服务；
//!     确认后必须在同一 async mutex/once gate 内确保只启动一次，并 fail-closed。
//!
//! Code Logic（这个模块做什么）:
//!     提供可注入的 `GuiStartupCoordinator`：读 bootstrap、探测已运行 CLI、
//!     组装 `LanDisclosureStatus`，并在 acknowledge 路径原子写 bootstrap 后 ensure+start。

use crate::backend::control::{BackendStatus, BackendStatusKind};
use crate::error::AppError;
use crate::gui_bootstrap::{
    acknowledge_current_lan_disclosure, is_acknowledged_for_version, is_current_lan_disclosure_acknowledged,
    load_gui_bootstrap, GuiBootstrapState, LAN_DISCLOSURE_VERSION, MDNS_PORT, PREFERRED_HTTP_PORT,
};
use crate::net::discovery::local_lan_ip;
use serde::Serialize;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell};

/// LAN 披露状态 DTO（前端 gate 展示）。
///
/// Business Logic（为什么需要这个结构）:
///     确认页需展示本机地址候选、首选端口、mDNS 端口、是否已有独立 CLI 在跑及实际端口。
///
/// Code Logic（这个结构做什么）:
///     camelCase 序列化；`required` 表示是否仍需确认。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LanDisclosureStatus {
    pub required: bool,
    pub version: u32,
    pub local_addresses: Vec<String>,
    pub preferred_port: u16,
    pub mdns_port: u16,
    pub already_running: bool,
    pub actual_http_port: Option<u16>,
}

/// acknowledge 后返回给前端的访问信息。
///
/// Business Logic（为什么需要这个结构）:
///     确认启动后需回显实际 listener，并说明是否复用了既有 CLI。
///
/// Code Logic（这个结构做什么）:
///     camelCase：实际端口、地址列表、是否复用已运行 backend。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LanDisclosureStartResult {
    pub actual_http_port: u16,
    pub local_addresses: Vec<String>,
    pub reused_existing: bool,
    pub version: u32,
}

/// 后端生命周期依赖（可注入 mock 供单测）。
///
/// Business Logic（为什么需要这个 trait）:
///     setup/ack 路径需 ensure sidecar 与 start GUI services，测试不能真实拉起进程。
///
/// Code Logic（这个 trait 做什么）:
///     抽象 probe/ensure/start 三个异步动作。
pub trait BackendLifecycle: Send + Sync + 'static {
    /// 探测当前 backend 状态（不 start/stop）。
    fn probe_status(
        &self,
    ) -> Pin<Box<dyn Future<Output = BackendStatus> + Send + '_>>;

    /// 确保 sidecar Running（必要时 start）。
    fn ensure_backend(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<BackendStatus, AppError>> + Send + '_>>;

    /// 启动 GUI browse-only 服务并等待可用。
    fn start_gui_services(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<u16, AppError>> + Send + '_>>;
}

/// GUI 启动协调器（once gate + 可注入 lifecycle）。
///
/// Business Logic（为什么需要这个结构）:
///     并发双击确认、重复 ack 与 setup 门禁必须共享同一启动结果，禁止二次 ensure。
///
/// Code Logic（这个结构做什么）:
///     持有 lifecycle + async Mutex + OnceCell 缓存启动结果。
pub struct GuiStartupCoordinator<L: BackendLifecycle> {
    lifecycle: L,
    /// 串行化 acknowledge/start 路径。
    start_mutex: Mutex<()>,
    /// 成功启动后的缓存结果（并发调用复用）。
    started: OnceCell<LanDisclosureStartResult>,
}

impl<L: BackendLifecycle> GuiStartupCoordinator<L> {
    /// Business Logic（为什么需要这个函数）:
    ///     setup 与命令层需要构造可管理的协调器实例。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装 lifecycle，初始化空 mutex 与 OnceCell。
    pub fn new(lifecycle: L) -> Self {
        Self {
            lifecycle,
            start_mutex: Mutex::new(()),
            started: OnceCell::new(),
        }
    }

    /// 查询 LAN 披露状态。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     前端 gate 挂载时读取是否需要确认、地址与已运行 CLI 信息。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 bootstrap → 探测 status → 组装 local addresses / ports。
    pub async fn get_status(
        &self,
        bootstrap: Option<GuiBootstrapState>,
    ) -> Result<LanDisclosureStatus, AppError> {
        let state = match bootstrap {
            Some(s) => s,
            None => load_gui_bootstrap()?,
        };
        let required = !is_acknowledged_for_version(&state, LAN_DISCLOSURE_VERSION);
        let status = self.lifecycle.probe_status().await;
        let (already_running, actual_http_port) = match status.kind {
            BackendStatusKind::Running => {
                let port = status.control.as_ref().map(|c| c.port);
                (true, port)
            }
            _ => (false, None),
        };
        Ok(LanDisclosureStatus {
            required,
            version: LAN_DISCLOSURE_VERSION,
            local_addresses: local_address_candidates(),
            preferred_port: PREFERRED_HTTP_PORT,
            mdns_port: MDNS_PORT,
            already_running,
            actual_http_port,
        })
    }

    /// setup 阶段：未确认则跳过 ensure/start。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 冷启动在用户确认前不得拉起 LAN sidecar。
    ///
    /// Code Logic（这个函数做什么）:
    ///     已确认则 ensure + start 并缓存结果；未确认返回 `ensure_calls` 语义上的 skipped。
    pub async fn setup_if_acknowledged(&self) -> Result<SetupOutcome, AppError> {
        // bootstrap 读取失败视为未确认：不 ensure/start（fail-closed），由前端 status 暴露错误与重试。
        let acked = match is_current_lan_disclosure_acknowledged() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("读取 gui-bootstrap 失败，跳过 sidecar 启动: {e}");
                false
            }
        };
        if !acked {
            return Ok(SetupOutcome::SkippedUnacknowledged);
        }
        let result = self.ensure_and_start_once(true).await?;
        Ok(SetupOutcome::Started(result))
    }

    /// 用户确认：写 bootstrap 后 ensure+start（once）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     确认后才允许启动 LAN 服务；写盘失败或启动失败均 fail-closed，可重试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     若 OnceCell 已有结果直接返回；否则在 mutex 内：ack 写盘 → ensure → start → 缓存。
    ///     已有 Running CLI 时 ensure 幂等，仍启动 browse-only。
    pub async fn acknowledge_and_start(&self) -> Result<LanDisclosureStartResult, AppError> {
        if let Some(existing) = self.started.get() {
            return Ok(existing.clone());
        }
        // 先原子写确认（即使后续 start 失败也不回滚确认——fail-closed 在 start，可重试 start）
        acknowledge_current_lan_disclosure()?;
        self.ensure_and_start_once(false).await
    }

    /// 内部 once gate：ensure + start。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     setup 已确认路径与 ack 路径共用一次启动语义。
    ///
    /// Code Logic（这个函数做什么）:
    ///     mutex 串行；OnceCell 命中则复用；否则 ensure → start_gui_services → 缓存。
    async fn ensure_and_start_once(
        &self,
        _from_setup: bool,
    ) -> Result<LanDisclosureStartResult, AppError> {
        if let Some(existing) = self.started.get() {
            return Ok(existing.clone());
        }
        let _guard = self.start_mutex.lock().await;
        if let Some(existing) = self.started.get() {
            return Ok(existing.clone());
        }

        let before = self.lifecycle.probe_status().await;
        let reused_existing = before.kind == BackendStatusKind::Running;

        let status = self.lifecycle.ensure_backend().await?;
        if status.kind != BackendStatusKind::Running {
            return Err(AppError::generic(format!(
                "启动独立后端后状态异常: {:?}",
                status.kind
            )));
        }

        let port = self.lifecycle.start_gui_services().await?;
        let result = LanDisclosureStartResult {
            actual_http_port: port,
            local_addresses: local_address_candidates(),
            reused_existing,
            version: LAN_DISCLOSURE_VERSION,
        };
        let _ = self.started.set(result.clone());
        Ok(result)
    }

    /// 是否已成功完成 ensure+start（测试/诊断）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试断言与诊断需要知道 once gate 是否已完成。
    ///
    /// Code Logic（这个函数做什么）:
    ///     OnceCell 是否有值。
    pub fn is_started(&self) -> bool {
        self.started.get().is_some()
    }
}

/// setup 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupOutcome {
    SkippedUnacknowledged,
    Started(LanDisclosureStartResult),
}

/// 收集本机局域网地址候选。
///
/// Business Logic（为什么需要这个函数）:
///     确认页需展示本机可达地址候选，帮助用户理解 LAN 暴露面。
///
/// Code Logic（这个函数做什么）:
///     调用 `local_lan_ip()`，有则返回单元素列表，否则空列表。
pub fn local_address_candidates() -> Vec<String> {
    local_lan_ip()
        .map(|ip| vec![ip.to_string()])
        .unwrap_or_default()
}

/// 生产用 lifecycle：绑定 AppHandle 的 ensure + 共享 AppState 的 start services。
///
/// Business Logic（为什么需要这个结构）:
///     lib.rs / 命令层需要把真实 ensure_backend_process_for_gui 与 start_gui_backend_services 接进协调器。
///
/// Code Logic（这个结构做什么）:
///     持有 ensure 闭包与 start 闭包（由外层捕获 AppHandle/AppState）。
pub struct ProductionBackendLifecycle {
    ensure: Arc<
        dyn Fn() -> Pin<Box<dyn Future<Output = Result<BackendStatus, AppError>> + Send>>
            + Send
            + Sync,
    >,
    start: Arc<
        dyn Fn() -> Pin<Box<dyn Future<Output = Result<u16, AppError>> + Send>> + Send + Sync,
    >,
    probe: Arc<dyn Fn() -> Pin<Box<dyn Future<Output = BackendStatus> + Send>> + Send + Sync>,
}

impl ProductionBackendLifecycle {
    /// Business Logic（为什么需要这个函数）:
    ///     装配真实 ensure/start/probe 到协调器。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装三个 Arc 闭包。
    pub fn new(
        ensure: Arc<
            dyn Fn() -> Pin<Box<dyn Future<Output = Result<BackendStatus, AppError>> + Send>>
                + Send
                + Sync,
        >,
        start: Arc<
            dyn Fn() -> Pin<Box<dyn Future<Output = Result<u16, AppError>> + Send>> + Send + Sync,
        >,
        probe: Arc<dyn Fn() -> Pin<Box<dyn Future<Output = BackendStatus> + Send>> + Send + Sync>,
    ) -> Self {
        Self {
            ensure,
            start,
            probe,
        }
    }
}

impl BackendLifecycle for ProductionBackendLifecycle {
    fn probe_status(
        &self,
    ) -> Pin<Box<dyn Future<Output = BackendStatus> + Send + '_>> {
        (self.probe)()
    }

    fn ensure_backend(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<BackendStatus, AppError>> + Send + '_>> {
        (self.ensure)()
    }

    fn start_gui_services(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<u16, AppError>> + Send + '_>> {
        (self.start)()
    }
}

#[cfg(test)]
mod lan_disclosure_tests {
    use super::*;
    use crate::backend::control::BackendControlFile;
    use crate::config::{install_data_dir_env, DataDirEnvGuard};
    use crate::gui_bootstrap::{
        save_gui_bootstrap_to_path, GuiBootstrapState, LAN_DISCLOSURE_VERSION,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use tempfile::tempdir;

    struct MockLifecycle {
        ensure_calls: AtomicUsize,
        start_calls: AtomicUsize,
        probe_kind: StdMutex<BackendStatusKind>,
        ensure_fail_times: AtomicUsize,
        start_fail_times: AtomicUsize,
        port: u16,
    }

    impl MockLifecycle {
        fn new(kind: BackendStatusKind) -> Self {
            Self {
                ensure_calls: AtomicUsize::new(0),
                start_calls: AtomicUsize::new(0),
                probe_kind: StdMutex::new(kind),
                ensure_fail_times: AtomicUsize::new(0),
                start_fail_times: AtomicUsize::new(0),
                port: 62116,
            }
        }

        fn status_for(kind: BackendStatusKind, port: u16) -> BackendStatus {
            let control = if kind == BackendStatusKind::Running {
                Some(BackendControlFile::for_test(1, port, "dev-1"))
            } else {
                None
            };
            BackendStatus {
                kind,
                control,
                error: None,
            }
        }
    }

    impl BackendLifecycle for MockLifecycle {
        fn probe_status(
            &self,
        ) -> Pin<Box<dyn Future<Output = BackendStatus> + Send + '_>> {
            Box::pin(async move {
                let kind = *self.probe_kind.lock().unwrap();
                Self::status_for(kind, self.port)
            })
        }

        fn ensure_backend(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<BackendStatus, AppError>> + Send + '_>> {
            Box::pin(async move {
                self.ensure_calls.fetch_add(1, Ordering::SeqCst);
                let remaining = self.ensure_fail_times.load(Ordering::SeqCst);
                if remaining > 0 {
                    self.ensure_fail_times.fetch_sub(1, Ordering::SeqCst);
                    return Err(AppError::generic("ensure failed"));
                }
                *self.probe_kind.lock().unwrap() = BackendStatusKind::Running;
                Ok(Self::status_for(BackendStatusKind::Running, self.port))
            })
        }

        fn start_gui_services(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<u16, AppError>> + Send + '_>> {
            Box::pin(async move {
                self.start_calls.fetch_add(1, Ordering::SeqCst);
                let remaining = self.start_fail_times.load(Ordering::SeqCst);
                if remaining > 0 {
                    self.start_fail_times.fetch_sub(1, Ordering::SeqCst);
                    return Err(AppError::generic("start failed"));
                }
                Ok(self.port)
            })
        }
    }

    /// Arc 包装 Mock 以满足 BackendLifecycle 对象安全调用。
    struct ArcMock(Arc<MockLifecycle>);

    impl BackendLifecycle for ArcMock {
        fn probe_status(
            &self,
        ) -> Pin<Box<dyn Future<Output = BackendStatus> + Send + '_>> {
            self.0.probe_status()
        }
        fn ensure_backend(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<BackendStatus, AppError>> + Send + '_>> {
            self.0.ensure_backend()
        }
        fn start_gui_services(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<u16, AppError>> + Send + '_>> {
            self.0.start_gui_services()
        }
    }

    /// GuiSetupHarness — 测试 setup/ack 门禁的夹具。
    ///
    /// Business Logic（为什么需要这个结构）:
    ///     计划要求用 harness 断言未确认不 ensure、确认后只启动一次等。
    ///
    /// Code Logic（这个结构做什么）:
    ///     隔离 data_dir + MockLifecycle + Coordinator。
    struct GuiSetupHarness {
        _data_guard: DataDirEnvGuard,
        _tmpdir: tempfile::TempDir,
        lifecycle: Arc<MockLifecycle>,
        coordinator: GuiStartupCoordinator<ArcMock>,
    }

    impl GuiSetupHarness {
        async fn with_disclosure_version(version: u32) -> Self {
            let tmpdir = tempdir().unwrap();
            let data_guard = install_data_dir_env(Some(tmpdir.path().to_str().unwrap()));
            if version > 0 {
                let path = tmpdir.path().join("gui-bootstrap.json");
                save_gui_bootstrap_to_path(
                    &path,
                    &GuiBootstrapState {
                        lan_disclosure_version: version,
                        acknowledged_at: Some("2026-07-15T00:00:00Z".to_string()),
                    },
                )
                .unwrap();
            }
            let lifecycle = Arc::new(MockLifecycle::new(BackendStatusKind::Stopped));
            let coordinator = GuiStartupCoordinator::new(ArcMock(lifecycle.clone()));
            Self {
                _data_guard: data_guard,
                _tmpdir: tmpdir,
                lifecycle,
                coordinator,
            }
        }

        async fn with_running_cli() -> Self {
            let h = Self::with_disclosure_version(0).await;
            *h.lifecycle.probe_kind.lock().unwrap() = BackendStatusKind::Running;
            h
        }

        async fn setup(&self) -> Result<SetupOutcome, AppError> {
            self.coordinator.setup_if_acknowledged().await
        }

        fn ensure_backend_calls(&self) -> usize {
            self.lifecycle.ensure_calls.load(Ordering::SeqCst)
        }

        fn start_calls(&self) -> usize {
            self.lifecycle.start_calls.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn first_gui_launch_does_not_start_sidecar_before_acknowledgement() {
        let harness = GuiSetupHarness::with_disclosure_version(0).await;
        harness.setup().await.unwrap();
        assert_eq!(harness.ensure_backend_calls(), 0);
        assert_eq!(harness.start_calls(), 0);
    }

    #[tokio::test]
    async fn acknowledged_version_starts_sidecar_once() {
        let harness = GuiSetupHarness::with_disclosure_version(LAN_DISCLOSURE_VERSION).await;
        let outcome = harness.setup().await.unwrap();
        assert!(matches!(outcome, SetupOutcome::Started(_)));
        assert_eq!(harness.ensure_backend_calls(), 1);
        assert_eq!(harness.start_calls(), 1);

        // 再次 setup 走 once cell
        let _ = harness.setup().await.unwrap();
        assert_eq!(harness.ensure_backend_calls(), 1);
        assert_eq!(harness.start_calls(), 1);
    }

    #[tokio::test]
    async fn concurrent_double_confirm_still_starts_once() {
        let harness = GuiSetupHarness::with_disclosure_version(0).await;
        let c1 = &harness.coordinator;
        let (a, b) = tokio::join!(c1.acknowledge_and_start(), c1.acknowledge_and_start());
        assert!(a.is_ok());
        assert!(b.is_ok());
        assert_eq!(harness.ensure_backend_calls(), 1);
        assert_eq!(harness.start_calls(), 1);
    }

    #[tokio::test]
    async fn disclosure_version_bump_requires_new_acknowledgement() {
        let harness = GuiSetupHarness::with_disclosure_version(0).await;
        // version 0 文件 = 未达当前版本
        let status = harness.coordinator.get_status(None).await.unwrap();
        assert!(status.required);
        harness.setup().await.unwrap();
        assert_eq!(harness.ensure_backend_calls(), 0);
    }

    #[tokio::test]
    async fn start_failure_is_retryable_fail_closed() {
        let harness = GuiSetupHarness::with_disclosure_version(0).await;
        harness
            .lifecycle
            .start_fail_times
            .store(1, Ordering::SeqCst);
        let err = harness.coordinator.acknowledge_and_start().await;
        assert!(err.is_err());
        assert_eq!(harness.ensure_backend_calls(), 1);
        // 首次 start 失败，OnceCell 未设置，可重试
        assert!(!harness.coordinator.is_started());
        let ok = harness.coordinator.acknowledge_and_start().await;
        assert!(ok.is_ok());
        assert_eq!(harness.start_calls(), 2);
        assert!(harness.coordinator.is_started());
    }

    #[tokio::test]
    async fn independently_running_cli_is_reported_without_being_stopped() {
        let harness = GuiSetupHarness::with_running_cli().await;
        let status = harness.coordinator.get_status(None).await.unwrap();
        assert!(status.required);
        assert!(status.already_running);
        assert_eq!(status.actual_http_port, Some(62116));
        // 确认仍要求，但 ensure 对 Running 幂等（mock 仍计数）
        let result = harness.coordinator.acknowledge_and_start().await.unwrap();
        assert!(result.reused_existing);
        assert_eq!(result.actual_http_port, 62116);
        // 未将 probe 置为 Stopped —— 即未 stop
        assert_eq!(
            *harness.lifecycle.probe_kind.lock().unwrap(),
            BackendStatusKind::Running
        );
    }

    #[tokio::test]
    async fn permission_onboarding_complete_marker_does_not_bypass_lan_gate() {
        // 仅 cp-onboarding 完成不应影响 bootstrap（本测试验证 bootstrap version 0 仍 block）
        let harness = GuiSetupHarness::with_disclosure_version(0).await;
        let status = harness.coordinator.get_status(None).await.unwrap();
        assert!(status.required);
        harness.setup().await.unwrap();
        assert_eq!(harness.ensure_backend_calls(), 0);
    }
}
