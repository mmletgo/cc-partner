//! workbench/remote_protocol.rs — Workbench 远端 HTTP 协议 DTO
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench 远端网关的 client 与 server route 需要共享请求体定义，避免协议字段漂移。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `/api/workbench/...` 远端路由请求 DTO，统一使用 camelCase 序列化/反序列化。

use serde::{Deserialize, Serialize};

/// 远端浏览器候选发现请求体。
///
/// Business Logic（为什么需要这个类型）:
///     本机 remote shortcut 需要把浏览器候选发现请求转发到项目 owning device 执行。
///
/// Code Logic（这个类型做什么）:
///     复用 Workbench browser 模型中的 camelCase discover 请求，避免远端协议字段漂移。
pub type RemoteWorkbenchBrowserDiscoverReq =
    crate::workbench::browser_models::WorkbenchBrowserDiscoverReq;

/// 远端浏览器 preview 创建请求体。
///
/// Business Logic（为什么需要这个类型）:
///     远端项目必须在 owning device 创建真实 preview，本机只创建 relay session。
///
/// Code Logic（这个类型做什么）:
///     复用 Workbench browser 模型中的 camelCase preview 请求，包含 projectId/worktreeId/targetUrl。
pub type RemoteWorkbenchBrowserPreviewReq =
    crate::workbench::browser_models::WorkbenchBrowserPreviewReq;

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

/// 远端 worktree ID 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     commit/push/merge/remove 等命令只需要定位远端设备上的一个本机 worktree。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{worktreeId}`，供 client 与 axum route 共用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorktreeReq {
    pub worktree_id: String,
    /// 稳定 client operation id（mutation-outcome.v1；push/merge 共用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_operation_id: Option<String>,
}

/// 远端 commit worktree 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     本机 remote shortcut 点击 commit 时，提交动作和可选 message 应发送到项目所在设备执行。
///
/// Code Logic（这个结构体做什么）:
///     保存远端本机 worktreeId 与可选提交信息，字段使用 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCommitWorktreeReq {
    pub worktree_id: String,
    pub message: Option<String>,
    /// 稳定 client operation id（mutation-outcome.v1）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_operation_id: Option<String>,
}

/// 远端删除 worktree 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     删除远端 worktree 时，用户可能选择强制删除未完全干净的工作区。
///
/// Code Logic（这个结构体做什么）:
///     保存远端本机 worktreeId 和可选 force 开关，字段使用 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRemoveWorktreeReq {
    pub worktree_id: String,
    pub force: Option<bool>,
    /// 稳定 client operation id（mutation-outcome.v1）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_operation_id: Option<String>,
}

/// 远端查询 mutation operation 请求。
///
/// Business Logic（为什么需要这个结构体）:
///     unknown 后需向 owning device 查询 ledger intent/state。
///
/// Code Logic（这个结构体做什么）:
///     camelCase clientOperationId。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMutationOperationReq {
    pub client_operation_id: String,
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

/// 远端 SQLite 预览请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     用户切换远端 SQLite 表时，必须由远端设备读取数据库，不能退回本机路径解析。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、可选 worktreeId、相对 path、可选 table 和 limitRows。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePreviewSqliteReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub path: String,
    pub table: Option<String>,
    pub limit_rows: Option<i64>,
}

/// 远端 HTML/Markdown 资源预览请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     远端 HTML iframe 与 Markdown 预览引用的相对资源必须在项目所在设备读取。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local projectId、可选 worktreeId、当前文档相对路径和资源引用路径。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePreviewHtmlAssetReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub document_path: String,
    pub asset_path: String,
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
///     focus、switch-pane、zoom-pane、close-pane、close-session 等操作只需要定位一个远端 terminal window。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local sessionId，供多个 session 路由复用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSessionReq {
    pub session_id: String,
}

/// 远端终端 replay 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     移动端首次打开远端终端时，需要在订阅增量事件前按 sessionId 拉取最近输出。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{sessionId}`，供 client 与 axum route 共用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteReplaySessionReq {
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

/// 远端 pane 坐标选中请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     remote terminal 也支持点击切换 pane，坐标命中必须在 owning device 的 tmux 上完成。
///
/// Code Logic（这个结构体做什么）:
///     保存远端 local sessionId 与终端字符格坐标（0 基，col 为列、row 为行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSelectPaneAtReq {
    pub session_id: String,
    pub col: u32,
    pub row: u32,
}

/// 远端 pane 坐标选中响应体。
///
/// Business Logic（为什么需要这个结构体）:
///     本机需要知道远端是否真的换了 pane，才能区分成功切换与 zoom/边框/已 active 的 no-op。
///
/// Code Logic（这个结构体做什么）:
///     保存被选中的 pane_id（无命中为 None）与 changed 标记；缺省字段前向兼容旧对端。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSelectPaneAtResp {
    #[serde(default)]
    pub pane_id: Option<String>,
    #[serde(default)]
    pub changed: bool,
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

/// 远端 Prompt 优化写入终端请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     本机 Workbench 连接远端 terminal 时，Prompt 优化必须在项目所在设备运行并写入远端终端。
///
/// Code Logic（这个结构体做什么）:
///     保存原始 prompt、可选远端工作目录、目标语种和远端 local sessionId，字段使用 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePromptOptimizerReq {
    pub prompt: String,
    pub working_directory: Option<String>,
    pub target_language: String,
    pub session_id: String,
}

/// 远端搜索 Claude session 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     本机在 remote shortcut 的 workbench 里搜索历史 Claude Code 会话时，
///     真实 transcript 解析必须发生在项目所在设备，故搜索请求需带上远端 local projectId、
///     可选 worktreeId（限定 worktree 范围）和 query 关键词。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{projectId, worktreeId?, query}`，供 client 发送和 axum 路由接收复用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSearchClaudeSessionsReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub query: String,
}

/// 远端读取单个 Claude session preview 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     用户在搜索结果中选中某条远端 session 后，preview 面板需要拿到该 session 的最近消息、
///     标题、cwd 等详情；这些数据只能由项目所在设备从 jsonl transcript 解析得到。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{projectId, worktreeId?, sessionId}`，供 client 与 axum route 共用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteClaudeSessionReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub session_id: String,
}

/// 远端 resume Claude session 结果体。
///
/// Business Logic（为什么需要这个结构体）:
///     resume 会在远端设备新建一个 workbench terminal 并注入 `claude --resume` 命令，
///     发起方需要拿到远端新建的 inner sessionId 以包装成本机统一 remote sessionId。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{ok, sessionId}`；route 返回 inner sessionId（不包装 remote: 前缀），
///     由发起方命令层按设备 ID 包装。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeClaudeSessionResult {
    pub ok: bool,
    pub session_id: String,
}

/// owner-local workspace restore preflight 请求（inner IDs only）。
///
/// Business Logic（为什么需要这个结构体）:
///     控制设备 layout 不上传；owner 只接收本机 project/worktree/session 做纯读探测。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；不含 layout name/绝对路径。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkspaceRestorePreflightReq {
    /// owner 本机 project id。
    pub project_id: String,
    /// owner 本机 worktree id。
    pub active_worktree_id: Option<String>,
    /// owner 本机 session id。
    pub active_session_id: Option<String>,
    /// workspace view token（与 layout 枚举对齐，由 serde 处理）。
    pub workspace_view: crate::workbench::workspace_layout::WorkspaceView,
    /// inspector tab。
    pub inspector_tab: crate::workbench::workspace_layout::InspectorTab,
    /// 可选 browser target。
    pub browser_target_url: Option<String>,
}

/// owner-local safe attach 请求。
///
/// Business Logic（为什么需要这个结构体）:
///     apply 阶段把 inner sessionId 交给 owning device 幂等 attach。
///
/// Code Logic（这个结构体做什么）:
///     仅 session_id。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSafeAttachReq {
    /// owner 本机 session id。
    pub session_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Business Logic（为什么需要这个测试）:
    ///     移动端普通浏览器调用 Prompt 优化 HTTP 路由时，workingDirectory 允许传 null 表示无项目上下文。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化 camelCase 请求体，断言 JSON null 被解析为 None。
    #[test]
    fn remote_prompt_optimizer_req_accepts_null_working_directory() {
        let req: RemotePromptOptimizerReq = serde_json::from_value(json!({
            "prompt": "优化这个任务",
            "workingDirectory": null,
            "targetLanguage": "zh",
            "sessionId": "session-1"
        }))
        .expect("null workingDirectory should be accepted");

        assert!(req.working_directory.is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     HTTP optional 字段省略时也必须成立，否则 `{ workingDirectory? }` 契约不完整。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化缺少 workingDirectory 的请求体，断言字段默认为 None。
    #[test]
    fn remote_prompt_optimizer_req_accepts_missing_working_directory() {
        let req: RemotePromptOptimizerReq = serde_json::from_value(json!({
            "prompt": "优化这个任务",
            "targetLanguage": "zh",
            "sessionId": "session-1"
        }))
        .expect("missing workingDirectory should be accepted");

        assert!(req.working_directory.is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     桌面远端 Workbench 仍会发送具体远端项目路径，不能因为可选契约丢失该路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化带字符串 workingDirectory 的请求体，断言字符串原样保留在 Some 中。
    #[test]
    fn remote_prompt_optimizer_req_preserves_string_working_directory() {
        let req: RemotePromptOptimizerReq = serde_json::from_value(json!({
            "prompt": "优化这个任务",
            "workingDirectory": "/remote/repo",
            "targetLanguage": "zh",
            "sessionId": "session-1"
        }))
        .expect("string workingDirectory should be accepted");

        assert_eq!(req.working_directory.as_deref(), Some("/remote/repo"));
    }
}
