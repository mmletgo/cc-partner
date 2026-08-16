//! agent_hub/projection/scheduler — durable projection job 调度
//!
//! Business Logic（为什么需要这个模块）:
//!     owner 进程需要持久 job 入队、按资产串行/全局并行 4、冲突冻结与 crash 对账恢复。
//!
//! Code Logic（这个模块做什么）:
//!     ProjectionScheduler::enqueue_projection / run_ready_jobs / recover_on_startup；
//!     未 opt-in 项目过滤；canonical/target conflict 冻结；atomic write + materialization commit。

use crate::agent_hub::models::{
    AgentTarget, DesiredPresence, MaterializationStatus, NewMaterialization, NewProjectionJob,
    ProjectionJob, ProjectionJobState, ProjectionPayloadKind, RevisionId,
};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::projection::atomic_writer::{
    AtomicProjectionWriter, AtomicWriteOutcome, DirectoryWriteRequest, FileWriteRequest,
    ProjectionWriteFault,
};
use crate::error::AppError;
use crate::storage::AgentHubRepo;
use futures_util::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// 全局并行投影资产上限。
pub const MAX_GLOBAL_PROJECTION_PARALLELISM: usize = 4;

/// package 激活阶段推进结果。
///
/// Business Logic（为什么需要这个枚举）:
///     ActivationRequired / Unsupported 不得变成 committed/full；blocked 与 verified 分流。
///
/// Code Logic（这个枚举做什么）:
///     描述下一状态或终态决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageActivationAdvance {
    /// 进入下一阶段
    Next(ProjectionJobState),
    /// 可提交 materialization（activationVerified 之后或无需激活）
    CommitReady,
    /// 阻塞（support blocked / activation required / unsupported）
    Block {
        /// materialization 状态
        status: MaterializationStatus,
        /// 稳定原因 token
        reason: String,
    },
}

/// 判定目标路径是否为 managed package 物化根下的路径。
///
/// Business Logic: 指令文件投影不得进入 package 激活阶段。
/// Code Logic: 路径包含 `agent-hub/materialized-packages`。
pub fn is_managed_package_target_path(path: &str) -> bool {
    let norm = path.replace('\\', "/");
    norm.contains("agent-hub/materialized-packages")
}

/// 判定是否为 OpenCode runtime bridge 保留路径。
///
/// Business Logic（为什么需要这个函数）:
///     `.opencode/plugins/cc-partner-runtime.ts` 是 app 派生物，不是用户 Plugin / Snapshot 资产；
///     portable 扫描命中匹配字节应忽略，不同字节 externalCollision，禁止投影 job 静默覆盖。
///
/// Code Logic（这个函数做什么）:
///     规范化相对路径后与 `OPENCODE_RUNTIME_BRIDGE_REL_PATH` 精确比较。
pub fn is_opencode_runtime_bridge_reserved_path(path: &str) -> bool {
    use crate::workbench::agent_runtime::opencode_bridge::OPENCODE_RUNTIME_BRIDGE_REL_PATH;
    let norm = path
        .trim_start_matches("./")
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_string();
    // 允许绝对路径后缀匹配
    norm == OPENCODE_RUNTIME_BRIDGE_REL_PATH
        || norm.ends_with(&format!("/{OPENCODE_RUNTIME_BRIDGE_REL_PATH}"))
}

/// 若路径为 bridge 保留位且磁盘字节与期望不同，返回 externalCollision 诊断。
///
/// Business Logic（为什么需要这个函数）:
///     projection/scheduler 与 portable 扫描共用碰撞检测，禁止静默 overwrite。
///
/// Code Logic（这个函数做什么）:
///     非保留路径 → Ok(None)；匹配生成 hash → Ok(Some(false=ours))；不同 → Ok(Some(true=collision))。
pub fn opencode_runtime_bridge_collision(
    absolute_or_relative_path: &str,
    file_bytes: Option<&[u8]>,
) -> Option<bool> {
    if !is_opencode_runtime_bridge_reserved_path(absolute_or_relative_path) {
        return None;
    }
    let Some(bytes) = file_bytes else {
        // 缺失：不是 collision，调用方应 materialize
        return Some(false);
    };
    use crate::workbench::agent_runtime::opencode_bridge::OpenCodeRuntimeBridge;
    // Some(true)=ours / Some(false)=collision in classify; invert to collision flag
    match OpenCodeRuntimeBridge::classify_reserved_path(
        crate::workbench::agent_runtime::opencode_bridge::OPENCODE_RUNTIME_BRIDGE_REL_PATH,
        bytes,
    ) {
        Some(true) => Some(false), // ours, not collision
        Some(false) => Some(true), // external collision
        None => None,
    }
}

/// package 激活状态机：当前状态 + inspect/apply 结果 → 下一动作。
///
/// Business Logic（为什么需要这个函数）:
///     recovery 必须先 inspect 再决定是否重复 CLI 命令；ActivationRequired 永不 committed。
///
/// Code Logic（这个函数做什么）:
///     pure 决策，无 IO。
pub fn advance_package_activation(
    current: ProjectionJobState,
    inspect_present: bool,
    inspect_enabled_matches: bool,
    activation_required: bool,
    support_blocked: bool,
    apply_ok: bool,
) -> PackageActivationAdvance {
    if support_blocked {
        return PackageActivationAdvance::Block {
            status: MaterializationStatus::Blocked,
            reason: "package_activation_support_blocked".into(),
        };
    }
    if activation_required {
        return PackageActivationAdvance::Block {
            status: MaterializationStatus::ActivationRequired,
            reason: "package_activation_required".into(),
        };
    }
    match current {
        ProjectionJobState::Prepared | ProjectionJobState::Writing => {
            PackageActivationAdvance::Next(ProjectionJobState::PackageWritten)
        }
        ProjectionJobState::PackageWritten => {
            if inspect_present && inspect_enabled_matches {
                // recovery：已符合期望，跳过重复命令
                PackageActivationAdvance::Next(ProjectionJobState::ActivationVerified)
            } else {
                PackageActivationAdvance::Next(ProjectionJobState::ActivationRequested)
            }
        }
        ProjectionJobState::ActivationRequested => {
            if apply_ok || (inspect_present && inspect_enabled_matches) {
                PackageActivationAdvance::Next(ProjectionJobState::ActivationVerified)
            } else {
                PackageActivationAdvance::Block {
                    status: MaterializationStatus::Blocked,
                    reason: "package_activation_apply_failed".into(),
                }
            }
        }
        ProjectionJobState::ActivationVerified => PackageActivationAdvance::CommitReady,
        other => PackageActivationAdvance::Block {
            status: MaterializationStatus::Blocked,
            reason: format!("package_activation_invalid_state:{}", other.as_str()),
        },
    }
}

/// 投影请求（入队输入）。
///
/// Business Logic（为什么需要这个结构体）:
///     revision commit 后 service 层把渲染结果交给 scheduler 入队。
///
/// Code Logic（这个结构体做什么）:
///     携带目标路径、hash、desired 状态与 payload。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionRequest {
    /// 逻辑资产 id
    pub asset_id: String,
    /// 目标 CLI
    pub target: crate::agent_hub::models::AgentTarget,
    /// target binding id
    pub target_binding_id: String,
    /// 期望 revision
    pub desired_revision_id: Option<RevisionId>,
    /// 目标绝对路径
    pub target_path: String,
    /// 写前外部 hash
    pub expected_external_hash: Option<String>,
    /// 渲染 hash
    pub rendered_hash: String,
    /// 渲染字节（file）或 tree 序列化字节（directory fingerprint 来源）
    pub rendered_bytes: Vec<u8>,
    /// desired presence
    pub desired_presence: DesiredPresence,
    /// desired enabled
    pub desired_enabled: bool,
    /// payload 形态
    pub payload_kind: ProjectionPayloadKind,
    /// 目录条目（directory 时）
    pub directory_entries: Option<Vec<(String, Vec<u8>)>>,
    /// 受管相对路径
    pub managed_paths: Option<Vec<String>>,
    /// hub project id（None=user scope）
    pub hub_project_id: Option<String>,
    /// base hash 快照
    pub base_hash: Option<String>,
}

/// 一轮 run_ready_jobs 统计。
///
/// Business Logic（为什么需要这个结构体）:
///     owner 与测试需观察 committed/failed/skipped 计数。
///
/// Code Logic（这个结构体做什么）:
///     简单计数器。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionRunStats {
    /// 尝试处理数
    pub attempted: u32,
    /// 成功 committed
    pub committed: u32,
    /// already rendered 当 committed
    pub already_synced: u32,
    /// drift
    pub drifted: u32,
    /// 冲突/opt-in 跳过
    pub skipped: u32,
    /// 失败
    pub failed: u32,
    /// 恢复对账数
    pub recovered: u32,
}

/// 持久投影调度器。
///
/// Business Logic（为什么需要这个结构体）:
///     sidecar owner 唯一持有 writer，全局并行 4 + 同资产串行。
///
/// Code Logic（这个结构体做什么）:
///     持有 repo/object_store/semaphore/per-asset mutex map。
pub struct ProjectionScheduler {
    repo: AgentHubRepo,
    object_store: ObjectStore,
    global_slots: Arc<Semaphore>,
    asset_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    #[cfg(any(test, debug_assertions))]
    inject_fault: Mutex<Option<ProjectionWriteFault>>,
    #[cfg(any(test, debug_assertions))]
    inject_db_commit_fail: Mutex<bool>,
    /// test/debug: 跳过 support manifest RenderInstruction 门闸（生产恒 false）
    #[cfg(any(test, debug_assertions))]
    inject_support_bypass: Mutex<bool>,
}

impl ProjectionScheduler {
    /// 构造调度器。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     owner 启动后注入 repo 与 CAS。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Semaphore(4) + 空 asset lock map。
    pub fn new(repo: AgentHubRepo, object_store: ObjectStore) -> Self {
        Self {
            repo,
            object_store,
            global_slots: Arc::new(Semaphore::new(MAX_GLOBAL_PROJECTION_PARALLELISM)),
            asset_locks: Mutex::new(HashMap::new()),
            #[cfg(any(test, debug_assertions))]
            inject_fault: Mutex::new(None),
            #[cfg(any(test, debug_assertions))]
            inject_db_commit_fail: Mutex::new(false),
            #[cfg(any(test, debug_assertions))]
            inject_support_bypass: Mutex::new(false),
        }
    }

    /// 克隆内部 CAS 句柄（adoption recovery 复用同一 object root）。
    ///
    /// Business Logic: owner recovery 不得另开嵌套 CAS 根。
    /// Code Logic: clone ObjectStore。
    pub fn object_store_handle(&self) -> ObjectStore {
        self.object_store.clone()
    }

    /// 测试注入写故障。
    #[cfg(any(test, debug_assertions))]
    pub async fn inject_write_fault(&self, fault: Option<ProjectionWriteFault>) {
        *self.inject_fault.lock().await = fault;
    }

    /// 测试注入 DB commit 失败。
    #[cfg(any(test, debug_assertions))]
    pub async fn inject_db_commit_failure(&self, enabled: bool) {
        *self.inject_db_commit_fail.lock().await = enabled;
    }

    /// test/debug: 允许在 manifest 仍 block renderInstruction 时验证 writer/CAS 路径。
    ///
    /// Business Logic: 生产 fail-closed；单测需隔离 atomic writer 行为。
    /// Code Logic: 置 inject_support_bypass。
    #[cfg(any(test, debug_assertions))]
    pub async fn inject_support_bypass(&self, enabled: bool) {
        *self.inject_support_bypass.lock().await = enabled;
    }

    /// 入队 projection job。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     revision 提交后必须 durable 记录 job；未 opt-in 项目直接拒绝。
    ///
    /// Code Logic（这个函数做什么）:
    ///     opt-in 过滤 → conflict 检查 → put CAS → insert prepared job。
    pub async fn enqueue_projection(
        &self,
        request: ProjectionRequest,
    ) -> Result<ProjectionJob, AppError> {
        if let Some(hub_project_id) = request.hub_project_id.as_deref() {
            if !self.repo.is_hub_project_opted_in(hub_project_id).await? {
                return Err(AppError::validation(format!(
                    "agent_hub_project_not_opted_in:{hub_project_id}"
                )));
            }
        }

        if self
            .repo
            .has_unresolved_canonical_conflict(&request.asset_id)
            .await?
        {
            return Err(AppError::conflict(format!(
                "agent_hub_canonical_conflict_blocks_projection:{}",
                request.asset_id
            )));
        }
        if self
            .repo
            .has_unresolved_target_conflict(&request.asset_id, request.target)
            .await?
        {
            return Err(AppError::conflict(format!(
                "agent_hub_target_conflict_blocks_projection:{}:{}",
                request.asset_id,
                request.target.as_str()
            )));
        }

        let payload_bytes = match request.payload_kind {
            ProjectionPayloadKind::File => {
                let computed = sha256_hex(&request.rendered_bytes);
                if computed != request.rendered_hash {
                    return Err(AppError::validation(format!(
                        "agent_hub_rendered_hash_mismatch:expected={},actual={computed}",
                        request.rendered_hash
                    )));
                }
                request.rendered_bytes
            }
            ProjectionPayloadKind::Directory => {
                let entries = request.directory_entries.unwrap_or_default();
                for (_rel, bytes) in &entries {
                    let _ = self.object_store.put_blob(bytes).await?;
                }
                // directory 运行时从 CAS 取 entries JSON
                serde_json::to_vec(&entries)
                    .map_err(|e| AppError::generic(format!("dir entries serialize: {e}")))?
            }
        };

        let stored = self.object_store.put_blob(&payload_bytes).await?;

        let write_token = Uuid::new_v4().to_string();
        let managed_paths_json = request
            .managed_paths
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| AppError::generic(format!("managed_paths_json: {e}")))?;

        let job = self
            .repo
            .insert_projection_job(NewProjectionJob {
                asset_id: request.asset_id,
                target: request.target,
                target_binding_id: request.target_binding_id,
                desired_revision_id: request.desired_revision_id,
                target_path: request.target_path,
                expected_external_hash: request.expected_external_hash,
                rendered_hash: request.rendered_hash,
                rendered_object_hash: stored.hash,
                write_token,
                desired_presence: request.desired_presence,
                desired_enabled: request.desired_enabled,
                payload_kind: request.payload_kind,
                managed_paths_json,
                hub_project_id: request.hub_project_id,
                base_hash: request.base_hash,
            })
            .await?;
        Ok(job)
    }

    /// 运行 ready prepared jobs。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     owner tick 驱动投影；取消令牌可中断领取。
    ///
    /// Code Logic（这个函数做什么）:
    ///     list prepared → 同资产保持领取顺序串行、不同资产并发；底层 semaphore 限制全局 4。
    pub async fn run_ready_jobs(
        &self,
        cancel: &CancellationToken,
    ) -> Result<ProjectionRunStats, AppError> {
        if cancel.is_cancelled() {
            return Ok(ProjectionRunStats::default());
        }
        let jobs = self.repo.list_prepared_projection_jobs(32).await?;
        let mut jobs_by_asset: BTreeMap<String, Vec<ProjectionJob>> = BTreeMap::new();
        for job in jobs {
            jobs_by_asset
                .entry(job.asset_id.clone())
                .or_default()
                .push(job);
        }

        let mut groups = FuturesUnordered::new();
        for jobs in jobs_by_asset.into_values() {
            groups.push(async move {
                let mut group_stats = ProjectionRunStats::default();
                for job in jobs {
                    if cancel.is_cancelled() {
                        break;
                    }
                    group_stats.attempted += 1;
                    record_job_result(&mut group_stats, self.execute_job(job).await);
                }
                group_stats
            });
        }

        let mut stats = ProjectionRunStats::default();
        while let Some(group_stats) = groups.next().await {
            merge_run_stats(&mut stats, &group_stats);
            if cancel.is_cancelled() {
                // 已启动的 group 会在下一条 job 前观察 cancel；继续 drain 保证 future 正常收尾。
                continue;
            }
        }
        Ok(stats)
    }

    /// owner 启动 crash recovery。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     不得仅因 DB prepared/writing 就当 committed；必须对照实际 hash。
    ///
    /// Code Logic（这个函数做什么）:
    ///     list recoverable → reconcile_prepared_job。
    pub async fn recover_on_startup(&self) -> Result<ProjectionRunStats, AppError> {
        let mut stats = ProjectionRunStats::default();
        let jobs = self.repo.list_recoverable_projection_jobs().await?;
        for job in jobs {
            stats.recovered += 1;
            match self.reconcile_recoverable_job(job).await {
                Ok(JobExecResult::Committed) | Ok(JobExecResult::AlreadySynced) => {
                    stats.committed += 1;
                }
                Ok(JobExecResult::Drifted) => stats.drifted += 1,
                Ok(JobExecResult::Skipped) => stats.skipped += 1,
                Ok(JobExecResult::Failed) => stats.failed += 1,
                Err(err) => {
                    tracing::warn!(error = %err, "projection recover error");
                    stats.failed += 1;
                }
            }
        }
        Ok(stats)
    }

    /// 对账可恢复 job。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     prepared + 目标未变 → 可重试；目标已是 rendered → commit；
    ///     目标仍是 base → 重试替换；目标双不符 → drift。
    ///     Absent 目标：文件已不存在 → 直接 commit；hash 仍匹配 managed → 重试删除；否则 drift。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读目标 hash 分支处理，绝不仅凭 DB 状态 commit。
    async fn reconcile_recoverable_job(
        &self,
        job: ProjectionJob,
    ) -> Result<JobExecResult, AppError> {
        if job.desired_presence == DesiredPresence::Absent {
            let target = PathBuf::from(&job.target_path);
            let current =
                current_target_hash(&target, job.payload_kind, job.managed_paths_json.as_deref())?;
            if current.is_none() {
                // 即使磁盘已经为空，也统一回到 execute_job 的 support gate；否则
                // recovery 会绕开执行时门禁，留下旧 manifest 物化的成功状态。
                return self.execute_job_by_id(&job.id).await;
            }
            if absent_hash_is_managed(&job, current.as_deref()) {
                self.repo
                    .update_projection_job_state(
                        &job.id,
                        ProjectionJobState::Prepared,
                        job.attempt,
                        None,
                        None,
                        None,
                    )
                    .await?;
                return self.execute_job_by_id(&job.id).await;
            }
            self.mark_absent_drift(&job, current.as_deref()).await?;
            return Ok(JobExecResult::Drifted);
        }

        let target = PathBuf::from(&job.target_path);
        let current =
            current_target_hash(&target, job.payload_kind, job.managed_paths_json.as_deref())?;

        if current.as_deref() == Some(job.rendered_hash.as_str()) {
            // 文件已是新内容，补 materialization，不能只改 job 状态
            self.commit_job_db(&job, &job.rendered_hash).await?;
            return Ok(JobExecResult::AlreadySynced);
        }

        if current.as_deref() == job.expected_external_hash.as_deref()
            || current.as_deref() == job.base_hash.as_deref()
        {
            // 目标未变或仍是 base → 重置为 prepared 后重试
            self.repo
                .update_projection_job_state(
                    &job.id,
                    ProjectionJobState::Prepared,
                    job.attempt,
                    None,
                    None,
                    None,
                )
                .await?;
            return self.execute_job_by_id(&job.id).await;
        }

        // 双不符 → drift
        self.mark_drift(&job, current.as_deref()).await?;
        Ok(JobExecResult::Drifted)
    }

    async fn execute_job_by_id(&self, id: &str) -> Result<JobExecResult, AppError> {
        let job = self
            .repo
            .get_projection_job(id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("projection job missing:{id}")))?;
        self.execute_job(job).await
    }

    /// 执行单个 job（持有资产锁 + 全局槽）。
    async fn execute_job(&self, job: ProjectionJob) -> Result<JobExecResult, AppError> {
        // 再次检查 conflict / opt-in
        if let Some(hub) = job.hub_project_id.as_deref() {
            if !self.repo.is_hub_project_opted_in(hub).await? {
                self.repo
                    .update_projection_job_state(
                        &job.id,
                        ProjectionJobState::Blocked,
                        job.attempt,
                        Some("project_not_opted_in"),
                        None,
                        None,
                    )
                    .await?;
                return Ok(JobExecResult::Skipped);
            }
        }
        if self
            .repo
            .has_unresolved_canonical_conflict(&job.asset_id)
            .await?
        {
            self.repo
                .update_projection_job_state(
                    &job.id,
                    ProjectionJobState::Blocked,
                    job.attempt,
                    Some("canonical_conflict"),
                    None,
                    None,
                )
                .await?;
            return Ok(JobExecResult::Skipped);
        }
        if self
            .repo
            .has_unresolved_target_conflict(&job.asset_id, job.target)
            .await?
        {
            self.repo
                .update_projection_job_state(
                    &job.id,
                    ProjectionJobState::Blocked,
                    job.attempt,
                    Some("target_conflict"),
                    None,
                    None,
                )
                .await?;
            return Ok(JobExecResult::Skipped);
        }

        // checkout blocked：预存 AGENTS.md 冲突时禁止 Present 写覆盖
        if job.desired_presence == DesiredPresence::Present {
            if let Some(reason) = self.checkout_write_block_reason(&job).await? {
                self.repo
                    .update_projection_job_state(
                        &job.id,
                        ProjectionJobState::Blocked,
                        job.attempt,
                        Some(&reason),
                        None,
                        None,
                    )
                    .await?;
                self.repo
                    .upsert_materialization(NewMaterialization {
                        asset_id: job.asset_id.clone(),
                        target: job.target,
                        target_binding_id: job.target_binding_id.clone(),
                        native_path: Some(job.target_path.clone()),
                        last_projected_revision_id: None,
                        rendered_hash: Some(job.rendered_hash.clone()),
                        observed_external_hash: current_target_hash(
                            Path::new(&job.target_path),
                            job.payload_kind,
                            job.managed_paths_json.as_deref(),
                        )?,
                        status: MaterializationStatus::Blocked,
                        last_error: Some(reason),
                    })
                    .await?;
                return Ok(JobExecResult::Skipped);
            }
        }

        // 外部整文件/目录删除 → detached：Present 不得自动重建；需 restore_detached_target。
        if job.desired_presence == DesiredPresence::Present {
            if let Some(mat) = self
                .repo
                .get_materialization_by_binding(&job.target_binding_id)
                .await?
            {
                if mat.status == MaterializationStatus::Detached {
                    self.repo
                        .update_projection_job_state(
                            &job.id,
                            ProjectionJobState::Blocked,
                            job.attempt,
                            Some("detached_no_auto_recreate"),
                            None,
                            None,
                        )
                        .await?;
                    // 保持 Detached 观测，不写盘
                    self.repo
                        .upsert_materialization(NewMaterialization {
                            asset_id: mat.asset_id,
                            target: mat.target,
                            target_binding_id: mat.target_binding_id,
                            native_path: mat.native_path.or(Some(job.target_path.clone())),
                            last_projected_revision_id: mat.last_projected_revision_id,
                            rendered_hash: mat.rendered_hash,
                            observed_external_hash: None,
                            status: MaterializationStatus::Detached,
                            last_error: Some("detached_no_auto_recreate".into()),
                        })
                        .await?;
                    return Ok(JobExecResult::Skipped);
                }
            }
        }

        // Present 与 Absent 共用同一处执行时 support gate。这里位于所有
        // detached/分支决策之后，任何后续路径只允许进入统一写盘处理。
        if let Some(reason) = self.support_render_block_reason(job.target).await {
            return self.block_job_for_support(&job, reason).await;
        }

        if job.desired_presence == DesiredPresence::Absent {
            return self.handle_absent_job(job).await;
        }

        let asset_lock = self.asset_lock(&job.asset_id).await;
        let _asset_guard = asset_lock.lock().await;
        let _slot = self
            .global_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::generic("projection semaphore closed"))?;

        // 锁/槽等待期间 manifest 也可能变化；写入前再做一次最终 gate。
        if let Some(reason) = self.support_render_block_reason(job.target).await {
            return self.block_job_for_support(&job, reason).await;
        }

        let attempt = job.attempt.saturating_add(1);
        self.repo
            .update_projection_job_state(
                &job.id,
                ProjectionJobState::Writing,
                attempt,
                None,
                None,
                None,
            )
            .await?;

        let bytes = self
            .object_store
            .get_blob(&job.rendered_object_hash)
            .await?;

        let writer = self.build_writer().await;
        let target = PathBuf::from(&job.target_path);

        let outcome = match job.payload_kind {
            ProjectionPayloadKind::File => writer.write_file(FileWriteRequest {
                target: &target,
                rendered_bytes: &bytes,
                rendered_hash: &job.rendered_hash,
                expected_external_hash: job.expected_external_hash.as_deref(),
            }),
            ProjectionPayloadKind::Directory => {
                let entries: Vec<(String, Vec<u8>)> = serde_json::from_slice(&bytes)
                    .map_err(|e| AppError::generic(format!("dir payload decode: {e}")))?;
                let managed = job
                    .managed_paths_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
                    .unwrap_or_else(|| entries.iter().map(|(p, _)| p.clone()).collect());
                writer.write_directory(DirectoryWriteRequest {
                    target_dir: &target,
                    managed_paths: &managed,
                    entries: &entries,
                    rendered_hash: &job.rendered_hash,
                    expected_external_hash: job.expected_external_hash.as_deref(),
                })
            }
        };

        match outcome {
            Ok(AtomicWriteOutcome::Replaced {
                target_hash,
                backup_path,
                ..
            }) => {
                #[cfg(any(test, debug_assertions))]
                if *self.inject_db_commit_fail.lock().await {
                    // 模拟 DB commit 失败：文件可能已新，job 保持 writing/prepared 供 recovery
                    self.repo
                        .update_projection_job_state(
                            &job.id,
                            ProjectionJobState::Prepared,
                            attempt,
                            Some("agent_hub_projection_injected_fault:db_commit"),
                            None,
                            backup_path
                                .as_ref()
                                .map(|p| p.to_string_lossy().to_string())
                                .as_deref(),
                        )
                        .await?;
                    return Err(AppError::generic(
                        "agent_hub_projection_injected_fault:db_commit",
                    ));
                }
                self.commit_job_db(&job, &target_hash).await?;
                if let Some(bak) = backup_path {
                    let _ = AtomicProjectionWriter::delete_backup_after_commit(&bak);
                }
                Ok(JobExecResult::Committed)
            }
            Ok(AtomicWriteOutcome::AlreadyRendered { target_hash }) => {
                self.commit_job_db(&job, &target_hash).await?;
                Ok(JobExecResult::AlreadySynced)
            }
            Ok(AtomicWriteOutcome::Drift { current_hash }) => {
                self.mark_drift(&job, current_hash.as_deref()).await?;
                Ok(JobExecResult::Drifted)
            }
            Ok(AtomicWriteOutcome::DirectoryUnknownFiles { unknown_paths }) => {
                let msg = format!("unknown_files:{}", unknown_paths.join(","));
                self.mark_drift(&job, None).await?;
                self.repo
                    .update_projection_job_state(
                        &job.id,
                        ProjectionJobState::Drifted,
                        attempt,
                        Some(&msg),
                        None,
                        None,
                    )
                    .await?;
                Ok(JobExecResult::Drifted)
            }
            Err(err) => {
                let msg = err.to_string();
                self.repo
                    .update_projection_job_state(
                        &job.id,
                        ProjectionJobState::Failed,
                        attempt,
                        Some(&msg),
                        None,
                        None,
                    )
                    .await?;
                // 可恢复：把 failed 中 prepared 可再试的仍标记 prepared 若目标未变
                if msg.contains("injected_fault") {
                    // 保持目标不变；转为 prepared 便于 retry 测试
                    self.repo
                        .update_projection_job_state(
                            &job.id,
                            ProjectionJobState::Prepared,
                            attempt,
                            Some(&msg),
                            None,
                            None,
                        )
                        .await?;
                }
                Ok(JobExecResult::Failed)
            }
        }
    }

    async fn commit_job_db(
        &self,
        job: &ProjectionJob,
        observed_hash: &str,
    ) -> Result<(), AppError> {
        self.repo.commit_projection_job(job, observed_hash).await?;
        Ok(())
    }

    async fn mark_drift(
        &self,
        job: &ProjectionJob,
        current_hash: Option<&str>,
    ) -> Result<(), AppError> {
        self.repo
            .update_projection_job_state(
                &job.id,
                ProjectionJobState::Drifted,
                job.attempt,
                Some("precondition_or_external_drift"),
                None,
                None,
            )
            .await?;
        self.repo
            .upsert_materialization(NewMaterialization {
                asset_id: job.asset_id.clone(),
                target: job.target,
                target_binding_id: job.target_binding_id.clone(),
                native_path: Some(job.target_path.clone()),
                last_projected_revision_id: None,
                rendered_hash: Some(job.rendered_hash.clone()),
                observed_external_hash: current_hash.map(|s| s.to_string()),
                status: MaterializationStatus::Drift,
                last_error: Some("precondition_or_external_drift".into()),
            })
            .await?;
        Ok(())
    }

    /// 安全执行 desiredPresence=Absent（删除受管文件或确认已不存在）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Present→Absent 必须只删除 Hub 已知受管内容；外部改动不得静默删。
    ///
    /// Code Logic（这个函数做什么）:
    ///     文件不存在 → Synced commit；hash 命中 rendered/expected/base → remove_file 后 Synced；
    ///     否则 Drift + absent_blocked_external_divergence，禁止删除。
    async fn handle_absent_job(&self, job: ProjectionJob) -> Result<JobExecResult, AppError> {
        let asset_lock = self.asset_lock(&job.asset_id).await;
        let _asset_guard = asset_lock.lock().await;
        let _slot = self
            .global_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::generic("projection semaphore closed"))?;

        // 删除同样属于 mutation；锁/槽等待期间 capability 回落时不得继续 remove。
        if let Some(reason) = self.support_render_block_reason(job.target).await {
            return self.block_job_for_support(&job, reason).await;
        }

        let attempt = job.attempt.saturating_add(1);
        self.repo
            .update_projection_job_state(
                &job.id,
                ProjectionJobState::Writing,
                attempt,
                None,
                None,
                None,
            )
            .await?;

        let target = PathBuf::from(&job.target_path);
        let current =
            current_target_hash(&target, job.payload_kind, job.managed_paths_json.as_deref())?;

        match current {
            None => {
                // 已符合 absent：无写盘，materialization Synced
                self.commit_absent_job_db(&job).await?;
                Ok(JobExecResult::AlreadySynced)
            }
            Some(ref hash) if absent_hash_is_managed(&job, Some(hash.as_str())) => {
                // 仅当内容仍是 Hub 管理的已知 hash 才安全删除
                if job.payload_kind == ProjectionPayloadKind::File {
                    if target.is_file() {
                        std::fs::remove_file(&target).map_err(|e| {
                            AppError::generic(format!("absent remove_file failed: {e}"))
                        })?;
                    }
                } else {
                    // directory absent：只删 managed_paths 内文件，且每项 hash 已由 fingerprint 对齐
                    let managed: Vec<String> = job
                        .managed_paths_json
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default();
                    for rel in managed {
                        let p = target.join(&rel);
                        if p.is_file() {
                            let _ = std::fs::remove_file(&p);
                        }
                    }
                }
                // 删除后再确认不存在
                let after = current_target_hash(
                    &target,
                    job.payload_kind,
                    job.managed_paths_json.as_deref(),
                )?;
                if after.is_some() && job.payload_kind == ProjectionPayloadKind::File {
                    // 文件仍在 → 失败
                    self.repo
                        .update_projection_job_state(
                            &job.id,
                            ProjectionJobState::Failed,
                            attempt,
                            Some("absent_delete_still_present"),
                            None,
                            None,
                        )
                        .await?;
                    return Ok(JobExecResult::Failed);
                }
                self.commit_absent_job_db(&job).await?;
                Ok(JobExecResult::Committed)
            }
            Some(ref hash) => {
                self.mark_absent_drift(&job, Some(hash.as_str())).await?;
                Ok(JobExecResult::Drifted)
            }
        }
    }

    /// Absent 成功：job committed + materialization Synced（matches desired absent）。
    ///
    /// Business Logic: 文件已不存在或已安全删除即视为与 desired absent 对齐。
    /// Code Logic: 复用 commit_projection_job，observed 用空串 hash（文件不存在）。
    async fn commit_absent_job_db(&self, job: &ProjectionJob) -> Result<(), AppError> {
        let empty_hash = sha256_hex(b"");
        self.repo.commit_projection_job(job, &empty_hash).await?;
        // 将 rendered_hash 观测为空（absent）；若 commit 写入了 job.rendered_hash，再 upsert 澄清
        self.repo
            .upsert_materialization(NewMaterialization {
                asset_id: job.asset_id.clone(),
                target: job.target,
                target_binding_id: job.target_binding_id.clone(),
                native_path: Some(job.target_path.clone()),
                last_projected_revision_id: job.desired_revision_id.clone(),
                rendered_hash: None,
                observed_external_hash: None,
                status: MaterializationStatus::Synced,
                last_error: None,
            })
            .await?;
        Ok(())
    }

    /// Absent 因外部漂移阻塞删除。
    ///
    /// Business Logic: 不得静默删外部改过的文件；Attention 友好 last_error。
    /// Code Logic: job Drifted + materialization Drift + absent_blocked_external_divergence。
    async fn mark_absent_drift(
        &self,
        job: &ProjectionJob,
        current_hash: Option<&str>,
    ) -> Result<(), AppError> {
        let msg = "absent_blocked_external_divergence";
        self.repo
            .update_projection_job_state(
                &job.id,
                ProjectionJobState::Drifted,
                job.attempt,
                Some(msg),
                None,
                None,
            )
            .await?;
        self.repo
            .upsert_materialization(NewMaterialization {
                asset_id: job.asset_id.clone(),
                target: job.target,
                target_binding_id: job.target_binding_id.clone(),
                native_path: Some(job.target_path.clone()),
                last_projected_revision_id: None,
                rendered_hash: Some(job.rendered_hash.clone()),
                observed_external_hash: current_hash.map(|s| s.to_string()),
                status: MaterializationStatus::Drift,
                last_error: Some(msg.into()),
            })
            .await?;
        Ok(())
    }

    /// 将 support gate 的阻断结果持久化为 job/materialization 状态。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Present/Absent 的所有执行阶段必须共享相同的 fail-closed 结果，避免某个
    ///     分支只返回内存错误而让 Attention 看不到阻断原因。
    ///
    /// Code Logic（这个函数做什么）:
    ///     更新 job=Blocked、记录当前外部 hash，并写入 materialization=Blocked；调用方
    ///     在任何 adapter/删除操作前返回 Skipped。
    async fn block_job_for_support(
        &self,
        job: &ProjectionJob,
        reason: String,
    ) -> Result<JobExecResult, AppError> {
        self.repo
            .update_projection_job_state(
                &job.id,
                ProjectionJobState::Blocked,
                job.attempt,
                Some(&reason),
                None,
                None,
            )
            .await?;
        self.repo
            .upsert_materialization(NewMaterialization {
                asset_id: job.asset_id.clone(),
                target: job.target,
                target_binding_id: job.target_binding_id.clone(),
                native_path: Some(job.target_path.clone()),
                last_projected_revision_id: None,
                rendered_hash: Some(job.rendered_hash.clone()),
                observed_external_hash: current_target_hash(
                    Path::new(&job.target_path),
                    job.payload_kind,
                    job.managed_paths_json.as_deref(),
                )?,
                status: MaterializationStatus::Blocked,
                last_error: Some(reason),
            })
            .await?;
        Ok(JobExecResult::Skipped)
    }

    /// 若 target binding 关联 checkout 且 status=blocked，返回阻塞原因。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     预存 AGENTS.md 的 checkout 标 blocked 时，禁止 Present 写覆盖用户文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     get_target_binding → checkout_binding_id → get_checkout_binding；status=="blocked" → Some(token)。
    /// 写盘前评估 RenderInstruction support（fail-closed）。
    ///
    /// Business Logic: 入队后 CLI 版本/manifest 变化仍不得写盘。
    /// Code Logic: fresh probe + builtin manifest evaluate。
    async fn support_render_block_reason(&self, target: AgentTarget) -> Option<String> {
        #[cfg(any(test, debug_assertions))]
        {
            if *self.inject_support_bypass.lock().await {
                return None;
            }
        }
        use crate::agent_hub::support::{
            builtin_support_manifest, evaluate_target_support, CapabilitySupport,
            RuntimeProbeSnapshot, TargetCapability,
        };
        use crate::agent_hub::targets::{
            AssetAdapter, ClaudeInstructionAdapter, CodexInstructionAdapter,
            OpenCodeInstructionAdapter, TargetEnvironment,
        };
        // 与 projection_ops 一致：注入当前 process 的 home/vars/PATH，不改真实 env
        let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
        let mut vars = std::collections::BTreeMap::new();
        for key in [
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "OPENCODE_CONFIG_DIR",
            "OPENCODE_CONFIG",
            "XDG_CONFIG_HOME",
        ] {
            if let Ok(v) = std::env::var(key) {
                if !v.trim().is_empty() {
                    vars.insert(key.to_string(), v);
                }
            }
        }
        let path_entries = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        let env = TargetEnvironment {
            home,
            vars,
            path_entries,
        };
        let probe = crate::agent_hub::targets::probe_target(target, &env).ok();
        let probe = match probe {
            Some(p) => p,
            None => {
                return Some("support_probe_failed".into());
            }
        };
        let snap = RuntimeProbeSnapshot {
            target: probe.target,
            executable: probe.executable.clone(),
            version: probe.version.clone(),
            config_root: probe.config_root.clone(),
            fingerprint: probe.fingerprint.clone(),
            help_fingerprint: None,
        };
        let eval = match builtin_support_manifest() {
            Ok(m) => evaluate_target_support(&m, &snap),
            Err(_) => return Some("support_manifest_unavailable".into()),
        };
        match eval.capability(TargetCapability::RenderInstruction) {
            CapabilitySupport::Supported | CapabilitySupport::SupportedAfterRestart
                if eval.write_allowed =>
            {
                None
            }
            CapabilitySupport::Blocked => Some(
                eval.reasons
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "render_instruction_blocked".into()),
            ),
            other => Some(format!("render_instruction_not_writable:{other:?}")),
        }
    }

    async fn checkout_write_block_reason(
        &self,
        job: &ProjectionJob,
    ) -> Result<Option<String>, AppError> {
        let Some(binding) = self.repo.get_target_binding(&job.target_binding_id).await? else {
            return Ok(None);
        };
        let Some(checkout_id) = binding.checkout_binding_id.as_deref() else {
            return Ok(None);
        };
        let Some(checkout) = self.repo.get_checkout_binding(checkout_id).await? else {
            return Ok(None);
        };
        if checkout.status == "blocked" {
            return Ok(Some("checkout_binding_blocked".into()));
        }
        Ok(None)
    }

    async fn asset_lock(&self, asset_id: &str) -> Arc<Mutex<()>> {
        let mut map = self.asset_locks.lock().await;
        map.entry(asset_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn build_writer(&self) -> AtomicProjectionWriter {
        #[cfg(any(test, debug_assertions))]
        {
            if let Some(fault) = *self.inject_fault.lock().await {
                return AtomicProjectionWriter::with_fault(fault);
            }
        }
        AtomicProjectionWriter::new()
    }
}

/// 把单 job 结果累加到本轮统计，集中保持 AlreadySynced 的双计数语义。
fn record_job_result(stats: &mut ProjectionRunStats, result: Result<JobExecResult, AppError>) {
    match result {
        Ok(JobExecResult::Committed) => stats.committed += 1,
        Ok(JobExecResult::AlreadySynced) => {
            stats.already_synced += 1;
            stats.committed += 1;
        }
        Ok(JobExecResult::Drifted) => stats.drifted += 1,
        Ok(JobExecResult::Skipped) => stats.skipped += 1,
        Ok(JobExecResult::Failed) => stats.failed += 1,
        Err(err) => {
            tracing::warn!(error = %err, "projection job execute error");
            stats.failed += 1;
        }
    }
}

/// 合并不同资产执行组的统计。
fn merge_run_stats(total: &mut ProjectionRunStats, part: &ProjectionRunStats) {
    total.attempted += part.attempted;
    total.committed += part.committed;
    total.already_synced += part.already_synced;
    total.drifted += part.drifted;
    total.skipped += part.skipped;
    total.failed += part.failed;
    total.recovered += part.recovered;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobExecResult {
    Committed,
    AlreadySynced,
    Drifted,
    Skipped,
    Failed,
}

/// 判断当前文件 hash 是否仍属 Hub 受管（可安全 delete for Absent）。
///
/// Business Logic: Present→Absent 时 rendered 可能为空串 hash，需用 expected/base/rendered 任一匹配。
/// Code Logic: 与 rendered_hash / expected_external_hash / base_hash 任一相等。
fn absent_hash_is_managed(job: &ProjectionJob, current: Option<&str>) -> bool {
    let Some(hash) = current else {
        return false;
    };
    if !job.rendered_hash.is_empty() && hash == job.rendered_hash {
        return true;
    }
    if job.expected_external_hash.as_deref() == Some(hash) {
        return true;
    }
    if job.base_hash.as_deref() == Some(hash) {
        return true;
    }
    false
}

/// 计算当前目标 hash（file 或 directory fingerprint）。
fn current_target_hash(
    target: &Path,
    kind: ProjectionPayloadKind,
    managed_paths_json: Option<&str>,
) -> Result<Option<String>, AppError> {
    if !target.exists() {
        return Ok(None);
    }
    match kind {
        ProjectionPayloadKind::File => {
            let bytes = std::fs::read(target)?;
            Ok(Some(sha256_hex(&bytes)))
        }
        ProjectionPayloadKind::Directory => {
            let managed: Vec<String> = managed_paths_json
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let mut parts: Vec<String> = Vec::new();
            let mut sorted = managed;
            sorted.sort();
            for rel in sorted {
                let p = target.join(&rel);
                if p.is_file() {
                    let b = std::fs::read(&p)?;
                    parts.push(format!("{rel}:{}", sha256_hex(&b)));
                } else {
                    parts.push(format!("{rel}:missing"));
                }
            }
            Ok(Some(sha256_hex(parts.join("\n").as_bytes())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{
        AgentTarget, AssetKind, AssetPolicy, NewLogicalAsset, NewScopeNode, NewTargetBinding,
        ScopeKind,
    };
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use tempfile::tempdir;

    async fn setup() -> (ProjectionScheduler, tempfile::TempDir, AgentHubRepo) {
        let dir = tempdir().unwrap();
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        let repo = AgentHubRepo::new(pool);
        let store = ObjectStore::open(dir.path().join("objects")).unwrap();
        let sched = ProjectionScheduler::new(repo.clone(), store);
        // 单测隔离 atomic writer / job 状态机；生产路径仍 fail-closed support manifest
        sched.inject_support_bypass(true).await;
        (sched, dir, repo)
    }

    async fn seed_asset_binding(repo: &AgentHubRepo) -> (String, String) {
        let scope = repo
            .insert_scope(NewScopeNode {
                id: Some("user".into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "root".into(),
                display_name: "root".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let binding = repo
            .upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target: AgentTarget::Claude,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
            })
            .await
            .unwrap();
        (asset.id, binding.id)
    }

    fn file_req(
        asset_id: &str,
        binding_id: &str,
        path: &Path,
        bytes: &[u8],
        expected: Option<&str>,
    ) -> ProjectionRequest {
        let hash = sha256_hex(bytes);
        ProjectionRequest {
            asset_id: asset_id.into(),
            target: AgentTarget::Claude,
            target_binding_id: binding_id.into(),
            desired_revision_id: Some(RevisionId::new_v7()),
            target_path: path.to_string_lossy().to_string(),
            expected_external_hash: expected.map(|s| s.to_string()),
            rendered_hash: hash,
            rendered_bytes: bytes.to_vec(),
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
            payload_kind: ProjectionPayloadKind::File,
            directory_entries: None,
            managed_paths: None,
            hub_project_id: None,
            base_hash: expected.map(|s| s.to_string()),
        }
    }

    #[tokio::test]
    async fn enqueue_and_commit_file_projection() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("CLAUDE.md");
        std::fs::write(&target, b"old").unwrap();
        let old = sha256_hex(b"old");
        let req = file_req(&asset, &binding, &target, b"new hub", Some(&old));
        let job = sched.enqueue_projection(req).await.unwrap();
        assert_eq!(job.state, ProjectionJobState::Prepared);
        let cancel = CancellationToken::new();
        let stats = sched.run_ready_jobs(&cancel).await.unwrap();
        assert_eq!(stats.committed, 1);
        assert_eq!(std::fs::read(&target).unwrap(), b"new hub");
        let mat = repo
            .get_materialization_by_binding(&binding)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mat.status, MaterializationStatus::Synced);
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_eq!(done.state, ProjectionJobState::Committed);
    }

    #[tokio::test]
    async fn prepared_unchanged_target_is_recoverable() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("f.md");
        std::fs::write(&target, b"base").unwrap();
        let base = sha256_hex(b"base");
        let req = file_req(&asset, &binding, &target, b"next", Some(&base));
        let job = sched.enqueue_projection(req).await.unwrap();
        // 模拟 writing 崩溃
        repo.update_projection_job_state(
            &job.id,
            ProjectionJobState::Writing,
            1,
            Some("crash"),
            None,
            None,
        )
        .await
        .unwrap();
        let stats = sched.recover_on_startup().await.unwrap();
        assert!(stats.recovered >= 1);
        assert_eq!(std::fs::read(&target).unwrap(), b"next");
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_eq!(done.state, ProjectionJobState::Committed);
    }

    #[tokio::test]
    async fn target_hash_equals_rendered_marks_committed() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("f.md");
        let bytes = b"already";
        let hash = sha256_hex(bytes);
        std::fs::write(&target, bytes).unwrap();
        let mut req = file_req(&asset, &binding, &target, bytes, Some("other"));
        req.rendered_hash = hash.clone();
        let job = sched.enqueue_projection(req).await.unwrap();
        repo.update_projection_job_state(&job.id, ProjectionJobState::Writing, 1, None, None, None)
            .await
            .unwrap();
        let _ = sched.recover_on_startup().await.unwrap();
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_eq!(done.state, ProjectionJobState::Committed);
        let mat = repo
            .get_materialization_by_binding(&binding)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mat.status, MaterializationStatus::Synced);
    }

    #[tokio::test]
    async fn target_differs_from_both_marks_drift() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("f.md");
        std::fs::write(&target, b"external").unwrap();
        let req = file_req(
            &asset,
            &binding,
            &target,
            b"hub",
            Some(&sha256_hex(b"base")),
        );
        let job = sched.enqueue_projection(req).await.unwrap();
        repo.update_projection_job_state(&job.id, ProjectionJobState::Writing, 1, None, None, None)
            .await
            .unwrap();
        let stats = sched.recover_on_startup().await.unwrap();
        assert!(stats.drifted >= 1);
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_eq!(done.state, ProjectionJobState::Drifted);
        // 外部文件保留
        assert_eq!(std::fs::read(&target).unwrap(), b"external");
    }

    #[tokio::test]
    async fn project_without_opt_in_filtered_before_insert() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("f.md");
        let mut req = file_req(&asset, &binding, &target, b"x", None);
        req.hub_project_id = Some("hub-not-opted".into());
        let err = sched.enqueue_projection(req).await.unwrap_err();
        assert!(err.to_string().contains("not_opted_in"));
    }

    #[tokio::test]
    async fn canonical_conflict_freezes_all_targets() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        repo.insert_conflict(&asset, None, "{\"kind\":\"canonical\"}")
            .await
            .unwrap();
        let target = tmp.path().join("f.md");
        let req = file_req(&asset, &binding, &target, b"x", None);
        let err = sched.enqueue_projection(req).await.unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
    }

    #[tokio::test]
    async fn temp_write_fault_keeps_old_or_no_partial() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("f.md");
        std::fs::write(&target, b"old").unwrap();
        let old = sha256_hex(b"old");
        let req = file_req(&asset, &binding, &target, b"new", Some(&old));
        let job = sched.enqueue_projection(req).await.unwrap();
        sched
            .inject_write_fault(Some(ProjectionWriteFault::TempWrite))
            .await;
        let cancel = CancellationToken::new();
        let stats = sched.run_ready_jobs(&cancel).await.unwrap();
        assert_eq!(stats.failed, 1);
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        let reloaded = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert!(
            reloaded.state == ProjectionJobState::Prepared
                || reloaded.state == ProjectionJobState::Failed
        );
    }

    #[tokio::test]
    async fn db_commit_fault_then_recover_by_hash() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("f.md");
        std::fs::write(&target, b"old").unwrap();
        let old = sha256_hex(b"old");
        let new_bytes = b"brand-new";
        let req = file_req(&asset, &binding, &target, new_bytes, Some(&old));
        let job = sched.enqueue_projection(req).await.unwrap();
        sched.inject_db_commit_failure(true).await;
        let cancel = CancellationToken::new();
        let _ = sched.run_ready_jobs(&cancel).await;
        // 文件可能已新，但 job 不得仅凭 DB 当 committed
        let mid = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_ne!(mid.state, ProjectionJobState::Committed);
        sched.inject_db_commit_failure(false).await;
        // 目标若已是 rendered，recover 应 commit
        if std::fs::read(&target).unwrap() == new_bytes {
            let _ = sched.recover_on_startup().await.unwrap();
            let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
            assert_eq!(done.state, ProjectionJobState::Committed);
        }
    }

    #[tokio::test]
    async fn never_commit_prepared_from_db_only() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("f.md");
        std::fs::write(&target, b"base").unwrap();
        let base = sha256_hex(b"base");
        let req = file_req(&asset, &binding, &target, b"want", Some(&base));
        let job = sched.enqueue_projection(req).await.unwrap();
        // 目标被外部改成 third
        std::fs::write(&target, b"third").unwrap();
        repo.update_projection_job_state(&job.id, ProjectionJobState::Writing, 1, None, None, None)
            .await
            .unwrap();
        let stats = sched.recover_on_startup().await.unwrap();
        assert!(stats.drifted >= 1);
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_ne!(done.state, ProjectionJobState::Committed);
    }

    /// Absent：文件 hash 命中 expected/base → 安全删除并 Synced。
    #[tokio::test]
    async fn absent_job_deletes_when_hash_matches_expected() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("CLAUDE.md");
        let managed = b"managed content";
        std::fs::write(&target, managed).unwrap();
        let managed_hash = sha256_hex(managed);
        let empty = b"";
        let empty_hash = sha256_hex(empty);
        let mut req = file_req(&asset, &binding, &target, empty, Some(&managed_hash));
        req.desired_presence = DesiredPresence::Absent;
        req.rendered_hash = empty_hash;
        req.base_hash = Some(managed_hash.clone());
        req.expected_external_hash = Some(managed_hash);
        let job = sched.enqueue_projection(req).await.unwrap();
        let cancel = CancellationToken::new();
        let stats = sched.run_ready_jobs(&cancel).await.unwrap();
        assert_eq!(stats.committed, 1, "stats={stats:?}");
        assert!(!target.exists(), "managed file must be deleted");
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_eq!(done.state, ProjectionJobState::Committed);
        let mat = repo
            .get_materialization_by_binding(&binding)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mat.status, MaterializationStatus::Synced);
        assert!(mat.last_error.is_none());
    }

    /// Absent：外部漂移 → 不删除 + Drift + absent_blocked_external_divergence。
    #[tokio::test]
    async fn absent_job_blocks_on_external_divergence() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("CLAUDE.md");
        std::fs::write(&target, b"external divergence").unwrap();
        let empty = b"";
        let empty_hash = sha256_hex(empty);
        let mut req = file_req(
            &asset,
            &binding,
            &target,
            empty,
            Some(&sha256_hex(b"old managed")),
        );
        req.desired_presence = DesiredPresence::Absent;
        req.rendered_hash = empty_hash;
        req.base_hash = Some(sha256_hex(b"old managed"));
        let job = sched.enqueue_projection(req).await.unwrap();
        let cancel = CancellationToken::new();
        let stats = sched.run_ready_jobs(&cancel).await.unwrap();
        assert!(stats.drifted >= 1, "stats={stats:?}");
        assert!(target.exists(), "diverged file must not be deleted");
        assert_eq!(std::fs::read(&target).unwrap(), b"external divergence");
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_eq!(done.state, ProjectionJobState::Drifted);
        assert_eq!(
            done.last_error.as_deref(),
            Some("absent_blocked_external_divergence")
        );
        let mat = repo
            .get_materialization_by_binding(&binding)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mat.status, MaterializationStatus::Drift);
    }

    /// Absent：目标已不存在 → 无写盘，直接 Synced/Committed。
    #[tokio::test]
    async fn absent_job_when_file_already_missing_is_synced() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("missing.md");
        assert!(!target.exists());
        let empty = b"";
        let empty_hash = sha256_hex(empty);
        let mut req = file_req(&asset, &binding, &target, empty, None);
        req.desired_presence = DesiredPresence::Absent;
        req.rendered_hash = empty_hash;
        req.base_hash = None;
        req.expected_external_hash = None;
        let job = sched.enqueue_projection(req).await.unwrap();
        let cancel = CancellationToken::new();
        let stats = sched.run_ready_jobs(&cancel).await.unwrap();
        assert_eq!(stats.committed, 1, "stats={stats:?}");
        assert!(!target.exists());
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_eq!(done.state, ProjectionJobState::Committed);
        let mat = repo
            .get_materialization_by_binding(&binding)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mat.status, MaterializationStatus::Synced);
    }

    /// scan-only target：Present 最终 gate 必须阻断旧 allowlist，且不得写文件。
    #[tokio::test]
    async fn scan_only_support_gate_blocks_present_without_write() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("scan-only-present.md");
        std::fs::write(&target, b"old").unwrap();
        let old_hash = sha256_hex(b"old");
        let mut req = file_req(
            &asset,
            &binding,
            &target,
            b"must-not-write",
            Some(&old_hash),
        );
        // OpenCode builtin manifest is scan-only in the unsupported/unknown-version case.
        req.target = AgentTarget::OpenCode;
        sched.inject_support_bypass(false).await;

        let job = sched.enqueue_projection(req).await.unwrap();
        let stats = sched
            .run_ready_jobs(&CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(stats.skipped, 1, "stats={stats:?}");
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_eq!(done.state, ProjectionJobState::Blocked);
        let mat = repo
            .get_materialization_by_binding(&binding)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mat.status, MaterializationStatus::Blocked);
    }

    /// scan-only target：Absent 也必须经过 execute_job 最终 gate，不能绕过而删除。
    #[tokio::test]
    async fn scan_only_support_gate_blocks_absent_without_delete() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("scan-only-absent.md");
        let managed = b"managed";
        std::fs::write(&target, managed).unwrap();
        let managed_hash = sha256_hex(managed);
        let mut req = file_req(&asset, &binding, &target, b"", Some(&managed_hash));
        req.target = AgentTarget::OpenCode;
        req.desired_presence = DesiredPresence::Absent;
        req.rendered_hash = sha256_hex(b"");
        req.base_hash = Some(managed_hash.clone());
        req.expected_external_hash = Some(managed_hash);
        sched.inject_support_bypass(false).await;

        let job = sched.enqueue_projection(req).await.unwrap();
        let stats = sched
            .run_ready_jobs(&CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(stats.skipped, 1, "stats={stats:?}");
        assert_eq!(std::fs::read(&target).unwrap(), managed);
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_eq!(done.state, ProjectionJobState::Blocked);
        let mat = repo
            .get_materialization_by_binding(&binding)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mat.status, MaterializationStatus::Blocked);
    }

    /// checkout status=blocked 时 Present 写 AGENTS.md 路径必须 Blocked，不得覆盖预存文件。
    #[tokio::test]
    async fn checkout_binding_blocked_skips_present_write() {
        use crate::storage::UpsertAgentHubCheckoutBinding;

        let (sched, tmp, repo) = setup().await;
        let scope = repo
            .insert_scope(NewScopeNode {
                id: Some("user-checkout".into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "root".into(),
                display_name: "root".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let checkout = repo
            .upsert_checkout_binding(UpsertAgentHubCheckoutBinding {
                hub_project_id: "hub-blocked".into(),
                workbench_worktree_id: None,
                checkout_kind: "main".into(),
                relative_root: Some(String::new()),
                local_absolute_path: Some(tmp.path().to_string_lossy().to_string()),
                enabled: true,
                status: "blocked".into(),
                warning: Some("AGENTS.md pre-exists".into()),
            })
            .await
            .unwrap();
        let binding = repo
            .upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target: AgentTarget::OpenCode,
                local_scope_mapping_id: None,
                checkout_binding_id: Some(checkout.id.clone()),
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
            })
            .await
            .unwrap();
        let target = tmp.path().join("AGENTS.md");
        std::fs::write(&target, b"user pre-existing agents").unwrap();
        let old = sha256_hex(b"user pre-existing agents");
        let mut req = file_req(
            &asset.id,
            &binding.id,
            &target,
            b"hub overwrite",
            Some(&old),
        );
        req.target = AgentTarget::OpenCode;
        let job = sched.enqueue_projection(req).await.unwrap();
        let cancel = CancellationToken::new();
        let stats = sched.run_ready_jobs(&cancel).await.unwrap();
        assert_eq!(stats.skipped, 1, "stats={stats:?}");
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"user pre-existing agents",
            "must not overwrite blocked checkout AGENTS.md"
        );
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_eq!(done.state, ProjectionJobState::Blocked);
        assert_eq!(done.last_error.as_deref(), Some("checkout_binding_blocked"));
        let mat = repo
            .get_materialization_by_binding(&binding.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mat.status, MaterializationStatus::Blocked);
    }

    /// Business Logic: OpenCode runtime bridge 保留路径精确匹配与碰撞分类。
    #[test]
    fn opencode_runtime_bridge_reserved_path_and_collision() {
        use crate::workbench::agent_runtime::opencode_bridge::{
            OpenCodeRuntimeBridge, OPENCODE_RUNTIME_BRIDGE_REL_PATH,
        };
        assert!(is_opencode_runtime_bridge_reserved_path(
            OPENCODE_RUNTIME_BRIDGE_REL_PATH
        ));
        assert!(is_opencode_runtime_bridge_reserved_path(&format!(
            "/tmp/proj/{OPENCODE_RUNTIME_BRIDGE_REL_PATH}"
        )));
        assert!(!is_opencode_runtime_bridge_reserved_path(
            ".opencode/plugins/other.ts"
        ));
        let ours = OpenCodeRuntimeBridge::generated_source().as_bytes();
        assert_eq!(
            opencode_runtime_bridge_collision(OPENCODE_RUNTIME_BRIDGE_REL_PATH, Some(ours)),
            Some(false)
        );
        assert_eq!(
            opencode_runtime_bridge_collision(
                OPENCODE_RUNTIME_BRIDGE_REL_PATH,
                Some(b"foreign-plugin")
            ),
            Some(true)
        );
        assert_eq!(
            opencode_runtime_bridge_collision("other.ts", Some(b"x")),
            None
        );
    }

    #[test]
    fn package_activation_phase_order_and_recovery() {
        use super::{
            advance_package_activation, is_managed_package_target_path, PackageActivationAdvance,
        };
        use crate::agent_hub::models::{MaterializationStatus, ProjectionJobState};

        assert!(is_managed_package_target_path(
            "/data/agent-hub/materialized-packages/claude/user/pkg"
        ));
        assert!(!is_managed_package_target_path(
            "/home/user/.claude/CLAUDE.md"
        ));

        // prepared → packageWritten
        let n = advance_package_activation(
            ProjectionJobState::Prepared,
            false,
            false,
            false,
            false,
            false,
        );
        assert_eq!(
            n,
            PackageActivationAdvance::Next(ProjectionJobState::PackageWritten)
        );

        // packageWritten + not present → activationRequested
        let n = advance_package_activation(
            ProjectionJobState::PackageWritten,
            false,
            false,
            false,
            false,
            false,
        );
        assert_eq!(
            n,
            PackageActivationAdvance::Next(ProjectionJobState::ActivationRequested)
        );

        // packageWritten + already present (recovery inspect) → skip to verified
        let n = advance_package_activation(
            ProjectionJobState::PackageWritten,
            true,
            true,
            false,
            false,
            false,
        );
        assert_eq!(
            n,
            PackageActivationAdvance::Next(ProjectionJobState::ActivationVerified)
        );

        // activationRequested + apply ok → verified
        let n = advance_package_activation(
            ProjectionJobState::ActivationRequested,
            false,
            false,
            false,
            false,
            true,
        );
        assert_eq!(
            n,
            PackageActivationAdvance::Next(ProjectionJobState::ActivationVerified)
        );

        // verified → commit ready
        let n = advance_package_activation(
            ProjectionJobState::ActivationVerified,
            true,
            true,
            false,
            false,
            true,
        );
        assert_eq!(n, PackageActivationAdvance::CommitReady);

        // activation required never commit
        let n = advance_package_activation(
            ProjectionJobState::PackageWritten,
            false,
            false,
            true,
            false,
            false,
        );
        match n {
            PackageActivationAdvance::Block { status, .. } => {
                assert_eq!(status, MaterializationStatus::ActivationRequired);
            }
            other => panic!("expected block, got {other:?}"),
        }

        // support blocked never commit
        let n = advance_package_activation(
            ProjectionJobState::PackageWritten,
            false,
            false,
            false,
            true,
            false,
        );
        match n {
            PackageActivationAdvance::Block { status, reason } => {
                assert_eq!(status, MaterializationStatus::Blocked);
                assert!(reason.contains("support_blocked"));
            }
            other => panic!("expected block, got {other:?}"),
        }
    }

    /// Business Logic: 外部整文件删除 → detached，Present job 不得自动重建。
    /// Code Logic: materialization Detached + Present enqueue → skipped，文件仍缺失。
    #[tokio::test]
    async fn external_whole_file_delete_stays_detached_without_auto_recreate() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("CLAUDE.md");
        // 不创建文件：模拟外部已删
        let req = file_req(&asset, &binding, &target, b"should-not-write", None);
        // 先标记 detached
        repo.upsert_materialization(NewMaterialization {
            asset_id: asset.clone(),
            target: AgentTarget::Claude,
            target_binding_id: binding.clone(),
            native_path: Some(target.to_string_lossy().to_string()),
            last_projected_revision_id: None,
            rendered_hash: Some(sha256_hex(b"old")),
            observed_external_hash: None,
            status: MaterializationStatus::Detached,
            last_error: Some("external_whole_file_missing".into()),
        })
        .await
        .unwrap();

        let job = sched.enqueue_projection(req).await.unwrap();
        let cancel = CancellationToken::new();
        let stats = sched.run_ready_jobs(&cancel).await.unwrap();
        assert_eq!(stats.committed, 0);
        assert!(!target.exists(), "detached must not auto-recreate file");
        let mat = repo
            .get_materialization_by_binding(&binding)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mat.status, MaterializationStatus::Detached);
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_eq!(done.state, ProjectionJobState::Blocked);
        assert_eq!(
            done.last_error.as_deref(),
            Some("detached_no_auto_recreate")
        );
    }

    /// Business Logic: desiredPresence=absent 仅删除该 target 受管文件，其它路径保留。
    /// Code Logic: Absent job 删除 managed 文件后 Synced；sibling 文件不删。
    #[tokio::test]
    async fn absent_removes_only_target_owned_path() {
        let (sched, tmp, repo) = setup().await;
        let (asset, binding) = seed_asset_binding(&repo).await;
        let target = tmp.path().join("CLAUDE.md");
        let sibling = tmp.path().join("NOTES.md");
        let managed = b"hub content";
        std::fs::write(&target, managed).unwrap();
        std::fs::write(&sibling, b"user notes").unwrap();
        let managed_hash = sha256_hex(managed);
        let empty = b"";
        let empty_hash = sha256_hex(empty);
        let mut req = file_req(&asset, &binding, &target, empty, Some(&managed_hash));
        req.desired_presence = DesiredPresence::Absent;
        req.rendered_hash = empty_hash;
        req.base_hash = Some(managed_hash.clone());
        req.expected_external_hash = Some(managed_hash);

        let _job = sched.enqueue_projection(req).await.unwrap();
        let cancel = CancellationToken::new();
        let stats = sched.run_ready_jobs(&cancel).await.unwrap();
        assert!(stats.committed >= 1);
        assert!(!target.exists());
        assert!(sibling.exists(), "non-owned sibling must remain");
        let mat = repo
            .get_materialization_by_binding(&binding)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mat.status, MaterializationStatus::Synced);
    }
}
