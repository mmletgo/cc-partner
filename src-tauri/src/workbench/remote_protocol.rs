//! workbench/remote_protocol.rs — Workbench 远端 HTTP 协议 DTO
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench 远端网关的 client 与 server route 需要共享请求体定义，避免协议字段漂移。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `/api/workbench/...` 远端路由请求 DTO，统一使用 camelCase 序列化/反序列化。

use serde::{Deserialize, Serialize};

/// 远端项目 ID 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     远端 worktree/Git/files 路由都需要先知道对端设备上的本机项目记录 ID。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{projectId}`，供 client 发送和 axum 路由接收复用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProjectReq {
    pub project_id: String,
}

/// 远端创建 worktree 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     用户在本机 remote shortcut 上创建 worktree 时，实际 Git 操作必须在远端设备执行。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、分支名和可选 baseBranch，字段使用 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCreateWorktreeReq {
    pub project_id: String,
    pub branch_name: String,
    pub base_branch: Option<String>,
}

/// 远端 Git 提交列表请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     本机查看远端项目 Git 历史时，需要让远端按自己的 worktree 路径执行 `git log`。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、可选 worktreeId 和 limit，字段使用 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitCommitsReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub limit: i64,
}

/// 远端文件树列表请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     文件树展开操作需要在远端 active worktree 根内解析相对路径。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、可选 worktreeId 和可选相对 path；path 缺失表示项目根。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteListDirReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub path: Option<String>,
}

/// 远端路径信息请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     远端文件树选中项需要读取远端设备上的真实 metadata。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、可选 worktreeId 和相对 path，字段使用 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePathInfoReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub path: String,
}

/// 远端打开文件请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     本机打开远端文件时，文件检测、预览和文本读取都必须在远端项目边界内完成。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、可选 worktreeId 和相对 path，字段使用 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteOpenFileReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub path: String,
}

/// 远端保存文本请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     远端文本保存要复用本地保存的类型校验和 baseHash 乐观锁，避免跨设备覆盖外部改动。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、可选 worktreeId、相对 path、UTF-8 content 和 baseHash。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSaveTextReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub path: String,
    pub content: String,
    pub base_hash: String,
}

/// 远端创建文件或目录请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     新建文件/目录动作需要在远端设备上验证父路径和单个子名称。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、可选 worktreeId、parentPath 与 name。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCreatePathReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub parent_path: String,
    pub name: String,
}

/// 远端重命名路径请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     远端重命名必须在远端工作区安全边界内执行，不能由本机拼接磁盘路径。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、可选 worktreeId、相对 path 与 newName。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRenamePathReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub path: String,
    pub new_name: String,
}

/// 远端删除路径请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     删除远端文件或目录必须由远端设备复用本地删除安全规则。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、可选 worktreeId 与相对 path。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeletePathReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub path: String,
}

/// 远端终端会话列表请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     本机 remote shortcut 只应按当前选中的远端项目拉取 terminal window，避免后台轮询全部设备。
///
/// Code Logic（这个结构体做什么）:
///     保存可选远端 local projectId；缺失时表示远端设备只返回本机本地范围内的会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteListSessionsReq {
    pub project_id: Option<String>,
}

/// 远端创建终端会话请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     用户在 remote shortcut 上新建 terminal window 时，真实 PTY/tmux 会话必须创建在项目所在设备。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、可选 worktreeId 和前端测量出的初始终端尺寸。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCreateSessionReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub initial_cols: Option<u16>,
    pub initial_rows: Option<u16>,
}

/// 远端终端输入请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     xterm 输入需要按 sessionId 转发到远端设备的 PTY writer。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local sessionId 和 UTF-8 输入数据，字段使用 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWriteSessionInputReq {
    pub session_id: String,
    pub data: String,
}

/// 远端终端 resize 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     本机 terminal viewport 变化时，远端 PTY/tmux 也必须同步行列数。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local sessionId 与新的 cols/rows。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteResizeSessionReq {
    pub session_id: String,
    pub cols: u16,
    pub rows: u16,
}

/// 远端终端 sessionId 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     focus、close-pane、close-session 等操作只需要定位一个远端 terminal window。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local sessionId，供多个 session 路由复用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSessionReq {
    pub session_id: String,
}

/// 远端当前聚焦会话查询请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     tmux status bar 内切换 window 后，本机顶部 tab 需要向远端查询当前 worktree 的 focused session。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId 和可选 worktreeId。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFocusedSessionReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
}

/// 远端当前聚焦会话响应体。
///
/// Business Logic（为什么需要这个结构体）:
///     focused 查询可能没有运行中的 tmux window，响应必须能表达空结果。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase `{sessionId}`，值为远端 local sessionId 或 null。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFocusedSessionResp {
    pub session_id: Option<String>,
}

/// 远端 pane 分屏请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     remote terminal 也需要支持左右/上下 pane 分屏，真实 tmux 操作在远端设备执行。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local sessionId 和 direction 字符串。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSplitPaneReq {
    pub session_id: String,
    pub direction: String,
}

/// 远端终端重命名请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     用户给 remote terminal tab 起名时，需要同步改远端 registry/SQLite/tmux window 名称。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local sessionId 和新名称。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRenameSessionReq {
    pub session_id: String,
    pub name: String,
}
