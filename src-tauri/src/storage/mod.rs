//! storage — SQLite 持久化层
//!
//! Business Logic: 封装所有数据库访问，单连接语义（与 Python aiosqlite 单连接一致），
//!     原地读写旧 `~/.claude-partner/data.db`。Prompt / 传输历史 / Claude Code 历史 三类仓库。
//!
//! Code Logic: 用 sqlx 0.8 的 SqlitePool（max_connections(1)），
//!     运行期 `sqlx::query`（非宏）规避编译期 DATABASE_URL 要求。

pub mod cc_history_repo;
pub mod claude_md_repo;
pub mod prompt_repo;
pub mod ssh_target_repo;
pub mod transfer_repo;

pub use cc_history_repo::ClaudeHistoryRepo;
pub use claude_md_repo::ClaudeMdRepo;
pub use prompt_repo::PromptRepo;
pub use ssh_target_repo::SshTargetRepo;
pub use transfer_repo::TransferRepo;
