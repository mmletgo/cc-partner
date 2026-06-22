//! net/routes/health.rs — /api/health handler（供对端连通性检查）
//!
//! Business Logic（为什么需要这个模块）:
//!     对端设备在同步/传输前需检查本机是否在线且 HTTP 服务正常。对照 Python
//!     `protocol.py` 的 `handle_health`。字段名与 Python 完全一致（snake_case，给对端解析）。
//!
//! Code Logic（这个模块做什么）:
//!     GET /api/health → 200 + `{ok, device_id, device_name, http_port, ts}`。
//!     从 AppState 取 device_id/device_name（config 读锁）与 actual_http_port（原子读）。

use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use chrono::Utc;
use serde::Serialize;
use std::sync::atomic::Ordering;

/// health 响应体（字段名对照 Python `handle_health`，供对端 peer_client 解析）。
///
/// Business Logic: 字段保持 snake_case 与 Python 一致；对端旧 Python 版仅检查 status==200，
///     新增字段不影响兼容性。
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub device_id: String,
    pub device_name: String,
    /// 本机 HTTP server 实际监听端口（对端据此回连）
    pub http_port: u16,
    /// 当前 UTC 时间戳（秒）
    pub ts: i64,
}

/// GET /api/health：返回本机设备信息与端口，供对端连通性验证。
///
/// Business Logic: 对端 peer_client.health() 调用此端点判断本机可达。
/// Code Logic: device_id/device_name 从 config RwLock 读；http_port 从 AtomicU16 读；
///             ts 取 Utc::now().timestamp()（对照 Python int(datetime.now(timezone.utc).timestamp())）。
pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let cfg = state
        .config
        .read()
        .expect("config 读锁中毒");
    let port = state.actual_http_port.load(Ordering::SeqCst);
    Json(HealthResponse {
        ok: true,
        device_id: cfg.device_id.clone(),
        device_name: cfg.device_name.clone(),
        http_port: port,
        ts: Utc::now().timestamp(),
    })
}
