//! cloud_sync — GitHub 私有仓库云端同步
//!
//! Business Logic（为什么需要这个模块）:
//!     局域网同步要求所有设备同时在线，无法跨网络、跨时段同步 prompts / CC 历史 / SSH 目标。
//!     把一个 GitHub 私有仓库当作"中心化对端"：本地 SQLite + 向量时钟仍是权威源，
//!     git 只承担传输与历史承载。一次同步 = pull 工作区 → import(merge 进本地) →
//!     export(本地写回工作区) → commit → push 的循环。冲突解决完全复用既有 merge_* 算法
//!     （向量时钟 + LWW + device_id tie-break），git 不参与合并，只保证最终文件一致。
//!     CLAUDE.md 不参与云端自动同步，只由 CLAUDE.md 页面用户主动推送。
//!
//! Code Logic（这个模块做什么）:
//!     - `git_cli`：tokio::process::Command 封装系统 git CLI（detect/run/clone/fetch/
//!       reset_hard/commit_all/push 等），应用不管理认证（复用本机 git 凭证）。
//!     - `snapshot`：工作区 JSON 文件 ↔ DB Row 的导入导出（含 id→文件名可逆映射）。
//!     - `engine`：拼装完整同步流程（含 push rejected 一次重试收敛）+ 测试连通；
//!       并导出 `ensure_repo_public` 供 Agent Hub device-lane 导出复用同一 workdir。
//!     - `runtime`：CloudSyncRuntime 全流程单飞门闸（手动/scheduler/CLAUDE.md push/
//!       AgentHubGitExport）。
//!     - `scheduler`：后台轮询任务，每 tick 重读 config 决定是否真同步、用多少间隔。
//!
//! 同步范围：prompts + CC 历史 + SSH 目标（含软删除传播）。
//! Agent Hub 远端 device lane **永不**经本模块 `snapshot::import_to_db` 自动导入。

pub mod engine;
pub mod git_cli;
pub mod runtime;
pub mod scheduler;
pub mod snapshot;

pub use runtime::{cloud_sync_phase_token, CloudSyncRuntime};
