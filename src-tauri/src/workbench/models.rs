//! workbench/models.rs — 工作台 DTO 与内部数据模型
//!
//! Business Logic（为什么需要这个模块）:
//!     前端工作台需要稳定的项目、会话、文件节点结构；后端也需要 SQLite row 表达项目记录。
//!
//! Code Logic（这个模块做什么）:
//!     定义 WorkbenchProjectRow/Dto、WorkbenchSessionRow/Dto、WorkbenchFileNode、WorkbenchPathInfo，
//!     所有前端 DTO 使用 camelCase。

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// 工作台项目数据库行模型。
///
/// Business Logic（为什么需要这个结构体）:
///     用户添加过的本机项目需要持久化，重启后仍可出现在最近项目列表中。
///
/// Code Logic（这个结构体做什么）:
///     对齐 SQLite `workbench_projects` 表字段，保持 snake_case 供后端内部和 SQL 映射使用。
#[derive(Debug, Clone)]
pub struct WorkbenchProjectRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub device_id: String,
    pub device_name: String,
    pub path: String,
    pub last_opened_at: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 工作台项目 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     前端工作台需要展示最近项目列表，并使用 camelCase 字段与 TypeScript 类型对齐。
///
/// Code Logic（这个结构体做什么）:
///     序列化/反序列化工作台项目的 UI 合同，字段来自 `WorkbenchProjectRow`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchProjectDto {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub device_id: String,
    pub device_name: String,
    pub path: String,
    pub last_opened_at: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Git worktree 状态摘要 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     Workbench 顶部 worktree 管理层需要展示当前分支是否有改动、是否领先/落后远端以及是否存在冲突。
///
/// Code Logic（这个结构体做什么）:
///     表达 `git status --porcelain --branch` 解析后的轻量状态，字段使用 camelCase 序列化给前端；
///     can_push 由后端按 upstream/origin 规则派生，供 UI 禁用不可推送项目。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchGitStatusDto {
    pub branch: Option<String>,
    pub changed: usize,
    pub ahead: u32,
    pub behind: u32,
    pub conflicts: usize,
    pub clean: bool,
    pub can_push: bool,
}

/// Git 引用类型 DTO。
///
/// Business Logic（为什么需要这个枚举）:
///     Git 历史树需要区分本地分支、远端分支和 tag，帮助用户判断本地与云端位置。
///
/// Code Logic（这个枚举做什么）:
///     用稳定字符串序列化给前端，避免前端解析 Git ref 前缀。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkbenchGitRefKindDto {
    Local,
    Remote,
    Tag,
    Head,
    Other,
}

/// Git 引用标签 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     Git 历史树需要在提交旁标识 main、origin/main、tag 等位置。
///
/// Code Logic（这个结构体做什么）:
///     保存展示名、完整 ref、类型、远端名和是否为当前 HEAD。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchGitRefDto {
    pub name: String,
    pub full_name: String,
    pub kind: WorkbenchGitRefKindDto,
    pub remote: Option<String>,
    pub is_head: bool,
}

/// Git 提交历史项 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     Workbench 右侧 Git 历史 tab 需要展示 active worktree 的最近提交与分支图。
///
/// Code Logic（这个结构体做什么）:
///     表达 `git log` 的单条提交摘要、父提交和 refs，字段使用 camelCase 序列化给前端。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchGitCommitDto {
    pub hash: String,
    pub short_hash: String,
    pub parent_hashes: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub authored_at: String,
    pub summary: String,
    pub refs: Vec<WorkbenchGitRefDto>,
}

/// 工作台 Git worktree 数据库行模型。
///
/// Business Logic（为什么需要这个结构体）:
///     用户在 Workbench 中创建的 worktree 需要持久化，重启后仍能作为项目下的工作区切换。
///
/// Code Logic（这个结构体做什么）:
///     对齐 SQLite `workbench_worktrees` 表字段；Git 实时状态不落库，由命令层动态查询后注入 DTO。
#[derive(Debug, Clone)]
pub struct WorkbenchWorktreeRow {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    pub path: String,
    pub is_main: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 工作台 Git worktree DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     前端需要用 worktree strip 切换工作区，并在同一层展示分支状态和路径。
///
/// Code Logic（这个结构体做什么）:
///     将持久化 row 与运行期 Git 状态合并为 camelCase UI 合同。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchWorktreeDto {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    pub path: String,
    pub is_main: bool,
    pub status: WorkbenchGitStatusDto,
    pub created_at: String,
    pub updated_at: String,
}

impl WorkbenchProjectRow {
    /// Business Logic（为什么需要这个函数）:
    ///     前端只消费 camelCase DTO，数据库 row 不应直接泄露给 UI。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆 row 字段并转换为 `WorkbenchProjectDto`。
    pub fn to_dto(&self) -> WorkbenchProjectDto {
        WorkbenchProjectDto {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            device_id: self.device_id.clone(),
            device_name: self.device_name.clone(),
            path: self.path.clone(),
            last_opened_at: self.last_opened_at.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

impl WorkbenchWorktreeRow {
    /// Business Logic（为什么需要这个函数）:
    ///     前端消费的是带 Git 状态的 DTO，数据库 row 只负责持久化 worktree 元数据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆 row 字段并注入调用方提供的 Git 状态摘要，转换为 `WorkbenchWorktreeDto`。
    pub fn to_dto(&self, status: WorkbenchGitStatusDto) -> WorkbenchWorktreeDto {
        WorkbenchWorktreeDto {
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            name: self.name.clone(),
            branch: self.branch.clone(),
            base_branch: self.base_branch.clone(),
            path: self.path.clone(),
            is_main: self.is_main,
            status,
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

/// 工作台 terminal window DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     前端需要展示本机项目 terminal window 状态、尺寸、退出信息以及 window 内 pane 数。
///
/// Code Logic（这个结构体做什么）:
///     定义 window 列表与状态事件可复用的数据形状，字段使用 camelCase 序列化。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchSessionDto {
    pub id: String,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub status: String,
    pub cols: u16,
    pub rows: u16,
    pub started_at: String,
    pub exited_at: Option<String>,
    pub exit_code: Option<i32>,
    pub supports_panes: bool,
    pub pane_count: usize,
}

/// 工作台 terminal window 数据库行模型。
///
/// Business Logic（为什么需要这个结构体）:
///     用户希望重启应用后之前打开的 terminal window 仍可恢复，因此 window 元数据需要独立于运行期 PTY 持久保存。
///
/// Code Logic（这个结构体做什么）:
///     对齐 SQLite `workbench_sessions` 表字段；backend_id 记录 worktree tmux session，backend_window_id 记录 tmux window，
///     DTO 投影仍只暴露前端展示所需字段。
#[derive(Debug, Clone)]
pub struct WorkbenchSessionRow {
    pub id: String,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub status: String,
    pub cols: u16,
    pub rows: u16,
    pub started_at: String,
    pub exited_at: Option<String>,
    pub exit_code: Option<i32>,
    pub backend: String,
    pub backend_id: Option<String>,
    pub backend_window_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl WorkbenchSessionRow {
    /// Business Logic（为什么需要这个函数）:
    ///     前端会话列表只需要 UI 字段，不应暴露后端重连实现细节。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆持久化 row 的展示字段，转换为 `WorkbenchSessionDto`，pane 数默认按单 pane 处理。
    pub fn to_dto(&self) -> WorkbenchSessionDto {
        self.to_dto_with_pane_count(1)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     项目列表需要显示真实 pane 数，而该数据来自运行期 tmux 查询，不属于 SQLite row。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆 row 字段并注入调用方计算出的 pane_count，生成前端 camelCase DTO。
    pub fn to_dto_with_pane_count(&self, pane_count: usize) -> WorkbenchSessionDto {
        WorkbenchSessionDto {
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            worktree_id: self.worktree_id.clone(),
            name: self.name.clone(),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            status: self.status.clone(),
            cols: self.cols,
            rows: self.rows,
            started_at: self.started_at.clone(),
            exited_at: self.exited_at.clone(),
            exit_code: self.exit_code,
            supports_panes: self.backend == "tmux" && self.backend_window_id.is_some(),
            pane_count,
        }
    }
}

/// 工作台文件树节点 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     工作台右侧文件树需要统一表示文件夹、文件、大小、修改时间和懒加载子节点。
///
/// Code Logic（这个结构体做什么）:
///     表达一项文件系统节点，children 为 None 表示未加载或非目录。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchFileNode {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub size: Option<u64>,
    pub modified_at: Option<String>,
    pub children: Option<Vec<WorkbenchFileNode>>,
}

/// 工作台路径信息 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     创建、重命名、查询路径后，前端需要拿到目标路径的最新元信息以刷新文件树。
///
/// Code Logic（这个结构体做什么）:
///     表达单个路径的名称、相对路径、类型、大小和修改时间，字段使用 camelCase 序列化。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchPathInfo {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub size: Option<u64>,
    pub modified_at: Option<String>,
}

/// Workbench 文件检测类型。
///
/// Business Logic（为什么需要这个类型）:
///     前端文件工作区需要知道一个文件应由图片预览、Markdown/HTML 预览编辑器、代码编辑器、
///     CSV/SQLite 只读预览还是普通文本编辑器打开。
///
/// Code Logic（这个类型做什么）:
///     用 serde lowercase 序列化为前端 helper 使用的稳定枚举字符串。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkbenchDetectedFileType {
    Image,
    Markdown,
    Html,
    Code,
    Json,
    Toml,
    Yaml,
    Csv,
    Sqlite,
    Text,
    Binary,
    Unsupported,
}

/// Workbench 文件工作区显示模式 DTO。
///
/// Business Logic（为什么需要这个枚举）:
///     前端打开文件后需要知道默认进入只读预览、代码编辑、Markdown 所见即所得、源码或分屏模式。
///
/// Code Logic（这个枚举做什么）:
///     用 lowercase 字符串序列化为 viewer/editor/wysiwyg/source/split，与前端 `WorkbenchFileMode` 对齐。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkbenchFileMode {
    Viewer,
    Editor,
    Wysiwyg,
    Source,
    Split,
}

/// Workbench 文件能力 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     文件工作区工具栏需要根据文件类型隐藏不可用操作，避免用户对只读预览执行保存或格式化。
///
/// Code Logic（这个结构体做什么）:
///     以 camelCase 字段告诉前端文件是否可预览、可编辑、可格式化、保存前是否必须校验，
///     以及默认模式和可切换模式列表。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchFileCapabilities {
    pub can_preview: bool,
    pub can_edit: bool,
    pub can_format: bool,
    pub must_validate_before_save: bool,
    pub default_mode: WorkbenchFileMode,
    pub available_modes: Vec<WorkbenchFileMode>,
}

/// Workbench 文本内容 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     可编辑文件打开后必须携带内容和保存基线，防止外部编辑器修改被 Workbench 静默覆盖。
///
/// Code Logic（这个结构体做什么）:
///     保存 UTF-8 文本、打开时的 SHA256 hash 和可选修改时间，字段序列化为 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchTextContent {
    pub content: String,
    pub base_hash: String,
    pub base_modified_at: Option<String>,
}

/// Workbench CSV 预览 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     CSV 第一版只读展示表格，用户需要看到表头、预览行以及是否被行数限制截断。
///
/// Code Logic（这个结构体做什么）:
///     columns 表示表头或 fallback 列名，rows 保存 JSON-safe 字符串矩阵，truncated 表示预览未覆盖全量文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchCsvPreview {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub truncated: bool,
}

/// Workbench 图片预览 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     图片文件需要在 Workbench 内只读查看，不能按文本读取导致乱码或内存浪费。
///
/// Code Logic（这个结构体做什么）:
///     data_url 供前端 img 直接渲染，mime 标识图片类型，width/height 在可解码时返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchImagePreview {
    pub data_url: String,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Workbench HTML/Markdown 预览资源 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     HTML iframe 与 Markdown WYSIWYG 预览需要把项目内相对 CSS、图片和字体资源安全内联，避免前端直接访问本机文件。
///
/// Code Logic（这个结构体做什么）:
///     返回资源相对路径、MIME、大小、data URL，以及文本类资源的 UTF-8 内容供前端二次重写 CSS url()。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchHtmlAssetDto {
    pub path: String,
    pub mime: String,
    pub size: u64,
    pub data_url: String,
    pub text: Option<String>,
}

/// Workbench SQLite 预览 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     SQLite 第一版只允许只读浏览表结构和少量行，避免通过文件工作区误写数据库。
///
/// Code Logic（这个结构体做什么）:
///     tables 保存用户表名，selected_table/columns/rows 表示当前表预览，truncated 表示只返回前 N 行。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchSqlitePreview {
    pub tables: Vec<String>,
    pub selected_table: Option<String>,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub truncated: bool,
}

/// Workbench 文件打开响应 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     前端打开文件时需要一次拿到元信息、检测类型、能力和具体内容或预览数据。
///
/// Code Logic（这个结构体做什么）:
///     用互斥 Option 字段承载文本、图片、CSV、SQLite 预览；不支持或超限时通过 notice 告知原因。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchOpenFileDto {
    pub metadata: WorkbenchPathInfo,
    pub detected_type: WorkbenchDetectedFileType,
    pub capabilities: WorkbenchFileCapabilities,
    pub text: Option<WorkbenchTextContent>,
    pub image: Option<WorkbenchImagePreview>,
    pub csv: Option<WorkbenchCsvPreview>,
    pub sqlite: Option<WorkbenchSqlitePreview>,
    pub truncated: bool,
    pub notice: Option<String>,
}

/// Workbench 文本保存响应 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     保存成功后前端需要刷新文件 metadata，并更新下一次保存使用的 hash 基线。
///
/// Code Logic（这个结构体做什么）:
///     返回最新路径信息、保存后 SHA256 hash 和可选修改时间，字段序列化为 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchSaveTextResultDto {
    pub metadata: WorkbenchPathInfo,
    pub base_hash: String,
    pub base_modified_at: Option<String>,
}
