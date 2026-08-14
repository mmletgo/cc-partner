//! wordgame — 记单词采集、调度、出题与预热。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户在 Workbench 与 agent 对话时产生大量英文输出；需要把有背诵价值的词
//!     沉淀到本机词库，并按艾宾浩斯曲线出闪卡。词库权威在玩游戏的这台机器上。
//!
//! Code Logic（这个模块做什么）:
//!     提供 tokenize/lemma/lexicon 纯函数、调度状态机、SQLite 仓储、jsonl 增量
//!     ingest、内部 Claude structured JSON 出题，以及启动后的 ingest/preheat worker。

pub mod generate;
pub mod ingest;
pub mod lemma;
pub mod lexicon;
pub mod models;
pub mod runtime;
pub mod schedule;
pub mod tokenize;

pub use runtime::{cancel_wordgame_runtime, start_wordgame_runtime};
