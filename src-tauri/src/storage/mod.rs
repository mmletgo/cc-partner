//! storage — SQLite 持久化层
//!
//! Business Logic: 封装所有数据库访问，单连接语义（与 Python aiosqlite 单连接一致），
//!     原地读写旧 `~/.claude-partner/data.db`。M1 仅实现 Prompt 仓库；传输历史留待 M5。
//!
//! Code Logic: 用 sqlx 0.8 的 SqlitePool（max_connections(1)），
//!     运行期 `sqlx::query`（非宏）规避编译期 DATABASE_URL 要求。

pub mod prompt_repo;
pub mod transfer_repo;

pub use prompt_repo::PromptRepo;
pub use transfer_repo::TransferRepo;
