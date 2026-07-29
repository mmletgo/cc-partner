//! agent_hub/projection/scheduler — durable projection job 调度
//!
//! Business Logic（为什么需要这个模块）:
//!     owner 进程需要持久 job 入队、按资产串行/全局并行 4、冲突冻结与 crash 对账恢复。
//!
//! Code Logic（这个模块做什么）:
//!     ProjectionScheduler::enqueue_projection / run_ready_jobs / recover_on_startup；
//!     未 opt-in 项目过滤；canonical/target conflict 冻结；atomic write + materialization commit。

use crate::agent_hub::models::{
    DesiredPresence, MaterializationStatus, NewMaterialization, NewProjectionJob, ProjectionJob,
    ProjectionJobState, ProjectionPayloadKind, RevisionId,
};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::projection::atomic_writer::{
    AtomicProjectionWriter, AtomicWriteOutcome, DirectoryWriteRequest, FileWriteRequest,
    ProjectionWriteFault,
};
use crate::error::AppError;
use crate::storage::AgentHubRepo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// 全局并行投影资产上限。
pub const MAX_GLOBAL_PROJECTION_PARALLELISM: usize = 4;

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
        }
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
            .map(|p| serde_json::to_string(p))
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
    ///     list prepared → 按资产锁 + 全局 semaphore 执行。
    pub async fn run_ready_jobs(
        &self,
        cancel: &CancellationToken,
    ) -> Result<ProjectionRunStats, AppError> {
        let mut stats = ProjectionRunStats::default();
        if cancel.is_cancelled() {
            return Ok(stats);
        }
        let jobs = self.repo.list_prepared_projection_jobs(32).await?;
        for job in jobs {
            if cancel.is_cancelled() {
                break;
            }
            stats.attempted += 1;
            match self.execute_job(job).await {
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
    ///
    /// Code Logic（这个函数做什么）:
    ///     读目标 hash 分支处理，绝不仅凭 DB 状态 commit。
    async fn reconcile_recoverable_job(
        &self,
        job: ProjectionJob,
    ) -> Result<JobExecResult, AppError> {
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

        if job.desired_presence == DesiredPresence::Absent {
            // Gate A：删除路径后续任务；此处标记 blocked 以免盲写
            self.repo
                .update_projection_job_state(
                    &job.id,
                    ProjectionJobState::Blocked,
                    job.attempt,
                    Some("desired_presence_absent_not_implemented"),
                    None,
                    None,
                )
                .await?;
            return Ok(JobExecResult::Skipped);
        }

        let asset_lock = self.asset_lock(&job.asset_id).await;
        let _asset_guard = asset_lock.lock().await;
        let _slot = self
            .global_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::generic("projection semaphore closed"))?;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobExecResult {
    Committed,
    AlreadySynced,
    Drifted,
    Skipped,
    Failed,
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
}
