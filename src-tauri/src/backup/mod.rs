//! backup — 可验证导出与事务恢复
//!
//! Business Logic（为什么需要这个模块）:
//!     用户需要不含项目源码/凭据的导出包、恢复预览、校验与事务恢复；
//!     全部在 sidecar owner 上执行，GUI 仅代理文件选择。
//!
//! Code Logic（这个模块做什么）:
//!     `archive` 负责 ZIP 写出/流式校验；`restore` 负责 inspect/preview/apply/rollback。

pub mod archive;
pub mod restore;

pub use archive::{
    create_export_archive, inspect_archive_streaming, ArchiveLimits, ArchiveManifest,
    DOMAIN_CC_HISTORY, DOMAIN_CLAUDE_MD, DOMAIN_DELETION_FLOORS, DOMAIN_PROMPTS, DOMAIN_SCRATCHPAD,
    DOMAIN_SSH_TARGETS, FORMAT_VERSION, MAX_ARCHIVE_BYTES, MAX_ENTRIES, MAX_ENTRY_BYTES,
    MAX_TOTAL_UNCOMPRESSED,
};
pub use restore::{
    create_pre_restore_backup, list_pre_restore_backups, parse_pre_restore_created_at,
    pre_restore_dir, pre_restore_infos_from_paths, prune_pre_restore_backups,
    rollback_from_pre_restore_backup, BackupRestoreService, CreateBackupResult, InspectPreview,
    PreRestoreBackupInfo, RestoreMode, RestoreRequest, RestoreResult,
};
