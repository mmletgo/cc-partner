//! models/scratchpad.rs — 速记本单例数据模型
//!
//! Business Logic（为什么需要这个模块）:
//!     速记本是一个跨设备同步的单例自动保存文本。它不是 Prompt 列表，也不拆成多笔记；
//!     DB 中全表仅一行，id 恒为 "scratchpad"，通过向量时钟在局域网和 GitHub 同步中合并。
//!
//! Code Logic（这个模块做什么）:
//!     - `ScratchpadRow`：snake_case，直接映射 scratchpad 表一行，用于 DB / P2P / cloud JSON；
//!     - `ScratchpadDto`：camelCase，给前端 IPC 使用；
//!     - `to_dto`：隐藏同步元数据以外仍返回必要的版本字段，方便页面展示保存状态。

use std::collections::HashMap;

/// 速记本单例 id。
pub const SCRATCHPAD_ID: &str = "scratchpad";

/// 速记本数据库行 / 同步实体（snake_case）。
///
/// Business Logic: 单个速记本文本也需要跨设备冲突解决，因此保留 device_id/vector_clock/deleted。
///     deleted 当前不作为页面删除语义使用（清空是 content=""），但保留字段可与同步快照结构统一。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ScratchpadRow {
    /// 单例主键，恒为 "scratchpad"
    pub id: String,
    /// 速记本文本内容
    pub content: String,
    /// 创建时间 ISO 字符串
    pub created_at: String,
    /// 更新时间 ISO 字符串
    pub updated_at: String,
    /// 最后修改设备 ID
    pub device_id: String,
    /// 向量时钟 {device_id: counter}
    pub vector_clock: HashMap<String, u64>,
    /// 软删除标记；当前清空不使用软删除
    pub deleted: bool,
}

/// 速记本前端 DTO（camelCase）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchpadDto {
    pub id: String,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
    pub device_id: String,
    pub vector_clock: HashMap<String, u64>,
    pub deleted: bool,
}

impl ScratchpadRow {
    /// 转换为前端 DTO（snake_case → camelCase）。
    ///
    /// Business Logic: 页面需要内容和更新时间；同步元数据保留给未来状态提示或调试。
    /// Code Logic: 字段克隆组装 DTO，serde 在边界做 camelCase 序列化。
    pub fn to_dto(&self) -> ScratchpadDto {
        ScratchpadDto {
            id: self.id.clone(),
            content: self.content.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            device_id: self.device_id.clone(),
            vector_clock: self.vector_clock.clone(),
            deleted: self.deleted,
        }
    }
}
