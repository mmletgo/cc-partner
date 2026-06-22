//! cc — Claude Code 历史 prompt 采集与跨设备同步
//!
//! Business Logic（为什么需要这个模块）:
//!     用户在本机用 Claude Code 时产生的所有"用户输入 prompt"分散在
//!     `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl` 中，难以检索和跨设备复用。
//!     本模块负责自动采集这些 prompt、按项目(cwd)归类入库，并复用同步基础设施跨设备同步，
//!     让用户在任意设备都能查到自己在各项目里问过 Claude 什么。
//!
//! Code Logic（这个模块做什么）:
//!     - `models`：数据库行/同步实体（snake_case）与前端 DTO（camelCase）及转换；
//!     - `merger`：复用 `sync::vector_clock` 做 LWW 合并（采集不递增时钟，仅删除递增）；
//!     - `collector`：定时扫描 jsonl，INSERT OR IGNORE 入库，记录 scan_state 增量去重；
//!     - `engine`：`cc_sync_with_peer` 与 prompts 同步链路独立，走 `/api/cc-history/sync/*`。

pub mod collector;
pub mod engine;
pub mod merger;
pub mod models;
