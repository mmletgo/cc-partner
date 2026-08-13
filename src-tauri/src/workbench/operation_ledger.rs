//! workbench/operation_ledger.rs — Workbench Git mutation 持久化 operation ledger
//!
//! Business Logic（为什么需要这个模块）:
//!     commit/push/merge/remove 在 timeout/network 下不能盲重放；sidecar 必须先 claim 稳定
//!     `client_operation_id`，持久化 canonical payload hash 与 reconciliation intent，供桌面/Mobile
//!     在 unknown 后按 intent 精确对账。
//!
//! Code Logic（这个模块做什么）:
//!     SQLite 表 `workbench_mutation_operations`（UNIQUE client_operation_id）；
//!     claim → running → terminal outcome；same id/same hash 回放状态；same id/different hash
//!     返回 conflict。提供 envelope DTO、intent 矩阵与纯 confirm helper。

use crate::error::AppError;
use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};

/// ledger schema（CREATE TABLE IF NOT EXISTS，兼容旧库）。
pub const WORKBENCH_MUTATION_OPERATIONS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS workbench_mutation_operations (
    client_operation_id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    intent_json TEXT NOT NULL,
    state TEXT NOT NULL,
    outcome_json TEXT,
    error_message TEXT,
    project_id TEXT,
    worktree_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
)
"#;

/// mutation 种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MutationKind {
    Commit,
    Push,
    Merge,
    Remove,
}

impl MutationKind {
    /// Business Logic: 持久化与 wire 使用稳定小写 token。
    /// Code Logic: 返回 kind 的静态字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Push => "push",
            Self::Merge => "merge",
            Self::Remove => "remove",
        }
    }

    /// Business Logic: 从 DB/wire 解析 kind。
    /// Code Logic: 精确匹配四种 token，其它返回 Validation。
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "commit" => Ok(Self::Commit),
            "push" => Ok(Self::Push),
            "merge" => Ok(Self::Merge),
            "remove" => Ok(Self::Remove),
            other => Err(AppError::validation(format!(
                "未知 workbench mutation kind: {other}"
            ))),
        }
    }
}

/// ledger 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MutationState {
    Claimed,
    Running,
    Succeeded,
    Failed,
}

impl MutationState {
    /// Business Logic: DB 用稳定小写状态字。
    /// Code Logic: 返回状态字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    /// Business Logic: 解析 DB 状态。
    /// Code Logic: 精确匹配四种状态。
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "claimed" => Ok(Self::Claimed),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            other => Err(AppError::generic(format!(
                "损坏的 mutation ledger state: {other}"
            ))),
        }
    }

    /// Business Logic: claimed/running 对前端仍是 pending，不得确认成功。
    /// Code Logic: claimed|running → true。
    pub fn is_pending(self) -> bool {
        matches!(self, Self::Claimed | Self::Running)
    }

    /// Business Logic: 终态可回放 outcome。
    /// Code Logic: succeeded|failed → true。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}

/// 不确定传输类别（与前端 MutationTransportClass 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MutationTransportClass {
    Timeout,
    Network,
}

impl MutationTransportClass {
    /// Business Logic: wire 用稳定 token。
    /// Code Logic: timeout|network。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Network => "network",
        }
    }
}

/// 触发 AI 修复的 Git hook 阶段。
///
/// Business Logic（为什么需要这个枚举）:
///     commit 与 push 各有自己的钩子；修复 prompt 与可重试动作按阶段区分。
///
/// Code Logic（这个枚举做什么）:
///     wire 用稳定小写 token；`hook_name` 返回 `.git/hooks/` 下对应文件名。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkbenchHookStage {
    PreCommit,
    PrePush,
}

impl WorkbenchHookStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreCommit => "preCommit",
            Self::PrePush => "prePush",
        }
    }

    /// `.git/hooks/` 下的钩子脚本文件名。
    pub fn hook_name(self) -> &'static str {
        match self {
            Self::PreCommit => "pre-commit",
            Self::PrePush => "pre-push",
        }
    }

    /// 解析 DB/wire token；非法返回 Validation。
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "preCommit" => Ok(Self::PreCommit),
            "prePush" => Ok(Self::PrePush),
            other => Err(AppError::validation(format!(
                "未知 workbench hook stage: {other}"
            ))),
        }
    }
}

/// 结构化的 hook 钩子失败（envelope `failedHook` 载荷，与前端 HookFailure DTO 对齐）。
///
/// Business Logic（为什么需要这个结构体）:
///     把钩子的原始 stdout/stderr/退出码原样交给前端与修复 agent，禁止靠文案匹配判业务。
///
/// Code Logic（这个结构体做什么）:
///     camelCase serde；`combined_output` 合并 stderr 优先 stdout 供 prompt 引用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchHookFailureDto {
    pub stage: WorkbenchHookStage,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

impl WorkbenchHookFailureDto {
    /// 合并输出（stderr 优先，非空时只用 stderr；否则用 stdout），用于 prompt 与摘要。
    pub fn combined_output(&self) -> String {
        let stderr = self.stderr.trim();
        if !stderr.is_empty() {
            stderr.to_string()
        } else {
            self.stdout.trim().to_string()
        }
    }

    /// 简短摘要（用于 ledger error_message 与前端兜底文案），不包含原始输出正文。
    pub fn summary(&self) -> String {
        match self.stage {
            WorkbenchHookStage::PreCommit => "pre-commit 钩子失败".to_string(),
            WorkbenchHookStage::PrePush => "pre-push 钩子失败".to_string(),
        }
    }
}

/// 成功通道 envelope：succeeded | unknown | failedHook。
///
/// Business Logic: definitive validation/conflict 仍走 AppError；仅 uncertain transport 走 unknown；
///     本地 commit/push 因 pre-commit/pre-push 钩子失败时走 failedHook，让前端展示「让 AI 修复并重试」
///     而不是把结构化 hook 输出拍平成 AppError 文案（远端/P2P 路径不产生 failedHook，保持 succeeded|unknown）。
///
/// Code Logic: tag=kind 的 camelCase 联合；unknown 不携带 intent（intent 由 ledger 查询提供）；
///     failedHook 携带结构化 hook 失败 DTO。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WorkbenchMutationEnvelopeDto<T> {
    /// 已确认成功，value 为权威结果。
    Succeeded {
        value: T,
        #[serde(rename = "clientOperationId")]
        client_operation_id: String,
    },
    /// 结果未知；仅携带 caller 已知 id 与可选 transport class。
    Unknown {
        #[serde(rename = "clientOperationId")]
        client_operation_id: String,
        #[serde(rename = "transportClass", skip_serializing_if = "Option::is_none")]
        transport_class: Option<MutationTransportClass>,
    },
    /// 本地 hook 钩子失败（仅 owner 本机 commit/push 产生）：携带结构化输出，供 AI 修复。
    FailedHook {
        #[serde(rename = "clientOperationId")]
        client_operation_id: String,
        #[serde(rename = "hookFailure")]
        hook_failure: WorkbenchHookFailureDto,
    },
}

impl<T> WorkbenchMutationEnvelopeDto<T> {
    /// Business Logic: 构造成功 envelope。
    /// Code Logic: kind=succeeded。
    pub fn succeeded(value: T, client_operation_id: impl Into<String>) -> Self {
        Self::Succeeded {
            value,
            client_operation_id: client_operation_id.into(),
        }
    }

    /// Business Logic: 构造 unknown envelope。
    /// Code Logic: kind=unknown，可选 transport_class。
    pub fn unknown(
        client_operation_id: impl Into<String>,
        transport_class: Option<MutationTransportClass>,
    ) -> Self {
        Self::Unknown {
            client_operation_id: client_operation_id.into(),
            transport_class,
        }
    }

    /// Business Logic: 构造 failedHook envelope（仅本地 commit/push 钩子失败产生）。
    /// Code Logic: kind=failedHook，携带结构化 hook 输出。
    pub fn failed_hook(
        client_operation_id: impl Into<String>,
        hook_failure: WorkbenchHookFailureDto,
    ) -> Self {
        Self::FailedHook {
            client_operation_id: client_operation_id.into(),
            hook_failure,
        }
    }
}

/// reconciliation intent（执行前捕获）。
///
/// Business Logic: unknown 后按精确后置条件确认，禁止用 message 代替 tree identity。
/// Code Logic: tag=kind 的 camelCase 联合。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MutationIntent {
    Commit {
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "worktreeId")]
        worktree_id: String,
        /// commit 前 HEAD（空仓库为 null）。
        #[serde(rename = "beforeHead")]
        before_head: Option<String>,
        ///  staged tree hash（`git write-tree`），非 message。
        #[serde(rename = "expectedTree")]
        expected_tree: String,
    },
    Push {
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "worktreeId")]
        worktree_id: String,
        /// 本地 ref 全名，如 refs/heads/feature/x。
        #[serde(rename = "localRef")]
        local_ref: String,
        /// 期望远端 ref 全名，如 refs/remotes/origin/feature/x。
        #[serde(rename = "remoteRef")]
        remote_ref: String,
        /// 推送前本地 HEAD。
        #[serde(rename = "localHead")]
        local_head: String,
    },
    Merge {
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "sourceWorktreeId")]
        source_worktree_id: String,
        #[serde(rename = "sourceHead")]
        source_head: String,
        #[serde(rename = "mainHead")]
        main_head: String,
    },
    CollectMerge {
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "worktreeId")]
        worktree_id: String,
        #[serde(rename = "homeBranch")]
        home_branch: String,
        #[serde(rename = "homeOid")]
        home_oid: String,
        sources: Vec<CollectMergeSource>,
    },
    Remove {
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "worktreeId")]
        worktree_id: String,
        path: String,
        branch: Option<String>,
    },
}

/// 主工作区 collect-merge 冻结的一条源分支。
///
/// Business Logic（为什么需要这个结构体）:
///     collect-merge 必须按冻结 name+oid 对账，不能在执行期再解析可能漂移的分支 tip。
///
/// Code Logic（这个结构体做什么）:
///     camelCase `{name, oid}`，供 intent / canonical payload / confirm 共用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CollectMergeSource {
    pub name: String,
    pub oid: String,
}

impl MutationIntent {
    /// Business Logic: 查询 DTO 需要带上 kind。
    /// Code Logic: 从 intent variant 映射 MutationKind。
    pub fn kind(&self) -> MutationKind {
        match self {
            Self::Commit { .. } => MutationKind::Commit,
            Self::Push { .. } => MutationKind::Push,
            Self::Merge { .. } | Self::CollectMerge { .. } => MutationKind::Merge,
            Self::Remove { .. } => MutationKind::Remove,
        }
    }

    /// Business Logic: 列表/过滤需要 project_id。
    /// Code Logic: 提取 project_id 字段。
    pub fn project_id(&self) -> &str {
        match self {
            Self::Commit { project_id, .. }
            | Self::Push { project_id, .. }
            | Self::Merge { project_id, .. }
            | Self::CollectMerge { project_id, .. }
            | Self::Remove { project_id, .. } => project_id,
        }
    }

    /// Business Logic: 列表/过滤需要 worktree_id（feature merge 用 source，collect-merge 用主工作区）。
    /// Code Logic: 提取 worktree 身份。
    pub fn worktree_id(&self) -> &str {
        match self {
            Self::Commit { worktree_id, .. }
            | Self::Push { worktree_id, .. }
            | Self::CollectMerge { worktree_id, .. }
            | Self::Remove { worktree_id, .. } => worktree_id,
            Self::Merge {
                source_worktree_id, ..
            } => source_worktree_id,
        }
    }
}

/// ledger 中的一条 operation 记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchMutationOperationDto {
    pub client_operation_id: String,
    pub kind: MutationKind,
    pub payload_hash: String,
    pub intent: MutationIntent,
    pub state: MutationState,
    /// 成功时的权威 value JSON（失败为 null）。
    pub outcome: Option<Value>,
    pub error_message: Option<String>,
    pub project_id: Option<String>,
    pub worktree_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// claim 结果。
#[derive(Debug, Clone)]
pub enum ClaimOutcome {
    /// 新 claim，调用方应执行 mutation。
    Fresh(WorkbenchMutationOperationDto),
    /// 同 id 同 payload：回放既有记录。
    Replay(WorkbenchMutationOperationDto),
    /// 同 id 不同 payload。
    Conflict {
        existing: WorkbenchMutationOperationDto,
    },
}

/// mutation 执行闭包的分类错误。
///
/// Business Logic（为什么需要这个枚举）:
///     commit/push 失败需要区分「pre-commit/pre-push 钩子拒绝（可让 AI 修复并重试）」与
///     「其它确定性失败（如身份未配置、冲突、远端拒绝）」。`run_claimed_mutation_with_hook`
///     据此决定回 failedHook envelope 还是 AppError。
///
/// Code Logic（这个枚举做什么）:
///     Hook 携带结构化钩子输出；Other 透传 AppError（保持原 run_claimed_mutation 语义）。
#[derive(Debug)]
pub enum MutationExecError {
    /// 本地 pre-commit/pre-push 钩子失败。
    Hook(WorkbenchHookFailureDto),
    /// 其它确定性失败。
    Other(AppError),
}

impl From<AppError> for MutationExecError {
    fn from(err: AppError) -> Self {
        Self::Other(err)
    }
}

/// 权威 Git/worktree 状态快照，供纯 confirm 矩阵使用。
///
/// N3 owner 侧 / 前端对账共享此快照形状；当前生产 path 由前端采集 authority，
/// 后端 `confirm_mutation` 提供 pure 矩阵（单测 + 后续 owner auto-confirm 复用）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(dead_code)] // N3 pure confirm 公共 surface；生产对账当前在前端，后端矩阵供测试与后续 owner 复用
pub struct MutationAuthoritySnapshot {
    /// 当前 HEAD hash。
    pub head: Option<String>,
    /// 当前 HEAD tree hash。
    pub head_tree: Option<String>,
    /// HEAD 的第一父 commit（无父则为 None）。
    pub head_parent: Option<String>,
    /// 远端 ref 当前 hash（push 后置）。
    pub remote_ref_head: Option<String>,
    /// main 是否包含 source HEAD（merge 后置）。
    pub main_contains_source_head: Option<bool>,
    /// source worktree 是否仍存在（merge/remove 后置）。
    pub source_worktree_present: Option<bool>,
    /// 精确 worktree 身份是否仍存在（remove 后置）。
    pub worktree_identity_present: Option<bool>,
}

/// 纯 confirm 结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // 与 MutationAuthoritySnapshot 配套的 pure 结果类型
pub enum MutationConfirmResult {
    ConfirmedSucceeded,
    Unknown,
}

/// Workbench mutation ledger（绑定 SqlitePool）。
#[derive(Debug, Clone)]
pub struct WorkbenchMutationLedger {
    pool: SqlitePool,
}

impl WorkbenchMutationLedger {
    /// Business Logic: 从 AppState.db 构造 ledger。
    /// Code Logic: 持有 pool clone。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Business Logic: 确保表存在（幂等）。
    /// Code Logic: 执行 CREATE TABLE IF NOT EXISTS。
    pub async fn ensure_schema(&self) -> Result<(), AppError> {
        sqlx::query(WORKBENCH_MUTATION_OPERATIONS_SCHEMA)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Business Logic: 执行前 claim，保证同 id 幂等与 payload 冲突检测。
    /// Code Logic:
    ///     1) 校验 id 非空；2) 查已有行；3) 同 hash → Replay；不同 hash → Conflict；
    ///     4) 无行 INSERT claimed。
    pub async fn claim(
        &self,
        client_operation_id: &str,
        kind: MutationKind,
        payload_hash: &str,
        intent: &MutationIntent,
    ) -> Result<ClaimOutcome, AppError> {
        // 幂等建表：测试/旧库无 init_db 时 claim 也可直接用
        self.ensure_schema().await?;
        // intent.kind 与 claim kind 必须一致，防止 payload 与 kind 漂移
        if intent.kind() != kind {
            return Err(AppError::generic(format!(
                "mutation intent kind 与 claim kind 不一致: intent={:?} claim={:?}",
                intent.kind(),
                kind
            )));
        }
        let id = normalize_client_operation_id(client_operation_id)?;
        if let Some(existing) = self.get(&id).await? {
            if existing.payload_hash == payload_hash {
                return Ok(ClaimOutcome::Replay(existing));
            }
            return Ok(ClaimOutcome::Conflict { existing });
        }

        let now = Utc::now().to_rfc3339();
        let intent_json = serde_json::to_string(intent)
            .map_err(|e| AppError::generic(format!("序列化 mutation intent 失败: {e}")))?;
        let project_id = intent.project_id().to_string();
        let worktree_id = intent.worktree_id().to_string();

        let insert = sqlx::query(
            r#"
            INSERT INTO workbench_mutation_operations
                (client_operation_id, kind, payload_hash, intent_json, state,
                 outcome_json, error_message, project_id, worktree_id, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, NULL, NULL, ?, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(kind.as_str())
        .bind(payload_hash)
        .bind(&intent_json)
        .bind(MutationState::Claimed.as_str())
        .bind(&project_id)
        .bind(&worktree_id)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await;

        match insert {
            Ok(_) => {
                let row = self
                    .get(&id)
                    .await?
                    .ok_or_else(|| AppError::generic("mutation claim 后读取失败"))?;
                Ok(ClaimOutcome::Fresh(row))
            }
            Err(e) => {
                // 并发 claim：回读既有行做 hash 比较。
                if let Some(existing) = self.get(&id).await? {
                    if existing.payload_hash == payload_hash {
                        return Ok(ClaimOutcome::Replay(existing));
                    }
                    return Ok(ClaimOutcome::Conflict { existing });
                }
                Err(AppError::from(e))
            }
        }
    }

    /// Business Logic: 执行开始时推进到 running。
    /// Code Logic: UPDATE state=running WHERE id AND state IN (claimed,running)。
    pub async fn mark_running(&self, client_operation_id: &str) -> Result<(), AppError> {
        let id = normalize_client_operation_id(client_operation_id)?;
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            UPDATE workbench_mutation_operations
            SET state = ?, updated_at = ?
            WHERE client_operation_id = ? AND state IN ('claimed', 'running')
            "#,
        )
        .bind(MutationState::Running.as_str())
        .bind(&now)
        .bind(&id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Business Logic: 执行成功后持久化 outcome，供同 id 重放。
    /// Code Logic: UPDATE state=succeeded + outcome_json。
    pub async fn mark_succeeded<T: Serialize>(
        &self,
        client_operation_id: &str,
        value: &T,
    ) -> Result<(), AppError> {
        let id = normalize_client_operation_id(client_operation_id)?;
        let now = Utc::now().to_rfc3339();
        let outcome_json = serde_json::to_string(value)
            .map_err(|e| AppError::generic(format!("序列化 mutation outcome 失败: {e}")))?;
        sqlx::query(
            r#"
            UPDATE workbench_mutation_operations
            SET state = ?, outcome_json = ?, error_message = NULL, updated_at = ?
            WHERE client_operation_id = ?
            "#,
        )
        .bind(MutationState::Succeeded.as_str())
        .bind(&outcome_json)
        .bind(&now)
        .bind(&id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Business Logic: 执行 definitive 失败后持久化错误，同 id 重放仍失败。
    /// Code Logic: UPDATE state=failed + error_message。
    pub async fn mark_failed(
        &self,
        client_operation_id: &str,
        error_message: &str,
    ) -> Result<(), AppError> {
        let id = normalize_client_operation_id(client_operation_id)?;
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            UPDATE workbench_mutation_operations
            SET state = ?, error_message = ?, updated_at = ?
            WHERE client_operation_id = ?
            "#,
        )
        .bind(MutationState::Failed.as_str())
        .bind(error_message)
        .bind(&now)
        .bind(&id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Business Logic: 本地 commit/push 钩子失败后持久化结构化 outcome，供同 id 回放为 failedHook。
    ///
    /// Code Logic（这个函数做什么）:
    ///     UPDATE state=failed + outcome_json=hook failure DTO + error_message=summary。
    ///     复用 Failed 终态（不新增 MutationState variant）；同 id 重放在 with_hook runner 中
    ///     会把 outcome_json 解码为 failedHook envelope 而不是 Err，因此修复后必须用新 clientOperationId 重试。
    pub async fn mark_failed_hook(
        &self,
        client_operation_id: &str,
        failure: &WorkbenchHookFailureDto,
    ) -> Result<(), AppError> {
        let id = normalize_client_operation_id(client_operation_id)?;
        let now = Utc::now().to_rfc3339();
        let outcome_json = serde_json::to_string(failure)
            .map_err(|e| AppError::generic(format!("序列化 hook failure 失败: {e}")))?;
        sqlx::query(
            r#"
            UPDATE workbench_mutation_operations
            SET state = ?, outcome_json = ?, error_message = ?, updated_at = ?
            WHERE client_operation_id = ?
            "#,
        )
        .bind(MutationState::Failed.as_str())
        .bind(&outcome_json)
        .bind(failure.summary())
        .bind(&now)
        .bind(&id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Business Logic: unknown 后按 id 查询 owning ledger 取得 intent/state。
    /// Code Logic: SELECT 一行映射 DTO；不存在返回 None。
    pub async fn get(
        &self,
        client_operation_id: &str,
    ) -> Result<Option<WorkbenchMutationOperationDto>, AppError> {
        let id = client_operation_id.trim();
        if id.is_empty() {
            return Ok(None);
        }
        let row = sqlx::query(
            r#"
            SELECT client_operation_id, kind, payload_hash, intent_json, state,
                   outcome_json, error_message, project_id, worktree_id, created_at, updated_at
            FROM workbench_mutation_operations
            WHERE client_operation_id = ?
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some(map_row(r)?)),
            None => Ok(None),
        }
    }
}

/// Business Logic: client_operation_id 必须非空且长度有界。
/// Code Logic: trim 后 1..=128，仅可打印 ASCII。
pub fn normalize_client_operation_id(raw: &str) -> Result<String, AppError> {
    let id = raw.trim();
    if id.is_empty() {
        return Err(AppError::validation(
            "clientOperationId 不能为空".to_string(),
        ));
    }
    if id.len() > 128 {
        return Err(AppError::validation(
            "clientOperationId 过长（最多 128 字节）".to_string(),
        ));
    }
    if !id.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return Err(AppError::validation(
            "clientOperationId 仅允许可打印 ASCII".to_string(),
        ));
    }
    Ok(id.to_string())
}

/// Business Logic: same id 比较依赖 canonical payload hash。
/// Code Logic: 对稳定 JSON 字节做 SHA256 hex。
pub fn hash_canonical_payload(payload: &Value) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(payload)
        .map_err(|e| AppError::generic(format!("序列化 mutation payload 失败: {e}")))?;
    let digest = Sha256::digest(&bytes);
    Ok(format!("{digest:x}"))
}

/// Business Logic: 构造 commit/push/merge/remove 的 canonical payload（不含 clientOperationId）。
/// Code Logic: 固定 key 顺序的 JSON object。
pub fn canonical_commit_payload(worktree_id: &str, message: &Option<String>) -> Value {
    serde_json::json!({
        "kind": "commit",
        "message": message,
        "worktreeId": worktree_id,
    })
}

/// Business Logic: push payload 仅 worktree 身份。
/// Code Logic: kind+worktreeId。
pub fn canonical_push_payload(worktree_id: &str) -> Value {
    serde_json::json!({
        "kind": "push",
        "worktreeId": worktree_id,
    })
}

/// Business Logic: merge payload 仅 worktree 身份。
/// Code Logic: kind+worktreeId。
pub fn canonical_merge_payload(worktree_id: &str) -> Value {
    serde_json::json!({
        "kind": "merge",
        "worktreeId": worktree_id,
    })
}

/// Business Logic: collect-merge payload 必须绑定 home 与源集合，换源不能 silently 复用同一 id。
/// Code Logic: kind+worktreeId+homeBranch+homeOid+按 name/oid 排序的 sources。
pub fn canonical_collect_merge_payload(
    worktree_id: &str,
    home_branch: &str,
    home_oid: &str,
    sources: &[CollectMergeSource],
) -> Value {
    let mut sources = sources.to_vec();
    sources.sort_by(|left, right| left.name.cmp(&right.name).then(left.oid.cmp(&right.oid)));
    serde_json::json!({
        "homeBranch": home_branch,
        "homeOid": home_oid,
        "kind": "collectMerge",
        "sources": sources,
        "worktreeId": worktree_id,
    })
}

/// Business Logic: remove payload 含 force 开关。
/// Code Logic: kind+worktreeId+force。
pub fn canonical_remove_payload(worktree_id: &str, force: bool) -> Value {
    serde_json::json!({
        "force": force,
        "kind": "remove",
        "worktreeId": worktree_id,
    })
}

/// Business Logic: 纯 confirm 矩阵——只有精确后置条件才 confirmedSucceeded。
/// Code Logic:
///     commit: (parent==beforeHead && tree==expectedTree) 或 (no-op: head==beforeHead && tree==expectedTree)
///     push: remote_ref_head == local_head
///     merge: main_contains_source_head==true && source_worktree_present==false
///     remove: worktree_identity_present==false
#[allow(dead_code)] // N3 pure confirm 矩阵；生产前端对账，后端单测 + 后续 owner auto-confirm
pub fn confirm_mutation(
    intent: &MutationIntent,
    authority: &MutationAuthoritySnapshot,
) -> MutationConfirmResult {
    match intent {
        MutationIntent::Commit {
            before_head,
            expected_tree,
            ..
        } => {
            let Some(head_tree) = authority.head_tree.as_deref() else {
                return MutationConfirmResult::Unknown;
            };
            if head_tree != expected_tree.as_str() {
                return MutationConfirmResult::Unknown;
            }
            // 有新 commit：parent == beforeHead
            if authority.head_parent.as_deref() == before_head.as_deref()
                && authority.head.is_some()
                && authority.head.as_deref() != before_head.as_deref()
            {
                return MutationConfirmResult::ConfirmedSucceeded;
            }
            // no-op：HEAD 未变且 tree 匹配
            if authority.head.as_deref() == before_head.as_deref() {
                return MutationConfirmResult::ConfirmedSucceeded;
            }
            MutationConfirmResult::Unknown
        }
        MutationIntent::Push { local_head, .. } => {
            if authority.remote_ref_head.as_deref() == Some(local_head.as_str()) {
                MutationConfirmResult::ConfirmedSucceeded
            } else {
                MutationConfirmResult::Unknown
            }
        }
        MutationIntent::Merge { .. } => {
            match (
                authority.main_contains_source_head,
                authority.source_worktree_present,
            ) {
                (Some(true), Some(false)) => MutationConfirmResult::ConfirmedSucceeded,
                _ => MutationConfirmResult::Unknown,
            }
        }
        MutationIntent::CollectMerge { .. } => {
            // home 已包含全部冻结 source oid 即成功；主 worktree 会留下，不得套用 Merge 的“源消失”规则。
            match authority.main_contains_source_head {
                Some(true) => MutationConfirmResult::ConfirmedSucceeded,
                _ => MutationConfirmResult::Unknown,
            }
        }
        MutationIntent::Remove { .. } => match authority.worktree_identity_present {
            Some(false) => MutationConfirmResult::ConfirmedSucceeded,
            _ => MutationConfirmResult::Unknown,
        },
    }
}

/// Business Logic: claim 后执行 mutation，并把终态写入 ledger。
/// Code Logic:
///     Fresh → mark_running → execute → mark_succeeded/failed；
///     Replay terminal succeeded → 反序列化 outcome；
///     Replay terminal failed → AppError；
///     Replay pending → unknown envelope（不二次执行）；
///     Conflict → AppError::Conflict。
pub async fn run_claimed_mutation<T, F, Fut>(
    ledger: &WorkbenchMutationLedger,
    client_operation_id: &str,
    claim: ClaimOutcome,
    execute: F,
) -> Result<WorkbenchMutationEnvelopeDto<T>, AppError>
where
    T: Serialize + DeserializeOwned,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, AppError>>,
{
    match claim {
        ClaimOutcome::Conflict { existing } => Err(AppError::conflict(format!(
            "clientOperationId 已绑定不同 payload（existingHash={}）",
            existing.payload_hash
        ))),
        ClaimOutcome::Replay(existing) => {
            // 终态回放 outcome；pending 返回 unknown（禁止二次执行）
            if existing.state.is_pending() {
                return Ok(WorkbenchMutationEnvelopeDto::unknown(
                    existing.client_operation_id,
                    None,
                ));
            }
            // is_terminal()：succeeded 回放 value；failed 回放错误
            debug_assert!(existing.state.is_terminal());
            match existing.state {
                MutationState::Succeeded => {
                    let value = existing.outcome.ok_or_else(|| {
                        AppError::generic("mutation ledger succeeded 但缺少 outcome")
                    })?;
                    let decoded: T = serde_json::from_value(value).map_err(|e| {
                        AppError::generic(format!("反序列化 mutation outcome 失败: {e}"))
                    })?;
                    Ok(WorkbenchMutationEnvelopeDto::succeeded(
                        decoded,
                        existing.client_operation_id,
                    ))
                }
                MutationState::Failed => Err(AppError::generic(
                    existing
                        .error_message
                        .unwrap_or_else(|| "mutation 先前已失败".to_string()),
                )),
                MutationState::Claimed | MutationState::Running => {
                    // 理论上 is_pending 已覆盖；防御性 unknown
                    Ok(WorkbenchMutationEnvelopeDto::unknown(
                        existing.client_operation_id,
                        None,
                    ))
                }
            }
        }
        ClaimOutcome::Fresh(fresh) => {
            // Fresh 行的 kind 写入 ledger 时已校验；此处仅消费字段消除 dead_code
            debug_assert_eq!(fresh.kind, fresh.intent.kind());
            ledger.mark_running(client_operation_id).await?;
            match execute().await {
                Ok(value) => {
                    ledger.mark_succeeded(client_operation_id, &value).await?;
                    Ok(WorkbenchMutationEnvelopeDto::succeeded(
                        value,
                        client_operation_id,
                    ))
                }
                Err(err) => {
                    let _ = ledger
                        .mark_failed(client_operation_id, &err.to_string())
                        .await;
                    Err(err)
                }
            }
        }
    }
}

/// 把 ledger 终态行的 outcome_json 解码为 hook failure DTO（用于 with_hook runner 的 Replay 回放）。
///
/// Business Logic: 钩子失败的 outcome_json 是 WorkbenchHookFailureDto 的序列化形态；
///     其它失败/成功行没有合法 hook failure 形态，返回 None。
///
/// Code Logic: serde_json::from_value；任意解析失败或 stage 非法均返回 None。
fn decode_hook_failure_outcome(outcome: &Option<Value>) -> Option<WorkbenchHookFailureDto> {
    let value = outcome.as_ref()?;
    let dto: WorkbenchHookFailureDto = serde_json::from_value(value.clone()).ok()?;
    // 校验 stage 可解析，防止任意 JSON 误判。
    WorkbenchHookStage::parse(dto.stage.as_str()).ok()?;
    Some(dto)
}

/// 与 `run_claimed_mutation` 相同的 claim/replay 语义，但执行闭包返回 `MutationExecError`，
/// 钩子失败时持久化结构化 outcome 并回 `failedHook` envelope（而非 Err）。
///
/// Business Logic（为什么需要这个函数）:
///     commit/push 钩子失败要让前端展示「让 AI 修复并重试」，必须走成功通道 envelope；
///     merge/remove 仍用旧 `run_claimed_mutation`（Err→AppError），不受影响。
///
/// Code Logic（这个函数做什么）:
///     Fresh: mark_running → execute → Ok→mark_succeeded/Succeeded；Hook→mark_failed_hook/FailedHook；Other→mark_failed/Err。
///     Replay 终态: succeeded 回放 value；failed 优先尝试解码 hook failure→FailedHook，否则 Err。
///     Replay pending / Conflict：与 run_claimed_mutation 一致。
pub async fn run_claimed_mutation_with_hook<T, F, Fut>(
    ledger: &WorkbenchMutationLedger,
    client_operation_id: &str,
    claim: ClaimOutcome,
    execute: F,
) -> Result<WorkbenchMutationEnvelopeDto<T>, AppError>
where
    T: Serialize + DeserializeOwned,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, MutationExecError>>,
{
    match claim {
        ClaimOutcome::Conflict { existing } => Err(AppError::conflict(format!(
            "clientOperationId 已绑定不同 payload（existingHash={}）",
            existing.payload_hash
        ))),
        ClaimOutcome::Replay(existing) => {
            if existing.state.is_pending() {
                return Ok(WorkbenchMutationEnvelopeDto::unknown(
                    existing.client_operation_id,
                    None,
                ));
            }
            debug_assert!(existing.state.is_terminal());
            match existing.state {
                MutationState::Succeeded => {
                    let value = existing.outcome.ok_or_else(|| {
                        AppError::generic("mutation ledger succeeded 但缺少 outcome")
                    })?;
                    let decoded: T = serde_json::from_value(value).map_err(|e| {
                        AppError::generic(format!("反序列化 mutation outcome 失败: {e}"))
                    })?;
                    Ok(WorkbenchMutationEnvelopeDto::succeeded(
                        decoded,
                        existing.client_operation_id,
                    ))
                }
                MutationState::Failed => {
                    // 钩子失败回放：把结构化 outcome 还原为 failedHook envelope。
                    if let Some(hook) = decode_hook_failure_outcome(&existing.outcome) {
                        return Ok(WorkbenchMutationEnvelopeDto::failed_hook(
                            existing.client_operation_id,
                            hook,
                        ));
                    }
                    Err(AppError::generic(
                        existing
                            .error_message
                            .unwrap_or_else(|| "mutation 先前已失败".to_string()),
                    ))
                }
                MutationState::Claimed | MutationState::Running => Ok(
                    WorkbenchMutationEnvelopeDto::unknown(existing.client_operation_id, None),
                ),
            }
        }
        ClaimOutcome::Fresh(fresh) => {
            debug_assert_eq!(fresh.kind, fresh.intent.kind());
            ledger.mark_running(client_operation_id).await?;
            match execute().await {
                Ok(value) => {
                    ledger.mark_succeeded(client_operation_id, &value).await?;
                    Ok(WorkbenchMutationEnvelopeDto::succeeded(
                        value,
                        client_operation_id,
                    ))
                }
                Err(MutationExecError::Hook(hook_failure)) => {
                    let _ = ledger
                        .mark_failed_hook(client_operation_id, &hook_failure)
                        .await;
                    Ok(WorkbenchMutationEnvelopeDto::failed_hook(
                        client_operation_id,
                        hook_failure,
                    ))
                }
                Err(MutationExecError::Other(err)) => {
                    let _ = ledger
                        .mark_failed(client_operation_id, &err.to_string())
                        .await;
                    Err(err)
                }
            }
        }
    }
}

/// Business Logic: 将 SQLite 行映射为 DTO。
/// Code Logic: 解析 kind/state/intent_json/outcome_json。
fn map_row(row: SqliteRow) -> Result<WorkbenchMutationOperationDto, AppError> {
    let client_operation_id: String = row.try_get("client_operation_id")?;
    let kind = MutationKind::parse(row.try_get::<String, _>("kind")?.as_str())?;
    let payload_hash: String = row.try_get("payload_hash")?;
    let intent_json: String = row.try_get("intent_json")?;
    let intent: MutationIntent = serde_json::from_str(&intent_json)
        .map_err(|e| AppError::generic(format!("解析 mutation intent 失败: {e}")))?;
    let state = MutationState::parse(row.try_get::<String, _>("state")?.as_str())?;
    let outcome_json: Option<String> = row.try_get("outcome_json")?;
    let outcome = match outcome_json {
        Some(raw) if !raw.is_empty() => Some(
            serde_json::from_str(&raw)
                .map_err(|e| AppError::generic(format!("解析 mutation outcome 失败: {e}")))?,
        ),
        _ => None,
    };
    let error_message: Option<String> = row.try_get("error_message")?;
    let project_id: Option<String> = row.try_get("project_id")?;
    let worktree_id: Option<String> = row.try_get("worktree_id")?;
    let created_at: String = row.try_get("created_at")?;
    let updated_at: String = row.try_get("updated_at")?;
    Ok(WorkbenchMutationOperationDto {
        client_operation_id,
        kind,
        payload_hash,
        intent,
        state,
        outcome,
        error_message,
        project_id,
        worktree_id,
        created_at,
        updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    /// Business Logic: 内存库 ledger 单测。
    /// Code Logic: 建内存 pool + schema。
    async fn memory_ledger() -> WorkbenchMutationLedger {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("memory sqlite");
        let ledger = WorkbenchMutationLedger::new(pool);
        ledger.ensure_schema().await.expect("schema");
        ledger
    }

    fn sample_commit_intent() -> MutationIntent {
        MutationIntent::Commit {
            project_id: "p1".into(),
            worktree_id: "wt1".into(),
            before_head: Some("aaa".into()),
            expected_tree: "tree1".into(),
        }
    }

    #[tokio::test]
    async fn claim_fresh_then_replay_same_payload() {
        let ledger = memory_ledger().await;
        let hash = "h1";
        let intent = sample_commit_intent();
        let first = ledger
            .claim("op-1", MutationKind::Commit, hash, &intent)
            .await
            .expect("claim");
        assert!(matches!(first, ClaimOutcome::Fresh(_)));

        let second = ledger
            .claim("op-1", MutationKind::Commit, hash, &intent)
            .await
            .expect("replay");
        assert!(matches!(second, ClaimOutcome::Replay(_)));
    }

    #[tokio::test]
    async fn claim_same_id_different_payload_conflicts() {
        let ledger = memory_ledger().await;
        let intent = sample_commit_intent();
        ledger
            .claim("op-2", MutationKind::Commit, "h-a", &intent)
            .await
            .expect("first");
        let second = ledger
            .claim("op-2", MutationKind::Commit, "h-b", &intent)
            .await
            .expect("conflict");
        assert!(matches!(second, ClaimOutcome::Conflict { .. }));
    }

    #[tokio::test]
    async fn run_claimed_mutation_persists_success_and_replays() {
        let ledger = memory_ledger().await;
        let intent = sample_commit_intent();
        let claim = ledger
            .claim("op-3", MutationKind::Commit, "h3", &intent)
            .await
            .expect("claim");
        let env = run_claimed_mutation(&ledger, "op-3", claim, || async {
            Ok::<_, AppError>(serde_json::json!({"ok": true, "n": 1}))
        })
        .await
        .expect("run");
        match env {
            WorkbenchMutationEnvelopeDto::Succeeded { value, .. } => {
                assert_eq!(value["n"], 1);
            }
            other => panic!("expected succeeded, got {other:?}"),
        }

        let claim2 = ledger
            .claim("op-3", MutationKind::Commit, "h3", &intent)
            .await
            .expect("replay claim");
        let env2: WorkbenchMutationEnvelopeDto<Value> =
            run_claimed_mutation(&ledger, "op-3", claim2, || async {
                panic!("must not re-execute");
                #[allow(unreachable_code)]
                Ok::<Value, AppError>(Value::Null)
            })
            .await
            .expect("replay");
        match env2 {
            WorkbenchMutationEnvelopeDto::Succeeded { value, .. } => {
                assert_eq!(value["ok"], true);
            }
            other => panic!("expected succeeded replay, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_claimed_mutation_with_hook_returns_failed_hook_and_replays() {
        let ledger = memory_ledger().await;
        let intent = sample_commit_intent();
        let claim = ledger
            .claim("op-hook", MutationKind::Commit, "hh", &intent)
            .await
            .expect("claim");
        let hook_failure = WorkbenchHookFailureDto {
            stage: WorkbenchHookStage::PreCommit,
            stdout: String::new(),
            stderr: "lint: 2 errors".to_string(),
            exit_code: Some(1),
        };
        let hook_for_closure = hook_failure.clone();
        let env: WorkbenchMutationEnvelopeDto<Value> =
            run_claimed_mutation_with_hook(&ledger, "op-hook", claim, || async {
                Err::<Value, _>(MutationExecError::Hook(hook_for_closure))
            })
            .await
            .expect("run hook");
        match env {
            WorkbenchMutationEnvelopeDto::FailedHook { hook_failure, .. } => {
                assert_eq!(hook_failure.stage, WorkbenchHookStage::PreCommit);
                assert_eq!(hook_failure.exit_code, Some(1));
            }
            other => panic!("expected failedHook, got {other:?}"),
        }

        // 同 id 重放：必须回放为 failedHook（而不是 Err），且不得重新执行闭包。
        let claim2 = ledger
            .claim("op-hook", MutationKind::Commit, "hh", &intent)
            .await
            .expect("replay claim");
        let env2: WorkbenchMutationEnvelopeDto<Value> =
            run_claimed_mutation_with_hook(&ledger, "op-hook", claim2, || async {
                panic!("must not re-execute on replay");
                #[allow(unreachable_code)]
                Ok::<Value, MutationExecError>(Value::Null)
            })
            .await
            .expect("replay");
        assert!(
            matches!(env2, WorkbenchMutationEnvelopeDto::FailedHook { .. }),
            "replay should be failedHook, got {env2:?}"
        );

        // 其它确定性失败仍走 Err（与旧 run_claimed_mutation 一致）。
        let claim3 = ledger
            .claim("op-other", MutationKind::Commit, "ho", &intent)
            .await
            .expect("claim other");
        let res: Result<WorkbenchMutationEnvelopeDto<Value>, AppError> =
            run_claimed_mutation_with_hook(&ledger, "op-other", claim3, || async {
                Err::<Value, _>(MutationExecError::Other(AppError::generic("boom")))
            })
            .await;
        assert!(res.is_err(), "non-hook failure must propagate as Err");
    }

    #[test]
    fn confirm_commit_requires_parent_and_tree() {
        let intent = MutationIntent::Commit {
            project_id: "p".into(),
            worktree_id: "w".into(),
            before_head: Some("old".into()),
            expected_tree: "treeA".into(),
        };
        // 同 message 不同 tree → unknown
        let authority = MutationAuthoritySnapshot {
            head: Some("new".into()),
            head_tree: Some("treeB".into()),
            head_parent: Some("old".into()),
            ..Default::default()
        };
        assert_eq!(
            confirm_mutation(&intent, &authority),
            MutationConfirmResult::Unknown
        );
        // parent + tree 匹配 → confirmed
        let authority2 = MutationAuthoritySnapshot {
            head: Some("new".into()),
            head_tree: Some("treeA".into()),
            head_parent: Some("old".into()),
            ..Default::default()
        };
        assert_eq!(
            confirm_mutation(&intent, &authority2),
            MutationConfirmResult::ConfirmedSucceeded
        );
        // no-op
        let authority3 = MutationAuthoritySnapshot {
            head: Some("old".into()),
            head_tree: Some("treeA".into()),
            head_parent: None,
            ..Default::default()
        };
        assert_eq!(
            confirm_mutation(&intent, &authority3),
            MutationConfirmResult::ConfirmedSucceeded
        );
    }

    #[test]
    fn confirm_push_merge_remove_matrix() {
        let push = MutationIntent::Push {
            project_id: "p".into(),
            worktree_id: "w".into(),
            local_ref: "refs/heads/f".into(),
            remote_ref: "refs/remotes/origin/f".into(),
            local_head: "abc".into(),
        };
        assert_eq!(
            confirm_mutation(
                &push,
                &MutationAuthoritySnapshot {
                    remote_ref_head: Some("abc".into()),
                    ..Default::default()
                }
            ),
            MutationConfirmResult::ConfirmedSucceeded
        );
        assert_eq!(
            confirm_mutation(
                &push,
                &MutationAuthoritySnapshot {
                    remote_ref_head: Some("zzz".into()),
                    ..Default::default()
                }
            ),
            MutationConfirmResult::Unknown
        );

        let merge = MutationIntent::Merge {
            project_id: "p".into(),
            source_worktree_id: "w".into(),
            source_head: "s".into(),
            main_head: "m".into(),
        };
        assert_eq!(
            confirm_mutation(
                &merge,
                &MutationAuthoritySnapshot {
                    main_contains_source_head: Some(true),
                    source_worktree_present: Some(false),
                    ..Default::default()
                }
            ),
            MutationConfirmResult::ConfirmedSucceeded
        );

        let remove = MutationIntent::Remove {
            project_id: "p".into(),
            worktree_id: "w".into(),
            path: "/tmp/w".into(),
            branch: Some("feature/x".into()),
        };
        assert_eq!(
            confirm_mutation(
                &remove,
                &MutationAuthoritySnapshot {
                    worktree_identity_present: Some(false),
                    ..Default::default()
                }
            ),
            MutationConfirmResult::ConfirmedSucceeded
        );
    }

    #[test]
    fn payload_hash_is_stable() {
        let a = canonical_commit_payload("wt", &Some("msg".into()));
        let b = canonical_commit_payload("wt", &Some("msg".into()));
        assert_eq!(
            hash_canonical_payload(&a).unwrap(),
            hash_canonical_payload(&b).unwrap()
        );
        let c = canonical_commit_payload("wt", &Some("other".into()));
        assert_ne!(
            hash_canonical_payload(&a).unwrap(),
            hash_canonical_payload(&c).unwrap()
        );
    }

    #[test]
    fn collect_merge_intent_kind_and_confirm_does_not_require_source_absent() {
        let collect = MutationIntent::CollectMerge {
            project_id: "p".into(),
            worktree_id: "p:main".into(),
            home_branch: "main".into(),
            home_oid: "home1".into(),
            sources: vec![
                CollectMergeSource {
                    name: "agent/a".into(),
                    oid: "aaa".into(),
                },
                CollectMergeSource {
                    name: "agent/b".into(),
                    oid: "bbb".into(),
                },
            ],
        };
        assert_eq!(collect.kind(), MutationKind::Merge);
        assert_eq!(collect.project_id(), "p");
        assert_eq!(collect.worktree_id(), "p:main");
        assert_eq!(
            serde_json::to_value(&collect).unwrap()["kind"],
            "collectMerge"
        );
        assert_eq!(
            confirm_mutation(
                &collect,
                &MutationAuthoritySnapshot {
                    main_contains_source_head: Some(true),
                    source_worktree_present: Some(true),
                    ..Default::default()
                }
            ),
            MutationConfirmResult::ConfirmedSucceeded
        );
        assert_eq!(
            confirm_mutation(&collect, &MutationAuthoritySnapshot::default()),
            MutationConfirmResult::Unknown
        );
    }

    #[test]
    fn collect_merge_payload_hash_differs_by_source_set() {
        let sources_ab = vec![
            CollectMergeSource {
                name: "agent/a".into(),
                oid: "aaa".into(),
            },
            CollectMergeSource {
                name: "agent/b".into(),
                oid: "bbb".into(),
            },
        ];
        let collect_a = canonical_collect_merge_payload("p:main", "main", "home1", &sources_ab);
        let collect_b = canonical_collect_merge_payload("p:main", "main", "home1", &sources_ab);
        assert_eq!(
            hash_canonical_payload(&collect_a).unwrap(),
            hash_canonical_payload(&collect_b).unwrap()
        );
        let sources_only_a = vec![CollectMergeSource {
            name: "agent/a".into(),
            oid: "aaa".into(),
        }];
        let collect_c = canonical_collect_merge_payload("p:main", "main", "home1", &sources_only_a);
        assert_ne!(
            hash_canonical_payload(&collect_a).unwrap(),
            hash_canonical_payload(&collect_c).unwrap()
        );
    }

    #[test]
    fn envelope_serde_roundtrip() {
        let env = WorkbenchMutationEnvelopeDto::succeeded(serde_json::json!({"id": "wt"}), "op-x");
        let raw = serde_json::to_string(&env).unwrap();
        assert!(raw.contains("\"kind\":\"succeeded\""));
        assert!(raw.contains("\"clientOperationId\":\"op-x\""));
        let back: WorkbenchMutationEnvelopeDto<Value> = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, env);

        let unk = WorkbenchMutationEnvelopeDto::<Value>::unknown(
            "op-y",
            Some(MutationTransportClass::Timeout),
        );
        let raw2 = serde_json::to_string(&unk).unwrap();
        assert!(raw2.contains("\"kind\":\"unknown\""));
        assert!(raw2.contains("\"transportClass\":\"timeout\""));
    }
}
