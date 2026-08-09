//! cc/sources — 多 Agent Prompt 历史采集适配器
//!
//! Business Logic: Claude Code / Codex / OpenCode 用户输入统一入库 Prompt 历史。
//! Code Logic: 各源独立 scan，共用 bulk_ingest 与 scan_state；失败只记日志不阻断其它源。

pub mod codex;
pub mod opencode;
