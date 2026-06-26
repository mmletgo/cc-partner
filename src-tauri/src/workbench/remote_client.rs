//! workbench/remote_client.rs — Workbench 远端 HTTP 客户端
//!
//! Business Logic（为什么需要这个模块）:
//!     本机 Workbench 需要通过局域网对端的 P2P HTTP server 浏览目录并打开远端项目，
//!     让用户不必手动挂载共享目录也能保存远端项目快捷方式。
//!
//! Code Logic（这个模块做什么）:
//!     封装 reqwest::Client，调用 `/api/workbench/...` 远端路由，并把网络、状态码与 JSON
//!     解析错误统一转换为简洁中文 AppError。

use crate::error::AppError;
use crate::workbench::models::{
    WorkbenchFileNode, WorkbenchGitCommitDto, WorkbenchOpenFileDto, WorkbenchPathInfo,
    WorkbenchProjectDto, WorkbenchRemoteDirectoryEntryDto, WorkbenchRemotePathInfoDto,
    WorkbenchRemoteRootDto, WorkbenchSaveTextResultDto, WorkbenchSessionDto, WorkbenchWorktreeDto,
};
use crate::workbench::remote_protocol::{
    RemoteCreatePathReq, RemoteCreateSessionReq, RemoteCreateWorktreeReq, RemoteDeletePathReq,
    RemoteFocusedSessionReq, RemoteFocusedSessionResp, RemoteGitCommitsReq, RemoteListDirReq,
    RemoteListSessionsReq, RemoteOpenFileReq, RemotePathInfoReq, RemoteProjectReq,
    RemoteRenamePathReq, RemoteRenameSessionReq, RemoteResizeSessionReq, RemoteSaveTextReq,
    RemoteSessionReq, RemoteSplitPaneReq, RemoteWriteSessionInputReq,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::time::Duration;

const SHORT_REMOTE_WORKBENCH_TIMEOUT_SECS: u64 = 15;
const LONG_REMOTE_WORKBENCH_TIMEOUT_SECS: u64 = 120;
const REMOTE_ERROR_BODY_MAX_CHARS: usize = 240;

/// 远端请求超时类别。
///
/// Business Logic（为什么需要这个枚举）:
///     Workbench 既有目录浏览这类短读操作，也有创建 worktree、保存文件等可能耗时的远端操作。
///
/// Code Logic（这个枚举做什么）:
///     区分短请求和长请求，供每个 reqwest request 单独设置 timeout。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteRequestTimeoutKind {
    Short,
    Long,
}

/// Workbench 远端 HTTP 客户端。
///
/// Business Logic（为什么需要这个结构体）:
///     多个远端 Workbench 命令需要复用同一套 HTTP 调用与错误映射规则。
///
/// Code Logic（这个结构体做什么）:
///     持有 cloneable 的 `reqwest::Client`，对外提供目录根、目录列表、路径信息和打开项目方法。
#[derive(Clone)]
pub struct RemoteWorkbenchClient {
    client: reqwest::Client,
}

impl RemoteWorkbenchClient {
    /// 创建 Workbench 远端客户端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     命令层每次处理远端请求时需要一个可直接使用的客户端实例。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造不带全局超时的 reqwest client；每个请求按短/长操作单独设置 timeout。
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .build()
            .expect("构造 Workbench 远端 reqwest Client 失败");
        Self { client }
    }

    /// 获取远端设备可浏览的根目录。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户添加远端项目时，需要先看到对端的 Home、下载、常用代码目录等入口。
    ///
    /// Code Logic（这个函数做什么）:
    ///     GET `{base_url}/api/workbench/fs/roots`，解析为 `WorkbenchRemoteRootDto` 列表。
    pub async fn roots(&self, base_url: &str) -> Result<Vec<WorkbenchRemoteRootDto>, AppError> {
        self.get_json(
            endpoint_url(base_url, "/api/workbench/fs/roots"),
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 列出远端目录下的一级条目。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     远端项目选择器需要逐层浏览对端文件系统，直到用户选中目标项目目录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/fs/list`，请求体 `{path}`，解析目录条目 DTO 列表。
    pub async fn list_dir(
        &self,
        base_url: &str,
        path: &str,
    ) -> Result<Vec<WorkbenchRemoteDirectoryEntryDto>, AppError> {
        self.post_path_json(
            endpoint_url(base_url, "/api/workbench/fs/list"),
            path,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 获取远端路径信息。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户选中远端路径时，前端需要判断路径是否可读、是否为 Git 仓库以及建议项目名。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/fs/info`，请求体 `{path}`，解析单个路径信息 DTO。
    pub async fn path_info(
        &self,
        base_url: &str,
        path: &str,
    ) -> Result<WorkbenchRemotePathInfoDto, AppError> {
        self.post_path_json(
            endpoint_url(base_url, "/api/workbench/fs/info"),
            path,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 在远端设备打开项目。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机保存远端快捷方式前，需要让远端设备先创建或复用它自己的本机 Workbench 项目记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/projects/open`，请求体 `{path}`，解析远端返回的项目 DTO。
    pub async fn open_project(
        &self,
        base_url: &str,
        path: &str,
    ) -> Result<WorkbenchProjectDto, AppError> {
        self.post_path_json(
            endpoint_url(base_url, "/api/workbench/projects/open"),
            path,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 列出远端项目下的 Git worktree。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机 remote shortcut 打开后，需要展示对端项目的主工作区和功能 worktree。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/worktrees/list`，请求体 `{projectId}`，解析 worktree DTO 列表。
    pub async fn list_worktrees(
        &self,
        base_url: &str,
        project_id: &str,
    ) -> Result<Vec<WorkbenchWorktreeDto>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/list"),
            &RemoteProjectReq {
                project_id: project_id.to_string(),
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 在远端项目中创建 Git worktree。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户对 remote shortcut 点击新建 worktree 时，分支和目录应创建在远端设备。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/worktrees/create`，解析远端新建后的 worktree DTO。
    pub async fn create_worktree(
        &self,
        base_url: &str,
        req: RemoteCreateWorktreeReq,
    ) -> Result<WorkbenchWorktreeDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/create"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 列出远端 worktree 的 Git 提交。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Git 历史树必须读取远端 worktree 的真实仓库状态，而不是本机 shortcut 路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/git/commits`，解析提交摘要 DTO 列表。
    pub async fn list_git_commits(
        &self,
        base_url: &str,
        project_id: &str,
        worktree_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<WorkbenchGitCommitDto>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/git/commits"),
            &RemoteGitCommitsReq {
                project_id: project_id.to_string(),
                worktree_id: worktree_id.map(str::to_string),
                limit,
            },
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 列出远端项目目录。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机文件树展开 remote shortcut 时，需要让远端设备按本地文件安全规则读取目录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/list-dir`，解析文件节点列表。
    pub async fn list_workbench_dir(
        &self,
        base_url: &str,
        project_id: &str,
        worktree_id: Option<&str>,
        path: Option<&str>,
    ) -> Result<Vec<WorkbenchFileNode>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/list-dir"),
            &RemoteListDirReq {
                project_id: project_id.to_string(),
                worktree_id: worktree_id.map(str::to_string),
                path: path.map(str::to_string),
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 获取远端项目内路径信息。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     文件树选中远端路径后，需要读取远端 metadata 供详情面板展示。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/info`，解析 `WorkbenchPathInfo`。
    pub async fn workbench_path_info(
        &self,
        base_url: &str,
        project_id: &str,
        worktree_id: Option<&str>,
        path: &str,
    ) -> Result<WorkbenchPathInfo, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/info"),
            &RemotePathInfoReq {
                project_id: project_id.to_string(),
                worktree_id: worktree_id.map(str::to_string),
                path: path.to_string(),
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 打开远端项目内文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     远端文件的检测、预览和文本读取必须由远端设备执行并返回统一 DTO。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/open`，解析完整文件打开响应。
    pub async fn open_file(
        &self,
        base_url: &str,
        project_id: &str,
        worktree_id: Option<&str>,
        path: &str,
    ) -> Result<WorkbenchOpenFileDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/open"),
            &RemoteOpenFileReq {
                project_id: project_id.to_string(),
                worktree_id: worktree_id.map(str::to_string),
                path: path.to_string(),
            },
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 保存远端项目内文本文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     远端文件编辑保存需要把 content 和 baseHash 发送到远端设备执行原子写入。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/save-text`，解析保存后的 metadata/hash。
    pub async fn save_text_file(
        &self,
        base_url: &str,
        req: RemoteSaveTextReq,
    ) -> Result<WorkbenchSaveTextResultDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/save-text"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 在远端项目内创建文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机文件树的新建文件动作需要在远端磁盘上创建空文件并返回 metadata。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/create-file`，解析 `WorkbenchPathInfo`。
    pub async fn create_file(
        &self,
        base_url: &str,
        req: RemoteCreatePathReq,
    ) -> Result<WorkbenchPathInfo, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/create-file"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 在远端项目内创建目录。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机文件树的新建目录动作需要在远端磁盘上执行并返回 metadata。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/create-dir`，解析 `WorkbenchPathInfo`。
    pub async fn create_dir(
        &self,
        base_url: &str,
        req: RemoteCreatePathReq,
    ) -> Result<WorkbenchPathInfo, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/create-dir"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 重命名远端项目内路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     文件树重命名 remote 文件/目录时，真实操作必须发生在远端设备。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/rename`，解析重命名后的 `WorkbenchPathInfo`。
    pub async fn rename_path(
        &self,
        base_url: &str,
        req: RemoteRenamePathReq,
    ) -> Result<WorkbenchPathInfo, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/rename"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 删除远端项目内路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     文件树删除 remote 文件/目录时，远端设备必须复用本地删除安全规则。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/delete`，解析轻量 `{ok,path}` 响应。
    pub async fn delete_path(
        &self,
        base_url: &str,
        req: RemoteDeletePathReq,
    ) -> Result<serde_json::Value, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/delete"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 列出远端终端会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机 remote shortcut 进入项目后，需要展示该远端项目下已有的 terminal window。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/list`，请求体 `{projectId?}`，解析 session DTO 列表。
    pub async fn list_sessions(
        &self,
        base_url: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<WorkbenchSessionDto>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/sessions/list"),
            &RemoteListSessionsReq {
                project_id: project_id.map(str::to_string),
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 创建远端终端会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户在 remote shortcut 上新建 terminal window 时，真实 PTY/tmux 必须创建在远端设备。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/create`，解析远端创建出的 session DTO。
    pub async fn create_session(
        &self,
        base_url: &str,
        req: RemoteCreateSessionReq,
    ) -> Result<WorkbenchSessionDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/sessions/create"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 向远端终端写入输入。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机 xterm 捕获键盘输入后，需要转发到远端设备的对应 PTY writer。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/write`，成功后忽略对端 `{ok}` 响应。
    pub async fn write_input(
        &self,
        base_url: &str,
        session_id: &str,
        data: &str,
    ) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/write"),
                &RemoteWriteSessionInputReq {
                    session_id: session_id.to_string(),
                    data: data.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(())
    }

    /// 调整远端终端尺寸。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机 terminal viewport 变化时，远端 PTY/tmux 也需要收到新的 cols/rows。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/resize`，成功后忽略对端 `{ok}` 响应。
    pub async fn resize(
        &self,
        base_url: &str,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/resize"),
                &RemoteResizeSessionReq {
                    session_id: session_id.to_string(),
                    cols,
                    rows,
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(())
    }

    /// 聚焦远端终端 window。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机顶部 tab 切换到 remote terminal 时，远端 tmux current window 需要同步切换。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/focus`，成功后忽略对端 `{ok}` 响应。
    pub async fn focus(&self, base_url: &str, session_id: &str) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/focus"),
                &RemoteSessionReq {
                    session_id: session_id.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(())
    }

    /// 查询远端当前聚焦终端 window。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户在远端 tmux status bar 内切换 window 后，本机 UI 需要知道当前 active session。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/focused`，解析远端 local sessionId 或 None。
    pub async fn focused(
        &self,
        base_url: &str,
        project_id: &str,
        worktree_id: Option<&str>,
    ) -> Result<Option<String>, AppError> {
        let response: RemoteFocusedSessionResp = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/focused"),
                &RemoteFocusedSessionReq {
                    project_id: project_id.to_string(),
                    worktree_id: worktree_id.map(str::to_string),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(response.session_id)
    }

    /// 创建远端 tmux pane 分屏。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote terminal 需要复用远端 tmux 的真实 pane 布局能力。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/split-pane`，成功后忽略对端 `{ok}` 响应。
    pub async fn split_pane(
        &self,
        base_url: &str,
        session_id: &str,
        direction: &str,
    ) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/split-pane"),
                &RemoteSplitPaneReq {
                    session_id: session_id.to_string(),
                    direction: direction.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(())
    }

    /// 关闭远端当前 pane。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户关闭 remote terminal pane 时，真实 kill-pane/close-window 应在远端设备执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/close-pane`，解析 `closedWindow` 布尔值。
    pub async fn close_pane(&self, base_url: &str, session_id: &str) -> Result<bool, AppError> {
        let response: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/close-pane"),
                &RemoteSessionReq {
                    session_id: session_id.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(response
            .get("closedWindow")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    /// 关闭远端终端会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户关闭 remote terminal tab 时，远端 registry、SQLite 和 PTY/tmux 后端都应清理。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/close`，成功后忽略对端 `{ok}` 响应。
    pub async fn close_session(&self, base_url: &str, session_id: &str) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/close"),
                &RemoteSessionReq {
                    session_id: session_id.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(())
    }

    /// 重命名远端终端会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户给 remote terminal tab 改名时，远端 tmux window 与持久化 row 需要同步更新。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/rename`，解析更新后的 session DTO。
    pub async fn rename_session(
        &self,
        base_url: &str,
        session_id: &str,
        name: &str,
    ) -> Result<WorkbenchSessionDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/sessions/rename"),
            &RemoteRenameSessionReq {
                session_id: session_id.to_string(),
                name: name.to_string(),
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端 Workbench GET 调用都需要统一处理网络错误、HTTP 状态码和 JSON 解析错误。
    ///
    /// Code Logic（这个函数做什么）:
    ///     发送 GET 请求，非成功状态转中文业务错误，成功后解析 JSON 为目标类型。
    async fn get_json<T>(
        &self,
        url: String,
        timeout_kind: RemoteRequestTimeoutKind,
    ) -> Result<T, AppError>
    where
        T: DeserializeOwned,
    {
        let response = self
            .client
            .get(&url)
            .timeout(remote_request_timeout(timeout_kind))
            .send()
            .await
            .map_err(|error| AppError::generic(format!("远端 Workbench 请求失败: {error}")))?;
        parse_json_response(response).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端路径类 POST 调用都使用相同的 `{path}` 请求体和响应解析规则。
    ///
    /// Code Logic（这个函数做什么）:
    ///     发送 JSON body `{path}`，非成功状态转中文业务错误，成功后解析 JSON 为目标类型。
    async fn post_path_json<T>(
        &self,
        url: String,
        path: &str,
        timeout_kind: RemoteRequestTimeoutKind,
    ) -> Result<T, AppError>
    where
        T: DeserializeOwned,
    {
        let body = serde_json::json!({ "path": path });
        self.post_json(url, &body, timeout_kind).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端 Workbench POST 调用大多使用不同 DTO 请求体，但错误处理与 JSON 解析规则一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     发送 JSON body，非成功状态转中文业务错误，成功后按泛型解析 JSON。
    async fn post_json<T, B>(
        &self,
        url: String,
        body: &B,
        timeout_kind: RemoteRequestTimeoutKind,
    ) -> Result<T, AppError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let response = self
            .client
            .post(&url)
            .json(body)
            .timeout(remote_request_timeout(timeout_kind))
            .send()
            .await
            .map_err(|error| AppError::generic(format!("远端 Workbench 请求失败: {error}")))?;
        parse_json_response(response).await
    }
}

impl Default for RemoteWorkbenchClient {
    /// 创建默认 Workbench 远端客户端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     调用方在需要默认客户端时可以复用标准构造逻辑。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `RemoteWorkbenchClient::new` 返回带默认超时的客户端。
    fn default() -> Self {
        Self::new()
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端 Workbench 需要同时支持快速浏览和耗时写入，不能用单一 client-level timeout 限制所有接口。
///
/// Code Logic（这个函数做什么）:
///     将请求类别映射为具体 Duration，供每个 request builder 单独设置超时。
fn remote_request_timeout(kind: RemoteRequestTimeoutKind) -> Duration {
    match kind {
        RemoteRequestTimeoutKind::Short => Duration::from_secs(SHORT_REMOTE_WORKBENCH_TIMEOUT_SECS),
        RemoteRequestTimeoutKind::Long => Duration::from_secs(LONG_REMOTE_WORKBENCH_TIMEOUT_SECS),
    }
}

/// Business Logic（为什么需要这个函数）:
///     调用方可能传入带尾斜杠的 base URL，远端客户端应始终拼出唯一规范路径。
///
/// Code Logic（这个函数做什么）:
///     去掉 base URL 尾部 `/`，再追加以 `/` 开头的 API path。
fn endpoint_url(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

/// Business Logic（为什么需要这个函数）:
///     所有远端 Workbench 响应都需要统一错误语义，避免各方法返回不同格式的错误文案。
///
/// Code Logic（这个函数做什么）:
///     检查 HTTP 2xx 状态；非 2xx 返回 `AppError::generic`；成功时按泛型解析 JSON。
async fn parse_json_response<T>(response: reqwest::Response) -> Result<T, AppError>
where
    T: DeserializeOwned,
{
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AppError::generic(remote_error_message(status, &body)));
    }
    response
        .json::<T>()
        .await
        .map_err(|error| AppError::generic(format!("远端 Workbench 响应解析失败: {error}")))
}

/// Business Logic（为什么需要这个函数）:
///     远端业务错误通常由对端 AppError 序列化为 `{error}`，本机应保留原始业务文案。
///
/// Code Logic（这个函数做什么）:
///     优先从 JSON body 读取非空 error 字段；否则返回 HTTP 状态与截断后的正文摘要。
fn remote_error_message(status: reqwest::StatusCode, body: &str) -> String {
    let trimmed = body.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            let error = error.trim();
            if !error.is_empty() {
                return error.to_string();
            }
        }
    }
    if trimmed.is_empty() {
        return format!("远端 Workbench 请求失败: HTTP {status}");
    }
    format!(
        "远端 Workbench 请求失败: HTTP {status}: {}",
        truncate_error_body(trimmed)
    )
}

/// Business Logic（为什么需要这个函数）:
///     远端非 JSON 错误可能包含代理 HTML 或长堆栈，完整回传会降低前端错误可读性。
///
/// Code Logic（这个函数做什么）:
///     按 Unicode char 截断错误正文，超长时追加省略号。
fn truncate_error_body(body: &str) -> String {
    let mut chars = body.chars();
    let truncated: String = chars.by_ref().take(REMOTE_ERROR_BODY_MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::models::{
        WorkbenchPathInfo, WorkbenchRemoteDirectoryEntryDto, WorkbenchSaveTextResultDto,
        WorkbenchSessionDto, WorkbenchWorktreeDto,
    };
    use crate::workbench::remote_protocol::RemoteCreateSessionReq;
    use axum::extract::State;
    use axum::routing::post;
    use axum::{http::StatusCode, Json, Router};
    use serde_json::Value;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    /// Business Logic（为什么需要这个函数）:
    ///     远端客户端测试需要一个本地 HTTP 服务来验证请求路径、请求体和响应解析。
    ///
    /// Code Logic（这个函数做什么）:
    ///     启动临时 axum server，记录收到的 JSON body，并返回本地 base URL 与共享记录。
    async fn spawn_list_dir_server() -> (String, Arc<Mutex<Option<Value>>>) {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/fs/list",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(vec![WorkbenchRemoteDirectoryEntryDto {
                            name: "src".to_string(),
                            path: "/tmp/app/src".to_string(),
                            kind: "dir".to_string(),
                            modified_at: None,
                            is_git_repo: false,
                        }])
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), seen_body)
    }

    /// Business Logic（为什么需要这个测试）:
    ///     浏览目录等短操作不应被长时间阻塞，但保存文件、创建 worktree 等远端重操作需要更宽松的等待窗口。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接校验远端请求超时策略 helper，确保短/长两类请求不会共用单一 client-level timeout。
    #[test]
    fn remote_request_timeout_separates_short_and_long_operations() {
        assert_eq!(
            remote_request_timeout(RemoteRequestTimeoutKind::Short),
            Duration::from_secs(15)
        );
        assert_eq!(
            remote_request_timeout(RemoteRequestTimeoutKind::Long),
            Duration::from_secs(120)
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端路由会返回本地业务错误，客户端必须保留这些错误文案给前端展示。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时服务返回非 2xx JSON `{error}`，断言远端客户端提取 error 字段而不是只报 HTTP 状态。
    #[tokio::test]
    async fn parse_json_response_uses_remote_error_field() {
        let app = Router::new().route(
            "/api/workbench/worktrees/list",
            post(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "远端项目必须是本机项目" })),
                )
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let error = RemoteWorkbenchClient::new()
            .list_worktrees(&format!("http://{addr}"), "project-1")
            .await
            .expect_err("non-success JSON error should fail");

        assert_eq!(error.to_string(), "远端项目必须是本机项目");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     非 JSON 错误响应仍应带上短正文，方便定位对端代理或反序列化问题。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时服务返回非 2xx 文本 body，断言错误包含 HTTP 状态和正文摘要。
    #[tokio::test]
    async fn parse_json_response_uses_plain_body_fallback() {
        let app = Router::new().route(
            "/api/workbench/worktrees/list",
            post(|| async { (StatusCode::BAD_GATEWAY, "plain upstream failure") }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let error = RemoteWorkbenchClient::new()
            .list_worktrees(&format!("http://{addr}"), "project-1")
            .await
            .expect_err("non-success plain body should fail");
        let message = error.to_string();

        assert!(message.contains("HTTP 502 Bad Gateway"));
        assert!(message.contains("plain upstream failure"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机通过远端目录选择器浏览对端目录时，必须调用约定的 HTTP 路由并发送 `{path}` 请求体。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动临时 HTTP 服务，调用 `list_dir`，断言请求体 path 正确且响应 DTO 被解析。
    #[tokio::test]
    async fn list_dir_posts_path_and_parses_entries() {
        let (base_url, seen_body) = spawn_list_dir_server().await;
        let client = RemoteWorkbenchClient::new();

        let entries = client.list_dir(&base_url, "/tmp/app").await.unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "src");
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["path"], "/tmp/app");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     设备发现拿到的 base URL 未来可能携带尾斜杠，客户端不能因此产生双斜杠路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入带尾斜杠的 base URL，断言拼出的 API URL 只保留一个路径分隔。
    #[test]
    fn endpoint_url_trims_trailing_slash() {
        let url = endpoint_url("http://127.0.0.1:1420/", "/api/workbench/fs/roots");

        assert_eq!(url, "http://127.0.0.1:1420/api/workbench/fs/roots");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 Workbench 文件保存必须沿用本地保存的乐观锁语义，调用方需要确认请求体字段名与前端/Rust DTO 一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动临时 HTTP 服务接收 save-text 请求，断言 client 发送 camelCase body 并解析保存结果。
    #[tokio::test]
    async fn save_text_file_posts_camel_case_body_and_parses_result() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/files/save-text",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(WorkbenchSaveTextResultDto {
                            metadata: WorkbenchPathInfo {
                                name: "note.md".to_string(),
                                path: "docs/note.md".to_string(),
                                kind: "file".to_string(),
                                size: Some(7),
                                modified_at: None,
                            },
                            base_hash: "new-hash".to_string(),
                            base_modified_at: None,
                        })
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = RemoteWorkbenchClient::new();

        let result = client
            .save_text_file(
                &format!("http://{addr}"),
                RemoteSaveTextReq {
                    project_id: "project-1".to_string(),
                    worktree_id: Some("worktree-1".to_string()),
                    path: "docs/note.md".to_string(),
                    content: "# Note\n".to_string(),
                    base_hash: "old-hash".to_string(),
                },
            )
            .await
            .unwrap();

        assert_eq!(result.base_hash, "new-hash");
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["projectId"], "project-1");
        assert_eq!(body["worktreeId"], "worktree-1");
        assert_eq!(body["baseHash"], "old-hash");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机打开远端项目后需要通过远端 local project id 读取对端 worktree 列表。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时 HTTP 服务返回一个 worktree DTO，断言 client 发送 projectId 并解析响应。
    #[tokio::test]
    async fn list_worktrees_posts_project_id_and_parses_items() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/worktrees/list",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(vec![WorkbenchWorktreeDto {
                            id: "inner-main".to_string(),
                            project_id: "inner-project".to_string(),
                            name: "main".to_string(),
                            branch: Some("main".to_string()),
                            base_branch: None,
                            path: "/repo".to_string(),
                            is_main: true,
                            status: Default::default(),
                            created_at: "2026-06-26T00:00:00Z".to_string(),
                            updated_at: "2026-06-26T00:00:00Z".to_string(),
                        }])
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = RemoteWorkbenchClient::new();

        let items = client
            .list_worktrees(&format!("http://{addr}"), "inner-project")
            .await
            .unwrap();

        assert_eq!(items[0].id, "inner-main");
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["projectId"], "inner-project");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机 remote shortcut 创建 terminal window 时，真实会话必须创建在远端设备的 local project 下。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动临时 HTTP 服务接收 sessions/create 请求，断言 client 发送 camelCase body 并解析 session DTO。
    #[tokio::test]
    async fn create_session_posts_camel_case_body_and_parses_session() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/sessions/create",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(WorkbenchSessionDto {
                            id: "inner-session".to_string(),
                            project_id: "inner-project".to_string(),
                            worktree_id: Some("inner-worktree".to_string()),
                            name: "Remote App".to_string(),
                            command: "/bin/zsh".to_string(),
                            cwd: "/repo".to_string(),
                            status: "running".to_string(),
                            cols: 120,
                            rows: 36,
                            started_at: "2026-06-26T00:00:00Z".to_string(),
                            exited_at: None,
                            exit_code: None,
                            supports_panes: true,
                            pane_count: 1,
                        })
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = RemoteWorkbenchClient::new();

        let session = client
            .create_session(
                &format!("http://{addr}"),
                RemoteCreateSessionReq {
                    project_id: "inner-project".to_string(),
                    worktree_id: Some("inner-worktree".to_string()),
                    initial_cols: Some(120),
                    initial_rows: Some(36),
                },
            )
            .await
            .unwrap();

        assert_eq!(session.id, "inner-session");
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["projectId"], "inner-project");
        assert_eq!(body["worktreeId"], "inner-worktree");
        assert_eq!(body["initialCols"], 120);
        assert_eq!(body["initialRows"], 36);
    }
}
