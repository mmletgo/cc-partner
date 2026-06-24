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

/// 工作台 terminal window DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     前端需要展示本机项目 terminal window 状态、尺寸和退出信息。
///
/// Code Logic（这个结构体做什么）:
///     定义 window 列表与状态事件可复用的数据形状，字段使用 camelCase 序列化。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchSessionDto {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub command: String,
    pub status: String,
    pub cols: u16,
    pub rows: u16,
    pub started_at: String,
    pub exited_at: Option<String>,
    pub exit_code: Option<i32>,
    pub supports_panes: bool,
}

/// 工作台 terminal window 数据库行模型。
///
/// Business Logic（为什么需要这个结构体）:
///     用户希望重启应用后之前打开的 terminal window 仍可恢复，因此 window 元数据需要独立于运行期 PTY 持久保存。
///
/// Code Logic（这个结构体做什么）:
///     对齐 SQLite `workbench_sessions` 表字段；backend_id 记录项目 tmux session，backend_window_id 记录 tmux window，
///     DTO 投影仍只暴露前端展示所需字段。
#[derive(Debug, Clone)]
pub struct WorkbenchSessionRow {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub command: String,
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
    ///     克隆持久化 row 的展示字段，转换为 `WorkbenchSessionDto`。
    pub fn to_dto(&self) -> WorkbenchSessionDto {
        WorkbenchSessionDto {
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            name: self.name.clone(),
            command: self.command.clone(),
            status: self.status.clone(),
            cols: self.cols,
            rows: self.rows,
            started_at: self.started_at.clone(),
            exited_at: self.exited_at.clone(),
            exit_code: self.exit_code,
            supports_panes: self.backend == "tmux" && self.backend_window_id.is_some(),
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
