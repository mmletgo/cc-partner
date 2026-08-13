//! storage — SQLite 持久化层
//!
//! Business Logic: 封装所有数据库访问，单连接语义（与 Python aiosqlite 单连接一致），
//!     原地读写 `~/.cc-partner/data.db`。Prompt / 传输历史 / Claude Code 历史 三类仓库。
//!
//! Code Logic: 用 sqlx 0.8 的 SqlitePool（max_connections(1)），
//!     运行期 `sqlx::query`（非宏）规避编译期 DATABASE_URL 要求。

pub mod agent_hub_repo;
pub mod agent_ledger_repo;
pub mod cc_history_repo;
pub mod claude_md_repo;
pub mod content_version_repo;
pub mod deletion_floor_repo;
pub mod health_repo;
pub mod maintenance_gate;
pub mod prompt_repo;
pub mod recovery_job_repo;
pub mod scratchpad_repo;
pub mod ssh_target_repo;
pub mod sync_delete_sequence_repo;
pub mod sync_request_ledger_repo;
pub mod sync_watermark_repo;
pub mod transfer_repo;
pub mod workbench_agent_session_repo;
pub mod workbench_browser_repo;
pub mod workbench_project_note_repo;
pub mod workbench_project_repo;
pub mod workbench_session_repo;
pub mod workbench_workspace_layout_repo;
pub mod workbench_worktree_repo;

pub use agent_hub_repo::{
    AgentHubCheckoutBindingRow, AgentHubImportFault, AgentHubRepo, UpsertAgentHubCheckoutBinding,
    UpsertAgentHubProjectMapping,
};
// AgentHubProjectMappingRow 由调用方按需 `crate::storage::agent_hub_repo::` 全路径引用，避免 unused re-export。
pub use agent_ledger_repo::AgentLedgerRepo;
pub use cc_history_repo::ClaudeHistoryRepo;
pub use claude_md_repo::ClaudeMdRepo;
pub use content_version_repo::ContentVersionRepo;
pub use deletion_floor_repo::DeletionFloorRepo;
// health_repo 的 ActivityRecord / HealthRepo 通过全限定路径 `crate::storage::health_repo::...`
// 引用（health 模块内部），不在此 re-export，避免 unused_imports 告警。
// begin_shared_write / lease helpers 由调用方经 `maintenance_gate::` 全路径导入，
// 避免 re-export 在 lib/lib-test 间出现 unused_imports。
pub use maintenance_gate::DatabaseMaintenanceGate;
pub use prompt_repo::PromptRepo;
pub use recovery_job_repo::{RecoveryJobRepo, RecoveryJobRow};
pub use scratchpad_repo::ScratchpadRepo;
pub use ssh_target_repo::SshTargetRepo;
pub use sync_delete_sequence_repo::SyncDeleteSequenceRepo;
pub use sync_request_ledger_repo::SyncRequestLedgerRepo;
pub use sync_watermark_repo::SyncWatermarkRepo;
#[allow(unused_imports)]
pub use transfer_repo::{SenderClaimOutcome, TransferRepo};
pub use workbench_agent_session_repo::WorkbenchAgentSessionRepo;
pub use workbench_browser_repo::WorkbenchBrowserRepo;
pub use workbench_project_note_repo::WorkbenchProjectNoteRepo;
pub use workbench_project_repo::WorkbenchProjectRepo;
pub use workbench_session_repo::WorkbenchSessionRepo;
pub use workbench_workspace_layout_repo::WorkbenchWorkspaceLayoutRepo;
pub use workbench_worktree_repo::WorkbenchWorktreeRepo;

/// 为 prompts / ssh_targets / scratchpad 确保 `delete_epoch INTEGER NOT NULL DEFAULT 0` 列。
///
/// Business Logic（为什么需要这个函数）:
///     本地/采纳删除时需在同一事务写入单调 delete_epoch；旧库无该列时必须幂等补齐。
///
/// Code Logic（这个函数做什么）:
///     对每张表 `PRAGMA table_info` 检查列名；缺失则 `ALTER TABLE ... ADD COLUMN delete_epoch INTEGER NOT NULL DEFAULT 0`。
pub async fn ensure_domain_delete_epoch_columns(
    pool: &sqlx::sqlite::SqlitePool,
) -> Result<(), crate::error::AppError> {
    for table in ["prompts", "ssh_targets", "scratchpad"] {
        let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(pool)
            .await?;
        // 表不存在时 PRAGMA 返回空集：单测/局部 schema 场景跳过，避免 ALTER 失败。
        if columns.is_empty() {
            continue;
        }
        let has_col = columns.iter().any(|row| {
            use sqlx::Row;
            row.try_get::<String, _>("name")
                .map(|name| name == "delete_epoch")
                .unwrap_or(false)
        });
        if !has_col {
            sqlx::query(&format!(
                "ALTER TABLE {table} ADD COLUMN delete_epoch INTEGER NOT NULL DEFAULT 0"
            ))
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// 为 prompts 表确保 `favorite INTEGER NOT NULL DEFAULT 0` 列。
///
/// Business Logic（为什么需要这个函数）:
///     Prompt 收藏(favorite)是跟随整行 vector_clock + LWW 同步的元数据字段；旧库无该列时
///     必须幂等补齐，否则新代码读写 favorite 会失败。模式与 `ensure_domain_delete_epoch_columns`
///     对齐，仅作用于 prompts 单表。
///
/// Code Logic（这个函数做什么）:
///     `PRAGMA table_info(prompts)` 检查列名；缺失则
///     `ALTER TABLE prompts ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0`；
///     表不存在时 PRAGMA 返回空集，跳过避免 ALTER 失败（单测/局部 schema 场景）。
pub async fn ensure_prompts_favorite_column(
    pool: &sqlx::sqlite::SqlitePool,
) -> Result<(), crate::error::AppError> {
    let columns = sqlx::query("PRAGMA table_info(prompts)")
        .fetch_all(pool)
        .await?;
    // 表不存在时 PRAGMA 返回空集：单测/局部 schema 场景跳过，避免 ALTER 失败。
    if columns.is_empty() {
        return Ok(());
    }
    let has_col = columns.iter().any(|row| {
        use sqlx::Row;
        row.try_get::<String, _>("name")
            .map(|name| name == "favorite")
            .unwrap_or(false)
    });
    if !has_col {
        sqlx::query("ALTER TABLE prompts ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0")
            .execute(pool)
            .await?;
    }
    Ok(())
}
