//! updater/runtime.rs — 单锁 generation 更新状态机
//!
//! Business Logic（为什么需要这个模块）:
//!     用户可连续检查/下载/取消/安装更新；旧下载回调与新 check 交错时不得覆盖新一代状态，
//!     安装失败后应保留安装包供重试，检查更新不得静默取消进行中的下载/安装。
//!
//! Code Logic（这个模块做什么）:
//!     单一 `std::sync::Mutex<UpdateRuntimeState>` 串行所有同步转移；
//!     generation 仅在 `begin_check` 递增；完成类方法要求 generation 匹配且 phase 合法；
//!     cancel 在锁内一次取出 token/handle，调用方在锁外 cancel/abort；
//!     锁内不做网络/安装 IO。

use std::sync::{Arc, Mutex};

use tauri::async_runtime::JoinHandle;
use tauri_plugin_updater::Update;
use tokio_util::sync::CancellationToken;

use crate::commands::updater::{UpdateDownloadStatus, UpdateStatusValue};
use crate::error::AppError;

/// 更新状态机相位（内部权威，比 DTO status 更细）。
///
/// Business Logic: 区分 checking/available/installing 等前端后续会展示的阶段，
///     并作为 generation 完成回调的守卫条件。
/// Code Logic: 纯枚举；与 `UpdateDownloadStatus.status` 通过映射同步。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePhase {
    /// 空闲：未检查 / 无可用更新
    Idle,
    /// 正在检查 endpoint
    Checking,
    /// 已发现可用更新（pending 已缓存）
    Available,
    /// 下载中
    Downloading,
    /// 下载完成，可安装（bytes 已缓存）
    Downloaded,
    /// 安装中
    Installing,
    /// 下载/检查失败
    Failed,
    /// 用户取消下载
    Cancelled,
}

/// 安装完成结果。
///
/// Business Logic: 安装成功后应请求重启；失败则保留 bytes 供重试。
/// Code Logic: 由 `finish_install` 返回给命令层，决定是否 `request_restart`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    /// 安装成功，命令层应请求重启
    RestartRequested,
    /// 安装失败，phase 已回到 Downloaded 并保留 bytes
    FailedRetained,
    /// generation/phase 不匹配，忽略本次完成（不改状态）
    Stale,
}

/// 下载启动租约：命令层在锁外 spawn 下载任务。
///
/// Business Logic: 下载必须在锁外执行，避免阻塞状态机。
/// Code Logic: 携带本代 generation、Update clone 与取消令牌。
#[derive(Clone)]
pub struct DownloadLease {
    /// 本代 generation
    pub generation: u64,
    /// 缓存的 Update 对象
    pub update: Update,
    /// 软取消令牌（cancel 时置位）
    pub cancel: CancellationToken,
}

/// 安装启动租约：命令层在锁外执行 install。
///
/// Business Logic: 安装从 Arc 克隆 bytes，失败后仍可重试同一份数据。
/// Code Logic: 携带 generation、Update clone 与 bytes 快照。
#[derive(Clone)]
pub struct InstallLease {
    /// 本代 generation
    pub generation: u64,
    /// 缓存的 Update 对象
    pub update: Update,
    /// 安装包字节（Arc 共享，不 take）
    pub bytes: Arc<[u8]>,
}

/// 取消操作取出的资源：调用方在锁外 cancel/abort。
///
/// Business Logic: cancel 必须先改状态再中断 IO，避免旧回调回写。
/// Code Logic: 锁内 take 一次 token/handle；无任务时两者皆 None。
pub struct CancelLease {
    /// 软取消令牌
    pub cancel: Option<CancellationToken>,
    /// 下载任务句柄
    pub task: Option<JoinHandle<()>>,
}

/// 单锁更新运行时状态。
///
/// Business Logic: 聚合 generation/phase 与全部缓存资源，作为唯一权威。
/// Code Logic: 字段均由 `UpdateRuntime` 方法在同一把 Mutex 下读写。
pub struct UpdateRuntimeState {
    /// 当前 generation；仅 begin_check 递增
    pub generation: u64,
    /// 当前相位
    pub phase: UpdatePhase,
    /// check 命中后缓存的 Update
    pub pending: Option<Update>,
    /// download 完成后缓存的安装包
    pub bytes: Option<Arc<[u8]>>,
    /// 当前下载取消令牌
    pub cancel: Option<CancellationToken>,
    /// 当前下载任务句柄
    pub task: Option<JoinHandle<()>>,
    /// 对齐前端的下载状态 DTO
    pub status: UpdateDownloadStatus,
}

impl Default for UpdateRuntimeState {
    /// Business Logic: 应用启动时更新器应处于空闲。
    /// Code Logic: generation=0，phase=Idle，资源全空，status 默认。
    fn default() -> Self {
        Self {
            generation: 0,
            phase: UpdatePhase::Idle,
            pending: None,
            bytes: None,
            cancel: None,
            task: None,
            status: UpdateDownloadStatus::default(),
        }
    }
}

/// 自动更新 generation 状态机（单锁）。
///
/// Business Logic: 所有检查/下载/取消/安装命令与异步回调只通过本类型转移状态，
///     保证旧代回调无法覆盖新代，安装失败可重试。
/// Code Logic: 内层 `Mutex<UpdateRuntimeState>`；方法在锁内做同步转移后立即释放。
pub struct UpdateRuntime {
    inner: Mutex<UpdateRuntimeState>,
}

impl Default for UpdateRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdateRuntime {
    /// Business Logic: AppState / 测试需要一份空闲初态运行时。
    /// Code Logic: 包装默认 `UpdateRuntimeState`。
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(UpdateRuntimeState::default()),
        }
    }

    /// Business Logic: 前端轮询下载进度条。
    /// Code Logic: 锁内 clone `status` 返回。
    pub fn status(&self) -> UpdateDownloadStatus {
        self.with_state(|s| s.status.clone())
    }

    /// Business Logic: 调试/测试观察内部相位与 generation。
    /// Code Logic: 返回 `(generation, phase)` 快照。
    pub fn snapshot(&self) -> (u64, UpdatePhase) {
        self.with_state(|s| (s.generation, s.phase))
    }

    /// Business Logic: 用户点击「检查更新」；下载/安装进行中禁止新 check，避免静默打断。
    /// Code Logic: Downloading/Installing → Conflict；否则 generation+=1，清 bytes/error/task/token，
    ///     phase=Checking，返回新 generation。
    pub fn begin_check(&self) -> Result<u64, AppError> {
        self.with_state(|s| {
            if matches!(s.phase, UpdatePhase::Downloading | UpdatePhase::Installing) {
                return Err(AppError::conflict(
                    "更新下载或安装进行中，请稍后再检查更新".to_string(),
                ));
            }
            s.generation = s.generation.saturating_add(1);
            s.phase = UpdatePhase::Checking;
            s.bytes = None;
            s.cancel = None;
            // 旧任务句柄丢弃（若仍在跑，其回调会因 generation 不匹配被忽略）
            let _old_task = s.task.take();
            s.status.error.clear();
            s.status.progress = 0.0;
            s.status.file_path.clear();
            s.status.status = UpdateStatusValue::Checking;
            Ok(s.generation)
        })
    }

    /// Business Logic: check 网络结果回写；仅当前代 Checking 才生效。
    /// Code Logic: generation 不匹配或非 Checking → 忽略返回 Ok(false)；
    ///     Some(update) → Available + 缓存 pending + 填 url/filename；
    ///     None → Idle；Err → Failed + error。
    pub fn finish_check(
        &self,
        generation: u64,
        result: Result<Option<Update>, String>,
    ) -> Result<bool, AppError> {
        self.with_state(|s| {
            if s.generation != generation || s.phase != UpdatePhase::Checking {
                return Ok(false);
            }
            match result {
                Ok(Some(update)) => {
                    let download_url = update.download_url.to_string();
                    let filename = filename_from_url(&download_url);
                    s.pending = Some(update);
                    s.phase = UpdatePhase::Available;
                    s.status.status = UpdateStatusValue::Idle;
                    s.status.url = download_url;
                    s.status.filename = filename;
                    s.status.size = 0;
                    s.status.error.clear();
                    s.status.progress = 0.0;
                }
                Ok(None) => {
                    s.pending = None;
                    s.phase = UpdatePhase::Idle;
                    s.status = UpdateDownloadStatus::default();
                }
                Err(err) => {
                    s.pending = None;
                    s.phase = UpdatePhase::Failed;
                    s.status.status = UpdateStatusValue::Failed;
                    s.status.error = err;
                    s.status.progress = 0.0;
                }
            }
            Ok(true)
        })
    }

    /// Business Logic: 启动下载前占用状态机；需已有可用更新且无进行中下载。
    /// Code Logic: 要求 pending 存在且非 Downloading/Installing；
    ///     置 Downloading、清旧 bytes、新建 cancel token，返回 DownloadLease。
    pub fn begin_download(&self) -> Result<DownloadLease, AppError> {
        self.with_state(|s| {
            if s.phase == UpdatePhase::Downloading {
                return Err(AppError::conflict("已有下载任务进行中".to_string()));
            }
            if s.phase == UpdatePhase::Installing {
                return Err(AppError::conflict("安装进行中，无法开始下载".to_string()));
            }
            if s.phase == UpdatePhase::Checking {
                return Err(AppError::conflict("正在检查更新，请稍后再下载".to_string()));
            }
            let update = s.pending.clone().ok_or_else(|| {
                AppError::validation("尚未检查到可用更新，请先调用 check_update".to_string())
            })?;

            let download_url = update.download_url.to_string();
            let download_filename = filename_from_url(&download_url);
            let cancel = CancellationToken::new();

            s.phase = UpdatePhase::Downloading;
            s.bytes = None;
            s.cancel = Some(cancel.clone());
            let _old_task = s.task.take();
            s.status.status = UpdateStatusValue::Downloading;
            s.status.progress = 0.0;
            s.status.error.clear();
            s.status.file_path.clear();
            s.status.url = download_url;
            s.status.filename = download_filename;
            s.status.size = 0;

            Ok(DownloadLease {
                generation: s.generation,
                update,
                cancel,
            })
        })
    }

    /// Business Logic: spawn 后登记 JoinHandle，供 cancel abort。
    /// Code Logic: 仅当 generation 匹配且 phase=Downloading 时写入 task。
    pub fn attach_download_task(&self, generation: u64, handle: JoinHandle<()>) {
        self.with_state(|s| {
            if s.generation == generation && s.phase == UpdatePhase::Downloading {
                s.task = Some(handle);
            }
        });
    }

    /// Business Logic: 下载进度回调写进度条；旧代/非下载中忽略。
    /// Code Logic: generation 匹配且 phase=Downloading 时写 progress/size，返回 true。
    pub fn record_progress(&self, generation: u64, progress: f64, size: Option<u64>) -> bool {
        self.with_state(|s| {
            if s.generation != generation || s.phase != UpdatePhase::Downloading {
                return false;
            }
            s.status.progress = progress.clamp(0.0, 1.0);
            if let Some(total) = size {
                s.status.size = total;
            }
            true
        })
    }

    /// Business Logic: 下载完成/失败/取消回写；旧代忽略。
    /// Code Logic: generation 匹配且 phase=Downloading 才转移：
    ///     Ok(bytes)→Downloaded+Completed；cancelled→Cancelled；Err→Failed。
    ///     返回 true 表示本代已采纳。
    pub fn finish_download(
        &self,
        generation: u64,
        result: Result<Vec<u8>, String>,
        cancelled: bool,
    ) -> bool {
        self.with_state(|s| {
            if s.generation != generation || s.phase != UpdatePhase::Downloading {
                return false;
            }
            // 下载结束清 task/token 引用（句柄已结束或即将结束）
            s.task = None;
            s.cancel = None;
            match result {
                Ok(bytes) => {
                    s.bytes = Some(Arc::from(bytes.into_boxed_slice()));
                    s.phase = UpdatePhase::Downloaded;
                    s.status.status = UpdateStatusValue::Completed;
                    s.status.progress = 1.0;
                    s.status.error.clear();
                }
                Err(_) if cancelled => {
                    s.bytes = None;
                    s.phase = UpdatePhase::Cancelled;
                    s.status.status = UpdateStatusValue::Cancelled;
                    s.status.error.clear();
                }
                Err(err) => {
                    s.bytes = None;
                    s.phase = UpdatePhase::Failed;
                    s.status.status = UpdateStatusValue::Failed;
                    s.status.error = err;
                }
            }
            true
        })
    }

    /// Business Logic: 用户取消下载；原子取出 token/handle 后由调用方在锁外中断。
    /// Code Logic: 若 phase=Downloading，take cancel+task，phase=Cancelled，返回 lease；
    ///     否则返回空 lease（ok=false 语义由命令层解释）。
    pub fn cancel(&self) -> CancelLease {
        self.with_state(|s| {
            if s.phase != UpdatePhase::Downloading {
                return CancelLease {
                    cancel: None,
                    task: None,
                };
            }
            let cancel = s.cancel.take();
            let task = s.task.take();
            s.phase = UpdatePhase::Cancelled;
            s.status.status = UpdateStatusValue::Cancelled;
            s.status.error.clear();
            CancelLease { cancel, task }
        })
    }

    /// Business Logic: 开始安装；要求下载完成且 bytes/pending 齐全。
    /// Code Logic: phase=Downloaded 且 bytes+pending 存在 → Installing，返回 InstallLease（clone，不 take）。
    pub fn begin_install(&self) -> Result<InstallLease, AppError> {
        self.with_state(|s| {
            if s.phase != UpdatePhase::Downloaded {
                return Err(AppError::validation(
                    "安装包未就绪，请先完成下载".to_string(),
                ));
            }
            let bytes = s
                .bytes
                .clone()
                .ok_or_else(|| AppError::validation("安装包数据缺失，请重新下载".to_string()))?;
            let update = s.pending.clone().ok_or_else(|| {
                AppError::validation("更新元数据缺失，请重新检查更新".to_string())
            })?;
            s.phase = UpdatePhase::Installing;
            // 安装中展示 installing；progress 保持下载完成值，不伪造
            s.status.status = UpdateStatusValue::Installing;
            s.status.error.clear();
            Ok(InstallLease {
                generation: s.generation,
                update,
                bytes,
            })
        })
    }

    /// Business Logic: 安装结果回写；失败保留 bytes 回到 Downloaded 供重试；成功请求重启。
    /// Code Logic: generation 匹配且 phase=Installing 才处理；
    ///     Ok → RestartRequested（清理 bytes/pending，phase=Idle）；
    ///     Err → FailedRetained（phase=Downloaded，保留 bytes，写 error）；
    ///     不匹配 → Stale。
    pub fn finish_install(&self, generation: u64, result: Result<(), String>) -> InstallOutcome {
        self.with_state(|s| {
            if s.generation != generation || s.phase != UpdatePhase::Installing {
                return InstallOutcome::Stale;
            }
            match result {
                Ok(()) => {
                    // 成功后清理；重启由命令层触发
                    s.phase = UpdatePhase::Idle;
                    s.bytes = None;
                    s.pending = None;
                    s.cancel = None;
                    s.task = None;
                    s.status = UpdateDownloadStatus::default();
                    InstallOutcome::RestartRequested
                }
                Err(err) => {
                    s.phase = UpdatePhase::Downloaded;
                    s.status.status = UpdateStatusValue::Completed;
                    s.status.error = err;
                    // bytes / pending 保留
                    InstallOutcome::FailedRetained
                }
            }
        })
    }

    /// Business Logic: 兼容层——命令临时直接读 pending（T6 前过渡）。
    /// Code Logic: clone pending。
    pub fn pending_clone(&self) -> Option<Update> {
        self.with_state(|s| s.pending.clone())
    }

    /// Business Logic: 兼容层——读 bytes 是否就绪。
    /// Code Logic: 返回 bytes 是否为 Some。
    pub fn has_bytes(&self) -> bool {
        self.with_state(|s| s.bytes.is_some())
    }

    /// Business Logic: 命令级竞态测试需要强制推进相位，无需真实 Update 对象。
    /// Code Logic: 写 phase/generation 并按 phase 同步 DTO status；Downloading 附带 cancel token。
    #[cfg(test)]
    pub fn force_phase_for_test(&self, phase: UpdatePhase, generation: u64) {
        self.with_state(|s| {
            s.phase = phase;
            s.generation = generation;
            match phase {
                UpdatePhase::Downloading => {
                    s.status.status = UpdateStatusValue::Downloading;
                    s.cancel = Some(CancellationToken::new());
                }
                UpdatePhase::Downloaded => {
                    s.status.status = UpdateStatusValue::Completed;
                    if s.bytes.is_none() {
                        s.bytes = Some(Arc::from(b"pkg".as_slice()));
                    }
                }
                UpdatePhase::Installing => {
                    s.status.status = UpdateStatusValue::Installing;
                    if s.bytes.is_none() {
                        s.bytes = Some(Arc::from(b"pkg".as_slice()));
                    }
                }
                UpdatePhase::Failed => {
                    s.status.status = UpdateStatusValue::Failed;
                }
                UpdatePhase::Cancelled => {
                    s.status.status = UpdateStatusValue::Cancelled;
                }
                UpdatePhase::Checking => {
                    s.status.status = UpdateStatusValue::Checking;
                }
                UpdatePhase::Available => {
                    s.status.status = UpdateStatusValue::Idle;
                }
                UpdatePhase::Idle => {
                    s.status = UpdateDownloadStatus::default();
                }
            }
        });
    }

    /// Business Logic: 安装重试测试需要注入假安装包字节。
    /// Code Logic: 覆盖 state.bytes。
    #[cfg(test)]
    pub fn force_bytes_for_test(&self, bytes: &[u8]) {
        self.with_state(|s| {
            s.bytes = Some(Arc::from(bytes));
        });
    }

    /// Code Logic: 统一加锁，毒化时 panic（与项目其他 Mutex 一致）。
    fn with_state<R>(&self, f: impl FnOnce(&mut UpdateRuntimeState) -> R) -> R {
        let mut guard = self.inner.lock().expect("update_runtime 锁中毒");
        f(&mut guard)
    }
}

/// 从下载 URL 解析文件名（取 path 末段）。
///
/// Business Logic: 前端需展示安装包文件名。
/// Code Logic: rsplit 取末段，空则返回空串。
fn filename_from_url(url: &str) -> String {
    url.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// 测试用：把状态机强制推进到指定 phase（不依赖真实 Update）。
    ///
    /// Business Logic: 纯转移测试需要注入 Downloading/Installing 等相位。
    /// Code Logic: 直接写 phase/generation/bytes，不构造 tauri Update。
    fn force_phase(rt: &UpdateRuntime, phase: UpdatePhase, generation: u64) {
        rt.with_state(|s| {
            s.phase = phase;
            s.generation = generation;
            match phase {
                UpdatePhase::Downloading => {
                    s.status.status = UpdateStatusValue::Downloading;
                    s.cancel = Some(CancellationToken::new());
                }
                UpdatePhase::Downloaded => {
                    s.status.status = UpdateStatusValue::Completed;
                    if s.bytes.is_none() {
                        s.bytes = Some(Arc::from(b"pkg".as_slice()));
                    }
                }
                UpdatePhase::Installing => {
                    s.status.status = UpdateStatusValue::Installing;
                    if s.bytes.is_none() {
                        s.bytes = Some(Arc::from(b"pkg".as_slice()));
                    }
                }
                UpdatePhase::Failed => {
                    s.status.status = UpdateStatusValue::Failed;
                }
                UpdatePhase::Cancelled => {
                    s.status.status = UpdateStatusValue::Cancelled;
                }
                UpdatePhase::Checking => {
                    s.status.status = UpdateStatusValue::Checking;
                }
                UpdatePhase::Available => {
                    s.status.status = UpdateStatusValue::Idle;
                }
                UpdatePhase::Idle => {
                    s.status = UpdateDownloadStatus::default();
                }
            }
        });
    }

    /// 注入假 bytes。
    fn force_bytes(rt: &UpdateRuntime, bytes: &[u8]) {
        rt.with_state(|s| {
            s.bytes = Some(Arc::from(bytes));
        });
    }

    #[test]
    fn begin_check_increments_generation_only() {
        let rt = UpdateRuntime::new();
        let (g0, p0) = rt.snapshot();
        assert_eq!(g0, 0);
        assert_eq!(p0, UpdatePhase::Idle);

        let g1 = rt.begin_check().expect("begin_check");
        assert_eq!(g1, 1);
        assert_eq!(rt.snapshot(), (1, UpdatePhase::Checking));
        assert_eq!(rt.status().status, UpdateStatusValue::Checking);

        // finish 无更新
        assert!(rt.finish_check(1, Ok(None)).unwrap());
        assert_eq!(rt.snapshot(), (1, UpdatePhase::Idle));
        assert_eq!(rt.status().status, UpdateStatusValue::Idle);

        // 再次 check generation 再 +1，非 begin 路径不递增
        let g2 = rt.begin_check().expect("begin_check 2");
        assert_eq!(g2, 2);
        // record_progress 等不会改 generation
        assert!(!rt.record_progress(2, 0.5, None)); // 非 Downloading
        assert_eq!(rt.snapshot().0, 2);
    }

    #[test]
    fn illegal_check_during_downloading_and_installing() {
        let rt = UpdateRuntime::new();
        force_phase(&rt, UpdatePhase::Downloading, 3);
        let err = rt.begin_check().expect_err("downloading blocks check");
        assert!(matches!(err, AppError::Conflict(_)));
        assert_eq!(rt.snapshot(), (3, UpdatePhase::Downloading));

        force_phase(&rt, UpdatePhase::Installing, 4);
        let err = rt.begin_check().expect_err("installing blocks check");
        assert!(matches!(err, AppError::Conflict(_)));
        assert_eq!(rt.snapshot(), (4, UpdatePhase::Installing));
    }

    #[test]
    fn legal_phase_edges_without_plugin_update() {
        let rt = UpdateRuntime::new();

        // Idle → Checking → Failed
        let g = rt.begin_check().unwrap();
        assert!(rt.finish_check(g, Err("network down".into())).unwrap());
        assert_eq!(rt.snapshot().1, UpdatePhase::Failed);
        assert_eq!(rt.status().status, UpdateStatusValue::Failed);

        // Failed → Checking → Idle (no update)
        let g = rt.begin_check().unwrap();
        assert!(rt.finish_check(g, Ok(None)).unwrap());
        assert_eq!(rt.snapshot().1, UpdatePhase::Idle);

        // Idle → Checking，再 stale finish 被忽略
        let g = rt.begin_check().unwrap();
        assert!(!rt.finish_check(g - 1, Ok(None)).unwrap());
        assert_eq!(rt.snapshot().1, UpdatePhase::Checking);

        // 模拟 Available → Downloading → Downloaded
        force_phase(&rt, UpdatePhase::Available, g);
        force_phase(&rt, UpdatePhase::Downloading, g);
        assert!(rt.finish_download(g, Ok(vec![1, 2, 3]), false));
        assert_eq!(rt.snapshot().1, UpdatePhase::Downloaded);
        assert_eq!(rt.status().status, UpdateStatusValue::Completed);
        assert!(rt.has_bytes());

        // Downloaded → Installing → FailedRetained → Downloaded
        force_phase(&rt, UpdatePhase::Installing, g);
        force_bytes(&rt, &[1, 2, 3]);
        let outcome = rt.finish_install(g, Err("disk full".into()));
        assert_eq!(outcome, InstallOutcome::FailedRetained);
        assert_eq!(rt.snapshot().1, UpdatePhase::Downloaded);
        assert!(rt.has_bytes());
        assert_eq!(rt.status().error, "disk full");

        // Downloaded → Installing → RestartRequested → Idle
        force_phase(&rt, UpdatePhase::Installing, g);
        force_bytes(&rt, &[1, 2, 3]);
        let outcome = rt.finish_install(g, Ok(()));
        assert_eq!(outcome, InstallOutcome::RestartRequested);
        assert_eq!(rt.snapshot().1, UpdatePhase::Idle);
        assert!(!rt.has_bytes());
    }

    #[test]
    fn old_generation_progress_and_completion_ignored() {
        let rt = UpdateRuntime::new();
        force_phase(&rt, UpdatePhase::Downloading, 5);

        // 旧 generation 进度忽略
        assert!(!rt.record_progress(4, 0.9, Some(100)));
        assert_eq!(rt.status().progress, 0.0);

        // 本代进度生效
        assert!(rt.record_progress(5, 0.5, Some(200)));
        assert!((rt.status().progress - 0.5).abs() < f64::EPSILON);
        assert_eq!(rt.status().size, 200);

        // 旧 generation 完成忽略，状态仍 Downloading
        assert!(!rt.finish_download(4, Ok(vec![9]), false));
        assert_eq!(rt.snapshot().1, UpdatePhase::Downloading);
        assert!(!rt.has_bytes());

        // 本代完成
        assert!(rt.finish_download(5, Ok(vec![9]), false));
        assert_eq!(rt.snapshot().1, UpdatePhase::Downloaded);
    }

    #[test]
    fn cancel_takes_handle_and_token_once() {
        let rt = UpdateRuntime::new();
        force_phase(&rt, UpdatePhase::Downloading, 2);

        // 放入一个假 task：用已完成的 spawn
        let finished = Arc::new(AtomicBool::new(false));
        let flag = finished.clone();
        let handle = tauri::async_runtime::spawn(async move {
            flag.store(true, Ordering::SeqCst);
        });
        // 等任务跑完再 attach（避免 abort 未启动任务）
        while !finished.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        rt.attach_download_task(2, handle);
        // 补一个 cancel token（force_phase 已放一个）
        rt.with_state(|s| {
            if s.cancel.is_none() {
                s.cancel = Some(CancellationToken::new());
            }
        });

        let lease1 = rt.cancel();
        assert!(lease1.cancel.is_some());
        assert!(lease1.task.is_some());
        assert_eq!(rt.snapshot().1, UpdatePhase::Cancelled);
        assert_eq!(rt.status().status, UpdateStatusValue::Cancelled);

        // 第二次 cancel 拿不到资源
        let lease2 = rt.cancel();
        assert!(lease2.cancel.is_none());
        assert!(lease2.task.is_none());
    }

    #[test]
    fn new_check_clears_old_bytes() {
        let rt = UpdateRuntime::new();
        force_phase(&rt, UpdatePhase::Downloaded, 1);
        force_bytes(&rt, b"old-package");
        assert!(rt.has_bytes());

        let g = rt.begin_check().unwrap();
        assert_eq!(g, 2);
        assert!(!rt.has_bytes());
        assert_eq!(rt.snapshot().1, UpdatePhase::Checking);
        assert_eq!(rt.status().status, UpdateStatusValue::Checking);
        assert!(rt.status().error.is_empty());
    }

    #[test]
    fn install_failure_retains_bytes_and_returns_downloaded() {
        let rt = UpdateRuntime::new();
        force_phase(&rt, UpdatePhase::Installing, 7);
        force_bytes(&rt, b"retryable-pkg");

        let outcome = rt.finish_install(7, Err("signature mismatch".into()));
        assert_eq!(outcome, InstallOutcome::FailedRetained);
        assert_eq!(rt.snapshot().1, UpdatePhase::Downloaded);
        assert!(rt.has_bytes());
        assert_eq!(rt.status().status, UpdateStatusValue::Completed);
        assert_eq!(rt.status().error, "signature mismatch");

        // stale finish 不破坏保留的 bytes
        let stale = rt.finish_install(7, Ok(()));
        assert_eq!(stale, InstallOutcome::Stale);
        assert!(rt.has_bytes());
        assert_eq!(rt.snapshot().1, UpdatePhase::Downloaded);
    }

    #[test]
    fn install_success_is_terminal_idle() {
        let rt = UpdateRuntime::new();
        force_phase(&rt, UpdatePhase::Installing, 9);
        force_bytes(&rt, b"pkg");

        let outcome = rt.finish_install(9, Ok(()));
        assert_eq!(outcome, InstallOutcome::RestartRequested);
        assert_eq!(rt.snapshot().1, UpdatePhase::Idle);
        assert!(!rt.has_bytes());
        assert_eq!(rt.status().status, UpdateStatusValue::Idle);
        assert!(rt.pending_clone().is_none());
    }

    #[test]
    fn finish_download_cancelled_vs_failed() {
        let rt = UpdateRuntime::new();
        force_phase(&rt, UpdatePhase::Downloading, 1);
        assert!(rt.finish_download(1, Err("aborted".into()), true));
        assert_eq!(rt.snapshot().1, UpdatePhase::Cancelled);
        assert_eq!(rt.status().status, UpdateStatusValue::Cancelled);

        force_phase(&rt, UpdatePhase::Downloading, 2);
        assert!(rt.finish_download(2, Err("timeout".into()), false));
        assert_eq!(rt.snapshot().1, UpdatePhase::Failed);
        assert_eq!(rt.status().status, UpdateStatusValue::Failed);
        assert_eq!(rt.status().error, "timeout");
    }

    #[test]
    fn status_dto_is_checking_during_check() {
        let rt = UpdateRuntime::new();
        let g = rt.begin_check().unwrap();
        assert_eq!(rt.status().status, UpdateStatusValue::Checking);
        assert_eq!(rt.snapshot().1, UpdatePhase::Checking);
        // Available path: finish with None -> Idle (no separate Available DTO status)
        assert!(rt.finish_check(g, Ok(None)).unwrap());
        assert_eq!(rt.status().status, UpdateStatusValue::Idle);
    }

    #[test]
    fn status_dto_is_installing_during_install() {
        let rt = UpdateRuntime::new();
        force_phase(&rt, UpdatePhase::Downloaded, 3);
        force_bytes(&rt, b"pkg");
        // 模拟 begin_install 的 DTO 效果
        force_phase(&rt, UpdatePhase::Installing, 3);
        force_bytes(&rt, b"pkg");
        assert_eq!(rt.status().status, UpdateStatusValue::Installing);
        // progress 不伪造：force 不改 progress，默认 0；真实 begin_install 保留下载进度
        let progress_before = rt.status().progress;
        let outcome = rt.finish_install(3, Err("boom".into()));
        assert_eq!(outcome, InstallOutcome::FailedRetained);
        assert_eq!(rt.status().status, UpdateStatusValue::Completed);
        assert_eq!(rt.status().error, "boom");
        // 失败后 progress 不变
        assert!((rt.status().progress - progress_before).abs() < f64::EPSILON);
    }

    #[test]
    fn progress_clamped_to_unit_interval() {
        let rt = UpdateRuntime::new();
        force_phase(&rt, UpdatePhase::Downloading, 1);
        assert!(rt.record_progress(1, 1.5, None));
        assert!((rt.status().progress - 1.0).abs() < f64::EPSILON);
        assert!(rt.record_progress(1, -0.2, None));
        assert!((rt.status().progress - 0.0).abs() < f64::EPSILON);
    }
}
