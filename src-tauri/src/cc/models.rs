//! cc/models.rs — Claude Code 历史数据模型
//!
//! Business Logic（为什么需要这个模块）:
//!     采集到的 Claude Code 用户输入 prompt 需要一个结构承载，同时服务四个场景：
//!     1) 数据库读写与 P2P 同步（snake_case，与对端 Rust 版互通）；
//!     2) 前端 IPC 返回（camelCase，对齐前端 types.ts）；
//!     3) 按项目归类的列表聚合（CcProjectDto，前端项目侧边栏用）；
//!     4) 分页同步摘要（CcSyncSummary：仅 id + vector_clock，避免正文内存峰值）。
//!
//! Code Logic（这个模块做什么）:
//!     - `ClaudeHistoryRow`：snake_case，直接映射 claude_history 表一行，
//!       vector_clock 为 HashMap<String,u64>，datetime 用 String 透传。
//!     - `ClaudeHistoryDto`：camelCase，给前端单条详情/列表用。
//!     - `CcProjectDto`：camelCase，按 project_path 聚合的 count + lastOccurredAt。
//!     - `CcSyncSummary`：snake_case，同步 manifest 页摘要 `{id, vector_clock}`。
//!     - 提供 Row→Dto 转换，字段对照前端类型定义。

use std::collections::HashMap;

/// Claude Code 历史数据库行 / 同步实体（snake_case）。
///
/// Business Logic: 持久化与跨设备同步需保留稳定字段命名，以便向量时钟的 JSON
///     格式与各端互通。采集入库时 vector_clock 恒为 `{本机device_id:1}` 且永不递增，
///     仅 delete_cc_prompt 软删除时递增本设备计数器（产生新因果事件让对端感知删除）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClaudeHistoryRow {
    /// 主键：`{session_id}:{message_uuid}`（同 session 内 uuid 唯一，跨 session 用 session_id 前缀隔离）
    pub id: String,
    /// 真实项目路径（取自 jsonl 的 cwd，非目录名反推）
    pub project_path: String,
    /// 项目名（project_path 的末段，前端展示与归组用）
    pub project_name: String,
    /// session id（jsonl 文件名去 .jsonl）
    pub session_id: String,
    /// 用户输入的 prompt 文本
    pub content: String,
    /// git 分支（jsonl 的 gitBranch，可能缺失）
    pub git_branch: Option<String>,
    /// Claude Code 版本（jsonl 的 version，可能缺失）
    pub cc_version: Option<String>,
    /// 该 prompt 发生时间（jsonl 的 timestamp）
    pub occurred_at: String,
    /// 采集/创建该条记录的设备 ID
    pub device_id: String,
    /// 向量时钟 {device_id: counter}（采集恒 {device_id:1}，仅删除递增）
    pub vector_clock: HashMap<String, u64>,
    /// 入库时间 ISO 字符串
    pub created_at: String,
    /// 更新时间 ISO 字符串（同步合并/删除时推进）
    pub updated_at: String,
    /// 软删除标记
    pub deleted: bool,
}

/// Claude Code 历史 前端 DTO（camelCase，对照前端 types.ts）。
///
/// Business Logic: 前端 TS 类型用 camelCase，需在 API 边界做字段名转换。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeHistoryDto {
    pub id: String,
    pub project_path: String,
    pub project_name: String,
    pub session_id: String,
    pub content: String,
    pub git_branch: Option<String>,
    pub cc_version: Option<String>,
    pub occurred_at: String,
    pub device_id: String,
    pub vector_clock: HashMap<String, u64>,
    pub created_at: String,
    pub updated_at: String,
    pub deleted: bool,
}

/// Claude Code 项目聚合 DTO（camelCase，前端项目侧边栏用）。
///
/// Business Logic: 前端按项目展示历史时，需要每个项目的 prompt 数量与最近活动时间，
///     由 list_projects 聚合查询直接产出（避免前端再统计）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcProjectDto {
    pub project_path: String,
    pub project_name: String,
    /// 该项目下未删除的 prompt 数量
    pub count: u64,
    /// 该项目下最近一条 prompt 的 occurred_at
    pub last_occurred_at: String,
}

/// Claude Code 历史同步摘要（snake_case，P2P manifest 页用）。
///
/// Business Logic（为什么需要这个类型）:
///     分页同步协议先交换摘要再按需拉正文，避免把全部 content 载入内存。
///     摘要只含主键与向量时钟，供客户端比较因果领先关系并决定 items/push 批次。
///
/// Code Logic（这个结构做什么）:
///     snake_case 字段 `{id, vector_clock}`，与 ClaudeHistoryRow 同步字段对齐；
///     由 `list_sync_manifest_page` 从 DB 直接投影，不读 content。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CcSyncSummary {
    /// 历史行主键（`{session_id}:{message_uuid}`）
    pub id: String,
    /// 向量时钟 {device_id: counter}，用于比较领先/并发
    pub vector_clock: HashMap<String, u64>,
}

impl ClaudeHistoryRow {
    /// 转换为前端 DTO（snake_case → camelCase）。
    ///
    /// Business Logic: 命令层返回给前端前需做字段名转换。
    pub fn to_dto(&self) -> ClaudeHistoryDto {
        ClaudeHistoryDto {
            id: self.id.clone(),
            project_path: self.project_path.clone(),
            project_name: self.project_name.clone(),
            session_id: self.session_id.clone(),
            content: self.content.clone(),
            git_branch: self.git_branch.clone(),
            cc_version: self.cc_version.clone(),
            occurred_at: self.occurred_at.clone(),
            device_id: self.device_id.clone(),
            vector_clock: self.vector_clock.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            deleted: self.deleted,
        }
    }

    /// 从 project_path 取末段作为 project_name（采集器构造 Row 时用）。
    ///
    /// Business Logic: 前端项目归组展示需要一个简短名称，取路径末段（如
    ///     `/Users/hans/foo` → `foo`）；末段为空（路径以 / 结尾）时回退整个路径。
    /// Code Logic: 同时按 `/` 与 `\` 分隔，兼容本机查看跨平台同步来的路径；
    ///     无有效末段时回退原路径字符串。
    pub fn derive_project_name(project_path: &str) -> String {
        project_path
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .find(|segment| !segment.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| project_path.to_string())
    }
}
