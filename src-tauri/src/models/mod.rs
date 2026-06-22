//! models — 数据模型层
//!
//! Business Logic: 定义与 Python `models/` 对应的 serde 结构体。
//!     返回给前端的 struct 统一 `#[serde(rename_all = "camelCase")]` 对齐前端 TypeScript 类型。
//!
//! Code Logic: 按模块组织 Prompt / Device / Transfer。Prompt 内部用 snake_case（数据库/同步用），
//!     暴露给前端的 DTO 用 camelCase。

pub mod device;
pub mod prompt;
pub mod transfer;
