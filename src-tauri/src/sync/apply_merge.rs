//! sync/apply_merge.rs — 三域 push-batch / 本地 pull 统一事务 apply
//!
//! Business Logic（为什么需要这个模块）:
//!     Prompt/SSH/Scratchpad 的 merge 落库必须在同一事务提交：active winner、conflict 副本、
//!     采纳删除时的 delete_epoch、可选 peer watermark ack 与 request ledger outcome。
//!     中途失败整批回滚，禁止半批次成功。
//!
//! Code Logic（这个模块做什么）:
//!     - `ApplyMergeFailPoint` 仅 test/debug 注入；
//!     - `apply_*_merge_batch`：ledger 幂等 + 事务内 upsert/conflict/epoch/ack；
//!     - `apply_*_pull_items`：本地 pull 无 ledger 的同形状事务写；
//!     - 供 HTTP push-batch 与引擎本地 apply 复用。

use crate::error::AppError;
use crate::models::prompt::PromptRow;
use crate::models::scratchpad::ScratchpadRow;
use crate::models::ssh_target::SshTargetRow;
use crate::storage::content_version_repo::{ContentVersionRepo, KIND_CONFLICT};
use crate::storage::deletion_floor_repo::{
    DeletionFloor, DeletionFloorDecision, DeletionFloorRepo,
};
use crate::storage::maintenance_gate::{begin_shared_write, DatabaseMaintenanceGate};
use crate::storage::sync_delete_sequence_repo::SyncDeleteSequenceRepo;
use crate::storage::sync_request_ledger_repo::{
    SyncBatchOutcome, SyncRequestLedgerRepo, DOMAIN_PROMPTS, DOMAIN_SCRATCHPAD, DOMAIN_SSH_TARGET,
};
use crate::storage::sync_watermark_repo::SyncWatermarkRepo;
use crate::storage::{PromptRepo, ScratchpadRepo, SshTargetRepo};
use crate::sync::merger::{
    merge_prompt_with_conflicts, prompt_text_content_hash, ContentVersionDraft,
};
use crate::sync::scratchpad::{merge_scratchpad_with_conflicts, scratchpad_text_content_hash};
use crate::sync::ssh_target::{merge_ssh_with_conflicts, ssh_text_content_hash};
use crate::sync::vector_clock;
use chrono::Utc;
use futures_util::FutureExt;
use sqlx::sqlite::SqlitePool;
use sqlx::{Sqlite, Transaction};
use std::sync::atomic::{AtomicU8, Ordering};

// ---------------------------------------------------------------------------
// Fail inject
// ---------------------------------------------------------------------------

/// 事务失败注入点（仅 test/debug_assertions）。
///
/// Business Logic: 验证 active 写后 / conflict 写后失败时 ledger 与全部副作用回滚。
/// Code Logic: 0=无注入；1=AfterActiveRows；2=AfterConflictOrMeta。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ApplyMergeFailPoint {
    /// 不注入
    None = 0,
    /// active winner 写入后失败
    AfterActiveRows = 1,
    /// conflict/epoch 元数据写入后失败
    AfterConflictOrMeta = 2,
}

/// 进程内 inject 开关（三域共用，测试用）。
static APPLY_FAIL: AtomicU8 = AtomicU8::new(0);

/// 串行化 inject 测试，避免并行测试互相覆盖 static 注入点。
#[cfg(test)]
pub fn apply_fail_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// 设置 apply_merge 失败注入点。
///
/// Business Logic: 单测在调用 push-batch 前注入，验证全量回滚。
/// Code Logic: 仅 `cfg(any(test, debug_assertions))` 生效。
#[cfg(any(test, debug_assertions))]
#[allow(dead_code)] // test/debug inject API
pub fn set_apply_merge_fail_point(point: ApplyMergeFailPoint) {
    APPLY_FAIL.store(point as u8, Ordering::SeqCst);
}

/// 清除 inject（返回先前值）。
#[cfg(any(test, debug_assertions))]
#[allow(dead_code)] // 测试 API：take-and-clear 语义
pub fn take_apply_merge_fail_point() -> ApplyMergeFailPoint {
    let prev = APPLY_FAIL.swap(0, Ordering::SeqCst);
    match prev {
        1 => ApplyMergeFailPoint::AfterActiveRows,
        2 => ApplyMergeFailPoint::AfterConflictOrMeta,
        _ => ApplyMergeFailPoint::None,
    }
}

/// 清除 inject。
#[cfg(any(test, debug_assertions))]
#[allow(dead_code)] // test/debug inject API
pub fn clear_apply_merge_fail_point() {
    APPLY_FAIL.store(0, Ordering::SeqCst);
}

/// RAII：作用域结束时自动清除 inject，避免测试 panic 后污染并行用例。
///
/// Business Logic: 并行 lib 测试共享 static inject；panic 路径也必须恢复。
/// Code Logic: Drop 调 clear_apply_merge_fail_point。
#[cfg(any(test, debug_assertions))]
#[allow(dead_code)] // test/debug inject API
pub struct ApplyMergeFailGuard;

#[cfg(any(test, debug_assertions))]
impl Drop for ApplyMergeFailGuard {
    fn drop(&mut self) {
        clear_apply_merge_fail_point();
    }
}

/// 设置 inject 并返回 Drop 守卫（推荐测试入口使用）。
///
/// Business Logic: 保证无论成功/失败/panic 都清 inject。
/// Code Logic: set + 返回 ApplyMergeFailGuard。
#[cfg(any(test, debug_assertions))]
#[allow(dead_code)] // test/debug inject API
pub fn arm_apply_merge_fail_point(point: ApplyMergeFailPoint) -> ApplyMergeFailGuard {
    set_apply_merge_fail_point(point);
    ApplyMergeFailGuard
}

/// 在事务内检查 inject 点（用 raw u8 比较，避免 enum 比较遗漏）。
///
/// Business Logic: 注入失败必须在目标写步骤之后立刻中断事务。
/// Code Logic: 比较 AtomicU8 与目标 point 的 repr 值。
fn check_fail_point(point: ApplyMergeFailPoint) -> Result<(), AppError> {
    if point == ApplyMergeFailPoint::None {
        return Ok(());
    }
    let current = APPLY_FAIL.load(Ordering::SeqCst);
    if current == point as u8 {
        let msg = match point {
            ApplyMergeFailPoint::AfterActiveRows => "injected apply_merge fail after active rows",
            ApplyMergeFailPoint::AfterConflictOrMeta => {
                "injected apply_merge fail after conflict/meta write"
            }
            ApplyMergeFailPoint::None => return Ok(()),
        };
        return Err(AppError::generic(msg));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plans
// ---------------------------------------------------------------------------

/// Prompt 预合并落库计划。
///
/// Business Logic: 路由先完成 floor/merge 决策，事务内只写结果。
/// Code Logic: winners + conflict drafts。
#[derive(Debug, Clone)]
pub struct PromptMergePlan {
    /// 需要 upsert 的 winner
    pub winners: Vec<PromptRow>,
    /// 并发 conflict 副本
    pub conflicts: Vec<ContentVersionDraft>,
}

/// SSH 预合并落库计划。
#[derive(Debug, Clone)]
pub struct SshMergePlan {
    /// 需要 upsert 的 winner
    pub winners: Vec<SshTargetRow>,
    /// 并发 conflict 副本
    pub conflicts: Vec<ContentVersionDraft>,
}

/// Scratchpad 预合并落库计划。
#[derive(Debug, Clone)]
pub struct ScratchpadMergePlan {
    /// 需要 upsert 的 winner
    pub winners: Vec<ScratchpadRow>,
    /// 并发 conflict 副本
    pub conflicts: Vec<ContentVersionDraft>,
}

// ---------------------------------------------------------------------------
// Prompt plan / write
// ---------------------------------------------------------------------------

/// 对单条 remote Prompt 做 floor + merge 决策，收集 winner/conflict。
///
/// Business Logic（为什么需要这个函数）:
///     离线 peer 带旧 live 不得复活已压缩删除；并发 floor 时 active 仍 delete-wins 但保留 history。
///
/// Code Logic（这个函数做什么）:
///     纯决策：调用方已在写事务内读出 local + floor，本函数无 pool I/O。
pub fn plan_prompt_item(
    local: Option<&PromptRow>,
    remote: &PromptRow,
    floor: Option<&DeletionFloor>,
    now: &str,
) -> Result<(Option<PromptRow>, Vec<ContentVersionDraft>), AppError> {
    if !remote.deleted {
        if let Some(floor) = floor {
            match DeletionFloorRepo::apply_deletion_floor(floor, &remote.vector_clock) {
                DeletionFloorDecision::DeleteWins => {
                    if let Some(local_row) = local {
                        if local_row.deleted {
                            let mut forced = remote.clone();
                            forced.deleted = true;
                            let merged = merge_prompt_with_conflicts(
                                local_row,
                                &forced,
                                DOMAIN_PROMPTS,
                                now,
                            );
                            let mut winner = merged.winner;
                            winner.deleted = true;
                            return Ok((Some(winner), merged.conflict_versions));
                        }
                        let mut winner = local_row.clone();
                        winner.deleted = true;
                        winner.vector_clock =
                            vector_clock::merge(&local_row.vector_clock, &remote.vector_clock);
                        winner.updated_at = now.to_string();
                        let conflicts = vec![ContentVersionDraft {
                            domain: DOMAIN_PROMPTS.to_string(),
                            item_id: remote.id.clone(),
                            source_device: remote.device_id.clone(),
                            content_hash: prompt_text_content_hash(remote),
                            created_at: now.to_string(),
                            kind: KIND_CONFLICT.to_string(),
                            snapshot_json: serde_json::to_string(remote).unwrap_or_default(),
                        }];
                        return Ok((Some(winner), conflicts));
                    }
                    let mut tomb = remote.clone();
                    tomb.deleted = true;
                    return Ok((Some(tomb), vec![]));
                }
                DeletionFloorDecision::KeepHistoryButDeleted => {
                    let conflicts = vec![ContentVersionDraft {
                        domain: DOMAIN_PROMPTS.to_string(),
                        item_id: remote.id.clone(),
                        source_device: remote.device_id.clone(),
                        content_hash: prompt_text_content_hash(remote),
                        created_at: now.to_string(),
                        kind: KIND_CONFLICT.to_string(),
                        snapshot_json: serde_json::to_string(remote).unwrap_or_default(),
                    }];
                    if let Some(local_row) = local {
                        let mut winner = local_row.clone();
                        winner.deleted = true;
                        winner.vector_clock = vector_clock::merge(
                            &vector_clock::merge(
                                &local_row.vector_clock,
                                &floor.delete_vector_clock,
                            ),
                            &remote.vector_clock,
                        );
                        if remote.updated_at > winner.updated_at {
                            winner.updated_at = remote.updated_at.clone();
                        }
                        return Ok((Some(winner), conflicts));
                    }
                    let mut tomb = remote.clone();
                    tomb.deleted = true;
                    tomb.vector_clock =
                        vector_clock::merge(&floor.delete_vector_clock, &remote.vector_clock);
                    return Ok((Some(tomb), conflicts));
                }
                DeletionFloorDecision::AcceptLive => {}
            }
        }
    }

    match local {
        None => Ok((Some(remote.clone()), vec![])),
        Some(local_row) => {
            let result = merge_prompt_with_conflicts(local_row, remote, DOMAIN_PROMPTS, now);
            let changed = result.winner.vector_clock != local_row.vector_clock
                || result.winner.updated_at != local_row.updated_at
                || result.winner.content != local_row.content
                || result.winner.title != local_row.title
                || result.winner.deleted != local_row.deleted
                || result.winner.tags != local_row.tags
                || result.winner.delete_epoch != local_row.delete_epoch
                || result.winner.favorite != local_row.favorite;
            if changed || !result.conflict_versions.is_empty() {
                Ok((
                    if changed { Some(result.winner) } else { None },
                    result.conflict_versions,
                ))
            } else {
                Ok((None, vec![]))
            }
        }
    }
}

/// 在写事务内构建一批 remote Prompt 的 merge 计划。
///
/// Business Logic（为什么需要这个函数）:
///     plan 与 write 必须共用同一事务快照；禁止事务外 pool 读 local/floor。
///
/// Code Logic（这个函数做什么）:
///     逐条 `get_on_tx` local；live remote 再 `DeletionFloorRepo::get_on_tx`；调用 plan_prompt_item。
pub async fn build_prompt_merge_plan_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    remotes: &[PromptRow],
    now: &str,
) -> Result<PromptMergePlan, AppError> {
    let mut winners = Vec::new();
    let mut conflicts = Vec::new();
    for remote in remotes {
        let local = PromptRepo::get_on_tx(tx, &remote.id).await?;
        let floor = if !remote.deleted {
            DeletionFloorRepo::get_on_tx(tx, DOMAIN_PROMPTS, &remote.id).await?
        } else {
            None
        };
        let (winner, cfs) = plan_prompt_item(local.as_ref(), remote, floor.as_ref(), now)?;
        if let Some(w) = winner {
            winners.push(w);
        }
        conflicts.extend(cfs);
    }
    Ok(PromptMergePlan { winners, conflicts })
}

/// 在已开启事务内写入 Prompt winners + conflicts + 铸造 delete_epoch。
///
/// Business Logic: 与 ledger 同事务；inject 点验证回滚。
/// Code Logic: mint deleted winners → bulk_upsert → inject AfterActive → insert conflicts → inject AfterMeta。
pub async fn write_prompt_merge_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    plan: &PromptMergePlan,
) -> Result<usize, AppError> {
    let mut winners = plan.winners.clone();
    for w in winners.iter_mut() {
        if w.deleted && w.delete_epoch == 0 {
            w.delete_epoch = SyncDeleteSequenceRepo::mint_on_tx(tx, DOMAIN_PROMPTS).await?;
        }
    }

    if !winners.is_empty() {
        PromptRepo::bulk_upsert_on_tx(tx, &winners, None).await?;
    }

    check_fail_point(ApplyMergeFailPoint::AfterActiveRows)?;

    for draft in &plan.conflicts {
        let version = draft.clone().into_content_version();
        ContentVersionRepo::insert_idempotent_on_tx(tx, &version).await?;
    }

    check_fail_point(ApplyMergeFailPoint::AfterConflictOrMeta)?;

    Ok(winners.len())
}

/// Prompt 域幂等 apply_merge_batch（ledger + 同事务 watermark ack）。
///
/// Business Logic（为什么需要这个函数）:
///     HTTP push-batch 与测试共用同一事务边界：active/conflict/epoch/ledger/ack 全有或全无。
///
/// Code Logic（这个函数做什么）:
///     ensure schemas → apply_batch_idempotent 内 plan_on_tx + write + optional advance_ack_on_tx。
///     禁止事务外 plan（max_connections(1) 与持有 tx 冲突）。
pub async fn apply_prompt_merge_batch(
    pool: &SqlitePool,
    repo: &PromptRepo,
    claimed_device_id: &str,
    client_request_id: &str,
    payload_hash: &str,
    remotes: &[PromptRow],
    acked_delete_epoch: Option<u64>,
) -> Result<SyncBatchOutcome, AppError> {
    ensure_sync_schemas(pool).await?;

    let now = Utc::now().to_rfc3339();
    // ledger 必须与域 repo 共享同一 maintenance gate，避免 restore exclusive 被旁路。
    let ledger = SyncRequestLedgerRepo::with_gate(pool.clone(), repo.gate());
    let claimed = claimed_device_id.to_string();
    let now_for_tx = now.clone();
    let remotes = remotes.to_vec();

    ledger
        .apply_batch_idempotent(
            claimed_device_id,
            DOMAIN_PROMPTS,
            client_request_id,
            payload_hash,
            |tx| {
                let remotes = remotes.clone();
                let claimed = claimed.clone();
                let now_for_tx = now_for_tx.clone();
                async move {
                    let plan = build_prompt_merge_plan_on_tx(tx, &remotes, &now_for_tx).await?;
                    let accepted = write_prompt_merge_on_tx(tx, &plan).await?;
                    if let Some(epoch) = acked_delete_epoch {
                        if !claimed.is_empty() {
                            SyncWatermarkRepo::advance_ack_on_tx(
                                tx,
                                &claimed,
                                DOMAIN_PROMPTS,
                                epoch,
                                &now_for_tx,
                            )
                            .await?;
                        }
                    }
                    Ok(SyncBatchOutcome { accepted })
                }
                .boxed()
            },
        )
        .await
}

/// 本地 pull 应用 remote Prompt（无 ledger，单事务 winner/conflict/epoch）。
///
/// Business Logic（为什么需要这个函数）:
///     引擎从对端拉取正文后也需与 push-batch 同形状落库，避免静默丢 conflict。
///
/// Code Logic（这个函数做什么）:
///     begin_shared_write 先 → plan_on_tx → write → commit；禁止事务外 plan。
pub async fn apply_prompt_pull_items(
    pool: &SqlitePool,
    gate: &DatabaseMaintenanceGate,
    _repo: &PromptRepo,
    remotes: &[PromptRow],
) -> Result<usize, AppError> {
    ensure_sync_schemas(pool).await?;
    let now = Utc::now().to_rfc3339();
    let (_permit, mut tx) = begin_shared_write(pool, gate).await?;
    let plan = build_prompt_merge_plan_on_tx(&mut tx, remotes, &now).await?;
    let accepted = write_prompt_merge_on_tx(&mut tx, &plan).await?;
    tx.commit().await?;
    Ok(accepted)
}

// ---------------------------------------------------------------------------
// SSH plan / write
// ---------------------------------------------------------------------------

/// 对单条 remote SSH 做 floor + merge 决策。
///
/// Business Logic（为什么需要这个函数）:
///     与 Prompt 同语义：floor 拒绝旧 live 复活；并发保留 conflict。
///
/// Code Logic（这个函数做什么）:
///     纯决策：调用方已在写事务内读出 local + floor；item_id=host。
pub fn plan_ssh_item(
    local: Option<&SshTargetRow>,
    remote: &SshTargetRow,
    floor: Option<&DeletionFloor>,
    now: &str,
) -> Result<(Option<SshTargetRow>, Vec<ContentVersionDraft>), AppError> {
    if !remote.deleted {
        if let Some(floor) = floor {
            match DeletionFloorRepo::apply_deletion_floor(floor, &remote.vector_clock) {
                DeletionFloorDecision::DeleteWins => {
                    if let Some(local_row) = local {
                        if local_row.deleted {
                            let mut forced = remote.clone();
                            forced.deleted = true;
                            let merged = merge_ssh_with_conflicts(local_row, &forced, now);
                            let mut winner = merged.winner;
                            winner.deleted = true;
                            return Ok((Some(winner), merged.conflict_versions));
                        }
                        let mut winner = local_row.clone();
                        winner.deleted = true;
                        winner.vector_clock =
                            vector_clock::merge(&local_row.vector_clock, &remote.vector_clock);
                        winner.updated_at = now.to_string();
                        let conflicts = vec![ContentVersionDraft {
                            domain: DOMAIN_SSH_TARGET.to_string(),
                            item_id: remote.host.clone(),
                            source_device: remote.device_id.clone(),
                            content_hash: ssh_text_content_hash(remote),
                            created_at: now.to_string(),
                            kind: KIND_CONFLICT.to_string(),
                            snapshot_json: serde_json::to_string(remote).unwrap_or_default(),
                        }];
                        return Ok((Some(winner), conflicts));
                    }
                    let mut tomb = remote.clone();
                    tomb.deleted = true;
                    return Ok((Some(tomb), vec![]));
                }
                DeletionFloorDecision::KeepHistoryButDeleted => {
                    let conflicts = vec![ContentVersionDraft {
                        domain: DOMAIN_SSH_TARGET.to_string(),
                        item_id: remote.host.clone(),
                        source_device: remote.device_id.clone(),
                        content_hash: ssh_text_content_hash(remote),
                        created_at: now.to_string(),
                        kind: KIND_CONFLICT.to_string(),
                        snapshot_json: serde_json::to_string(remote).unwrap_or_default(),
                    }];
                    if let Some(local_row) = local {
                        let mut winner = local_row.clone();
                        winner.deleted = true;
                        winner.vector_clock = vector_clock::merge(
                            &vector_clock::merge(
                                &local_row.vector_clock,
                                &floor.delete_vector_clock,
                            ),
                            &remote.vector_clock,
                        );
                        if remote.updated_at > winner.updated_at {
                            winner.updated_at = remote.updated_at.clone();
                        }
                        return Ok((Some(winner), conflicts));
                    }
                    let mut tomb = remote.clone();
                    tomb.deleted = true;
                    tomb.vector_clock =
                        vector_clock::merge(&floor.delete_vector_clock, &remote.vector_clock);
                    return Ok((Some(tomb), conflicts));
                }
                DeletionFloorDecision::AcceptLive => {}
            }
        }
    }

    match local {
        None => Ok((Some(remote.clone()), vec![])),
        Some(local_row) => {
            let result = merge_ssh_with_conflicts(local_row, remote, now);
            let changed = result.winner.vector_clock != local_row.vector_clock
                || result.winner.updated_at != local_row.updated_at
                || result.winner.username != local_row.username
                || result.winner.port != local_row.port
                || result.winner.label != local_row.label
                || result.winner.deleted != local_row.deleted
                || result.winner.delete_epoch != local_row.delete_epoch;
            if changed || !result.conflict_versions.is_empty() {
                Ok((
                    if changed { Some(result.winner) } else { None },
                    result.conflict_versions,
                ))
            } else {
                Ok((None, vec![]))
            }
        }
    }
}

/// 在写事务内构建一批 remote SSH 的 merge 计划。
///
/// Business Logic（为什么需要这个函数）:
///     plan 与 write 共用同一事务快照，禁止事务外 pool 读。
///
/// Code Logic（这个函数做什么）:
///     逐条 get_on_tx local；live remote 再 floor get_on_tx；plan_ssh_item。
pub async fn build_ssh_merge_plan_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    remotes: &[SshTargetRow],
    now: &str,
) -> Result<SshMergePlan, AppError> {
    let mut winners = Vec::new();
    let mut conflicts = Vec::new();
    for remote in remotes {
        let local = SshTargetRepo::get_on_tx(tx, &remote.host).await?;
        let floor = if !remote.deleted {
            DeletionFloorRepo::get_on_tx(tx, DOMAIN_SSH_TARGET, &remote.host).await?
        } else {
            None
        };
        let (winner, cfs) = plan_ssh_item(local.as_ref(), remote, floor.as_ref(), now)?;
        if let Some(w) = winner {
            winners.push(w);
        }
        conflicts.extend(cfs);
    }
    Ok(SshMergePlan { winners, conflicts })
}

/// 事务内写入 SSH winners + conflicts + epoch。
///
/// Business Logic: 与 ledger 同事务。
/// Code Logic: mint → bulk_upsert → inject → conflicts → inject。
pub async fn write_ssh_merge_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    plan: &SshMergePlan,
) -> Result<usize, AppError> {
    let mut winners = plan.winners.clone();
    for w in winners.iter_mut() {
        if w.deleted && w.delete_epoch == 0 {
            w.delete_epoch = SyncDeleteSequenceRepo::mint_on_tx(tx, DOMAIN_SSH_TARGET).await?;
        }
    }
    if !winners.is_empty() {
        SshTargetRepo::bulk_upsert_on_tx(tx, &winners, None).await?;
    }
    check_fail_point(ApplyMergeFailPoint::AfterActiveRows)?;
    for draft in &plan.conflicts {
        let version = draft.clone().into_content_version();
        ContentVersionRepo::insert_idempotent_on_tx(tx, &version).await?;
    }
    check_fail_point(ApplyMergeFailPoint::AfterConflictOrMeta)?;
    Ok(winners.len())
}

/// SSH 域幂等 apply_merge_batch。
///
/// Business Logic（为什么需要这个函数）:
///     SSH push-batch 与 Prompt 同事务语义。
///
/// Code Logic（这个函数做什么）:
///     ensure → ledger 内 plan_on_tx + write + optional advance_ack_on_tx。
pub async fn apply_ssh_merge_batch(
    pool: &SqlitePool,
    repo: &SshTargetRepo,
    claimed_device_id: &str,
    client_request_id: &str,
    payload_hash: &str,
    remotes: &[SshTargetRow],
    acked_delete_epoch: Option<u64>,
) -> Result<SyncBatchOutcome, AppError> {
    ensure_sync_schemas(pool).await?;
    let now = Utc::now().to_rfc3339();
    // ledger 必须与域 repo 共享同一 maintenance gate，避免 restore exclusive 被旁路。
    let ledger = SyncRequestLedgerRepo::with_gate(pool.clone(), repo.gate());
    let claimed = claimed_device_id.to_string();
    let now_for_tx = now.clone();
    let remotes = remotes.to_vec();

    ledger
        .apply_batch_idempotent(
            claimed_device_id,
            DOMAIN_SSH_TARGET,
            client_request_id,
            payload_hash,
            |tx| {
                let remotes = remotes.clone();
                let claimed = claimed.clone();
                let now_for_tx = now_for_tx.clone();
                async move {
                    let plan = build_ssh_merge_plan_on_tx(tx, &remotes, &now_for_tx).await?;
                    let accepted = write_ssh_merge_on_tx(tx, &plan).await?;
                    if let Some(epoch) = acked_delete_epoch {
                        if !claimed.is_empty() {
                            SyncWatermarkRepo::advance_ack_on_tx(
                                tx,
                                &claimed,
                                DOMAIN_SSH_TARGET,
                                epoch,
                                &now_for_tx,
                            )
                            .await?;
                        }
                    }
                    Ok(SyncBatchOutcome { accepted })
                }
                .boxed()
            },
        )
        .await
}

/// 本地 pull 应用 remote SSH（无 ledger，单事务 winner/conflict/epoch）。
///
/// Business Logic（为什么需要这个函数）:
///     引擎拉取 SSH 正文后需与 push-batch 同形状落库，避免静默丢 conflict。
///
/// Code Logic（这个函数做什么）:
///     begin_shared_write 先 → plan_on_tx → write → commit。
pub async fn apply_ssh_pull_items(
    pool: &SqlitePool,
    gate: &DatabaseMaintenanceGate,
    _repo: &SshTargetRepo,
    remotes: &[SshTargetRow],
) -> Result<usize, AppError> {
    ensure_sync_schemas(pool).await?;
    let now = Utc::now().to_rfc3339();
    let (_permit, mut tx) = begin_shared_write(pool, gate).await?;
    let plan = build_ssh_merge_plan_on_tx(&mut tx, remotes, &now).await?;
    let accepted = write_ssh_merge_on_tx(&mut tx, &plan).await?;
    tx.commit().await?;
    Ok(accepted)
}

// ---------------------------------------------------------------------------
// Scratchpad plan / write
// ---------------------------------------------------------------------------

/// 对单条 remote Scratchpad 做 floor + merge 决策。
///
/// Business Logic（为什么需要这个函数）:
///     与 Prompt/SSH 同语义的删除 floor 与 conflict 保留。
///
/// Code Logic（这个函数做什么）:
///     纯决策：调用方已在写事务内读出 local + floor。
pub fn plan_scratchpad_item(
    local: Option<&ScratchpadRow>,
    remote: &ScratchpadRow,
    floor: Option<&DeletionFloor>,
    now: &str,
) -> Result<(Option<ScratchpadRow>, Vec<ContentVersionDraft>), AppError> {
    if !remote.deleted {
        if let Some(floor) = floor {
            match DeletionFloorRepo::apply_deletion_floor(floor, &remote.vector_clock) {
                DeletionFloorDecision::DeleteWins => {
                    if let Some(local_row) = local {
                        if local_row.deleted {
                            let mut forced = remote.clone();
                            forced.deleted = true;
                            let merged = merge_scratchpad_with_conflicts(local_row, &forced, now);
                            let mut winner = merged.winner;
                            winner.deleted = true;
                            return Ok((Some(winner), merged.conflict_versions));
                        }
                        let mut winner = local_row.clone();
                        winner.deleted = true;
                        winner.vector_clock =
                            vector_clock::merge(&local_row.vector_clock, &remote.vector_clock);
                        winner.updated_at = now.to_string();
                        let conflicts = vec![ContentVersionDraft {
                            domain: DOMAIN_SCRATCHPAD.to_string(),
                            item_id: remote.id.clone(),
                            source_device: remote.device_id.clone(),
                            content_hash: scratchpad_text_content_hash(remote),
                            created_at: now.to_string(),
                            kind: KIND_CONFLICT.to_string(),
                            snapshot_json: serde_json::to_string(remote).unwrap_or_default(),
                        }];
                        return Ok((Some(winner), conflicts));
                    }
                    let mut tomb = remote.clone();
                    tomb.deleted = true;
                    return Ok((Some(tomb), vec![]));
                }
                DeletionFloorDecision::KeepHistoryButDeleted => {
                    let conflicts = vec![ContentVersionDraft {
                        domain: DOMAIN_SCRATCHPAD.to_string(),
                        item_id: remote.id.clone(),
                        source_device: remote.device_id.clone(),
                        content_hash: scratchpad_text_content_hash(remote),
                        created_at: now.to_string(),
                        kind: KIND_CONFLICT.to_string(),
                        snapshot_json: serde_json::to_string(remote).unwrap_or_default(),
                    }];
                    if let Some(local_row) = local {
                        let mut winner = local_row.clone();
                        winner.deleted = true;
                        winner.vector_clock = vector_clock::merge(
                            &vector_clock::merge(
                                &local_row.vector_clock,
                                &floor.delete_vector_clock,
                            ),
                            &remote.vector_clock,
                        );
                        if remote.updated_at > winner.updated_at {
                            winner.updated_at = remote.updated_at.clone();
                        }
                        return Ok((Some(winner), conflicts));
                    }
                    let mut tomb = remote.clone();
                    tomb.deleted = true;
                    tomb.vector_clock =
                        vector_clock::merge(&floor.delete_vector_clock, &remote.vector_clock);
                    return Ok((Some(tomb), conflicts));
                }
                DeletionFloorDecision::AcceptLive => {}
            }
        }
    }

    match local {
        None => Ok((Some(remote.clone()), vec![])),
        Some(local_row) => {
            let result = merge_scratchpad_with_conflicts(local_row, remote, now);
            let changed = result.winner.vector_clock != local_row.vector_clock
                || result.winner.updated_at != local_row.updated_at
                || result.winner.title != local_row.title
                || result.winner.content != local_row.content
                || result.winner.deleted != local_row.deleted
                || result.winner.delete_epoch != local_row.delete_epoch;
            if changed || !result.conflict_versions.is_empty() {
                Ok((
                    if changed { Some(result.winner) } else { None },
                    result.conflict_versions,
                ))
            } else {
                Ok((None, vec![]))
            }
        }
    }
}

/// 在写事务内构建一批 remote Scratchpad 的 merge 计划。
///
/// Business Logic（为什么需要这个函数）:
///     plan 与 write 共用同一事务快照，禁止事务外 pool 读。
///
/// Code Logic（这个函数做什么）:
///     逐条 get_on_tx local；live remote 再 floor get_on_tx；plan_scratchpad_item。
pub async fn build_scratchpad_merge_plan_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    remotes: &[ScratchpadRow],
    now: &str,
) -> Result<ScratchpadMergePlan, AppError> {
    let mut winners = Vec::new();
    let mut conflicts = Vec::new();
    for remote in remotes {
        let local = ScratchpadRepo::get_on_tx(tx, &remote.id).await?;
        let floor = if !remote.deleted {
            DeletionFloorRepo::get_on_tx(tx, DOMAIN_SCRATCHPAD, &remote.id).await?
        } else {
            None
        };
        let (winner, cfs) = plan_scratchpad_item(local.as_ref(), remote, floor.as_ref(), now)?;
        if let Some(w) = winner {
            winners.push(w);
        }
        conflicts.extend(cfs);
    }
    Ok(ScratchpadMergePlan { winners, conflicts })
}

/// 事务内写入 Scratchpad winners + conflicts + epoch。
///
/// Business Logic: 与 ledger 同事务。
/// Code Logic: mint → bulk_upsert → inject → conflicts → inject。
pub async fn write_scratchpad_merge_on_tx(
    tx: &mut Transaction<'_, Sqlite>,
    plan: &ScratchpadMergePlan,
) -> Result<usize, AppError> {
    let mut winners = plan.winners.clone();
    for w in winners.iter_mut() {
        if w.deleted && w.delete_epoch == 0 {
            w.delete_epoch = SyncDeleteSequenceRepo::mint_on_tx(tx, DOMAIN_SCRATCHPAD).await?;
        }
    }
    if !winners.is_empty() {
        ScratchpadRepo::bulk_upsert_on_tx(tx, &winners, None).await?;
    }
    check_fail_point(ApplyMergeFailPoint::AfterActiveRows)?;
    for draft in &plan.conflicts {
        let version = draft.clone().into_content_version();
        ContentVersionRepo::insert_idempotent_on_tx(tx, &version).await?;
    }
    check_fail_point(ApplyMergeFailPoint::AfterConflictOrMeta)?;
    Ok(winners.len())
}

/// Scratchpad 域幂等 apply_merge_batch。
///
/// Business Logic（为什么需要这个函数）:
///     Scratchpad push-batch 与 Prompt 同事务语义。
///
/// Code Logic（这个函数做什么）:
///     ensure → ledger 内 plan_on_tx + write + optional advance_ack_on_tx。
pub async fn apply_scratchpad_merge_batch(
    pool: &SqlitePool,
    repo: &ScratchpadRepo,
    claimed_device_id: &str,
    client_request_id: &str,
    payload_hash: &str,
    remotes: &[ScratchpadRow],
    acked_delete_epoch: Option<u64>,
) -> Result<SyncBatchOutcome, AppError> {
    ensure_sync_schemas(pool).await?;
    let now = Utc::now().to_rfc3339();
    // ledger 必须与域 repo 共享同一 maintenance gate，避免 restore exclusive 被旁路。
    let ledger = SyncRequestLedgerRepo::with_gate(pool.clone(), repo.gate());
    let claimed = claimed_device_id.to_string();
    let now_for_tx = now.clone();
    let remotes = remotes.to_vec();

    ledger
        .apply_batch_idempotent(
            claimed_device_id,
            DOMAIN_SCRATCHPAD,
            client_request_id,
            payload_hash,
            |tx| {
                let remotes = remotes.clone();
                let claimed = claimed.clone();
                let now_for_tx = now_for_tx.clone();
                async move {
                    let plan = build_scratchpad_merge_plan_on_tx(tx, &remotes, &now_for_tx).await?;
                    let accepted = write_scratchpad_merge_on_tx(tx, &plan).await?;
                    if let Some(epoch) = acked_delete_epoch {
                        if !claimed.is_empty() {
                            SyncWatermarkRepo::advance_ack_on_tx(
                                tx,
                                &claimed,
                                DOMAIN_SCRATCHPAD,
                                epoch,
                                &now_for_tx,
                            )
                            .await?;
                        }
                    }
                    Ok(SyncBatchOutcome { accepted })
                }
                .boxed()
            },
        )
        .await
}

// ---------------------------------------------------------------------------
// Shared ensure
// ---------------------------------------------------------------------------

/// 确保 apply_merge 依赖的全部 schema。
///
/// Business Logic: 旧库无迁移框架也必须能走完整 merge 路径。
/// Code Logic: content_versions / delete_seq / floor / ledger / watermark / delete_epoch 列。
async fn ensure_sync_schemas(pool: &SqlitePool) -> Result<(), AppError> {
    ContentVersionRepo::ensure_schema(pool).await?;
    SyncDeleteSequenceRepo::ensure_schema(pool).await?;
    DeletionFloorRepo::ensure_schema(pool).await?;
    SyncRequestLedgerRepo::ensure_schema(pool).await?;
    SyncWatermarkRepo::ensure_schema(pool).await?;
    crate::storage::ensure_domain_delete_epoch_columns(pool).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// 本地 pull 应用 remote Scratchpad（无 ledger，单事务 winner/conflict/epoch）。
///
/// Business Logic（为什么需要这个函数）:
///     引擎拉取速记本正文后需与 push-batch 同形状落库。
///
/// Code Logic（这个函数做什么）:
///     begin_shared_write 先 → plan_on_tx → write → commit。
pub async fn apply_scratchpad_pull_items(
    pool: &SqlitePool,
    gate: &DatabaseMaintenanceGate,
    _repo: &ScratchpadRepo,
    remotes: &[ScratchpadRow],
) -> Result<usize, AppError> {
    ensure_sync_schemas(pool).await?;
    let now = Utc::now().to_rfc3339();
    let (_permit, mut tx) = begin_shared_write(pool, gate).await?;
    let plan = build_scratchpad_merge_plan_on_tx(&mut tx, remotes, &now).await?;
    let accepted = write_scratchpad_merge_on_tx(&mut tx, &plan).await?;
    tx.commit().await?;
    Ok(accepted)
}

#[cfg(test)]
mod tests {
    //! apply_merge 单测：inject 全量回滚 + conflict 落库。

    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::HashMap;
    use std::str::FromStr;

    async fn setup() -> (SqlitePool, PromptRepo) {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS prompts (
                id TEXT PRIMARY KEY, title TEXT NOT NULL, content TEXT NOT NULL,
                tags TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0,
                delete_epoch INTEGER NOT NULL DEFAULT 0, favorite INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        ContentVersionRepo::ensure_schema(&pool).await.unwrap();
        SyncDeleteSequenceRepo::ensure_schema(&pool).await.unwrap();
        DeletionFloorRepo::ensure_schema(&pool).await.unwrap();
        SyncRequestLedgerRepo::ensure_schema(&pool).await.unwrap();
        SyncWatermarkRepo::ensure_schema(&pool).await.unwrap();
        (pool.clone(), PromptRepo::new(pool))
    }

    fn prompt(id: &str, device: &str, content: &str, vc: u64, deleted: bool) -> PromptRow {
        let mut vector_clock = HashMap::new();
        vector_clock.insert(device.to_string(), vc);
        PromptRow {
            id: id.to_string(),
            title: format!("t-{device}"),
            content: content.to_string(),
            tags: vec![],
            created_at: "2024-01-01T00:00:00+00:00".to_string(),
            updated_at: "2024-01-02T00:00:00+00:00".to_string(),
            device_id: device.to_string(),
            vector_clock,
            deleted,
            delete_epoch: 0,
            favorite: false,
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional serial inject lock across await
    async fn apply_merge_fail_after_active_rolls_back_all() {
        let _lock = apply_fail_test_lock();
        let _fail = arm_apply_merge_fail_point(ApplyMergeFailPoint::AfterActiveRows);
        let (pool, repo) = setup().await;
        let remote = prompt("p1", "d2", "remote-body", 1, false);
        let err = apply_prompt_merge_batch(
            &pool,
            &repo,
            "peer-1",
            "req-fail-active",
            "hash-a",
            &[remote],
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Bad(_)));
        assert!(repo.get("p1").await.unwrap().is_none());
        let versions = ContentVersionRepo::new(pool.clone())
            .list_versions(DOMAIN_PROMPTS, "p1")
            .await
            .unwrap();
        assert!(versions.is_empty());
        let gate = DatabaseMaintenanceGate::new();
        let (_permit, mut tx) = begin_shared_write(&pool, &gate).await.unwrap();
        let row =
            SyncRequestLedgerRepo::get_on_tx(&mut tx, "peer-1", DOMAIN_PROMPTS, "req-fail-active")
                .await
                .unwrap();
        assert!(row.is_none());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional serial inject lock across await
    async fn apply_merge_fail_after_conflict_rolls_back_all() {
        let _lock = apply_fail_test_lock();
        let _fail = arm_apply_merge_fail_point(ApplyMergeFailPoint::AfterConflictOrMeta);
        let (pool, repo) = setup().await;
        let local = prompt("p1", "left", "left-body", 1, false);
        repo.bulk_upsert(std::slice::from_ref(&local))
            .await
            .unwrap();
        let mut remote = prompt("p1", "right", "right-body", 1, false);
        remote.updated_at = "2024-01-03T00:00:00+00:00".to_string();
        let err = apply_prompt_merge_batch(
            &pool,
            &repo,
            "peer-1",
            "req-fail-conflict",
            "hash-b",
            &[remote],
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Bad(_)));
        let got = repo.get("p1").await.unwrap().unwrap();
        assert_eq!(got.content, "left-body");
        let versions = ContentVersionRepo::new(pool.clone())
            .list_versions(DOMAIN_PROMPTS, "p1")
            .await
            .unwrap();
        assert!(versions.is_empty());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional serial inject lock across await
    async fn apply_merge_writes_conflict_and_accepts() {
        let _lock = apply_fail_test_lock();
        clear_apply_merge_fail_point();
        let (pool, repo) = setup().await;
        let local = prompt("p1", "left", "left-body", 1, false);
        repo.bulk_upsert(&[local]).await.unwrap();
        let mut remote = prompt("p1", "right", "right-body", 1, false);
        remote.updated_at = "2024-01-03T00:00:00+00:00".to_string();
        let outcome = apply_prompt_merge_batch(
            &pool,
            &repo,
            "peer-1",
            "req-ok",
            "hash-ok",
            &[remote],
            Some(3),
        )
        .await
        .unwrap();
        assert_eq!(outcome.accepted, 1);
        let got = repo.get("p1").await.unwrap().unwrap();
        assert_eq!(got.content, "right-body");
        let versions = ContentVersionRepo::new(pool.clone())
            .list_versions(DOMAIN_PROMPTS, "p1")
            .await
            .unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].kind, "conflict");
        let wm = SyncWatermarkRepo::new(pool)
            .get("peer-1", DOMAIN_PROMPTS)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(wm.acked_delete_epoch, 3);
    }

    /// plan_on_tx 必须看到同事务内先前写入的 local。
    #[tokio::test]
    async fn plan_on_tx_sees_local_written_earlier_in_same_tx() {
        let (pool, repo) = setup().await;
        let gate = repo.gate();
        let (_permit, mut tx) = begin_shared_write(&pool, &gate).await.unwrap();
        let local = prompt("p-plan", "left", "left-body", 1, false);
        PromptRepo::bulk_upsert_on_tx(&mut tx, std::slice::from_ref(&local), None)
            .await
            .unwrap();
        let mut remote = prompt("p-plan", "right", "right-body", 1, false);
        remote.updated_at = "2024-01-03T00:00:00+00:00".to_string();
        let plan = build_prompt_merge_plan_on_tx(&mut tx, &[remote], "2024-01-04T00:00:00+00:00")
            .await
            .unwrap();
        assert_eq!(plan.winners.len(), 1);
        assert_eq!(plan.winners[0].content, "right-body");
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].kind, KIND_CONFLICT);
        // 不 commit：仅验证 plan 读到同 tx 行
        drop(tx);
    }

    /// pull_items 对并发左右正文必须保留 conflict 副本。
    ///
    /// 必须持有 apply_fail_test_lock：并行 inject 测试会写进程级 APPLY_FAIL，
    /// 否则可能误命中 AfterActiveRows 注入错误。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional serial inject lock across await
    async fn apply_prompt_pull_items_keeps_conflict_copy() {
        let _lock = apply_fail_test_lock();
        clear_apply_merge_fail_point();
        let (pool, repo) = setup().await;
        let local = prompt("p-pull", "left", "left-body", 1, false);
        repo.bulk_upsert(std::slice::from_ref(&local))
            .await
            .unwrap();
        let mut remote = prompt("p-pull", "right", "right-body", 1, false);
        remote.updated_at = "2024-01-03T00:00:00+00:00".to_string();
        let accepted = apply_prompt_pull_items(&pool, &repo.gate(), &repo, &[remote])
            .await
            .unwrap();
        assert_eq!(accepted, 1);
        let got = repo.get("p-pull").await.unwrap().unwrap();
        assert_eq!(got.content, "right-body");
        let versions = ContentVersionRepo::new(pool)
            .list_versions(DOMAIN_PROMPTS, "p-pull")
            .await
            .unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].kind, "conflict");
    }
}
