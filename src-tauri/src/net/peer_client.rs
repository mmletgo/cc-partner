//! net/peer_client.rs — 对端 HTTP 客户端（reqwest）
//!
//! Business Logic（为什么需要这个模块）:
//!     P2P 架构中每个实例也是客户端，需主动向其他设备发起请求：健康检查、同步 pull/push、
//!     文件传输 init/chunk/status。对照 Python `network/client.py`（aiohttp 实现）。
//!     M3 仅实现 health 调用 + 基础结构；sync/transfer 留 M4/M5 填实现。
//!
//! Code Logic（这个模块做什么）:
//!     - 持有一个 reqwest::Client（连接池复用，rustls-tls 避免 OpenSSL 依赖）。
//!     - `health(addr, port)`：GET `http://{addr}:{port}/api/health`，10s 超时，
//!       成功且 status==200 返回 true，否则 false（与 Python `health_check` 一致）。
//!     - sync/transfer 方法预留签名（TODO M4/M5）。

use std::time::Duration;

/// health 请求超时（秒）。对照 Python `DEFAULT_TIMEOUT=5`，Rust 版略放宽到 10s 提升弱网容错。
const HEALTH_TIMEOUT_SECS: u64 = 10;

/// sync/pull 响应体（字段名对照 Python `handle_sync_pull` 返回 `{prompts: [...]}`）。
#[derive(Debug, serde::Deserialize)]
struct SyncPullResp {
    #[serde(default)]
    prompts: Vec<crate::models::prompt::PromptRow>,
}

/// sync/push 响应体（字段名对照 Python `handle_sync_push` 返回 `{accepted: <count>}`）。
#[derive(Debug, serde::Deserialize)]
struct SyncPushResp {
    #[serde(default)]
    accepted: u64,
}

/// cc-history/sync/pull 响应体（字段名对照 routes/cc_history.rs 的 CcSyncPullResp）。
#[derive(Debug, serde::Deserialize)]
struct CcSyncPullResp {
    #[serde(default)]
    items: Vec<crate::cc::models::ClaudeHistoryRow>,
}

/// cc-history/sync/push 响应体（字段名对照 routes/cc_history.rs 的 CcSyncPushResp）。
#[derive(Debug, serde::Deserialize)]
struct CcSyncPushResp {
    #[serde(default)]
    accepted: u64,
}

/// 对端 HTTP 客户端，封装 reqwest::Client。
///
/// Business Logic: 所有对端调用复用同一 Client（内部连接池），提升效率。
///     Client 本身是 Clone 廉贵的（内部 Arc），故 PeerClient 可直接 Clone 共享。
#[allow(dead_code)]
pub struct PeerClient {
    client: reqwest::Client,
}

impl PeerClient {
    /// 创建客户端，配置默认超时。
    ///
    /// Code Logic: reqwest::Client::builder 设置 timeout；rustls-tls feature 已在 Cargo.toml 启用，
    ///     无需系统 OpenSSL。本机自签场景实际走 http，TLS 仅用于 https 资源（如 GitHub Releases）。
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(HEALTH_TIMEOUT_SECS))
            .build()
            .expect("构造 reqwest Client 失败（rustls-tls 初始化异常）");
        Self { client }
    }

    /// 健康检查：GET 对端 /api/health，返回 true 表示可达。
    ///
    /// Business Logic: 同步/传输前需验证对端在线且 HTTP 服务正常。
    /// Code Logic: 拼接 `http://{addr}:{port}/api/health`，发送 GET，status==200 即 true；
    ///             任何异常（网络、超时、非 200）返回 false（与 Python `health_check` 一致，不向上抛错）。
    #[allow(dead_code)]
    pub async fn health(&self, addr: &str, port: u16) -> bool {
        let url = format!("http://{addr}:{port}/api/health");
        match self.client.get(&url).send().await {
            Ok(resp) => resp.status().as_u16() == 200,
            Err(e) => {
                tracing::debug!("health_check 失败 ({url}): {e}");
                false
            }
        }
    }

    /// 同步 pull：向对端发送本端 prompt 摘要，获取对端认为本端需要的 prompt。
    ///
    /// Business Logic: Prompt 同步第一步——把本端摘要发给对端，对端比对后返回本端需要更新的
    ///     prompt 完整数据。对照 Python `sync_pull`。
    ///
    /// Code Logic: POST `{base_url}/api/sync/pull`，请求体 `{summaries: [...]}`，
    ///     期望响应 `{prompts: [PromptRow snake_case, ...]}`。失败返回空 Vec（不阻断同步）。
    pub async fn sync_pull(
        &self,
        base_url: &str,
        local_summary: Vec<serde_json::Value>,
    ) -> Vec<crate::models::prompt::PromptRow> {
        let url = format!("{base_url}/api/sync/pull");
        let body = serde_json::json!({ "summaries": local_summary });
        match self.client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().as_u16() == 200 => match resp.json::<SyncPullResp>().await {
                Ok(data) => {
                    tracing::info!("sync_pull 从 {base_url} 获取 {} 条 prompt", data.prompts.len());
                    data.prompts
                }
                Err(e) => {
                    tracing::error!("sync_pull 解析响应失败 ({base_url}): {e}");
                    Vec::new()
                }
            },
            Ok(resp) => {
                tracing::warn!("sync_pull 失败 ({base_url}): HTTP {}", resp.status());
                Vec::new()
            }
            Err(e) => {
                tracing::error!("sync_pull 异常 ({base_url}): {e}");
                Vec::new()
            }
        }
    }

    /// 同步 push：将本端有但对端缺少的 prompt 推送给对端。
    ///
    /// Business Logic: Prompt 同步第二步——把本端独有或领先的 prompt 推过去。对照 Python `sync_push`。
    ///
    /// Code Logic: POST `{base_url}/api/sync/push`，请求体 `{prompts: [...]}`，
    ///     期望响应 `{accepted: <count>}`。HTTP 200 即视为成功（accepted 仅作日志）。
    pub async fn sync_push(
        &self,
        base_url: &str,
        prompts: &[crate::models::prompt::PromptRow],
    ) -> bool {
        let url = format!("{base_url}/api/sync/push");
        let body = serde_json::json!({ "prompts": prompts });
        match self.client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().as_u16() == 200 => match resp.json::<SyncPushResp>().await {
                Ok(data) => {
                    tracing::info!("sync_push 到 {base_url} 成功，对端接收 {} 条", data.accepted);
                    true
                }
                Err(e) => {
                    // 即使 JSON 解析失败，HTTP 200 已表示对端接收，保守返回 true 并告警
                    tracing::warn!("sync_push 响应解析失败 ({base_url}): {e}");
                    true
                }
            },
            Ok(resp) => {
                tracing::warn!("sync_push 失败 ({base_url}): HTTP {}", resp.status());
                false
            }
            Err(e) => {
                tracing::error!("sync_push 异常 ({base_url}): {e}");
                false
            }
        }
    }

    /// 文件传输初始化：向对端发送文件元数据，获取 accepted 与 resume_offset。
    ///
    /// Business Logic: 发送端分块前先握手，告知对端文件名/大小/SHA256，对端确认并返回续传 offset。
    ///     对照 Python `transfer_init`（POST /api/transfer/init）。
    ///
    /// Code Logic: POST `{base_url}/api/transfer/init`，body `{transfer_id, filename, size, sha256, chunk_size}`，
    ///     期望响应 `{transfer_id, accepted, resume_offset}`。成功返回完整响应 JSON；
    ///     HTTP 非 200 或网络异常返回 Err（调用方据此标记任务 failed）。
    pub async fn transfer_init(
        &self,
        base_url: &str,
        metadata: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let url = format!("{base_url}/api/transfer/init");
        let resp = self
            .client
            .post(&url)
            .json(&metadata)
            .send()
            .await
            .map_err(|e| format!("请求 init 失败: {e}"))?;
        if resp.status().as_u16() != 200 {
            return Err(format!("init HTTP {}", resp.status()));
        }
        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| format!("init 响应解析失败: {e}"))
    }

    /// 发送一个数据块到对端。
    ///
    /// Business Logic: 分块传输核心调用，body 为原始字节，header X-Chunk-Offset 标明写入 offset。
    ///     对照 Python `transfer_chunk`（POST /api/transfer/chunk/{id}）。
    ///
    /// Code Logic: POST `{base_url}/api/transfer/chunk/{id}`，header `X-Chunk-Offset: <offset>`，
    ///     body = bytes（reqwest Body）。期望响应 `{success, received_bytes}`。
    ///     成功且 success==true 返回 Ok(true)；success==false 返回 Ok(false)；HTTP 非 200 或异常返回 Err。
    pub async fn transfer_chunk(
        &self,
        base_url: &str,
        transfer_id: &str,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<bool, String> {
        let url = format!("{base_url}/api/transfer/chunk/{transfer_id}");
        let resp = self
            .client
            .post(&url)
            .header("X-Chunk-Offset", offset.to_string())
            .body(data)
            .send()
            .await
            .map_err(|e| format!("请求 chunk 失败: {e}"))?;
        if resp.status().as_u16() != 200 {
            return Err(format!("chunk HTTP {}", resp.status()));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("chunk 响应解析失败: {e}"))?;
        let success = body
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(success)
    }

    /// 查询对端某接收任务的状态。
    ///
    /// Business Logic: 发送端可轮询对端接收进度（M5 当前未强制使用，保留供扩展）。
    ///     对照 Python `get_transfer_status`（GET /api/transfer/status/{id}）。
    #[allow(dead_code)]
    pub async fn transfer_status(
        &self,
        base_url: &str,
        transfer_id: &str,
    ) -> Result<serde_json::Value, String> {
        let url = format!("{base_url}/api/transfer/status/{transfer_id}");
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("请求 status 失败: {e}"))?;
        if resp.status().as_u16() != 200 {
            return Err(format!("status HTTP {}", resp.status()));
        }
        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| format!("status 响应解析失败: {e}"))
    }

    /// Claude Code 历史同步 pull：向对端发送本端 cc 历史摘要，获取对端认为本端需要的 cc 历史。
    ///
    /// Business Logic: CC 历史同步第一步——把本端摘要发给对端，对端比对后返回本端需要更新的
    ///     cc 历史完整数据。走独立链路 `/api/cc-history/sync/pull`，与 prompts 同步解耦。
    ///
    /// Code Logic: POST `{base_url}/api/cc-history/sync/pull`，请求体 `{summaries: [...]}`，
    ///     期望响应 `{items: [ClaudeHistoryRow snake_case, ...]}`。失败返回空 Vec（不阻断同步）。
    pub async fn cc_sync_pull(
        &self,
        base_url: &str,
        local_summary: Vec<serde_json::Value>,
    ) -> Vec<crate::cc::models::ClaudeHistoryRow> {
        let url = format!("{base_url}/api/cc-history/sync/pull");
        let body = serde_json::json!({ "summaries": local_summary });
        match self.client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().as_u16() == 200 => {
                match resp.json::<CcSyncPullResp>().await {
                    Ok(data) => {
                        tracing::info!(
                            "cc_sync_pull 从 {base_url} 获取 {} 条 CC 历史",
                            data.items.len()
                        );
                        data.items
                    }
                    Err(e) => {
                        tracing::error!("cc_sync_pull 解析响应失败 ({base_url}): {e}");
                        Vec::new()
                    }
                }
            }
            Ok(resp) => {
                tracing::warn!(
                    "cc_sync_pull 失败 ({base_url}): HTTP {}",
                    resp.status()
                );
                Vec::new()
            }
            Err(e) => {
                tracing::error!("cc_sync_pull 异常 ({base_url}): {e}");
                Vec::new()
            }
        }
    }

    /// Claude Code 历史同步 push：将本端有而对端缺少的 cc 历史推送给对端。
    ///
    /// Business Logic: CC 历史同步第二步——把本端独有或领先的 cc 历史推过去。
    ///
    /// Code Logic: POST `{base_url}/api/cc-history/sync/push`，请求体 `{items: [...]}`，
    ///     期望响应 `{accepted: <count>}`。HTTP 200 即视为成功。
    pub async fn cc_sync_push(
        &self,
        base_url: &str,
        items: &[crate::cc::models::ClaudeHistoryRow],
    ) -> bool {
        let url = format!("{base_url}/api/cc-history/sync/push");
        let body = serde_json::json!({ "items": items });
        match self.client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().as_u16() == 200 => match resp.json::<CcSyncPushResp>().await {
                Ok(data) => {
                    tracing::info!(
                        "cc_sync_push 到 {base_url} 成功，对端接收 {} 条",
                        data.accepted
                    );
                    true
                }
                Err(e) => {
                    tracing::warn!("cc_sync_push 响应解析失败 ({base_url}): {e}");
                    true
                }
            },
            Ok(resp) => {
                tracing::warn!("cc_sync_push 失败 ({base_url}): HTTP {}", resp.status());
                false
            }
            Err(e) => {
                tracing::error!("cc_sync_push 异常 ({base_url}): {e}");
                false
            }
        }
    }
}

impl Default for PeerClient {
    fn default() -> Self {
        Self::new()
    }
}
