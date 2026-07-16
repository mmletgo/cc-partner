//! workbench/agent_ledger — Agent Metadata Ledger
//!
//! Business Logic（为什么需要这个模块）:
//!     从 A1 runtime 终态自动生成 metadata-only 历史，提供本机分页、时间窗聚合与自动保留清理；
//!     Ledger 失败不得阻断 Agent 完成，也不得成为 runtime/task 真值。
//!
//! Code Logic（这个模块做什么）:
//!     导出 models / service / retention / aggregation。

pub mod aggregation;
pub mod models;
pub mod retention;
pub mod service;

#[allow(unused_imports)]
pub use models::{
    AgentLedgerEntry, AgentLedgerFinalizeInput, AgentLedgerOutcome, AgentLedgerPage,
    AgentLedgerQuery, AgentLedgerSummary, CurrencyAmount, LedgerUsageCoverage, LedgerWindow,
    ReliableUsageSnapshot,
};
#[allow(unused_imports)]
pub use retention::{AgentLedgerRetentionTask, RetentionClock};
pub use service::AgentLedgerService;
