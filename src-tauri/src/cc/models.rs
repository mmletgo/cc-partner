//! cc/models.rs — Prompt 历史数据模型（内部表名仍为 claude_history）
//!
//! Business Logic（为什么需要这个模块）:
//!     采集到的多 Agent 用户输入 prompt 需要一个结构承载，同时服务四个场景：
//!     1) 数据库读写与 P2P 同步（snake_case，与对端 Rust 版互通）；
//!     2) 前端 IPC 返回（camelCase，对齐前端 types.ts）；
//!     3) 按项目归类的列表聚合（CcProjectDto，前端项目侧边栏用）；
//!     4) 分页同步摘要（CcSyncSummary：仅 id + vector_clock，避免正文内存峰值）。
//!
//! Code Logic（这个模块做什么）:
//!     - `ClaudeHistoryRow`：snake_case，直接映射 claude_history 表一行，
//!       vector_clock 为 HashMap<String,u64>，datetime 用 String 透传；
//!       `source` 为 claude|codex|opencode|grok|gemini，缺省 `claude` 兼容旧数据；
//!       cursor/pi token 仅为 catalog 预留，本模块没有对应采集器。
//!     - `ClaudeHistoryDto`：camelCase，给前端单条详情/列表用。
//!     - `CcProjectDto`：camelCase，按 project_path 聚合的 count + lastOccurredAt。
//!     - `CcSyncSummary`：snake_case，同步 manifest 页摘要 `{id, vector_clock}`。
//!     - 提供 Row→Dto 转换，字段对照前端类型定义。

use std::collections::HashMap;

/// Prompt 历史来源：Claude Code。
pub const SOURCE_CLAUDE: &str = "claude";
/// Prompt 历史来源：Codex。
pub const SOURCE_CODEX: &str = "codex";
/// Prompt 历史来源：OpenCode。
pub const SOURCE_OPENCODE: &str = "opencode";
/// Prompt 历史来源：Grok Build。
pub const SOURCE_GROK: &str = "grok";
/// Prompt 历史来源：Gemini CLI。
pub const SOURCE_GEMINI: &str = "gemini";

/// 缺省 source（旧对端 / 旧备份 / 旧库行）。
fn default_source_claude() -> String {
    SOURCE_CLAUDE.to_string()
}

/// Prompt 历史数据库行 / 同步实体（snake_case）。
///
/// Business Logic: 持久化与跨设备同步需保留稳定字段命名，以便向量时钟的 JSON
///     格式与各端互通。采集入库时 vector_clock 恒为 `{本机device_id:1}` 且永不递增，
///     仅 delete_cc_prompt 软删除时递增本设备计数器（产生新因果事件让对端感知删除）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClaudeHistoryRow {
    /// 主键：Claude 为 `{session_id}:{message_uuid}`；Codex/OpenCode 带源前缀防碰撞
    pub id: String,
    /// 真实项目路径（取自 cwd，非目录名反推）
    pub project_path: String,
    /// 项目名（project_path 的末段，前端展示与归组用）
    pub project_name: String,
    /// session id
    pub session_id: String,
    /// 用户输入的 prompt 文本
    pub content: String,
    /// git 分支（可能缺失）
    pub git_branch: Option<String>,
    /// 来源工具版本（字段名历史遗留 cc_version；可能缺失）
    pub cc_version: Option<String>,
    /// 该 prompt 发生时间
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
    /// 采集来源：`claude` | `codex` | `opencode` | `grok` | `gemini`；
    /// `cursor` / `pi` 仅预留且当前无 collector；旧数据缺字段默认 claude
    #[serde(default = "default_source_claude")]
    pub source: String,
}

/// Prompt 历史 前端 DTO（camelCase，对照前端 types.ts）。
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
    /// 采集来源：`claude` | `codex` | `opencode` | `grok` | `gemini`；
    /// `cursor` / `pi` 仅预留且当前无 collector
    #[serde(default = "default_source_claude")]
    pub source: String,
}

/// Prompt 历史项目聚合 DTO（camelCase，前端项目侧边栏用）。
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

/// Prompt 历史所属设备 DTO（camelCase，前端设备筛选器用）。
///
/// Business Logic: 同步后历史可能来自多台设备，页面需要稳定设备 id、可读名称以及本机标记，
///     才能默认只显示本机项目，并允许用户显式切换到其他设备。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcHistoryDeviceDto {
    pub id: String,
    pub name: String,
    pub is_self: bool,
}

/// Prompt 历史同步摘要（snake_case，P2P manifest 页用）。
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
    /// 历史行主键
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
            source: self.source.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_row_without_source_defaults_to_claude() {
        let json = r#"{
            "id":"s:u","project_path":"/p","project_name":"p","session_id":"s",
            "content":"hi","occurred_at":"t","device_id":"d1",
            "vector_clock":{"d1":1},"created_at":"t","updated_at":"t","deleted":false
        }"#;
        let row: ClaudeHistoryRow = serde_json::from_str(json).unwrap();
        assert_eq!(row.source, SOURCE_CLAUDE);
    }
}
