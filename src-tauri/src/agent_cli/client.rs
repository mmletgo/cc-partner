//! agent_cli/client.rs — 本机 control-file Agent 客户端。
//!
//! Business Logic（为什么需要这个模块）:
//!     CLI 不得直连 SQLite 或绕过 loopback control；query 可刷新 descriptor 重试一次，
//!     NeverReplay mutation 只发送一次且连接丢失→outcomeUnknown。
//!
//! Code Logic（这个模块做什么）:
//!     读 backend-control.json，POST `/api/backend/control/agent/{query,mutate}`；
//!     用 `AgentTransport` 枚举支持生产 HTTP 与测试 Fake。

use crate::agent_cli::output::CliError;
use crate::agent_cli::protocol::{
    AgentControlMutation, AgentControlQuery, AgentControlRequest, MutationReplayPolicy,
};
use crate::agent_cli::selectors::ProjectSelector;
use crate::backend::control::{self, BackendControlFile};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
const MUTATE_TIMEOUT: Duration = Duration::from_secs(120);

/// transport 实现（HTTP 或 Fake）。
///
/// Business Logic（为什么需要这个枚举）:
///     hit-count 与 drop-after-apply 必须可单测，且避免引入 async_trait。
///
/// Code Logic（这个枚举做什么）:
///     Http 走 loopback；Fake 记录 hit 并按 mode 返回。
#[derive(Clone)]
pub enum AgentTransport {
    Http {
        port: u16,
        http: reqwest::Client,
    },
    Fake(FakeTransport),
}

impl AgentTransport {
    /// Business Logic（为什么需要这个函数）:
    ///     统一 query/mutate 发送语义。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按变体 POST 或记录 Fake hit。
    pub async fn post_json(&self, path: &str, body: Value) -> Result<Value, CliError> {
        match self {
            Self::Http { port, http } => http_post_json(http, *port, path, body).await,
            Self::Fake(fake) => fake.post_json(path, body).await,
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     生产路径 POST control agent 路由。
///
/// Code Logic（这个函数做什么）:
///     reqwest POST `http://127.0.0.1:{port}/api/backend/control/{path}`。
async fn http_post_json(
    http: &reqwest::Client,
    port: u16,
    path: &str,
    body: Value,
) -> Result<Value, CliError> {
    let url = format!("http://127.0.0.1:{}/api/backend/control/{path}", port);
    let timeout = if path.contains("mutate") {
        MUTATE_TIMEOUT
    } else {
        QUERY_TIMEOUT
    };
    let response = http
        .post(&url)
        .timeout(timeout)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                CliError::unavailable("backend_offline", "control connection failed")
            } else if e.is_timeout() {
                CliError::outcome_unknown("control request timed out after dispatch")
            } else {
                CliError::outcome_unknown("control transport failed after dispatch")
            }
        })?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| CliError::outcome_unknown("control response body unreadable"))?;
    if status.is_success() {
        serde_json::from_slice(&bytes)
            .map_err(|_| CliError::outcome_unknown("control success response unparseable"))
    } else {
        Err(map_control_error_bytes(&bytes, status.as_u16()))
    }
}

/// 从 control 错误 body 映射 CliError。
///
/// Business Logic（为什么需要这个函数）:
///     HTTP 4xx/5xx 需映射到 exit 体系；优先结构化 code，不依赖本地化 message。
///
/// Code Logic（这个函数做什么）:
///     解析嵌套 error 或扁平 code；否则按 status 分类。
pub fn map_control_error_bytes(bytes: &[u8], status: u16) -> CliError {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Nested {
        error: Option<NestedError>,
        code: Option<String>,
        message: Option<String>,
        #[serde(default)]
        outcome_unknown: bool,
        request_id: Option<String>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct NestedError {
        code: Option<String>,
        message: Option<String>,
        #[serde(default)]
        retryable: bool,
        #[serde(default)]
        outcome_unknown: bool,
        request_id: Option<String>,
    }

    if let Ok(v) = serde_json::from_slice::<Nested>(bytes) {
        if let Some(err) = v.error {
            let code = err.code.unwrap_or_else(|| "internal".into());
            let message = err
                .message
                .unwrap_or_else(|| "control request failed".into());
            return map_code_to_cli_error(
                &code,
                &message,
                err.retryable,
                err.outcome_unknown,
                err.request_id,
            );
        }
        if let Some(code) = v.code {
            let message = v.message.unwrap_or_else(|| "control request failed".into());
            return map_code_to_cli_error(
                &code,
                &message,
                false,
                v.outcome_unknown,
                v.request_id,
            );
        }
    }
    match status {
        400 => CliError::usage("validation", "control validation failed"),
        401 | 403 => CliError::unavailable("unauthorized", "control unauthorized"),
        404 => CliError::not_found("resource not found"),
        409 => CliError::conflict("conflict"),
        503 | 504 => CliError::unavailable("unavailable", "control unavailable"),
        _ => CliError::internal("control request failed"),
    }
}

/// Business Logic（为什么需要这个函数）:
///     稳定 code → exit 映射。
///
/// Code Logic（这个函数做什么）:
///     按 code 关键字分类并附 request_id/retryable。
pub fn map_code_to_cli_error(
    code: &str,
    message: &str,
    retryable: bool,
    outcome_unknown: bool,
    request_id: Option<String>,
) -> CliError {
    if outcome_unknown || code == "outcome_unknown" {
        return CliError::outcome_unknown(message).with_request_id(request_id);
    }
    let err = if code.contains("not_found") || code == "not_found" {
        CliError::not_found(message)
    } else if code.contains("conflict") || code.contains("ambiguous") {
        CliError::conflict(message).with_code(code)
    } else if code.contains("unsupported") || code.contains("capability") {
        CliError::unsupported(message)
    } else if code.contains("unavailable")
        || code.contains("timeout")
        || code.contains("offline")
    {
        CliError::unavailable(code, message)
    } else if code.contains("validation")
        || code.contains("invalid")
        || code.contains("usage")
    {
        CliError::usage(code, message)
    } else if code.contains("partial") {
        CliError::partial(message)
    } else {
        CliError::internal(message).with_code(code)
    };
    err.with_retryable(retryable).with_request_id(request_id)
}

/// Agent CLI 客户端。
///
/// Business Logic（为什么需要这个结构）:
///     封装 token + transport + 可选 control 刷新。
///
/// Code Logic（这个结构做什么）:
///     query 允许 offline 后 refresh 一次；mutate 不重放 NeverReplay。
pub struct AgentCliClient {
    transport: AgentTransport,
    control_token: String,
    allow_refresh: bool,
    port: u16,
}

impl AgentCliClient {
    /// Business Logic（为什么需要这个函数）:
    ///     生产路径从本机 control file 装配客户端。
    ///
    /// Code Logic（这个函数做什么）:
    ///     read_control_file → Http transport。
    pub fn from_control_file() -> Result<Self, CliError> {
        let file = read_control_file_cli()?;
        Ok(Self::from_file(file))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用已读取的 control 描述符构造客户端。
    ///
    /// Code Logic（这个函数做什么）:
    ///     启用 refresh。
    pub fn from_file(file: BackendControlFile) -> Self {
        Self {
            transport: AgentTransport::Http {
                port: file.port,
                http: reqwest::Client::new(),
            },
            control_token: file.control_token,
            allow_refresh: true,
            port: file.port,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     单测注入 FakeTransport。
    ///
    /// Code Logic（这个函数做什么）:
    ///     allow_refresh=false。
    pub fn with_transport(transport: AgentTransport, control_token: impl Into<String>) -> Self {
        let port = match &transport {
            AgentTransport::Http { port, .. } => *port,
            AgentTransport::Fake(_) => 0,
        };
        Self {
            transport,
            control_token: control_token.into(),
            allow_refresh: false,
            port,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     query 可在连接失败后刷新 descriptor 重试一次。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST agent/query；backend_offline 且 allow_refresh → 重读 control 再试。
    pub async fn query(&self, op: AgentControlQuery) -> Result<Value, CliError> {
        let body = serde_json::to_value(AgentControlRequest {
            control_token: self.control_token.clone(),
            op: op.clone(),
        })
        .map_err(|_| CliError::internal("query serialize failed"))?;

        match self.transport.post_json("agent/query", body).await {
            Ok(v) => extract_data(v),
            Err(e)
                if self.allow_refresh
                    && (e.code() == "backend_offline"
                        || (e.exit == crate::agent_cli::output::CliExitCode::Unavailable
                            && !e.outcome_unknown_flag())) =>
            {
                let file = read_control_file_cli()?;
                let retry = Self::from_file(file);
                let body2 = serde_json::to_value(AgentControlRequest {
                    control_token: retry.control_token.clone(),
                    op,
                })
                .map_err(|_| CliError::internal("query serialize failed"))?;
                match retry.transport.post_json("agent/query", body2).await {
                    Ok(v) => extract_data(v),
                    Err(e2) if e2.code() == "backend_offline" => Err(CliError::unavailable(
                        "backend_offline",
                        "backend unavailable after control refresh",
                    )),
                    Err(e2) => Err(e2),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     mutation 遵循 replay policy：NeverReplay 只一击。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST agent/mutate 一次；不自动重试。
    pub async fn mutate(&self, op: AgentControlMutation) -> Result<Value, CliError> {
        let policy = op.replay_policy();
        let body = serde_json::to_value(AgentControlRequest {
            control_token: self.control_token.clone(),
            op,
        })
        .map_err(|_| CliError::internal("mutate serialize failed"))?;

        match self.transport.post_json("agent/mutate", body).await {
            Ok(v) => extract_data(v),
            Err(e) => {
                if policy == MutationReplayPolicy::NeverReplay
                    && !e.outcome_unknown_flag()
                    && e.code() != "backend_offline"
                    && e.exit == crate::agent_cli::output::CliExitCode::Unavailable
                {
                    Err(CliError::outcome_unknown(e.message.clone()))
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     诊断需要当前 port。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 port。
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// 读本机 control file 并映射 CliError。
///
/// Business Logic（为什么需要这个函数）:
///     CLI 启动时必须有运行中的 backend owner。
///
/// Code Logic（这个函数做什么）:
///     control::read_control_file → unavailable。
pub fn read_control_file_cli() -> Result<BackendControlFile, CliError> {
    match control::read_control_file() {
        Ok(Some(file)) => Ok(file),
        Ok(None) => Err(CliError::unavailable(
            "backend_offline",
            "backend control file missing; start cc-partner-backend first",
        )),
        Err(_) => Err(CliError::unavailable(
            "backend_offline",
            "backend control file unreadable",
        )),
    }
}

/// 从响应提取 data 字段。
fn extract_data(v: Value) -> Result<Value, CliError> {
    if let Some(data) = v.get("data") {
        return Ok(data.clone());
    }
    Ok(v)
}

/// 测试用 Fake transport 状态。
#[derive(Clone)]
pub struct FakeTransport {
    hits: Arc<AtomicUsize>,
    mode: Arc<Mutex<FakeMode>>,
    last_headers: Arc<Mutex<Vec<(String, String)>>>,
}

impl Default for FakeTransport {
    /// Business Logic（为什么需要这个函数）:
    ///     测试夹具默认 drop-after-apply 语义便于 uncertainty 用例。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 drop_after_apply()。
    fn default() -> Self {
        Self::drop_after_apply()
    }
}

#[derive(Clone)]
enum FakeMode {
    Ok(Value),
    DropAfterApply,
    Fail(CliError),
}

impl FakeTransport {
    /// Business Logic（为什么需要这个函数）:
    ///     连接丢失后 hit count 必须为 1。
    ///
    /// Code Logic（这个函数做什么）:
    ///     mode=DropAfterApply。
    pub fn drop_after_apply() -> Self {
        Self {
            hits: Arc::new(AtomicUsize::new(0)),
            mode: Arc::new(Mutex::new(FakeMode::DropAfterApply)),
            last_headers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     返回固定成功。
    ///
    /// Code Logic（这个函数做什么）:
    ///     mode=Ok。
    pub fn ok(data: Value) -> Self {
        Self {
            hits: Arc::new(AtomicUsize::new(0)),
            mode: Arc::new(Mutex::new(FakeMode::Ok(data))),
            last_headers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     断言只发送一次。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 AtomicUsize。
    pub fn hit_count(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     remote 测试断言未发送 control token header。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆 last_headers。
    pub fn last_headers(&self) -> Vec<(String, String)> {
        self.last_headers
            .lock()
            .expect("headers lock")
            .clone()
    }

    async fn post_json(&self, _path: &str, _body: Value) -> Result<Value, CliError> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        let mode = self.mode.lock().expect("fake mode lock").clone();
        match mode {
            FakeMode::Ok(v) => Ok(v),
            FakeMode::DropAfterApply => Err(CliError::outcome_unknown(
                "connection lost after mutation dispatch",
            )),
            FakeMode::Fail(e) => Err(e),
        }
    }
}

/// 便捷：构造带 Fake 的客户端。
///
/// Business Logic（为什么需要这个函数）:
///     单测缩短样板。
///
/// Code Logic（这个函数做什么）:
///     with_transport(Fake(...), test-token)。
pub fn client(transport: FakeTransport) -> AgentCliClient {
    AgentCliClient::with_transport(AgentTransport::Fake(transport), "test-token")
}

/// 测试用 terminal send mutation。
///
/// Business Logic（为什么需要这个函数）:
///     hit-count 测试构造器。
///
/// Code Logic（这个函数做什么）:
///     SessionSend 变体。
pub fn terminal_send(session_id: &str, data: &[u8]) -> AgentControlMutation {
    AgentControlMutation::SessionSend {
        session_id: session_id.into(),
        data: String::from_utf8_lossy(data).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn terminal_send_connection_loss_is_never_replayed() {
        let transport = FakeTransport::drop_after_apply();
        let error = client(transport.clone())
            .mutate(terminal_send("s1", b"pwd\n"))
            .await
            .unwrap_err();
        assert_eq!(transport.hit_count(), 1);
        assert!(error.outcome_unknown_flag());
    }

    #[tokio::test]
    async fn query_success_returns_data() {
        let transport = FakeTransport::ok(serde_json::json!({
            "ownerInstanceId": "o1",
            "data": {"items": []}
        }));
        let data = client(transport)
            .query(AgentControlQuery::ProjectList)
            .await
            .unwrap();
        assert_eq!(data["items"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn never_replay_worktree_create_single_hit() {
        let transport = FakeTransport::drop_after_apply();
        let op = AgentControlMutation::WorktreeCreate {
            project: ProjectSelector::Id("p1".into()),
            payload: serde_json::json!({"branchName": "f"}),
        };
        let err = client(transport.clone()).mutate(op).await.unwrap_err();
        assert_eq!(transport.hit_count(), 1);
        assert!(err.outcome_unknown_flag());
    }

    #[tokio::test]
    async fn browser_verify_never_replayed() {
        let transport = FakeTransport::drop_after_apply();
        let op = AgentControlMutation::BrowserVerify {
            project: ProjectSelector::Id("p1".into()),
            payload: serde_json::json!({"previewId": "pv1"}),
        };
        let err = client(transport.clone()).mutate(op).await.unwrap_err();
        assert_eq!(transport.hit_count(), 1);
        assert!(err.outcome_unknown_flag());
    }
}
