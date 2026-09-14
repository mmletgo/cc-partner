//! provider_manager/remote.rs — Provider Manager 远端 HTTP 客户端
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端 Provider Manager 页允许把局域网内其他 cc-partner 设备作为目标，查询/切换
//!     对端 cc-switch 已配置的 provider，并在对端安装 cc-switch CLI。对端已有现成 P2P
//!     路由（`GET /api/provider-manager/summary`、`POST /api/provider-manager/switch`，
//!     能力 token `provider-manager.v1`）与本轮新增的 `POST
//!     /api/provider-manager/install-cli`（能力 token `provider-manager.install.v1`），
//!     本模块只做客户端封装。
//!
//! Code Logic（这个模块做什么）:
//!     `RemoteProviderManagerClient` 仿 `workbench::remote_client::RemoteWorkbenchClient`
//!     的最小子集：自持 `reqwest::Client`，每个出站请求带 `X-CC-Request-Id`（多跳调用链
//!     关联）；可选绑定 `X-Cc-Partner-Expected-Device-Id`，绑定时先做 health 预检（要求
//!     对端宣告 `device.request-binding.v1` 且 device_id 精确匹配，防 stale peer 映射
//!     fail-open 打错设备）。调用前经 `PeerClient::require_capability` 预检对应能力 token，
//!     对端版本过旧时返回可区分的中文 validation 错误。
//!     网络错误/非 2xx/对端错误信封统一经 `net::peer_error` 解析并透出对端 message。
//!     switch 与 install 在对端是 cc-switch CLI 写盘/安装（非幂等），按协议约定单次发送、
//!     不做传输重试；install 超时 420s（brew 安装可能数分钟）。

use crate::error::AppError;
use crate::net::peer_client::PeerClient;
use crate::net::peer_error::{parse_peer_response, peer_call_error_to_app_error, PeerCallError};
use crate::net::protocol::{
    CAPABILITY_DEVICE_REQUEST_BINDING_V1, CAPABILITY_PROVIDER_MANAGER_INSTALL_V1,
    CAPABILITY_PROVIDER_MANAGER_V1,
};
use crate::provider_manager::models::{
    AgentApp, AppProviders, InstallResult, ProviderManagerSummary,
};
use serde::{de::DeserializeOwned, Serialize};
use std::time::Duration;

/// summary（只读快照）请求超时。
const SUMMARY_TIMEOUT_SECS: u64 = 15;

/// switch（对端 CLI 写盘）请求超时。
const SWITCH_TIMEOUT_SECS: u64 = 120;

/// install（对端 CLI 安装；macOS brew 可能数分钟）请求超时，
/// 对齐 `workbench::remote_client` 的 VeryLong 语义。
const INSTALL_TIMEOUT_SECS: u64 = 420;

/// 错误文案里的客户端标签，便于日志定位调用方。
const CLIENT_LABEL: &str = "远端 Provider 管理";

/// `POST /api/provider-manager/switch` 请求体（camelCase，对齐对端路由 `ProviderSwitchReq`）。
///
/// Business Logic（为什么需要这个结构体）:
///     对端路由按 `{app, providerId}` camelCase 解析，`app` 复用 `AgentApp` 的 lowercase
///     serde；本机请求体必须与其逐字段一致。
///
/// Code Logic（这个结构体做什么）:
///     序列化为 `{"app": "claude", "providerId": "p-1"}` 形态的 JSON body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteProviderSwitchReq<'a> {
    /// 目标 agent（lowercase，对齐 cc-switch CLI `--app`）。
    app: AgentApp,
    /// 要切换到的 provider id。
    provider_id: &'a str,
}

/// Provider Manager 远端 HTTP 客户端。
///
/// Business Logic（为什么需要这个结构体）:
///     远端 provider 查询/切换需要统一的超时、request_id 注入、能力预检与错误映射，
///     避免调用方各自手写 HTTP 细节。
///
/// Code Logic（这个结构体做什么）:
///     持有 cloneable 的 `reqwest::Client` 与可选的期望 device_id 绑定；对外提供
///     `summary`（GET，15s 超时）、`switch`（POST，120s 超时、单次不重试）与
///     `install_cli`（POST，420s 超时、单次不重试）。
#[derive(Clone)]
pub struct RemoteProviderManagerClient {
    client: reqwest::Client,
    /// 可选：出站绑定期望 device_id（与 health 预检配合，对端 header guard 校验）。
    expected_device_id: Option<String>,
}

impl RemoteProviderManagerClient {
    /// 创建 Provider Manager 远端客户端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     命令层每次处理远端请求时需要一个可直接使用的客户端实例。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造不带全局超时的 reqwest client；每个请求按 summary/switch 单独设置 timeout。
    ///     `expected_device_id` 默认 None（不注入期望设备 header）。
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .build()
            .expect("构造 Provider Manager 远端 reqwest Client 失败");
        Self {
            client,
            expected_device_id: None,
        }
    }

    /// 绑定期望远端 device_id，使每个 GET/POST 携带 `X-Cc-Partner-Expected-Device-Id`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     health 预检与真实请求是独立 HTTP 连接；端口复用/设备切换时必须让业务请求自己
    ///     携带期望 device_id，由对端服务端 header guard fail closed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     空串清为 None；非空写入 `expected_device_id`，出站时注入 header 并触发
    ///     `ensure_expected_device_binding` 预检。
    pub fn with_expected_device_id(mut self, device_id: impl Into<String>) -> Self {
        let id = device_id.into();
        self.expected_device_id = if id.trim().is_empty() { None } else { Some(id) };
        self
    }

    /// 查询远端设备 Provider Manager 整体状态快照。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     选中远端设备时，前端需要展示对端 cc-switch DB/CLI/GUI 检测与各 agent provider 列表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     预检 `provider-manager.v1` 能力 → GET `{base}/api/provider-manager/summary`
    ///     （15s 超时），解析为 `ProviderManagerSummary` camelCase DTO。
    pub async fn summary(&self, base_url: &str) -> Result<ProviderManagerSummary, AppError> {
        self.require_provider_manager_capability(base_url).await?;
        self.get_json(
            endpoint_url(base_url, "/api/provider-manager/summary"),
            Duration::from_secs(SUMMARY_TIMEOUT_SECS),
        )
        .await
    }

    /// 切换远端设备某 agent 的当前 provider。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     选中远端设备时，切换必须在对端执行（写对端的 `~/.claude/settings.json` 等），
    ///     不能误写本机。
    ///
    /// Code Logic（这个函数做什么）:
    ///     预检 `provider-manager.v1` 能力 → POST `{base}/api/provider-manager/switch`
    ///     （camelCase body，120s 超时）。switch 在对端是 cc-switch CLI 写盘、非幂等，
    ///     按协议约定单次发送，**不做**传输层重试；成功返回对端重读的 `AppProviders`。
    pub async fn switch(
        &self,
        base_url: &str,
        app: AgentApp,
        provider_id: &str,
    ) -> Result<AppProviders, AppError> {
        self.require_provider_manager_capability(base_url).await?;
        let body = RemoteProviderSwitchReq { app, provider_id };
        self.post_json(
            endpoint_url(base_url, "/api/provider-manager/switch"),
            &body,
            Duration::from_secs(SWITCH_TIMEOUT_SECS),
        )
        .await
    }

    /// 安装远端设备的 cc-switch CLI（显式用户动作）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     选中远端设备且对端未安装 cc-switch CLI 时，用户应能在本页直接对对端执行安装，
    ///     而不是只能收到"请去对端安装"的提示。
    ///
    /// Code Logic（这个函数做什么）:
    ///     预检 `provider-manager.install.v1` 能力（缺失 → validation 中文错误，引导升级对端）
    ///     → POST `{base}/api/provider-manager/install-cli`（无业务请求体，420s 超时）。
    ///     install 在对端是 brew 写盘（非幂等），按协议约定单次发送，**不做**传输层重试；
    ///     返回对端 `InstallResult`（macOS brew 成功/失败，其余平台 manual 人工指引）。
    pub async fn install_cli(&self, base_url: &str) -> Result<InstallResult, AppError> {
        self.require_capability_with_label(
            base_url,
            CAPABILITY_PROVIDER_MANAGER_INSTALL_V1,
            "远程安装 cc-switch CLI",
        )
        .await?;
        self.post_json(
            endpoint_url(base_url, "/api/provider-manager/install-cli"),
            &serde_json::Map::<String, serde_json::Value>::new(),
            Duration::from_secs(INSTALL_TIMEOUT_SECS),
        )
        .await
    }

    /// 能力门：调用远端路由前检查对端是否支持 `provider-manager.v1`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧版对端没有 provider-manager 路由；直接调用会得到 404/HTML 噪音错误。
    ///     必须先拉取对端 health 元数据，缺能力时给用户可区分的中文提示（引导升级），
    ///     而不是模糊的网络失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托通用 `require_capability_with_label`，功能标签为 "Provider 管理"。
    async fn require_provider_manager_capability(&self, base_url: &str) -> Result<(), AppError> {
        self.require_capability_with_label(
            base_url,
            CAPABILITY_PROVIDER_MANAGER_V1,
            "Provider 管理",
        )
        .await
    }

    /// 能力门通用实现（summary/switch 与 install 共享，避免文案与映射逻辑漂移）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     所有 provider-manager 远端调用的能力预检共享同一错误语义：缺 token 时给
    ///     可区分的中文 validation 错误（含功能标签与能力 token，引导升级对端），
    ///     其余探测失败（离线/响应非法）统一映射。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `PeerClient::require_capability(base_url, capability)`；缺失 →
    ///     validation 中文错误（"对端设备版本过旧，不支持{feature}（{url} 缺少能力
    ///     {capability}）；请在对端升级 cc-partner 后重试"）；其他探测失败 →
    ///     经 `peer_call_error_to_app_error` 统一映射。探测成功只需布尔结论，
    ///     丢弃 `HealthResponse` 载荷。capability 为 `&'static str`（能力 token 常量，
    ///     对齐 `PeerClient::require_capability` 签名）。
    async fn require_capability_with_label(
        &self,
        base_url: &str,
        capability: &'static str,
        feature: &str,
    ) -> Result<(), AppError> {
        PeerClient::new()
            .require_capability(base_url, capability)
            .await
            .map(|_health| ())
            .map_err(|err| match err {
                PeerCallError::Unsupported { url, capability } => AppError::validation(format!(
                    "对端设备版本过旧，不支持{feature}（{url} 缺少能力 {capability}）；请在对端升级 cc-partner 后重试"
                )),
                other => peer_call_error_to_app_error(other, CLIENT_LABEL),
            })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     绑定 expected_device_id 时，旧 peer 会忽略设备头并 fail-open；必须先确认对端
    ///     宣告 `device.request-binding.v1` 且 health.device_id 精确匹配，防止 stale peer
    ///     映射把 provider 切到错误设备。
    ///
    /// Code Logic（这个函数做什么）:
    ///     expected_device_id 为 None 时直接 Ok；否则 require_capability(binding) +
    ///     device_id 精确匹配，不匹配 → conflict；缺能力 → validation（经统一映射）。
    async fn ensure_expected_device_binding(&self, base_url: &str) -> Result<(), AppError> {
        let Some(expected) = self.expected_device_id.as_deref() else {
            return Ok(());
        };
        let expected = expected.trim();
        if expected.is_empty() {
            return Ok(());
        }
        let health = PeerClient::new()
            .require_capability(base_url, CAPABILITY_DEVICE_REQUEST_BINDING_V1)
            .await
            .map_err(|err| peer_call_error_to_app_error(err, CLIENT_LABEL))?;
        if health.device_id.trim() != expected {
            return Err(AppError::conflict(format!(
                "{CLIENT_LABEL} device_id 不匹配: expected={expected}, got={}",
                health.device_id
            )));
        }
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端 GET 调用需要统一处理 device 绑定预检、request_id 注入、网络错误与响应解析。
    ///
    /// Code Logic（这个函数做什么）:
    ///     已绑定期望设备时先做 health 绑定预检；随后 GET（附 `X-CC-Request-Id` 新 UUID
    ///     与期望设备 header，按调用方传入的超时），委托 `parse_json_response` 统一解析。
    async fn get_json<T>(&self, url: String, timeout: Duration) -> Result<T, AppError>
    where
        T: DeserializeOwned,
    {
        if self.expected_device_id.is_some() {
            let base = origin_base_url(&url)?;
            self.ensure_expected_device_binding(&base).await?;
        }
        let mut request = self
            .client
            .get(&url)
            .header(
                crate::net::request_context::REQUEST_ID_HEADER,
                crate::net::request_context::new_request_id(),
            )
            .timeout(timeout);
        if let Some(device_id) = self.expected_device_id.as_deref() {
            request = request.header(
                crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER.as_str(),
                device_id,
            );
        }
        let response = request.send().await.map_err(map_send_error)?;
        parse_json_response(response).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端 POST（switch/install）与 GET 共享绑定预检/注入/错误映射，仅方法与超时不同；
    ///     且必须单次发送（对端 CLI 写盘/安装非幂等，协议 no-transport-retry）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     已绑定期望设备时先做 health 绑定预检；随后 POST JSON body（附 `X-CC-Request-Id`
    ///     新 UUID 与期望设备 header，按调用方传入的超时，单次不重试），委托
    ///     `parse_json_response`。
    async fn post_json<T, B>(&self, url: String, body: &B, timeout: Duration) -> Result<T, AppError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        if self.expected_device_id.is_some() {
            let base = origin_base_url(&url)?;
            self.ensure_expected_device_binding(&base).await?;
        }
        let mut request = self
            .client
            .post(&url)
            .json(body)
            .header(
                crate::net::request_context::REQUEST_ID_HEADER,
                crate::net::request_context::new_request_id(),
            )
            .timeout(timeout);
        if let Some(device_id) = self.expected_device_id.as_deref() {
            request = request.header(
                crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER.as_str(),
                device_id,
            );
        }
        let response = request.send().await.map_err(map_send_error)?;
        parse_json_response(response).await
    }
}

impl Default for RemoteProviderManagerClient {
    /// 创建默认 Provider Manager 远端客户端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     clippy 要求 `new` 存在时提供 `Default`，调用方语义一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `RemoteProviderManagerClient::new`。
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
///     绑定预检只需要 origin base；完整 endpoint URL 需先解析回 `scheme://host:port`。
///
/// Code Logic（这个函数做什么）:
///     用 `reqwest::Url` 解析 scheme/host/port，拼回 origin 字符串；非法 URL → generic 错误。
fn origin_base_url(full_url: &str) -> Result<String, AppError> {
    let parsed = reqwest::Url::parse(full_url)
        .map_err(|err| AppError::generic(format!("{CLIENT_LABEL} URL 无效: {err}")))?;
    let scheme = parsed.scheme();
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::generic(format!("{CLIENT_LABEL} URL 缺少 host")))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| AppError::generic(format!("{CLIENT_LABEL} URL 缺少 port")))?;
    Ok(format!("{scheme}://{host}:{port}"))
}

/// Business Logic: 传输层 send 失败需要区分 timeout（对端忙/链路黑洞）与离线（unavailable），
///     供上层按分类决策；文案带 `CLIENT_LABEL` 便于日志定位。
/// Code Logic: reqwest error.is_timeout() → AppError::timeout，其它 send 失败 → unavailable。
fn map_send_error(error: reqwest::Error) -> AppError {
    if error.is_timeout() {
        AppError::timeout(format!("{CLIENT_LABEL} 请求超时: {error}"))
    } else {
        AppError::unavailable(format!("{CLIENT_LABEL} 请求失败: {error}"))
    }
}

/// Business Logic（为什么需要这个函数）:
///     所有远端响应都需要统一错误语义：成功按泛型解析 JSON；非 2xx 解析对端错误信封
///     （v1 `{code,message,request_id}` 或 v0 老形态 `{error}`）并**透出对端 message**，
///     保留 code/status/retryable/request_id 结构化元数据供上层决策。
///
/// Code Logic（这个函数做什么）:
///     委托 `net::peer_error::parse_peer_response` 一次性消费 status/header/body，
///     失败经 `peer_call_error_to_app_error` 映射为带 `CLIENT_LABEL` 的 `AppError`。
async fn parse_json_response<T>(response: reqwest::Response) -> Result<T, AppError>
where
    T: DeserializeOwned,
{
    let url = response.url().as_str().to_string();
    parse_peer_response::<T>(response, &url)
        .await
        .map_err(|err| peer_call_error_to_app_error(err, CLIENT_LABEL))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppErrorCategory;
    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::{http::StatusCode, Json, Router};
    use serde_json::Value;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    /// 宣告完整能力（含 provider-manager.v1 与 device.request-binding.v1）的 health JSON。
    fn health_json(device_id: &str, capabilities: &[&str]) -> Value {
        serde_json::json!({
            "ok": true,
            "device_id": device_id,
            "device_name": "Test Device",
            "http_port": 62116,
            "ts": 1_800_000_000i64,
            "protocol_version": 1,
            "capabilities": capabilities,
        })
    }

    /// 启动临时 axum server，返回本地 base URL。
    async fn spawn_server(app: Router) -> String {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// Business Logic（为什么需要这个测试）:
    ///     对端路由按 camelCase `providerId` 与 lowercase `app` 解析请求体；客户端请求体
    ///     serde 必须与其逐字段一致，否则对端 400。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 `RemoteProviderSwitchReq`，断言键名为 `providerId`、app 值为 lowercase。
    #[test]
    fn switch_req_body_serializes_camel_case_and_lowercase_app() {
        let body = RemoteProviderSwitchReq {
            app: AgentApp::Claude,
            provider_id: "p-1",
        };
        let json = serde_json::to_value(&body).expect("序列化应成功");
        assert_eq!(json["providerId"], "p-1");
        assert_eq!(json["app"], "claude");
        assert!(json.get("provider_id").is_none(), "禁止 snake_case 泄露");

        let body = RemoteProviderSwitchReq {
            app: AgentApp::Codex,
            provider_id: "gpt",
        };
        let json = serde_json::to_value(&body).expect("序列化应成功");
        assert_eq!(json["app"], "codex");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     设备发现拿到的 base URL 可能带尾斜杠，客户端不能因此产生双斜杠路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入带尾斜杠的 base URL，断言拼出的 API URL 只保留一个路径分隔。
    #[test]
    fn endpoint_url_trims_trailing_slash() {
        assert_eq!(
            endpoint_url("http://127.0.0.1:62116/", "/api/provider-manager/summary"),
            "http://127.0.0.1:62116/api/provider-manager/summary"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     选中远端设备后查询 summary，必须打约定的 GET 路由并解析 camelCase DTO。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时服务宣告 provider-manager.v1 并返回 summary JSON，断言客户端解析出
    ///     DB 存在标记与 provider 列表，且出站带 36 字符 `X-CC-Request-Id`。
    #[tokio::test]
    async fn summary_gets_and_parses_summary_dto_with_request_id() {
        let observed_request_id = Arc::new(Mutex::new(String::new()));
        let observed = observed_request_id.clone();
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async {
                    Json(health_json(
                        "dev-A",
                        &["provider-manager.v1", "errors.envelope.v1"],
                    ))
                }),
            )
            .route(
                "/api/provider-manager/summary",
                get(move |headers: axum::http::HeaderMap| {
                    let observed = observed.clone();
                    async move {
                        let id = headers
                            .get("x-cc-request-id")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        *observed.lock().unwrap() = id;
                        Json(serde_json::json!({
                            "ccSwitchDbPresent": true,
                            "cli": {
                                "available": true,
                                "path": "/opt/homebrew/bin/cc-switch",
                                "version": "1.2.3"
                            },
                            "gui": null,
                            "apps": [
                                {
                                    "app": "claude",
                                    "providers": [
                                        {
                                            "id": "p-1",
                                            "name": "DeepSeek",
                                            "category": "third-party",
                                            "isCurrent": true
                                        }
                                    ],
                                    "currentProviderId": "p-1"
                                }
                            ]
                        }))
                    }
                }),
            );
        let base = spawn_server(app).await;

        let summary = RemoteProviderManagerClient::new()
            .summary(&base)
            .await
            .expect("summary 应成功");
        assert!(summary.cc_switch_db_present);
        assert!(summary.cli.available);
        assert_eq!(summary.apps.len(), 1);
        assert_eq!(summary.apps[0].app, AgentApp::Claude);
        assert_eq!(summary.apps[0].current_provider_id.as_deref(), Some("p-1"));
        assert_eq!(
            observed_request_id.lock().unwrap().len(),
            36,
            "出站必须带 36 字符 UUID request id"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端切换必须把目标 agent 与 provider id 以 camelCase 发给对端，并解析对端重读的
    ///     `AppProviders` 返回值。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时服务记录 switch 请求体并返回 AppProviders JSON，断言
    ///     `{app: "claude", providerId: "p-2"}` 与响应解析正确。
    #[tokio::test]
    async fn switch_posts_camel_case_body_and_parses_app_providers() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async {
                    Json(health_json("dev-A", &["provider-manager.v1"]))
                }),
            )
            .route(
                "/api/provider-manager/switch",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(serde_json::json!({
                            "app": "claude",
                            "providers": [
                                { "id": "p-2", "name": "Kimi", "category": null, "isCurrent": true },
                                { "id": "p-1", "name": "DeepSeek", "category": null, "isCurrent": false }
                            ],
                            "currentProviderId": "p-2"
                        }))
                    },
                ),
            )
            .with_state(seen_body.clone());
        let base = spawn_server(app).await;

        let updated = RemoteProviderManagerClient::new()
            .switch(&base, AgentApp::Claude, "p-2")
            .await
            .expect("switch 应成功");

        assert_eq!(updated.app, AgentApp::Claude);
        assert_eq!(updated.current_provider_id.as_deref(), Some("p-2"));
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["app"], "claude");
        assert_eq!(body["providerId"], "p-2");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧版对端缺 `provider-manager.v1` 时必须给出可区分的中文错误（引导升级），
    ///     且分类为 validation，不能与"设备离线"或业务失败混淆。
    ///
    /// Code Logic（这个测试做什么）:
    ///     health 不宣告 provider-manager.v1，调用 summary 应失败：文案含「版本过旧」
    ///     与能力 token，classify() == Validation。
    #[tokio::test]
    async fn missing_capability_maps_to_distinct_chinese_validation_error() {
        let app = Router::new().route(
            "/api/health",
            get(|| async { Json(health_json("dev-A", &["errors.envelope.v1"])) }),
        );
        let base = spawn_server(app).await;

        let error = RemoteProviderManagerClient::new()
            .summary(&base)
            .await
            .expect_err("缺能力应失败");

        let message = error.to_string();
        assert!(message.contains("版本过旧"), "文案应可区分: {message}");
        assert!(message.contains("Provider 管理"), "文案应可区分: {message}");
        assert!(message.contains("provider-manager.v1"));
        assert_eq!(error.classify(), AppErrorCategory::Validation);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     对端 switch 失败（如未装 CLI）返回错误信封时，客户端必须透出对端 message
    ///     供前端展示，并保留结构化 code/status 元数据。
    ///
    /// Code Logic（这个测试做什么）:
    ///     switch 路由返回 503 + v1 信封，断言错误文案 == 对端 message、
    ///     remote_meta.code == "unavailable"、status == 503。
    #[tokio::test]
    async fn error_envelope_message_is_surfaced_to_caller() {
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async { Json(health_json("dev-A", &["provider-manager.v1"])) }),
            )
            .route(
                "/api/provider-manager/switch",
                post(|| async {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "error": "未找到 cc-switch CLI，请在对端「设置 → 依赖环境」安装后再切换",
                            "code": "unavailable",
                            "request_id": "req-pm-remote-1",
                            "retryable": false
                        })),
                    )
                }),
            );
        let base = spawn_server(app).await;

        let error = RemoteProviderManagerClient::new()
            .switch(&base, AgentApp::Codex, "p-1")
            .await
            .expect_err("对端业务错误应上抛");

        assert_eq!(
            error.to_string(),
            "未找到 cc-switch CLI，请在对端「设置 → 依赖环境」安装后再切换",
            "必须透出对端信封 message"
        );
        let meta = error.remote_meta().expect("应携带结构化 meta");
        assert_eq!(meta.code, "unavailable");
        assert_eq!(meta.status, 503);
        assert!(!meta.retryable);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     绑定 expected_device_id 后，客户端必须先经 health 确认对端支持请求绑定且
    ///     device_id 精确匹配，再让业务请求携带 `X-Cc-Partner-Expected-Device-Id`；
    ///     不匹配时 fail closed 返回 conflict，防止 stale peer 映射切错设备。
    ///
    /// Code Logic（这个测试做什么）:
    ///     health 宣告 `device.request-binding.v1` + device_id=dev-A；绑定 dev-A 调用
    ///     summary 应成功且对端观测到正确 header；绑定 dev-B 应返回 conflict 文案。
    #[tokio::test]
    async fn with_expected_device_id_prechecks_health_and_binds_header() {
        let observed = Arc::new(Mutex::new(String::new()));
        let observed_for_handler = observed.clone();
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async {
                    Json(health_json(
                        "dev-A",
                        &["device.request-binding.v1", "provider-manager.v1"],
                    ))
                }),
            )
            .route(
                "/api/provider-manager/summary",
                get(move |headers: axum::http::HeaderMap| {
                    let observed = observed_for_handler.clone();
                    async move {
                        *observed.lock().unwrap() = headers
                            .get("x-cc-partner-expected-device-id")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        Json(serde_json::json!({
                            "ccSwitchDbPresent": false,
                            "cli": { "available": false, "path": null, "version": null },
                            "gui": null,
                            "apps": []
                        }))
                    }
                }),
            );
        let base = spawn_server(app).await;

        RemoteProviderManagerClient::new()
            .with_expected_device_id("dev-A")
            .summary(&base)
            .await
            .expect("绑定匹配应成功");
        assert_eq!(observed.lock().unwrap().as_str(), "dev-A");

        let error = RemoteProviderManagerClient::new()
            .with_expected_device_id("dev-B")
            .summary(&base)
            .await
            .expect_err("绑定不匹配应失败");
        let message = error.to_string();
        assert!(
            message.contains("device_id 不匹配"),
            "应 conflict: {message}"
        );
        assert!(message.contains("dev-B") && message.contains("dev-A"));
        assert_eq!(error.classify(), AppErrorCategory::Conflict);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     switch 是对端 cc-switch CLI 写盘、非幂等；客户端任何路径都不得对它做自动
    ///     传输重试（协议 no-transport-retry）。该约束由实现保证（无重试包裹），
    ///     用对端观测调用次数锁死：单次调用即使失败也只应产生 1 次出站请求。
    ///
    /// Code Logic（这个测试做什么）:
    ///     switch 路由返回 503 并计数；调用一次失败后断言对端恰好观测到 1 次请求。
    #[tokio::test]
    async fn switch_does_not_transport_retry_on_failure() {
        let attempts = Arc::new(AtomicU32::new(0));
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async { Json(health_json("dev-A", &["provider-manager.v1"])) }),
            )
            .route(
                "/api/provider-manager/switch",
                post(move |attempts: State<Arc<AtomicU32>>| async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "error": "boom",
                            "code": "unavailable",
                            "retryable": true
                        })),
                    )
                }),
            )
            .with_state(attempts.clone());
        let base = spawn_server(app).await;

        let result = RemoteProviderManagerClient::new()
            .switch(&base, AgentApp::Gemini, "p-1")
            .await;
        assert!(result.is_err());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "switch 禁止传输层自动重试"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧版对端缺 `provider-manager.install.v1` 时必须给出可区分的中文错误（引导升级），
    ///     且文案要指向"远程安装 cc-switch CLI"这一具体功能，与 summary/switch 的能力缺失
    ///     提示可区分，分类必须是 validation。
    ///
    /// Code Logic（这个测试做什么）:
    ///     health 只宣告 errors.envelope.v1，调用 install_cli 应失败：文案含「版本过旧」
    ///     「远程安装 cc-switch CLI」与能力 token，classify() == Validation。
    #[tokio::test]
    async fn install_missing_capability_maps_to_distinct_chinese_validation_error() {
        let app = Router::new().route(
            "/api/health",
            get(|| async { Json(health_json("dev-A", &["errors.envelope.v1"])) }),
        );
        let base = spawn_server(app).await;

        let error = RemoteProviderManagerClient::new()
            .install_cli(&base)
            .await
            .expect_err("缺能力应失败");

        let message = error.to_string();
        assert!(message.contains("版本过旧"), "文案应可区分: {message}");
        assert!(
            message.contains("远程安装 cc-switch CLI"),
            "文案应可区分: {message}"
        );
        assert!(message.contains("provider-manager.install.v1"));
        assert_eq!(error.classify(), AppErrorCategory::Validation);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端安装必须打约定的 POST 路由并解析对端 `InstallResult`（camelCase DTO），
    ///     出站带 request id；manual 指引形态（ok=false + message/url）也要原样透传。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时服务宣告 provider-manager.install.v1 并返回 manual 指引 JSON，断言客户端
    ///     解析出 method/ok/message/url，且出站带 36 字符 `X-CC-Request-Id`。
    #[tokio::test]
    async fn install_posts_route_and_parses_install_result() {
        let observed_request_id = Arc::new(Mutex::new(String::new()));
        let observed = observed_request_id.clone();
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async {
                    Json(health_json(
                        "dev-A",
                        &["provider-manager.install.v1", "errors.envelope.v1"],
                    ))
                }),
            )
            .route(
                "/api/provider-manager/install-cli",
                post(move |headers: axum::http::HeaderMap| {
                    let observed = observed.clone();
                    async move {
                        let id = headers
                            .get("x-cc-request-id")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        *observed.lock().unwrap() = id;
                        Json(serde_json::json!({
                            "method": "manual",
                            "ok": false,
                            "version": null,
                            "path": null,
                            "message": "未检测到 Homebrew。请安装 cc-switch-cli。",
                            "url": "https://github.com/SaladDay/cc-switch-cli#-installation"
                        }))
                    }
                }),
            );
        let base = spawn_server(app).await;

        let result = RemoteProviderManagerClient::new()
            .install_cli(&base)
            .await
            .expect("install 应成功解析对端 InstallResult");

        assert_eq!(result.method, "manual");
        assert!(!result.ok);
        assert_eq!(
            result.message.as_deref(),
            Some("未检测到 Homebrew。请安装 cc-switch-cli。")
        );
        assert_eq!(
            result.url.as_deref(),
            Some("https://github.com/SaladDay/cc-switch-cli#-installation")
        );
        assert_eq!(
            observed_request_id.lock().unwrap().len(),
            36,
            "出站必须带 36 字符 UUID request id"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     install 在对端是 brew 安装（非幂等）；客户端任何路径都不得对它做自动传输重试
    ///     （协议 no-transport-retry）。用对端观测调用次数锁死：单次调用即使失败也只应
    ///     产生 1 次出站请求。
    ///
    /// Code Logic（这个测试做什么）:
    ///     install-cli 路由返回 503 并计数；调用一次失败后断言对端恰好观测到 1 次请求。
    #[tokio::test]
    async fn install_does_not_transport_retry_on_failure() {
        let attempts = Arc::new(AtomicU32::new(0));
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async { Json(health_json("dev-A", &["provider-manager.install.v1"])) }),
            )
            .route(
                "/api/provider-manager/install-cli",
                post(move |attempts: State<Arc<AtomicU32>>| async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "error": "boom",
                            "code": "unavailable",
                            "retryable": true
                        })),
                    )
                }),
            )
            .with_state(attempts.clone());
        let base = spawn_server(app).await;

        let result = RemoteProviderManagerClient::new().install_cli(&base).await;
        assert!(result.is_err());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "install 禁止传输层自动重试"
        );
    }
}
