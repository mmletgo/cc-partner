//! agent_hub/user_mirror — 用户级全 Agent 镜像 Pull/Push 契约
//!
//! Business Logic（为什么需要这个模块）:
//!     生产 UI 要把用户级 Pull/Push 从逐项复制改成一次镜像全部已登记 Agent；
//!     本模块先落地 wire 合同（能力 token 之外的 DTO / 错误码），后续 inventory/apply 复用同一形状。
//!
//! Code Logic（这个模块做什么）:
//!     导出 DTO/错误码、本机全 Agent 用户级 inventory 扫描、源端 CAS selection 冻结，
//!     源/目标 inventory 的 replace-plus-delete preview diff、dest 写盘 apply，
//!     以及 preview plan ledger 与 get() 幂等对账。
mod apply;
mod inventory;
mod ledger;
mod models;
mod preview;
mod selection;
mod service;

pub use apply::apply_user_mirror_instructions;
pub use inventory::build_local_user_mirror_inventory;
pub use ledger::{UserMirrorClaim, UserMirrorPlanRecord};
pub use models::*;
pub use preview::{diff_inventories, preview_from_two_inventories};
pub use selection::{
    freeze_user_mirror_selection, source_read_user_mirror_object_chunk, BuiltUserMirrorSelection,
    UserMirrorObjectBinding,
};
pub use service::{apply_user_mirror, get_user_mirror, preview_user_mirror};
