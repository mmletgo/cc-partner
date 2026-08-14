//! 充电模式账本：额度策略、SQLite 状态与扣时。
//!
//! Business Logic（为什么需要这个模块）:
//!     充电模式用健康行为与闪卡换工作分钟，需要本机权威余额，而不是前端 localStorage。
//!
//! Code Logic（这个模块做什么）:
//!     policy 是纯计算；repo 管 schema / 状态 / 流水；service 组合 credit / debit / mode。

pub mod policy;
pub mod service;

pub use policy::{credit_delta_ms, credit_delta_ms_explicit, debit_delta_ms, MS_PER_MINUTE};
pub use service::{
    credit, credit_explicit, game_plugin_source_id, get_snapshot, habit_source_id, list_ledger,
    report_focus, set_mode, wordgame_source_id, BatterySnapshotDto,
};
