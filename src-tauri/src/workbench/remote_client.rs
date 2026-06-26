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
    WorkbenchProjectDto, WorkbenchRemoteDirectoryEntryDto, WorkbenchRemotePathInfoDto,
    WorkbenchRemoteRootDto,
};
use serde::de::DeserializeOwned;
use std::time::Duration;

const REMOTE_WORKBENCH_TIMEOUT_SECS: u64 = 15;

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
    ///     构造带默认超时的 reqwest client；client 内部连接池可 clone 复用。
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REMOTE_WORKBENCH_TIMEOUT_SECS))
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
        self.get_json(endpoint_url(base_url, "/api/workbench/fs/roots"))
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
        self.post_path_json(endpoint_url(base_url, "/api/workbench/fs/list"), path)
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
        self.post_path_json(endpoint_url(base_url, "/api/workbench/fs/info"), path)
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
        self.post_path_json(endpoint_url(base_url, "/api/workbench/projects/open"), path)
            .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端 Workbench GET 调用都需要统一处理网络错误、HTTP 状态码和 JSON 解析错误。
    ///
    /// Code Logic（这个函数做什么）:
    ///     发送 GET 请求，非成功状态转中文业务错误，成功后解析 JSON 为目标类型。
    async fn get_json<T>(&self, url: String) -> Result<T, AppError>
    where
        T: DeserializeOwned,
    {
        let response = self
            .client
            .get(&url)
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
    async fn post_path_json<T>(&self, url: String, path: &str) -> Result<T, AppError>
    where
        T: DeserializeOwned,
    {
        let body = serde_json::json!({ "path": path });
        let response = self
            .client
            .post(&url)
            .json(&body)
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
        return Err(AppError::generic(format!(
            "远端 Workbench 请求失败: HTTP {status}"
        )));
    }
    response
        .json::<T>()
        .await
        .map_err(|error| AppError::generic(format!("远端 Workbench 响应解析失败: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::models::WorkbenchRemoteDirectoryEntryDto;
    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
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
}
