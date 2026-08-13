//! config_runtime.rs — 配置内存态的串行事务更新与 owner generation CAS
//!
//! Business Logic（为什么需要这个模块）:
//!     多个命令可能并发修改不同配置字段；若各自 clone→改→写盘→swap，会产生 lost update。
//!     需要单一 writer gate：clone → mutate → validate → durable save → memory swap。
//!     截图快捷键 OS 注册也必须与 config 事务同锁串行，避免 OS/config 分叉。
//!     跨进程（GUI→sidecar）配置更新必须带 owner/generation CAS，禁止提交完整 stale AppConfig。
//!
//! Code Logic（这个模块做什么）:
//!     `ConfigRuntime` 持有 `Arc<RwLock<AppConfig>>`、异步 `update_lock`、`ConfigStore`、
//!     owner 实例 id 与单调 `generation`；
//!     `update_config_transactionally` 串行化写路径，durable IO 走 `spawn_blocking`，
//!     且不把 std `RwLockGuard` 跨 await；成功 swap 后递增 generation。
//!     `apply_patch_if_generation` 在同一 update_lock 下校验 owner/generation 后应用 allowlist patch。

use crate::config::{
    normalize_prompt_optimizer_fill_language, AppConfig, GithubTrendingConfig, HealthConfig,
    OrchestratorAutomationConfig,
};
use crate::config_store::ConfigStore;
use crate::error::AppError;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// 配置运行时：共享内存值 + 串行 writer + 持久化后端 + owner generation。
///
/// Business Logic（为什么需要这个结构）:
///     读路径需要廉价 clone；写路径必须串行并在落盘成功后才 swap，避免半提交状态。
///     配置 CAS 需要稳定 owner 身份与单调 generation，供 GUI 对账与冲突重试。
///
/// Code Logic（这个结构做什么）:
///     `value` 供读；`update_lock` 串行事务；`store` 执行 durable save；
///     `owner_instance_id`/`generation` 在成功 memory swap 后可被 CAS 路径观测。
pub struct ConfigRuntime {
    pub value: Arc<RwLock<AppConfig>>,
    update_lock: tokio::sync::Mutex<()>,
    store: Arc<dyn ConfigStore>,
    owner_instance_id: String,
    generation: AtomicU64,
    started_at: String,
}

impl ConfigRuntime {
    /// 用已加载的配置与 store 构造 runtime（owner 为空串，generation=0）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     启动时 load 一次后注入共享状态，供命令层读写；无 owner 的路径（单元测试/过渡）仍可事务写。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `with_owner(..., "")`。
    pub fn new(initial: AppConfig, store: Arc<dyn ConfigStore>) -> Self {
        Self::with_owner(initial, store, String::new())
    }

    /// 用指定 owner 实例 id 构造 runtime。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     sidecar 启动时生成一次 owner UUID，必须同时写入 ConfigRuntime 与控制文件，
    ///     以便 control API 的 CAS 与 status 对账。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装 value/store，初始化 update_lock、generation=0、started_at=UTC now。
    pub fn with_owner(
        initial: AppConfig,
        store: Arc<dyn ConfigStore>,
        owner_instance_id: String,
    ) -> Self {
        Self {
            value: Arc::new(RwLock::new(initial)),
            update_lock: tokio::sync::Mutex::new(()),
            store,
            owner_instance_id,
            generation: AtomicU64::new(0),
            started_at: Utc::now().to_rfc3339(),
        }
    }

    /// 返回共享内存配置句柄（与 `value` 相同）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     迁移期 `AppState.config` 可与 runtime 共享同一 `Arc`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone `Arc<RwLock<AppConfig>>`。
    pub fn shared_value(&self) -> Arc<RwLock<AppConfig>> {
        Arc::clone(&self.value)
    }

    /// 返回本 runtime 的 owner 实例 id。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     control 文件写入与 status/CAS 响应需要同一 owner 身份。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 owner_instance_id 字符串切片。
    pub fn owner_instance_id(&self) -> &str {
        &self.owner_instance_id
    }

    /// 返回当前 generation（无锁快照）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     status/get-config 需要快速读取 generation；写路径在 update_lock 下 CAS 比对。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `AtomicU64::load(SeqCst)`。
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// 返回 owner 启动时间（RFC3339）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     RuntimeOwnerStatus 需要展示 sidecar 启动时间。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回构造时写入的 started_at。
    pub fn started_at(&self) -> &str {
        &self.started_at
    }

    /// 只读克隆当前内存配置。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     命令层读配置不应长时间持锁。
    ///
    /// Code Logic（这个函数做什么）:
    ///     短暂读锁后 clone 并释放。
    pub fn snapshot(&self) -> Result<AppConfig, AppError> {
        self.value
            .read()
            .map(|g| g.clone())
            .map_err(|_| AppError::generic("配置读锁中毒"))
    }

    /// 返回带 generation/owner 的配置快照。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     CAS 测试与 control get-config 需要 generation 与配置一并观测。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读配置后组装 `ConfigSnapshot`（含非敏感 fingerprint）。
    pub fn snapshot_with_generation(&self) -> Result<ConfigSnapshot, AppError> {
        let config = self.snapshot()?;
        Ok(ConfigSnapshot::from_runtime(
            &self.owner_instance_id,
            self.generation(),
            &config,
        ))
    }

    /// 构造运行时 owner 状态（供 control status）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 诊断与对账需要 owner/generation/fingerprint；终端/bridge 计数由调用方注入。
    ///
    /// Code Logic（这个函数做什么）:
    ///     组装 `RuntimeOwnerStatus`；metrics 与 bridge 快照由参数填入。
    pub fn owner_status(
        &self,
        terminal_session_count: usize,
        bridge_count: usize,
        cloud_sync_phase: &str,
        orchestrator: OrchestratorRuntimeSummary,
    ) -> Result<RuntimeOwnerStatus, AppError> {
        self.owner_status_with_bridges(
            terminal_session_count,
            bridge_count,
            cloud_sync_phase,
            orchestrator,
            Vec::new(),
        )
    }

    /// 构造含 bridge 脱敏快照的 owner 状态。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Task 6 诊断需要 phases/error codes，但仍禁止 token/内容。
    ///
    /// Code Logic（这个函数做什么）:
    ///     与 `owner_status` 相同，并附带 `bridges` 快照列表。
    pub fn owner_status_with_bridges(
        &self,
        terminal_session_count: usize,
        bridge_count: usize,
        cloud_sync_phase: &str,
        orchestrator: OrchestratorRuntimeSummary,
        bridges: Vec<crate::workbench::remote_events::RemoteEventBridgeSnapshot>,
    ) -> Result<RuntimeOwnerStatus, AppError> {
        let config = self.snapshot()?;
        Ok(RuntimeOwnerStatus {
            owner_instance_id: self.owner_instance_id.clone(),
            generation: self.generation(),
            started_at: self.started_at.clone(),
            config_fingerprint: config_fingerprint(&config),
            cloud_sync_phase: cloud_sync_phase.to_string(),
            terminal_session_count,
            bridge_count,
            bridges,
            orchestrator,
        })
    }

    /// 获取串行 writer 锁（热键 OS 切换等需与 config 事务同临界区时使用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     截图快捷键 OS 注册必须与落盘/内存 swap 串行，命令层需要显式持有同一把 gate。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `await` 异步 `update_lock`，返回守卫；持有期间其它 writer 阻塞。
    pub async fn lock_for_update(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.update_lock.lock().await
    }

    /// 返回可克隆的 store 句柄（供持锁路径 `spawn_blocking(save_atomic)`）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     热键同锁路径需在命令层自行 spawn_blocking 落盘，但仍必须走同一 ConfigStore。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone `Arc<dyn ConfigStore>`。
    pub fn store_handle(&self) -> Arc<dyn ConfigStore> {
        Arc::clone(&self.store)
    }

    /// 将已落盘成功的 candidate 写入内存并递增 generation（仅在持有 update_lock 且 save 成功后调用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     热键同锁路径在锁外 spawn_blocking 落盘后，需在同一临界区完成 memory swap 与 generation 递增。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写锁覆盖 `value`，再 `fetch_add(1)` generation。
    pub fn swap_memory(&self, candidate: AppConfig) -> Result<(), AppError> {
        self.commit_memory_swap(candidate)?;
        Ok(())
    }

    /// 在 owner/generation CAS 下应用 allowlist patch。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 不得提交完整 stale AppConfig；必须用 expected generation + allowlist patch
    ///     更新 sidecar 权威配置，冲突时刷新后重试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持有 `update_lock`：校验 owner/generation → 应用 patch → validate →
    ///     spawn_blocking save_atomic → memory swap → generation+1；
    ///     失败不 swap、不递增。
    pub async fn apply_patch_if_generation(
        &self,
        expected_owner_instance_id: &str,
        expected_generation: u64,
        patch: RuntimeConfigPatch,
    ) -> Result<ConfigUpdateResponse, AppError> {
        let _guard = self.update_lock.lock().await;

        if self.owner_instance_id != expected_owner_instance_id {
            return Err(AppError::conflict("config_owner_conflict"));
        }
        let current_gen = self.generation.load(Ordering::SeqCst);
        if current_gen != expected_generation {
            return Err(AppError::conflict("config_generation_conflict"));
        }

        let mut candidate = {
            let read = self
                .value
                .read()
                .map_err(|_| AppError::generic("配置读锁中毒"))?;
            read.clone()
        };

        patch.apply_to(&mut candidate)?;
        candidate.validate()?;

        let store = Arc::clone(&self.store);
        let to_save = candidate.clone();
        let save_result = tokio::task::spawn_blocking(move || store.save_atomic(&to_save))
            .await
            .map_err(|e| AppError::generic(format!("配置落盘任务失败: {e}")))?;
        save_result?;

        let generation = self.commit_memory_swap(candidate.clone())?;
        Ok(ConfigUpdateResponse {
            owner_instance_id: self.owner_instance_id.clone(),
            generation,
            snapshot: ConfigSnapshot::from_runtime(&self.owner_instance_id, generation, &candidate),
        })
    }

    /// memory swap + generation 递增（调用方须已持 update_lock 且 durable save 成功）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     所有成功配置替换必须共享同一 commit 点语义：内存与 generation 同步前进。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写锁替换 value；`generation.fetch_add(1, SeqCst)+1` 返回新值。
    fn commit_memory_swap(&self, candidate: AppConfig) -> Result<u64, AppError> {
        let mut write = self
            .value
            .write()
            .map_err(|_| AppError::generic("配置写锁中毒"))?;
        *write = candidate;
        drop(write);
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(generation)
    }

    /// 测试专用：强制设置 generation。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     CAS 单测需要从指定 generation 起步，而不必先成功提交 N 次。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `store` 指定 generation。
    #[cfg(test)]
    pub fn force_generation_for_test(&self, generation: u64) {
        self.generation.store(generation, Ordering::SeqCst);
    }
}

/// 串行事务更新配置：clone → mutate → validate → save_atomic → swap → generation++。
///
/// Business Logic（为什么需要这个函数）:
///     所有配置 writer 必须经此 helper，保证失败时内存与旧文件不变，成功时字段合并不丢。
///
/// Code Logic（这个函数做什么）:
///     1) 持有异步 `update_lock` 全程；2) 读锁 clone candidate 后立即释放；
///     3) mutate；4) `validate`；5) `spawn_blocking(store.save_atomic)`（不阻塞 runtime worker）；
///     6) 写锁 swap 内存并递增 generation；返回提交后的配置与 mutate 结果。
///     错误路径不做 memory swap（rename 已提交时 store 返回 Ok，故仍会 swap）。
///     热键 OS 副作用请走 `lock_for_update` + 命令层同临界区路径，不经过本 helper 的闭包钩子。
pub async fn update_config_transactionally<T, F>(
    runtime: &ConfigRuntime,
    mutate: F,
) -> Result<(AppConfig, T), AppError>
where
    F: FnOnce(&mut AppConfig) -> Result<T, AppError>,
{
    let _guard = runtime.update_lock.lock().await;

    let mut candidate = {
        let read = runtime
            .value
            .read()
            .map_err(|_| AppError::generic("配置读锁中毒"))?;
        read.clone()
    }; // 读锁在此释放，绝不跨 await

    let result = mutate(&mut candidate)?;
    candidate.validate()?;

    let store = Arc::clone(&runtime.store);
    let to_save = candidate.clone();
    // durable IO 放到 blocking 池：持有 tokio Mutex 保持单 writer，但不把 fsync 钉在 async worker 上。
    let save_result = tokio::task::spawn_blocking(move || store.save_atomic(&to_save))
        .await
        .map_err(|e| AppError::generic(format!("配置落盘任务失败: {e}")))?;
    save_result?;

    runtime.commit_memory_swap(candidate.clone())?;

    Ok((candidate, result))
}

// ---------------------------------------------------------------------------
// CAS / control DTO
// ---------------------------------------------------------------------------

/// 配置快照（owner + generation + 可展示运行配置投影）。
///
/// Business Logic（为什么需要这个结构）:
///     GUI get-config / CAS 响应对账需要 generation 与权威字段，且不含 GUI 主题/窗口偏好。
///
/// Code Logic（这个结构做什么）:
///     camelCase DTO；fingerprint 为非敏感字段摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSnapshot {
    pub owner_instance_id: String,
    pub generation: u64,
    pub config_fingerprint: String,
    pub device_name: String,
    pub receive_dir: String,
    pub http_port: i64,
    pub screenshot_hotkey: String,
    pub prompt_optimizer_hotkey: String,
    pub prompt_optimizer_fill_language: String,
    /// Prompt 库 Quick Input 面板快捷键（pynput 风格；窗口级，不走 GlobalShortcut）。
    pub prompt_quick_input_hotkey: String,
    pub cloud_sync_repo_url: Option<String>,
    pub cloud_sync_enabled: bool,
    pub cloud_sync_auto: bool,
    pub cloud_sync_interval_secs: u64,
    pub cloud_sync_branch: Option<String>,
    pub health: HealthConfig,
    pub orchestrator: OrchestratorAutomationConfig,
    pub github_trending: GithubTrendingConfig,
    pub internal_claude: crate::config::InternalClaudeConfig,
}

impl ConfigSnapshot {
    /// 从 runtime 身份与当前配置构造快照。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     status/get-config/update 成功响应共用同一投影，避免字段漂移。
    ///
    /// Code Logic（这个函数做什么）:
    ///     复制可展示运行配置字段并计算 fingerprint。
    pub fn from_runtime(owner_instance_id: &str, generation: u64, config: &AppConfig) -> Self {
        Self {
            owner_instance_id: owner_instance_id.to_string(),
            generation,
            config_fingerprint: config_fingerprint(config),
            device_name: config.device_name.clone(),
            receive_dir: config.receive_dir.clone(),
            http_port: config.http_port,
            screenshot_hotkey: config.screenshot_hotkey.clone(),
            prompt_optimizer_hotkey: config.prompt_optimizer_hotkey.clone(),
            prompt_optimizer_fill_language: normalize_prompt_optimizer_fill_language(
                &config.prompt_optimizer_fill_language,
            ),
            prompt_quick_input_hotkey: config.prompt_quick_input_hotkey.clone(),
            cloud_sync_repo_url: config.cloud_sync_repo_url.clone(),
            cloud_sync_enabled: config.cloud_sync_enabled,
            cloud_sync_auto: config.cloud_sync_auto,
            cloud_sync_interval_secs: config.cloud_sync_interval_secs,
            cloud_sync_branch: config.cloud_sync_branch.clone(),
            health: config.health.clone(),
            orchestrator: config.orchestrator.clone(),
            github_trending: config.github_trending.clone(),
            internal_claude: config.internal_claude.clone(),
        }
    }

    /// 将权威快照字段写回 GUI 本地缓存配置（不推进 GUI generation）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 成功代理 mutation 后需刷新本地只读缓存，避免设置页立刻再读到旧值；
    ///     GUI generation 非权威，仅镜像 allowlist 业务字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     覆盖 device/path/hotkey/cloud/health/orchestrator/github_trending 等投影字段。
    pub fn apply_to_local_config(&self, cfg: &mut AppConfig) {
        cfg.device_name = self.device_name.clone();
        cfg.receive_dir = self.receive_dir.clone();
        cfg.http_port = self.http_port;
        cfg.screenshot_hotkey = self.screenshot_hotkey.clone();
        cfg.prompt_optimizer_hotkey = self.prompt_optimizer_hotkey.clone();
        cfg.prompt_optimizer_fill_language =
            normalize_prompt_optimizer_fill_language(&self.prompt_optimizer_fill_language);
        cfg.prompt_quick_input_hotkey = self.prompt_quick_input_hotkey.clone();
        cfg.cloud_sync_repo_url = self.cloud_sync_repo_url.clone();
        cfg.cloud_sync_enabled = self.cloud_sync_enabled;
        cfg.cloud_sync_auto = self.cloud_sync_auto;
        cfg.cloud_sync_interval_secs = self.cloud_sync_interval_secs;
        cfg.cloud_sync_branch = self.cloud_sync_branch.clone();
        cfg.health = self.health.clone();
        cfg.orchestrator = self.orchestrator.clone();
        cfg.github_trending = self.github_trending.clone();
        cfg.internal_claude = self.internal_claude.clone();
    }
}

/// 配置 CAS 更新请求。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 必须携带 expected owner/generation 与 allowlist patch，禁止提交完整 stale AppConfig。
///
/// Code Logic（这个结构做什么）:
///     camelCase DTO，供 control update-config 反序列化。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigUpdateRequest {
    pub expected_owner_instance_id: String,
    pub expected_generation: u64,
    pub patch: RuntimeConfigPatch,
}

/// 配置 CAS 更新成功响应。
///
/// Business Logic（为什么需要这个结构）:
///     成功后 GUI 需用新 generation 与快照刷新表单/对账。
///
/// Code Logic（这个结构做什么）:
///     返回 owner、新 generation 与 ConfigSnapshot。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigUpdateResponse {
    pub owner_instance_id: String,
    pub generation: u64,
    pub snapshot: ConfigSnapshot,
}

/// 运行时 owner 状态（control status）。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 诊断与配置对账需要 owner/generation/fingerprint 与轻量 runtime 计数。
///
/// Code Logic（这个结构做什么）:
///     camelCase DTO；不含 token/Prompt/路径凭据；bridges 仅 phase/attempt/error class。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeOwnerStatus {
    pub owner_instance_id: String,
    pub generation: u64,
    pub started_at: String,
    pub config_fingerprint: String,
    pub cloud_sync_phase: String,
    pub terminal_session_count: usize,
    pub bridge_count: usize,
    /// 各 bridge 脱敏相位快照（默认空以兼容旧调用方）。
    #[serde(default)]
    pub bridges: Vec<crate::workbench::remote_events::RemoteEventBridgeSnapshot>,
    pub orchestrator: OrchestratorRuntimeSummary,
}

impl RuntimeOwnerStatus {
    /// 转为可复制的脱敏诊断摘要。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Settings「复制脱敏诊断摘要」只允许 counts/phases/error codes。
    ///
    /// Code Logic（这个函数做什么）:
    ///     映射为 `SanitizedRuntimeDiagnostics`（字段集合与 status 对齐，无额外敏感键）。
    pub fn to_sanitized_diagnostics(&self) -> SanitizedRuntimeDiagnostics {
        SanitizedRuntimeDiagnostics {
            owner_instance_id: self.owner_instance_id.clone(),
            generation: self.generation,
            started_at: self.started_at.clone(),
            config_fingerprint: self.config_fingerprint.clone(),
            cloud_sync_phase: self.cloud_sync_phase.clone(),
            terminal_session_count: self.terminal_session_count,
            bridge_count: self.bridge_count,
            bridges: self.bridges.clone(),
            orchestrator: self.orchestrator.clone(),
        }
    }
}

/// 脱敏运行诊断摘要（可复制 JSON）。
///
/// Business Logic（为什么需要这个结构）:
///     用户反馈问题需要一份不含 secret/内容的 owner 快照。
///
/// Code Logic（这个结构做什么）:
///     与 RuntimeOwnerStatus 同构；序列化后供剪贴板；测试扫描禁止 token/content 键。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SanitizedRuntimeDiagnostics {
    pub owner_instance_id: String,
    pub generation: u64,
    pub started_at: String,
    pub config_fingerprint: String,
    pub cloud_sync_phase: String,
    pub terminal_session_count: usize,
    pub bridge_count: usize,
    pub bridges: Vec<crate::workbench::remote_events::RemoteEventBridgeSnapshot>,
    pub orchestrator: OrchestratorRuntimeSummary,
}

impl SanitizedRuntimeDiagnostics {
    /// 漂亮打印 JSON（供复制）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     前端一键复制需要稳定字符串。
    ///
    /// Code Logic（这个函数做什么）:
    ///     serde_json pretty；失败回落紧凑序列化或 `{}`。
    pub fn to_pretty_json(&self) -> String {
        serde_json::to_string_pretty(self)
            .or_else(|_| serde_json::to_string(self))
            .unwrap_or_else(|_| "{}".to_string())
    }
}

/// Orchestrator 运行时摘要（status 轻量字段）。
///
/// Business Logic（为什么需要这个结构）:
///     status 只需展示最近 tick 类摘要，不暴露任务正文。
///
/// Code Logic（这个结构做什么）:
///     可序列化占位；后续 diagnostics 任务可扩展字段。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorRuntimeSummary {
    /// 最近 scheduler tick 时间（RFC3339），未知时为空。
    #[serde(default)]
    pub latest_tick_at: Option<String>,
    /// 最近错误类别（脱敏 token），未知时为空。
    #[serde(default)]
    pub latest_error_class: Option<String>,
}

/// 权威运行配置 allowlist patch（deny_unknown_fields）。
///
/// Business Logic（为什么需要这个结构）:
///     只允许改 sidecar 权威运行配置；GUI 主题/窗口与 N4 `gui-bootstrap.json` 永不进入本 DTO。
///
/// Code Logic（这个结构做什么）:
///     全部字段 Option 表示“未传则保留”；`deny_unknown_fields` 拒绝未知键。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeConfigPatch {
    #[serde(default)]
    pub device_name: Option<String>,
    #[serde(default)]
    pub receive_dir: Option<String>,
    #[serde(default)]
    pub http_port: Option<i64>,
    #[serde(default)]
    pub screenshot_hotkey: Option<String>,
    #[serde(default)]
    pub prompt_optimizer_hotkey: Option<String>,
    #[serde(default)]
    pub prompt_optimizer_fill_language: Option<String>,
    /// Prompt 库 Quick Input 面板快捷键（窗口级 keydown，不走 GlobalShortcut/hotkey.rs）。
    #[serde(default)]
    pub prompt_quick_input_hotkey: Option<String>,
    #[serde(default)]
    pub cloud_sync_repo_url: Option<String>,
    #[serde(default)]
    pub cloud_sync_enabled: Option<bool>,
    #[serde(default)]
    pub cloud_sync_auto: Option<bool>,
    #[serde(default)]
    pub cloud_sync_interval_secs: Option<u64>,
    #[serde(default)]
    pub cloud_sync_branch: Option<String>,
    #[serde(default)]
    pub health: Option<HealthRuntimePatch>,
    #[serde(default)]
    pub orchestrator: Option<OrchestratorRuntimePatch>,
    #[serde(default)]
    pub github_trending: Option<GithubTrendingRuntimePatch>,
    #[serde(default)]
    pub internal_claude: Option<InternalClaudeRuntimePatch>,
}

impl RuntimeConfigPatch {
    /// 将 allowlist patch 应用到 candidate 配置。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     CAS 写路径只改调用方显式提交的字段，未编辑字段保持当前权威值。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 Some 字段写入 candidate；空串 cloud URL/branch 归一为 None；语言字段归一化。
    pub fn apply_to(&self, cfg: &mut AppConfig) -> Result<(), AppError> {
        if let Some(ref name) = self.device_name {
            cfg.device_name = name.clone();
        }
        if let Some(ref dir) = self.receive_dir {
            cfg.receive_dir = dir.clone();
        }
        if let Some(port) = self.http_port {
            cfg.http_port = port;
        }
        if let Some(ref hotkey) = self.screenshot_hotkey {
            cfg.screenshot_hotkey = hotkey.clone();
        }
        if let Some(ref hotkey) = self.prompt_optimizer_hotkey {
            cfg.prompt_optimizer_hotkey = hotkey.clone();
        }
        if let Some(ref language) = self.prompt_optimizer_fill_language {
            cfg.prompt_optimizer_fill_language = normalize_prompt_optimizer_fill_language(language);
        }
        if let Some(ref hotkey) = self.prompt_quick_input_hotkey {
            cfg.prompt_quick_input_hotkey = hotkey.clone();
        }
        if let Some(ref url) = self.cloud_sync_repo_url {
            cfg.cloud_sync_repo_url = if url.trim().is_empty() {
                None
            } else {
                Some(url.clone())
            };
        }
        if let Some(enabled) = self.cloud_sync_enabled {
            cfg.cloud_sync_enabled = enabled;
        }
        if let Some(auto) = self.cloud_sync_auto {
            cfg.cloud_sync_auto = auto;
        }
        if let Some(interval) = self.cloud_sync_interval_secs {
            cfg.cloud_sync_interval_secs = interval.max(30);
        }
        if let Some(ref branch) = self.cloud_sync_branch {
            cfg.cloud_sync_branch = if branch.trim().is_empty() {
                None
            } else {
                Some(branch.clone())
            };
        }
        if let Some(ref health) = self.health {
            health.apply_to(&mut cfg.health);
        }
        if let Some(ref orch) = self.orchestrator {
            orch.apply_to(&mut cfg.orchestrator)?;
        }
        if let Some(ref trending) = self.github_trending {
            trending.apply_to(&mut cfg.github_trending);
        }
        if let Some(ref internal) = self.internal_claude {
            internal.apply_to(&mut cfg.internal_claude);
        }
        Ok(())
    }
}

/// Health 运行配置 allowlist patch。
///
/// Business Logic（为什么需要这个结构）:
///     健康监测参数属 sidecar 运行态，经 CAS 更新；不包含 GUI 主题。
///
/// Code Logic（这个结构做什么）:
///     Option 字段 patch；deny_unknown_fields。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthRuntimePatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub work_window_seconds: Option<i64>,
    #[serde(default)]
    pub break_seconds: Option<i64>,
    #[serde(default)]
    pub record_window_title: Option<bool>,
    #[serde(default)]
    pub retain_days: Option<i64>,
    #[serde(default)]
    pub notify_enabled: Option<bool>,
    #[serde(default)]
    pub dnd_start: Option<Option<String>>,
    #[serde(default)]
    pub dnd_end: Option<Option<String>>,
    #[serde(default)]
    pub water_interval_seconds: Option<i64>,
    /// 可配置提醒模板整表覆盖。
    #[serde(default)]
    pub reminders: Option<Vec<crate::config::HealthReminderTemplate>>,
}

impl HealthRuntimePatch {
    /// 将 health patch 应用到 candidate。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     只覆盖传入字段，保留未编辑阈值。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 Some 字段写入 HealthConfig。
    fn apply_to(&self, health: &mut HealthConfig) {
        if let Some(v) = self.enabled {
            health.enabled = v;
        }
        if let Some(v) = self.work_window_seconds {
            health.work_window_seconds = v;
        }
        if let Some(v) = self.break_seconds {
            health.break_seconds = v;
        }
        if let Some(v) = self.record_window_title {
            health.record_window_title = v;
        }
        if let Some(v) = self.retain_days {
            health.retain_days = v;
        }
        if let Some(v) = self.notify_enabled {
            health.notify_enabled = v;
        }
        if let Some(ref v) = self.dnd_start {
            health.dnd_start = v.clone();
        }
        if let Some(ref v) = self.dnd_end {
            health.dnd_end = v.clone();
        }
        if let Some(v) = self.water_interval_seconds {
            health.water_interval_seconds = v;
        }
        if let Some(ref v) = self.reminders {
            health.reminders = v.clone();
        }
    }
}

/// Orchestrator 运行配置 allowlist patch。
///
/// Business Logic（为什么需要这个结构）:
///     设备级自动化策略经 sidecar CAS 更新。
///
/// Code Logic（这个结构做什么）:
///     Option 字段；verification_commands 直接传 Vec（已归一化列表）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrchestratorRuntimePatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub max_concurrent_tasks: Option<i64>,
    #[serde(default)]
    pub verification_commands: Option<Vec<String>>,
    #[serde(default)]
    pub auto_commit: Option<bool>,
    #[serde(default)]
    pub auto_push_task_branch: Option<bool>,
    #[serde(default)]
    pub auto_merge_to_main: Option<bool>,
    #[serde(default)]
    pub auto_push_main: Option<bool>,
    #[serde(default)]
    pub notify_human_review: Option<bool>,
    #[serde(default)]
    pub notify_blocked: Option<bool>,
    #[serde(default)]
    pub notify_remote_outbox_failed: Option<bool>,
    #[serde(default)]
    pub notify_task_done: Option<bool>,
}

impl OrchestratorRuntimePatch {
    /// 将 orchestrator patch 应用到 candidate。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     CAS 路径与 Settings 自动化 tab 字段对齐。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入传入字段；max_concurrent_tasks 基础范围校验委托最终 `AppConfig::validate`。
    fn apply_to(&self, orch: &mut OrchestratorAutomationConfig) -> Result<(), AppError> {
        if let Some(v) = self.enabled {
            orch.enabled = v;
        }
        if let Some(v) = self.max_concurrent_tasks {
            orch.max_concurrent_tasks = v;
        }
        if let Some(ref cmds) = self.verification_commands {
            orch.verification_commands = cmds.clone();
        }
        if let Some(v) = self.auto_commit {
            orch.auto_commit = v;
        }
        if let Some(v) = self.auto_push_task_branch {
            orch.auto_push_task_branch = v;
        }
        if let Some(v) = self.auto_merge_to_main {
            orch.auto_merge_to_main = v;
        }
        if let Some(v) = self.auto_push_main {
            orch.auto_push_main = v;
        }
        if let Some(v) = self.notify_human_review {
            orch.notify_human_review = v;
        }
        if let Some(v) = self.notify_blocked {
            orch.notify_blocked = v;
        }
        if let Some(v) = self.notify_remote_outbox_failed {
            orch.notify_remote_outbox_failed = v;
        }
        if let Some(v) = self.notify_task_done {
            orch.notify_task_done = v;
        }
        Ok(())
    }
}

/// GitHub Trending 运行配置 allowlist patch。
///
/// Business Logic（为什么需要这个结构）:
///     CLI 路径/模型/缓存属 sidecar 运行偏好，经 CAS 更新。
///
/// Code Logic（这个结构做什么）:
///     Option 字段 + deny_unknown_fields。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GithubTrendingRuntimePatch {
    #[serde(default)]
    pub ai_enabled: Option<bool>,
    #[serde(default)]
    pub claude_cli_path: Option<String>,
    #[serde(default)]
    pub claude_model: Option<String>,
    #[serde(default)]
    pub cache_ttl_hours: Option<i64>,
}

impl GithubTrendingRuntimePatch {
    /// 将 github_trending patch 应用到 candidate。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     只覆盖传入 AI/CLI 偏好字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入 GithubTrendingConfig。
    fn apply_to(&self, trending: &mut GithubTrendingConfig) {
        if let Some(v) = self.ai_enabled {
            trending.ai_enabled = v;
        }
        if let Some(ref path) = self.claude_cli_path {
            trending.claude_cli_path = path.clone();
        }
        if let Some(ref model) = self.claude_model {
            trending.claude_model = model.clone();
        }
        if let Some(v) = self.cache_ttl_hours {
            trending.cache_ttl_hours = v;
        }
    }
}

/// cc-partner 内部 Claude provider 覆盖 patch。
///
/// Business Logic（为什么需要这个结构）:
///     设置页 AI tab 选择「内部 Claude provider」，经 CAS 写入 sidecar 权威配置；
///     仅 provider_id 一个字段，空串归一为 None（= 沿用 OS 默认）。
///
/// Code Logic（这个结构做什么）:
///     Option 字段 + deny_unknown_fields；apply_to 把空串/None 统一回退为 None。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalClaudeRuntimePatch {
    #[serde(default)]
    pub provider_id: Option<String>,
}

impl InternalClaudeRuntimePatch {
    /// 将 internal_claude patch 应用到 candidate。
    ///
    /// Code Logic（这个函数做什么）:
    ///     trim 后空串视为 None；非空写入 provider_id。
    fn apply_to(&self, internal: &mut crate::config::InternalClaudeConfig) {
        if let Some(ref id) = self.provider_id {
            internal.provider_id = if id.trim().is_empty() {
                None
            } else {
                Some(id.trim().to_string())
            };
        }
    }
}

/// 计算非敏感配置 fingerprint。
///
/// Business Logic（为什么需要这个函数）:
///     status/诊断需要判断配置是否一致，但不得包含 URL 凭据、token 或路径敏感内容的原始泄露路径；
///     fingerprint 用规范化摘要，不记录 control token。
///
/// Code Logic（这个函数做什么）:
///     选取非敏感运行字段构造稳定 JSON，SHA256 十六进制摘要。
pub fn config_fingerprint(config: &AppConfig) -> String {
    let payload = serde_json::json!({
        "device_name": config.device_name,
        "receive_dir": config.receive_dir,
        "http_port": config.http_port,
        "screenshot_hotkey": config.screenshot_hotkey,
        "prompt_optimizer_hotkey": config.prompt_optimizer_hotkey,
        "prompt_optimizer_fill_language": config.prompt_optimizer_fill_language,
        "prompt_quick_input_hotkey": config.prompt_quick_input_hotkey,
        "cloud_sync_enabled": config.cloud_sync_enabled,
        "cloud_sync_auto": config.cloud_sync_auto,
        "cloud_sync_interval_secs": config.cloud_sync_interval_secs,
        "cloud_sync_branch": config.cloud_sync_branch,
        "cloud_sync_repo_configured": config.cloud_sync_repo_url.as_ref().map(|u| !u.trim().is_empty()).unwrap_or(false),
        "health_enabled": config.health.enabled,
        "orchestrator_enabled": config.orchestrator.enabled,
        "orchestrator_max_concurrent_tasks": config.orchestrator.max_concurrent_tasks,
        "github_trending_ai_enabled": config.github_trending.ai_enabled,
        "github_trending_cache_ttl_hours": config.github_trending.cache_ttl_hours,
        "internal_claude_provider_id": config.internal_claude.provider_id,
    });
    let encoded = serde_json::to_vec(&payload).unwrap_or_default();
    let digest = Sha256::digest(&encoded);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig};
    use crate::config_store::{
        ConfigIoStage, FaultInjectingConfigIo, FsConfigStore, MemoryConfigStore, StdConfigIo,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Barrier;

    fn sample_config() -> AppConfig {
        AppConfig {
            device_id: "dev-rt-1".into(),
            device_name: "runtime-device".into(),
            http_port: 0,
            receive_dir: "/tmp/recv".into(),
            db_path: "/tmp/db.db".into(),
            screenshot_hotkey: "<ctrl>+<shift>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            prompt_quick_input_hotkey: "<ctrl>+/".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            internal_claude: crate::config::InternalClaudeConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
        }
    }

    /// 构造带 owner/generation 的测试 runtime。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     CAS 并发测试需要可控的 owner 与起始 generation。
    ///
    /// Code Logic（这个函数做什么）:
    ///     MemoryConfigStore + with_owner + force_generation_for_test。
    async fn test_config_runtime(owner: &str, generation: u64) -> ConfigRuntime {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = ConfigRuntime::with_owner(initial, store, owner.to_string());
        runtime.force_generation_for_test(generation);
        runtime
    }

    /// 仅改 device_name 的 allowlist patch。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     并发 CAS 测试只需互不相同的可见字段变化。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 RuntimeConfigPatch { device_name: Some(name) }。
    fn patch_name(name: &str) -> RuntimeConfigPatch {
        RuntimeConfigPatch {
            device_name: Some(name.to_string()),
            ..Default::default()
        }
    }

    /// 同一 expected generation 的并发 CAS 只允许一个 writer 成功。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI 双请求/重试不得导致 split-brain 配置；generation 必须单调 +1。
    ///
    /// Code Logic（这个测试做什么）:
    ///     并发两个 expected_generation=0 的 patch，断言恰好一个 Ok，最终 generation=1。
    #[tokio::test]
    async fn concurrent_expected_generation_allows_one_writer() {
        let runtime = Arc::new(test_config_runtime("owner-a", 0).await);
        let first = runtime.apply_patch_if_generation("owner-a", 0, patch_name("first"));
        let second = runtime.apply_patch_if_generation("owner-a", 0, patch_name("second"));
        let (a, b) = tokio::join!(first, second);
        assert_eq!([a.is_ok(), b.is_ok()].into_iter().filter(|v| *v).count(), 1);
        assert_eq!(
            runtime
                .snapshot_with_generation()
                .expect("snapshot")
                .generation,
            1
        );
    }

    /// 错误 owner 必须 conflict，不改 generation。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     旧 sidecar/错误实例不得写配置。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用错误 owner 调用 apply_patch_if_generation，断言 conflict 且 generation 不变。
    #[tokio::test]
    async fn wrong_owner_is_conflict_and_leaves_generation() {
        let runtime = test_config_runtime("owner-a", 3).await;
        let err = runtime
            .apply_patch_if_generation("owner-b", 3, patch_name("x"))
            .await
            .expect_err("wrong owner");
        assert_eq!(err.classify(), crate::error::AppErrorCategory::Conflict);
        assert_eq!(err.to_string(), "config_owner_conflict");
        assert_eq!(runtime.generation(), 3);
        assert_eq!(runtime.snapshot().unwrap().device_name, "runtime-device");
    }

    /// stale generation 必须 conflict。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI 持有过期 generation 时必须刷新后重试。
    ///
    /// Code Logic（这个测试做什么）:
    ///     expected_generation 落后当前值，断言 conflict 码。
    #[tokio::test]
    async fn stale_generation_is_conflict() {
        let runtime = test_config_runtime("owner-a", 2).await;
        let err = runtime
            .apply_patch_if_generation("owner-a", 1, patch_name("x"))
            .await
            .expect_err("stale generation");
        assert_eq!(err.to_string(), "config_generation_conflict");
        assert_eq!(runtime.generation(), 2);
    }

    /// 未知 patch 字段必须被 deny_unknown_fields 拒绝。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI 主题/未知键不得混入 sidecar 配置 DTO。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化含 theme 字段的 JSON，断言失败。
    #[test]
    fn runtime_config_patch_denies_unknown_fields() {
        let raw = r#"{"deviceName":"a","theme":"dark"}"#;
        let err = serde_json::from_str::<RuntimeConfigPatch>(raw).expect_err("theme 必须拒绝");
        let msg = err.to_string();
        assert!(
            msg.contains("unknown field") || msg.contains("theme"),
            "应拒绝未知字段: {msg}"
        );
    }

    /// 事务路径成功后 generation 递增。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     本地 writer 与 CAS 共享 generation 单调语义。
    ///
    /// Code Logic（这个测试做什么）:
    ///     update_config_transactionally 成功后 generation 从 0→1。
    #[tokio::test]
    async fn transactional_writer_increments_generation() {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = ConfigRuntime::with_owner(initial, store, "owner-a".into());
        assert_eq!(runtime.generation(), 0);
        update_config_transactionally(&runtime, |cfg| {
            cfg.device_name = "n1".into();
            Ok(())
        })
        .await
        .expect("update");
        assert_eq!(runtime.generation(), 1);
        assert_eq!(runtime.snapshot().unwrap().device_name, "n1");
    }

    /// 落盘失败不递增 generation。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     半失败不得伪装配置已替换。
    ///
    /// Code Logic（这个测试做什么）:
    ///     store.fail_next_save 后 CAS，断言 Err 且 generation 不变。
    #[tokio::test]
    async fn save_failure_does_not_increment_generation() {
        // 并行测试可能短暂设置 CC_PARTNER_DATA_DIR；清空 override，避免 sample /tmp/db.db 被隔离校验提前拒绝。
        let _data_dir_guard = crate::config::install_data_dir_env(None);
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        store.fail_next_save();
        let runtime = ConfigRuntime::with_owner(initial, store, "owner-a".into());
        let err = runtime
            .apply_patch_if_generation("owner-a", 0, patch_name("x"))
            .await
            .expect_err("save fail");
        assert!(err.to_string().contains("注入故障") || err.to_string().contains("Memory"));
        assert_eq!(runtime.generation(), 0);
    }

    #[tokio::test]
    async fn save_failure_leaves_memory_unchanged() {
        // 并行测试可能短暂设置 CC_PARTNER_DATA_DIR；清空 override，避免 sample /tmp/db.db 被隔离校验提前拒绝。
        let _data_dir_guard = crate::config::install_data_dir_env(None);
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        store.fail_next_save();
        let runtime = ConfigRuntime::new(initial.clone(), store.clone());

        let err = update_config_transactionally(&runtime, |cfg| {
            cfg.device_name = "mutated".into();
            Ok(())
        })
        .await
        .expect_err("save 失败应返回 Err");
        assert!(
            err.to_string().contains("注入故障") || err.to_string().contains("MemoryConfigStore"),
            "应是 store 注入错误: {err}"
        );

        let snap = runtime.snapshot().expect("snapshot");
        assert_eq!(snap.device_name, "runtime-device");
        assert_eq!(
            store.snapshot().unwrap().device_name,
            "runtime-device",
            "磁盘/store 侧也应保持旧值"
        );
        assert_eq!(runtime.generation(), 0, "失败不得递增 generation");
    }

    #[tokio::test]
    async fn concurrent_writers_preserve_non_conflicting_patches() {
        // 持有 CC_PARTNER_DATA_DIR 测试锁并清空 override：并行 config 测试不得让 sample 的 /tmp/db.db 触发隔离校验。
        let _data_dir_guard = crate::config::install_data_dir_env(None);
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = Arc::new(ConfigRuntime::new(initial, store));

        let barrier = Arc::new(Barrier::new(2));
        let started = Arc::new(AtomicUsize::new(0));

        let r1 = Arc::clone(&runtime);
        let b1 = Arc::clone(&barrier);
        let s1 = Arc::clone(&started);
        let t1 = tokio::spawn(async move {
            s1.fetch_add(1, Ordering::SeqCst);
            b1.wait().await;
            update_config_transactionally(&r1, |cfg| {
                cfg.device_name = "name-from-a".into();
                Ok(())
            })
            .await
        });

        let r2 = Arc::clone(&runtime);
        let b2 = Arc::clone(&barrier);
        let s2 = Arc::clone(&started);
        let t2 = tokio::spawn(async move {
            s2.fetch_add(1, Ordering::SeqCst);
            b2.wait().await;
            update_config_transactionally(&r2, |cfg| {
                cfg.receive_dir = "/tmp/from-b".into();
                Ok(())
            })
            .await
        });

        let (r_a, r_b) = tokio::join!(t1, t2);
        r_a.expect("join a").expect("update a");
        r_b.expect("join b").expect("update b");

        let final_cfg = runtime.snapshot().expect("final");
        assert_eq!(final_cfg.device_name, "name-from-a");
        assert_eq!(final_cfg.receive_dir, "/tmp/from-b");
        assert_eq!(runtime.generation(), 2);
    }

    #[tokio::test]
    async fn validate_failure_does_not_save_or_swap() {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = ConfigRuntime::new(initial.clone(), store.clone());

        let err = update_config_transactionally(&runtime, |cfg| {
            cfg.device_id = "".into();
            Ok(())
        })
        .await
        .expect_err("非法配置应失败");
        assert!(
            err.to_string().contains("device_id") || err.to_string().contains("设备"),
            "应是 validation 错误: {err}"
        );
        assert_eq!(runtime.snapshot().unwrap().device_id, initial.device_id);
        assert_eq!(store.snapshot().unwrap().device_id, initial.device_id);
        assert_eq!(runtime.generation(), 0);
    }

    /// H1：DirectorySync 故障发生在 rename 之后，内存必须跟随磁盘 NEW，避免后续 lost update。
    #[tokio::test]
    async fn directory_sync_fault_after_rename_still_swaps_memory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.json");
        let mut initial = sample_config();
        initial.device_name = "old-mem".into();
        initial.receive_dir = temp.path().join("recv").to_string_lossy().to_string();
        initial.db_path = temp.path().join("db.db").to_string_lossy().to_string();

        let seed = FsConfigStore::new(path.clone(), Arc::new(StdConfigIo));
        seed.save_atomic(&initial).expect("seed");

        let io = Arc::new(FaultInjectingConfigIo::fail_once(
            Arc::new(StdConfigIo),
            ConfigIoStage::DirectorySync,
        ));
        let store: Arc<dyn ConfigStore> = Arc::new(FsConfigStore::new(path.clone(), io));
        let runtime = ConfigRuntime::new(initial.clone(), store);

        let (committed, _) = update_config_transactionally(&runtime, |cfg| {
            cfg.device_name = "new-after-rename".into();
            Ok(())
        })
        .await
        .expect("rename 后 DirectorySync 失败仍应提交");

        assert_eq!(committed.device_name, "new-after-rename");
        let mem = runtime.snapshot().expect("snapshot");
        assert_eq!(
            mem.device_name, "new-after-rename",
            "内存必须 swap 到 NEW，禁止 disk=NEW/memory=OLD"
        );
        let disk: AppConfig =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(disk.device_name, "new-after-rename");
        assert_eq!(runtime.generation(), 1);

        // 后续 writer 基于 NEW 内存，不会丢失已提交字段。
        let (next, _) = update_config_transactionally(&runtime, |cfg| {
            cfg.receive_dir = "/tmp/only-recv".into();
            Ok(())
        })
        .await
        .expect("后续更新");
        assert_eq!(next.device_name, "new-after-rename");
        assert_eq!(next.receive_dir, "/tmp/only-recv");
        assert_eq!(runtime.generation(), 2);
    }

    /// H2：`lock_for_update` 与 config 事务同锁；并发 side-effect 不得交错。
    #[tokio::test]
    async fn lock_for_update_serializes_side_effects_with_writers() {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = Arc::new(ConfigRuntime::new(initial, store));
        let concurrent = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let spawn_one = |rt: Arc<ConfigRuntime>,
                         c: Arc<AtomicUsize>,
                         p: Arc<AtomicUsize>,
                         b: Arc<Barrier>,
                         name: &'static str| {
            tokio::spawn(async move {
                b.wait().await;
                let _guard = rt.lock_for_update().await;
                let now = c.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(now, Ordering::SeqCst);
                // 模拟 OS 热键替换耗时
                std::thread::sleep(std::time::Duration::from_millis(30));
                let mut candidate = rt.snapshot().unwrap();
                candidate.device_name = name.into();
                let store = rt.store_handle();
                let to_save = candidate.clone();
                store.save_atomic(&to_save).unwrap();
                rt.swap_memory(candidate).unwrap();
                c.fetch_sub(1, Ordering::SeqCst);
                Ok::<(), AppError>(())
            })
        };

        let t1 = spawn_one(
            runtime.clone(),
            concurrent.clone(),
            peak.clone(),
            barrier.clone(),
            "a",
        );
        let t2 = spawn_one(
            runtime.clone(),
            concurrent.clone(),
            peak.clone(),
            barrier.clone(),
            "b",
        );
        t1.await.unwrap().unwrap();
        t2.await.unwrap().unwrap();
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "side-effect 不得与其它 writer 重叠"
        );
        assert_eq!(runtime.generation(), 2);
    }

    /// 持锁路径 save 失败时不得 swap 内存（补偿由命令层负责）。
    #[tokio::test]
    async fn locked_path_save_failure_does_not_swap() {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        store.fail_next_save();
        let runtime = ConfigRuntime::new(initial, store);

        let _guard = runtime.lock_for_update().await;
        let mut candidate = runtime.snapshot().unwrap();
        candidate.device_name = "x".into();
        let err = runtime
            .store_handle()
            .save_atomic(&candidate)
            .expect_err("save 应失败");
        assert!(err.to_string().contains("注入故障"));
        // 故意不 swap
        drop(_guard);
        assert_eq!(runtime.snapshot().unwrap().device_name, "runtime-device");
        assert_eq!(runtime.generation(), 0);
    }
}
