//! workbench/models.rs — 工作台 DTO 与内部数据模型
//!
//! Business Logic（为什么需要这个模块）:
//!     前端工作台需要稳定的项目、会话、文件节点结构；后端也需要 SQLite row 表达项目记录。
//!
//! Code Logic（这个模块做什么）:
//!     定义 WorkbenchProjectRow/Dto、WorkbenchSessionDto、WorkbenchFileNode、WorkbenchPathInfo，
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

/// 工作台终端会话 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     前端需要展示本机 Claude Code 终端会话状态、尺寸和退出信息。
///
/// Code Logic（这个结构体做什么）:
///     定义会话列表与会话状态事件可复用的数据形状，字段使用 camelCase 序列化。
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
