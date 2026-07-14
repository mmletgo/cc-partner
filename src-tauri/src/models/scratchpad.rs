//! models/scratchpad.rs — 速记本多页面数据模型
//!
//! Business Logic（为什么需要这个模块）:
//!     速记本支持多个自动保存页面。旧单例内容迁移为 id="scratchpad" 的默认页，
//!     新建页面使用 UUID id，并通过向量时钟在局域网和 GitHub 同步中合并。
//!
//! Code Logic（这个模块做什么）:
//!     - `ScratchpadRow`：snake_case，直接映射 scratchpad 表一行，用于 DB / P2P / cloud JSON；
//!     - `ScratchpadPageDto`：camelCase，给前端 IPC 页面详情使用；
//!     - `ScratchpadPageSummaryDto`：camelCase，给前端 IPC 页面列表使用。

use std::collections::HashMap;

/// 旧单例迁移后的默认页 id。
pub const SCRATCHPAD_ID: &str = "scratchpad";

/// 速记本数据库行 / 同步实体（snake_case）。
///
/// Business Logic: 每个速记本页面都需要跨设备冲突解决，因此保留 title/content/device_id/vector_clock/deleted。
///     deleted 用于传播页面软删除；清空内容仍是 content="" 的普通更新。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ScratchpadRow {
    /// 页面主键；默认页为 "scratchpad"，新页为 UUID。
    pub id: String,
    /// 页面标题
    pub title: String,
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
    /// 本域删除序号（tombstone 水位/GC 用；未删除为 0；旧 JSON 缺省 0）
    #[serde(default)]
    pub delete_epoch: u64,
}

/// 速记本页面详情 DTO（camelCase）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchpadPageDto {
    pub id: String,
    pub title: String,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
    pub device_id: String,
    pub vector_clock: HashMap<String, u64>,
    pub deleted: bool,
}

/// 速记本页面列表摘要 DTO（camelCase）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchpadPageSummaryDto {
    pub id: String,
    pub title: String,
    pub updated_at: String,
    pub device_id: String,
    pub deleted: bool,
}

impl ScratchpadRow {
    /// 转换为页面详情 DTO（snake_case → camelCase）。
    ///
    /// Business Logic: 页面需要标题、内容和更新时间；同步元数据保留给未来状态提示或调试。
    /// Code Logic: 字段克隆组装 DTO，serde 在边界做 camelCase 序列化。
    pub fn to_dto(&self) -> ScratchpadPageDto {
        ScratchpadPageDto {
            id: self.id.clone(),
            title: self.title.clone(),
            content: self.content.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            device_id: self.device_id.clone(),
            vector_clock: self.vector_clock.clone(),
            deleted: self.deleted,
        }
    }

    /// 转换为页面列表摘要 DTO。
    ///
    /// Business Logic: 侧栏列表不需要完整 content，但需要标题和更新时间来排序/展示。
    /// Code Logic: 从完整 Row 投影轻量字段，避免命令层重复组装。
    pub fn to_summary_dto(&self) -> ScratchpadPageSummaryDto {
        ScratchpadPageSummaryDto {
            id: self.id.clone(),
            title: self.title.clone(),
            updated_at: self.updated_at.clone(),
            device_id: self.device_id.clone(),
            deleted: self.deleted,
        }
    }
}
