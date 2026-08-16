//! workbench/browser_models.rs — Workbench 浏览器预览 DTO
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench 浏览器预览需要在桌面端、移动端和局域网远端之间传递统一的发现候选、
//!     预览会话和请求数据。
//!
//! Code Logic（这个模块做什么）:
//!     定义 serde camelCase DTO，供后续命令、HTTP route、远端 client 与前端共享字段契约。

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// 浏览器预览候选来源。
///
/// Business Logic（为什么需要这个枚举）:
///     用户需要知道候选 URL 是来自历史选择、终端输出、项目配置、端口探测还是手动输入。
///
/// Code Logic（这个枚举做什么）:
///     作为 WorkbenchBrowserTarget.source 的稳定枚举，并用 camelCase 序列化给前端。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkbenchBrowserTargetSource {
    Remembered,
    TerminalOutput,
    ProjectConfig,
    PortProbe,
    Manual,
}

/// 浏览器预览目标候选。
///
/// Business Logic（为什么需要这个结构体）:
///     Workbench 需要展示可预览的本机 dev server 候选，并让用户选择其中一个创建预览。
///
/// Code Logic（这个结构体做什么）:
///     保存候选的稳定 id、规范化 URL、展示 URL、来源、兼容 label key 和当前可达性。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchBrowserTarget {
    pub id: String,
    pub url: String,
    pub display_url: String,
    pub source: WorkbenchBrowserTargetSource,
    pub label: String,
    pub reachable: bool,
}

/// 浏览器预览自动发现结果。
///
/// Business Logic（为什么需要这个结构体）:
///     打开 Workbench browser tab 时，前端需要一次性拿到项目、worktree、候选列表和默认选择。
///
/// Code Logic（这个结构体做什么）:
///     聚合 project_id、可选 worktree_id、已排序候选和默认 selected_target_id（不含 PortProbe）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchBrowserDiscovery {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub targets: Vec<WorkbenchBrowserTarget>,
    pub selected_target_id: Option<String>,
}

/// 浏览器预览代理会话。
///
/// Business Logic（为什么需要这个结构体）:
///     创建预览后，桌面端和移动端需要通过不同代理 URL 安全访问同一个 dev server。
///
/// Code Logic（这个结构体做什么）:
///     保存预览 id、所属项目/worktree、目标 URL、桌面代理 URL、移动端代理 path 和过期时间。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchBrowserPreview {
    pub preview_id: String,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub target_url: String,
    pub desktop_proxy_url: String,
    pub mobile_proxy_path: String,
    pub expires_at_ms: i64,
}

/// 浏览器预览发现请求。
///
/// Business Logic（为什么需要这个结构体）:
///     桌面端、移动端和远端 route 都需要用同一请求体触发候选发现。
///
/// Code Logic（这个结构体做什么）:
///     保存项目 id 和可选 worktree id，序列化为 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchBrowserDiscoverReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
}

/// 浏览器预览创建请求。
///
/// Business Logic（为什么需要这个结构体）:
///     用户选择目标 URL 后，需要创建一个绑定项目/worktree 的预览代理会话。
///
/// Code Logic（这个结构体做什么）:
///     保存项目 id、可选 worktree id 和待代理的 target_url。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchBrowserPreviewReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub target_url: String,
}
