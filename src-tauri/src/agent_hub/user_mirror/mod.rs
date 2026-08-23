//! agent_hub/user_mirror — 用户级全 Agent 镜像 Pull/Push 契约
//!
//! Business Logic（为什么需要这个模块）:
//!     生产 UI 要把用户级 Pull/Push 从逐项复制改成一次镜像全部已登记 Agent；
//!     本模块先落地 wire 合同（能力 token 之外的 DTO / 错误码），后续 inventory/apply 复用同一形状。
//!
//! Code Logic（这个模块做什么）:
//!     导出 DTO/错误码、本机全 Agent 用户级 inventory 扫描，以及源端 CAS selection 冻结。
mod inventory;
mod models;
mod selection;

pub use inventory::build_local_user_mirror_inventory;
pub use models::*;
pub use selection::{
    freeze_user_mirror_selection, source_read_user_mirror_object_chunk, BuiltUserMirrorSelection,
    UserMirrorObjectBinding,
};
