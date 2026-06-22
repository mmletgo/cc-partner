//! models/prompt.rs — Prompt 数据模型
//!
//! Business Logic（为什么需要这个模块）:
//!     Prompt 是核心业务实体，承载可同步的文本记录。需要同时服务两个场景：
//!     1) 数据库读写与 P2P 同步（snake_case，与 Python to_dict/from_dict 互通）；
//!     2) 前端 IPC 返回（camelCase，对齐前端 types.ts）。
//!
//! Code Logic（这个模块做什么）:
//!     - `PromptRow`：snake_case，直接映射 prompts 表一行，tags 为 Vec<String>，
//!       vector_clock 为 HashMap<String,u64>，datetime 用 String 透传（兼容有无时区格式）。
//!     - `PromptDto`：camelCase，比 Row 多一个 `tag`（tags[0] 的投影，兼容旧前端）。
//!     - 提供 Row→Dto 转换，与 Python `_prompt_to_frontend_dict` 字段一一对应。

use std::collections::HashMap;

/// Prompt 数据库行 / 同步实体（snake_case，与 Python Prompt.to_dict 互通）。
///
/// Business Logic: 持久化与跨设备同步需要保留 Python 端的原始字段命名，
///     以便 vector_clock 的 JSON 格式与旧库数据无缝互通。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PromptRow {
    /// UUID 主键
    pub id: String,
    pub title: String,
    pub content: String,
    /// 标签列表（DB 中存为 JSON TEXT）
    pub tags: Vec<String>,
    /// 创建时间 ISO 字符串
    pub created_at: String,
    /// 更新时间 ISO 字符串
    pub updated_at: String,
    /// 创建设备 ID
    pub device_id: String,
    /// 向量时钟 {device_id: counter}（CRDT 同步用）
    pub vector_clock: HashMap<String, u64>,
    /// 软删除标记（0/1）
    pub deleted: bool,
}

/// Prompt 前端 DTO（camelCase，对照前端 types.ts）。
///
/// Business Logic: 前端 TS 类型用 camelCase，与后端 snake_case 不一致，
///     需在 API 边界转换。`tag` 字段为 tags[0] 投影，仅为旧前端向后兼容保留。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptDto {
    pub id: String,
    pub title: String,
    pub content: String,
    /// tags[0] 的投影，无标签时为 null（旧前端兼容）
    pub tag: Option<String>,
    pub tags: Vec<String>,
    pub updated_at: String,
    pub created_at: String,
    pub device_id: String,
    pub vector_clock: HashMap<String, u64>,
    pub deleted: bool,
}

impl PromptRow {
    /// 转换为前端 DTO（snake_case → camelCase + tag 投影）。
    ///
    /// Business Logic: 命令层返回给前端前需做字段名转换并补 tag 投影字段。
    pub fn to_dto(&self) -> PromptDto {
        PromptDto {
            id: self.id.clone(),
            title: self.title.clone(),
            content: self.content.clone(),
            tag: self.tags.first().cloned(),
            tags: self.tags.clone(),
            updated_at: self.updated_at.clone(),
            created_at: self.created_at.clone(),
            device_id: self.device_id.clone(),
            vector_clock: self.vector_clock.clone(),
            deleted: self.deleted,
        }
    }
}

/// 占位：TransferTask 前端 DTO。M5 文件传输里程碑再完善字段。
///
/// Business Logic: M1 不实现传输命令，但模型层先声明类型占位，避免后续大改模块边界。
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferTaskDto {
    pub id: String,
}
