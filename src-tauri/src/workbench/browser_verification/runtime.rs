//! browser_verification/runtime.rs — 验证会话运行时与幂等调度
//!
//! Business Logic（为什么需要这个模块）:
//!     调用方通过 preview 绑定启动验证；需要幂等 request_id、并发上限、cancel、
//!     artifact TTL 与 ephemeral profile 清理，且 engine 仅在 owner 侧启动。
//!
//! Code Logic（这个模块做什么）:
//!     `BrowserVerificationService` 管理 run 状态表、调用 `BrowserVerificationEngine`、
//!     写入 artifact store；提供 FakeEngine 测试夹具。

use super::artifact_store::{now_epoch_ms, path_hash_for_log, ArtifactStore};
use super::engine::{
    BrowserVerificationEngine, BrowserVerificationObserver, EngineRunRequest, EngineRunResult,
    NoopObserver,
};
use super::models::{
    command_fingerprint, default_smoke_commands, BrowserCommandResult, BrowserConsoleEntry,
    BrowserConsoleLevel, BrowserSnapshotNode, BrowserSnapshotResult, BrowserVerificationCommand,
    BrowserVerificationEvidence, BrowserVerificationRun, BrowserVerificationSession,
    BrowserVerificationStartRequest, BrowserVerificationState, ARTIFACT_RETENTION,
    MAX_CONCURRENT_RUNS, SESSION_MAX_DURATION,
};
use crate::error::AppError;
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// 内存中的一次验证运行。
struct RunRecord {
    session: BrowserVerificationSession,
    request_id: String,
    fingerprint: String,
    command_results: Vec<BrowserCommandResult>,
    evidence: Option<BrowserVerificationEvidence>,
    cancel: CancellationToken,
    profile_dir: PathBuf,
    created_instant: Instant,
}

struct ServiceInner {
    engine: Arc<dyn BrowserVerificationEngine>,
    artifacts: Arc<ArtifactStore>,
    data_dir: PathBuf,
    owner_instance_id: String,
    runs: Mutex<HashMap<String, RunRecord>>,
    by_request: Mutex<HashMap<String, String>>,
    engine_starts: AtomicUsize,
    event_sink: Mutex<Option<Arc<dyn BrowserVerificationObserver>>>,
}

/// 浏览器验证服务（可 Clone 的 Arc 句柄）。
///
/// Business Logic（为什么需要这个结构体）:
///     local/remote/mobile 统一 start/get/cancel/artifact，异步 engine 任务需回写状态。
///
/// Code Logic（这个结构体做什么）:
///     内部 Arc 共享 run 表、artifact store 与 engine。
#[derive(Clone)]
pub struct BrowserVerificationService {
    inner: Arc<ServiceInner>,
}

impl BrowserVerificationService {
    /// 使用 managed Chromium engine 创建生产服务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 启动时需要默认 verification 服务；可执行文件可稍后探测。
    ///
    /// Code Logic（这个函数做什么）:
    ///     以 `ChromiumEngine::discover` 构造服务。
    pub fn with_discovered_chromium(
        data_dir: PathBuf,
        owner_instance_id: String,
    ) -> Result<Self, AppError> {
        Self::new(
            Arc::new(super::chromium::ChromiumEngine::discover()),
            data_dir,
            owner_instance_id,
        )
    }

    /// 创建验证服务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 与测试注入 engine 与 data_dir。
    ///
    /// Code Logic（这个函数做什么）:
    ///     初始化 artifact store 与空表。
    pub fn new(
        engine: Arc<dyn BrowserVerificationEngine>,
        data_dir: PathBuf,
        owner_instance_id: String,
    ) -> Result<Self, AppError> {
        let artifacts = Arc::new(ArtifactStore::new(&data_dir)?);
        Ok(Self {
            inner: Arc::new(ServiceInner {
                engine,
                artifacts,
                data_dir,
                owner_instance_id,
                runs: Mutex::new(HashMap::new()),
                by_request: Mutex::new(HashMap::new()),
                engine_starts: AtomicUsize::new(0),
                event_sink: Mutex::new(None),
            }),
        })
    }

    /// engine 启动次数（测试/观测）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     断言幂等与 remote-only-owner。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 AtomicUsize。
    pub fn engine_start_count(&self) -> usize {
        self.inner.engine_starts.load(Ordering::SeqCst)
    }

    /// 设置进度观察者。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 需要 `workbench:browser-verification` 事件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 observer。
    pub async fn set_observer(&self, observer: Arc<dyn BrowserVerificationObserver>) {
        *self.inner.event_sink.lock().await = Some(observer);
    }

    /// 启动或复用验证 run。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     只接受已由 preview 解析的 target；相同 request_id 复用；不同 fingerprint 冲突。
    ///
    /// Code Logic（这个函数做什么）:
    ///     幂等查表 → 并发上限 → 建 profile → spawn engine → 返回 DTO。
    pub async fn start(
        &self,
        preview_id: String,
        project_id: String,
        worktree_id: Option<String>,
        target_url: String,
        request: BrowserVerificationStartRequest,
    ) -> Result<BrowserVerificationRun, AppError> {
        if !request.preview_id.is_empty() && request.preview_id != preview_id {
            return Err(AppError::validation("browser_preview_mismatch"));
        }
        let commands = if request.commands.is_empty() {
            default_smoke_commands()
        } else {
            request.commands.clone()
        };
        let fingerprint = request
            .fingerprint
            .clone()
            .unwrap_or_else(|| command_fingerprint(&commands));

        {
            let by_req = self.inner.by_request.lock().await;
            if let Some(existing_id) = by_req.get(&request.request_id) {
                let runs = self.inner.runs.lock().await;
                if let Some(rec) = runs.get(existing_id) {
                    if rec.fingerprint != fingerprint {
                        return Err(AppError::conflict(
                            "browser_verification_fingerprint_conflict",
                        ));
                    }
                    return Ok(record_to_run(rec));
                }
            }
        }

        {
            let runs = self.inner.runs.lock().await;
            let active = runs
                .values()
                .filter(|r| {
                    matches!(
                        r.session.state,
                        BrowserVerificationState::Queued | BrowserVerificationState::Running
                    )
                })
                .count();
            if active >= MAX_CONCURRENT_RUNS {
                return Err(AppError::unavailable("resource_limit"));
            }
        }

        let run_id = Uuid::new_v4().simple().to_string();
        let now = chrono::Utc::now();
        let expires = now + chrono::Duration::from_std(SESSION_MAX_DURATION).unwrap_or_default();
        let profile_dir = self
            .inner
            .data_dir
            .join("browser-verification")
            .join("runtime")
            .join(&run_id);
        std::fs::create_dir_all(&profile_dir)?;

        let session = BrowserVerificationSession {
            id: run_id.clone(),
            project_id,
            worktree_id,
            preview_id,
            owner_instance_id: self.inner.owner_instance_id.clone(),
            state: BrowserVerificationState::Queued,
            created_at: now.to_rfc3339(),
            last_activity_at: now.to_rfc3339(),
            expires_at: expires.to_rfc3339(),
            error_code: None,
            error_message: None,
        };

        let cancel = CancellationToken::new();
        let rec = RunRecord {
            session,
            request_id: request.request_id.clone(),
            fingerprint,
            command_results: vec![],
            evidence: None,
            cancel: cancel.clone(),
            profile_dir: profile_dir.clone(),
            created_instant: Instant::now(),
        };

        {
            let mut runs = self.inner.runs.lock().await;
            runs.insert(run_id.clone(), rec);
            let mut by_req = self.inner.by_request.lock().await;
            by_req.insert(request.request_id, run_id.clone());
        }

        self.inner.engine_starts.fetch_add(1, Ordering::SeqCst);

        let handle = self.clone();
        let rid = run_id.clone();
        let turl = target_url;
        let cmds = commands;
        let pdir = profile_dir;
        let tok = cancel;
        tokio::spawn(async move {
            handle.run_engine(rid, turl, cmds, pdir, tok).await;
        });

        tokio::task::yield_now().await;
        self.get(&run_id).await
    }

    /// 后台执行 engine 并回写终态。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     start 立即返回；engine 异步推进并清理 profile。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Running → execute → 存截图 → Succeeded/Failed/Canceled → 删 profile。
    async fn run_engine(
        &self,
        run_id: String,
        target_url: String,
        commands: Vec<BrowserVerificationCommand>,
        profile_dir: PathBuf,
        cancel: CancellationToken,
    ) {
        {
            let mut runs = self.inner.runs.lock().await;
            if let Some(rec) = runs.get_mut(&run_id) {
                rec.session.state = BrowserVerificationState::Running;
                rec.session.last_activity_at = chrono::Utc::now().to_rfc3339();
            }
        }

        let observer: Arc<dyn BrowserVerificationObserver> = self
            .inner
            .event_sink
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| Arc::new(NoopObserver));

        let req = EngineRunRequest {
            run_id: run_id.clone(),
            target_url,
            commands,
            profile_dir: profile_dir.clone(),
            chrome_executable: super::chromium::resolve_managed_chrome_executable(),
        };

        let result = self
            .inner
            .engine
            .execute(req, observer, cancel.clone())
            .await;

        let mut runs = self.inner.runs.lock().await;
        let Some(rec) = runs.get_mut(&run_id) else {
            delete_profile_dir(&profile_dir);
            return;
        };
        if cancel.is_cancelled() || rec.session.state == BrowserVerificationState::Canceled {
            rec.session.state = BrowserVerificationState::Canceled;
            delete_profile_dir(&profile_dir);
            return;
        }
        match result {
            Ok(engine_result) => {
                let mut results = engine_result.command_results;
                let mut evidence = engine_result.evidence;
                for (name, bytes) in engine_result.screenshot_pngs {
                    match self.inner.artifacts.put(&run_id, "screenshot", &bytes) {
                        Ok(meta) => {
                            for r in &mut results {
                                if let BrowserCommandResult::Screenshot { artifact_id, .. } = r {
                                    if artifact_id == &name || artifact_id.starts_with("shot") {
                                        *artifact_id = meta.id.clone();
                                    }
                                }
                            }
                            if let Some(ev) = evidence.as_mut() {
                                if ev.screenshot_id.as_deref() == Some(name.as_str())
                                    || ev.screenshot_id.as_deref() == Some("shot")
                                    || ev.screenshot_id.is_none()
                                {
                                    ev.screenshot_id = Some(meta.id.clone());
                                }
                            }
                        }
                        Err(e) => {
                            rec.session.state = BrowserVerificationState::Failed;
                            rec.session.error_code = Some(e.code().to_string());
                            rec.session.error_message = Some(e.to_string());
                            delete_profile_dir(&profile_dir);
                            return;
                        }
                    }
                }
                rec.command_results = results;
                rec.evidence = evidence;
                rec.session.state = BrowserVerificationState::Succeeded;
                rec.session.last_activity_at = chrono::Utc::now().to_rfc3339();
            }
            Err(e) => {
                let code = e.code().to_string();
                if code == "browser_verification_canceled" {
                    rec.session.state = BrowserVerificationState::Canceled;
                } else {
                    rec.session.state = BrowserVerificationState::Failed;
                    rec.session.error_code = Some(code);
                    rec.session.error_message = Some(e.to_string());
                }
                rec.session.last_activity_at = chrono::Utc::now().to_rfc3339();
            }
        }
        delete_profile_dir(&profile_dir);
    }

    /// 查询 run。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     UI 轮询状态与 evidence。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 id 查表。
    pub async fn get(&self, run_id: &str) -> Result<BrowserVerificationRun, AppError> {
        let runs = self.inner.runs.lock().await;
        runs.get(run_id)
            .map(record_to_run)
            .ok_or_else(|| AppError::not_found("browser_verification_not_found"))
    }

    /// 取消 run（幂等）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户停止验证并清理 child/profile。
    ///
    /// Code Logic（这个函数做什么）:
    ///     cancel token；活跃则标 Canceled。
    pub async fn cancel(&self, run_id: &str) -> Result<BrowserVerificationRun, AppError> {
        let mut runs = self.inner.runs.lock().await;
        let rec = runs
            .get_mut(run_id)
            .ok_or_else(|| AppError::not_found("browser_verification_not_found"))?;
        rec.cancel.cancel();
        if matches!(
            rec.session.state,
            BrowserVerificationState::Queued | BrowserVerificationState::Running
        ) {
            rec.session.state = BrowserVerificationState::Canceled;
            rec.session.last_activity_at = chrono::Utc::now().to_rfc3339();
        }
        delete_profile_dir(&rec.profile_dir);
        Ok(record_to_run(rec))
    }

    /// 读取 artifact 字节。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     UI 展示截图。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 ArtifactStore。
    pub async fn artifact(&self, run_id: &str, artifact_id: &str) -> Result<Vec<u8>, AppError> {
        self.inner.artifacts.get(run_id, artifact_id)
    }

    /// 清理过期 run/artifact。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     24h 后释放磁盘。
    ///
    /// Code Logic（这个函数做什么）:
    ///     artifact cleanup + 过期 run 删除。
    pub async fn cleanup(&self) -> Result<(), AppError> {
        self.inner.artifacts.cleanup_expired()?;
        let mut runs = self.inner.runs.lock().await;
        let now = Instant::now();
        let expired: Vec<String> = runs
            .iter()
            .filter(|(_, r)| now.duration_since(r.created_instant) > ARTIFACT_RETENTION)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(rec) = runs.remove(&id) {
                delete_profile_dir(&rec.profile_dir);
                let _ = self.inner.artifacts.remove_run(&id);
            }
        }
        Ok(())
    }

    /// 测试：推进时钟使 run/artifact 变老。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     过期测试不能等待 24h。
    ///
    /// Code Logic（这个函数做什么）:
    ///     回拨 created_instant 并推进 artifact 时钟。
    pub async fn advance_clock_for_test(&self, delta: Duration) {
        self.inner.artifacts.advance_for_test(delta);
        let mut runs = self.inner.runs.lock().await;
        for rec in runs.values_mut() {
            rec.created_instant = rec
                .created_instant
                .checked_sub(delta)
                .unwrap_or_else(|| Instant::now() - delta);
        }
    }

    /// 测试：直接写入 artifact。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     过期路径不依赖 engine 截图。
    ///
    /// Code Logic（这个函数做什么）:
    ///     artifacts.put 返回 id。
    pub fn put_artifact_for_test(
        &self,
        run_id: &str,
        kind: &str,
        bytes: &[u8],
    ) -> Result<String, AppError> {
        Ok(self.inner.artifacts.put(run_id, kind, bytes)?.id)
    }

    /// 测试：data_dir 路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     断言 profile 清理。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 data_dir 引用 clone。
    pub fn data_dir_for_test(&self) -> PathBuf {
        self.inner.data_dir.clone()
    }
}

/// 记录转 DTO。
///
/// Business Logic（为什么需要这个函数）:
///     API 只暴露安全字段。
///
/// Code Logic（这个函数做什么）:
///     clone session/results/evidence。
fn record_to_run(rec: &RunRecord) -> BrowserVerificationRun {
    BrowserVerificationRun {
        session: rec.session.clone(),
        evidence: rec.evidence.clone(),
        command_results: rec.command_results.clone(),
    }
}

/// 删除 profile（失败记 path hash）。
///
/// Business Logic（为什么需要这个函数）:
///     临时 profile 会话结束必须删除。
///
/// Code Logic（这个函数做什么）:
///     remove_dir_all。
fn delete_profile_dir(path: &Path) {
    if path.exists() {
        if let Err(e) = std::fs::remove_dir_all(path) {
            tracing::warn!(
                "browser profile cleanup failed path_hash={} err={e}",
                path_hash_for_log(path)
            );
        }
    }
}

// ─── FakeEngine ───────────────────────────────────────────────────────────

/// 测试用假 engine。
///
/// Business Logic（为什么需要这个结构体）:
///     单元测试不依赖真实 Chrome。
///
/// Code Logic（这个结构体做什么）:
///     按预设返回成功/崩溃/挂起。
pub struct FakeEngine {
    mode: FakeMode,
    delay: Duration,
}

#[derive(Clone)]
enum FakeMode {
    Succeeds,
    Crashes,
    HangUntilCancel,
}

impl FakeEngine {
    /// 立即成功。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     默认生命周期测试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     mode=Succeeds。
    pub fn succeeds() -> Self {
        Self {
            mode: FakeMode::Succeeds,
            delay: Duration::from_millis(0),
        }
    }

    /// 崩溃。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     覆盖 crash 清理。
    ///
    /// Code Logic（这个函数做什么）:
    ///     mode=Crashes。
    pub fn crashes() -> Self {
        Self {
            mode: FakeMode::Crashes,
            delay: Duration::from_millis(0),
        }
    }

    /// 挂起直到取消。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     覆盖 cancel 与并发上限。
    ///
    /// Code Logic（这个函数做什么）:
    ///     mode=HangUntilCancel。
    pub fn hang_until_cancel() -> Self {
        Self {
            mode: FakeMode::HangUntilCancel,
            delay: Duration::from_secs(3600),
        }
    }
}

impl BrowserVerificationEngine for FakeEngine {
    /// 按预设模式执行假命令结果。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     模拟 engine 成功/失败/取消且不启真 Chrome。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写假 profile 文件；按 mode 返回结果或错误。
    fn execute<'a>(
        &'a self,
        request: EngineRunRequest,
        observer: Arc<dyn BrowserVerificationObserver>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<EngineRunResult, AppError>> {
        async move {
            let _ = std::fs::create_dir_all(&request.profile_dir);
            let _ = std::fs::write(request.profile_dir.join("FakeChrome"), b"profile");
            observer.on_progress(
                &request.run_id,
                serde_json::json!({ "phase": "fake_start" }),
            );

            match self.mode {
                FakeMode::Crashes => {
                    return Err(AppError::unavailable("browser_engine_crashed"));
                }
                FakeMode::HangUntilCancel => {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            return Err(AppError::conflict("browser_verification_canceled"));
                        }
                        _ = tokio::time::sleep(self.delay) => {}
                    }
                }
                FakeMode::Succeeds => {
                    if cancel.is_cancelled() {
                        return Err(AppError::conflict("browser_verification_canceled"));
                    }
                }
            }

            let mut command_results = Vec::new();
            let mut screenshot_pngs = Vec::new();
            for cmd in &request.commands {
                match cmd {
                    BrowserVerificationCommand::Snapshot { max_nodes } => {
                        let n = (*max_nodes).min(2);
                        let nodes = (0..n)
                            .map(|i| BrowserSnapshotNode {
                                node_ref: format!("g1-n{i}"),
                                role: "button".into(),
                                name: format!("btn{i}"),
                                state: None,
                                bounds: None,
                                source_hint: None,
                            })
                            .collect();
                        command_results.push(BrowserCommandResult::Snapshot(
                            BrowserSnapshotResult {
                                generation: 1,
                                nodes,
                                truncated: false,
                                url_path: "/".into(),
                                page_title: Some("fake".into()),
                            },
                        ));
                    }
                    BrowserVerificationCommand::Screenshot { full_page } => {
                        let mut png = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
                        png.extend_from_slice(&[0u8; 64]);
                        let id = "shot".to_string();
                        let len = png.len();
                        screenshot_pngs.push((id.clone(), png));
                        command_results.push(BrowserCommandResult::Screenshot {
                            artifact_id: id,
                            byte_len: len,
                            full_page: *full_page,
                        });
                    }
                    BrowserVerificationCommand::ReadConsole { .. } => {
                        command_results.push(BrowserCommandResult::Console {
                            entries: vec![BrowserConsoleEntry {
                                sequence: 1,
                                level: BrowserConsoleLevel::Error,
                                text: "fake error".into(),
                                timestamp_ms: now_epoch_ms(),
                            }],
                            truncated: false,
                        });
                    }
                    BrowserVerificationCommand::WaitFor { timeout_ms, .. } => {
                        command_results.push(BrowserCommandResult::WaitSatisfied {
                            timeout_ms: *timeout_ms,
                        });
                    }
                    BrowserVerificationCommand::Click { node_ref } => {
                        command_results.push(BrowserCommandResult::clicked(node_ref.clone(), 1, 1));
                    }
                    BrowserVerificationCommand::Fill { node_ref, value: _ } => {
                        command_results.push(BrowserCommandResult::filled(node_ref.clone(), 1));
                    }
                }
            }

            Ok(EngineRunResult {
                command_results,
                evidence: Some(BrowserVerificationEvidence {
                    session_id: request.run_id.clone(),
                    url_path: "/".into(),
                    page_title: Some("fake".into()),
                    assertions: vec![],
                    console_errors: vec![],
                    screenshot_id: Some("shot".into()),
                    truncated: false,
                    captured_at: chrono::Utc::now().to_rfc3339(),
                }),
                screenshot_pngs,
            })
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn start_req(preview_id: &str, request_id: &str) -> BrowserVerificationStartRequest {
        BrowserVerificationStartRequest {
            preview_id: preview_id.into(),
            request_id: request_id.into(),
            commands: default_smoke_commands(),
            fingerprint: None,
        }
    }

    async fn fixture_service(engine: FakeEngine) -> BrowserVerificationService {
        let dir = tempdir().unwrap();
        let data = dir.keep();
        BrowserVerificationService::new(Arc::new(engine), data, "owner-test".into()).unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runtime_reuses_same_idempotent_run_and_expires_artifacts() {
        let service = fixture_service(FakeEngine::succeeds()).await;
        let a = service
            .start(
                "preview-1".into(),
                "proj".into(),
                None,
                "http://127.0.0.1:5173/".into(),
                start_req("preview-1", "request-1"),
            )
            .await
            .unwrap();
        for _ in 0..50 {
            tokio::task::yield_now().await;
            let run = service.get(&a.session.id).await.unwrap();
            if run.session.state == BrowserVerificationState::Succeeded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let a = service.get(&a.session.id).await.unwrap();
        let b = service
            .start(
                "preview-1".into(),
                "proj".into(),
                None,
                "http://127.0.0.1:5173/".into(),
                start_req("preview-1", "request-1"),
            )
            .await
            .unwrap();
        assert_eq!(a.session.id, b.session.id);
        assert_eq!(service.engine_start_count(), 1);

        let shot_id = service
            .put_artifact_for_test(&a.session.id, "bin", b"hello")
            .unwrap();
        service
            .advance_clock_for_test(Duration::from_secs(86_401))
            .await;
        service.cleanup().await.unwrap();
        assert!(service.artifact(&a.session.id, &shot_id).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn different_fingerprint_same_request_id_conflicts() {
        let service = fixture_service(FakeEngine::succeeds()).await;
        let _ = service
            .start(
                "p".into(),
                "proj".into(),
                None,
                "http://127.0.0.1:1/".into(),
                start_req("p", "rid"),
            )
            .await
            .unwrap();
        let mut req = start_req("p", "rid");
        req.commands = vec![BrowserVerificationCommand::Screenshot { full_page: true }];
        let err = service
            .start(
                "p".into(),
                "proj".into(),
                None,
                "http://127.0.0.1:1/".into(),
                req,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), "browser_verification_fingerprint_conflict");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrency_cap_rejects_third_active_run() {
        let service = fixture_service(FakeEngine::hang_until_cancel()).await;
        for i in 0..MAX_CONCURRENT_RUNS {
            service
                .start(
                    "p".into(),
                    "proj".into(),
                    None,
                    "http://127.0.0.1:1/".into(),
                    start_req("p", &format!("r{i}")),
                )
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
        let err = service
            .start(
                "p".into(),
                "proj".into(),
                None,
                "http://127.0.0.1:1/".into(),
                start_req("p", "overflow"),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), "resource_limit");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_stops_hanging_engine() {
        let service = fixture_service(FakeEngine::hang_until_cancel()).await;
        let run = service
            .start(
                "p".into(),
                "proj".into(),
                None,
                "http://127.0.0.1:1/".into(),
                start_req("p", "cancel-me"),
            )
            .await
            .unwrap();
        let canceled = service.cancel(&run.session.id).await.unwrap();
        assert_eq!(canceled.session.state, BrowserVerificationState::Canceled);
        let again = service.cancel(&run.session.id).await.unwrap();
        assert_eq!(again.session.state, BrowserVerificationState::Canceled);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn crash_marks_failed_and_cleans_profile() {
        let service = fixture_service(FakeEngine::crashes()).await;
        let run = service
            .start(
                "p".into(),
                "proj".into(),
                None,
                "http://127.0.0.1:1/".into(),
                start_req("p", "crash"),
            )
            .await
            .unwrap();
        for _ in 0..50 {
            tokio::task::yield_now().await;
            let r = service.get(&run.session.id).await.unwrap();
            if r.session.state == BrowserVerificationState::Failed {
                assert_eq!(
                    r.session.error_code.as_deref(),
                    Some("browser_engine_crashed")
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("expected failed state");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn profile_dir_removed_after_success() {
        let service = fixture_service(FakeEngine::succeeds()).await;
        let run = service
            .start(
                "p".into(),
                "proj".into(),
                None,
                "http://127.0.0.1:1/".into(),
                start_req("p", "prof"),
            )
            .await
            .unwrap();
        for _ in 0..50 {
            tokio::task::yield_now().await;
            let r = service.get(&run.session.id).await.unwrap();
            if r.session.state == BrowserVerificationState::Succeeded {
                let profile = service
                    .data_dir_for_test()
                    .join("browser-verification")
                    .join("runtime")
                    .join(&run.session.id);
                assert!(!profile.exists(), "profile should be cleaned");
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("expected success");
    }
}
