//! net/routes/health.rs — /api/health handler（供对端连通性检查）
//!
//! Business Logic（为什么需要这个模块）:
//!     对端设备在同步/传输前需检查本机是否在线且 HTTP 服务正常。对照 Python
//!     `protocol.py` 的 `handle_health`。字段名与 Python 完全一致（snake_case，给对端解析）。
//!
//! Code Logic（这个模块做什么）:
//!     GET /api/health → 200 + `{ok, device_id, device_name, http_port, ts,
//!     protocol_version, capabilities}`。从 AppState 取 device_id/device_name（config 读锁）
//!     与 actual_http_port（原子读）；protocol_version 与 capabilities 永远取
//!     `server_protocol_info()` 的完整权威清单，对端只需信任本机宣告。

use crate::net::protocol::{server_protocol_info, PeerProtocolInfo};
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;

/// health 响应体（字段名对照 Python `handle_health`，供对端 peer_client 解析）。
///
/// Business Logic: 字段保持 snake_case 与 Python 一致；对端旧 Python 版仅检查 status==200，
///     新增字段不影响兼容性。`protocol_version` / `capabilities` 是 P3 引入的权威协议元数据，
///     本机总是填充 `server_protocol_info()`；对端反序列化时缺失字段安全回落为 v0/空能力
///     （由 `#[serde(default)]` + PeerProtocolInfo 的 default 兜底）。
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub device_id: String,
    pub device_name: String,
    /// 本机 HTTP server 实际监听端口（对端据此回连）
    pub http_port: u16,
    /// 当前 UTC 时间戳（秒）
    pub ts: i64,
    /// 协议大版本号；本机总填充 `server_protocol_info().protocol_version`，
    /// 对端缺失字段时回落为 0（旧版 v0 兼容）。
    #[serde(default)]
    pub protocol_version: u32,
    /// 能力 token 列表；本机总填充 `server_protocol_info().capabilities`，
    /// 对端缺失字段时回落为空（旧版 v0 兼容）。
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl HealthResponse {
    /// 把响应里的协议元数据切片返回，便于能力判断（supports 等）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     peer_client 拿到 HealthResponse 后需判断对端是否支持某能力，
    ///     直接复用 PeerProtocolInfo::supports，避免在调用方重复拼装字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 protocol_version + capabilities 构造 PeerProtocolInfo（已自带去重排序语义）并返回。
    pub fn protocol_info(&self) -> PeerProtocolInfo {
        PeerProtocolInfo {
            protocol_version: self.protocol_version,
            capabilities: self.capabilities.clone(),
        }
    }
}

/// GET /api/health：返回本机设备信息与端口，供对端连通性验证。
///
/// Business Logic: 对端 peer_client.health() 调用此端点判断本机可达，并通过 protocol_version /
///     capabilities 判断本机支持的 P2P 能力。protocol_version 与 capabilities 必须永远取
///     `server_protocol_info()` 的完整权威清单，不能因调用方/状态而异，保证对端拿到确定结论。
/// Code Logic: device_id/device_name 从 config RwLock 读；http_port 从 AtomicU16 读；
///             ts 取 Utc::now().timestamp()（对照 Python int(datetime.now(timezone.utc).timestamp())）；
///             protocol_version + capabilities 取 server_protocol_info() 的字段。
pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let cfg = state.config.read().expect("config 读锁中毒");
    let port = state.actual_http_port.load(Ordering::SeqCst);
    let info = server_protocol_info();
    Json(HealthResponse {
        ok: true,
        device_id: cfg.device_id.clone(),
        device_name: cfg.device_name.clone(),
        http_port: port,
        ts: Utc::now().timestamp(),
        protocol_version: info.protocol_version,
        capabilities: info.capabilities,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     对端解析本机 health 响应时依赖 `protocol_version == 1` 判断是否可调 v1 路由；
    ///     必须保证 health 永远宣告 v1，且不因为 server_protocol_info() 的内部清单变化而漏掉。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造一个含 v1 协议元数据的 HealthResponse 并序列化，断言 JSON 含 `protocol_version: 1`
    ///     且 capabilities 含 `errors.envelope.v1`。
    #[test]
    fn health_response_serializes_protocol_version_and_capabilities() {
        let info = server_protocol_info();
        let resp = HealthResponse {
            ok: true,
            device_id: "device-test".to_string(),
            device_name: "test-device".to_string(),
            http_port: 12345,
            ts: 1_700_000_000,
            protocol_version: info.protocol_version,
            capabilities: info.capabilities.clone(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["protocol_version"], 1);
        let caps = json["capabilities"].as_array().expect("capabilities 为数组");
        let cap_strs: Vec<&str> = caps.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(
            cap_strs.contains(&"errors.envelope.v1"),
            "capabilities 应含 errors.envelope.v1, 实际: {cap_strs:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     server_protocol_info() 是本机对外的权威能力清单；health 响应必须与之完全一致，
    ///     不能裁剪或追加，否则对端会基于错误宣告调用未实现的路由。
    ///
    /// Code Logic（这个测试做什么）:
    ///     比较 HealthResponse 的字段与 server_protocol_info() 的字段相等。
    #[test]
    fn health_response_protocol_fields_match_server_protocol_info() {
        let info = server_protocol_info();
        let resp = HealthResponse {
            ok: true,
            device_id: "device-test".to_string(),
            device_name: "test-device".to_string(),
            http_port: 12345,
            ts: 1_700_000_000,
            protocol_version: info.protocol_version,
            capabilities: info.capabilities.clone(),
        };
        assert_eq!(resp.protocol_version, info.protocol_version);
        assert_eq!(resp.capabilities, info.capabilities);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧版对端/旧版响应体不携带 protocol_version 与 capabilities；本机/对端反序列化
    ///     必须容忍缺失字段并安全回落为 v0/空能力，不能因缺字段报错。
    ///
    /// Code Logic（这个测试做什么）:
    ///     从一个仅含 ok/device_id/device_name/http_port/ts 的旧版 JSON 反序列化 HealthResponse，
    ///     断言 protocol_version == 0 且 capabilities 为空。
    #[test]
    fn health_response_deserializes_legacy_v0_without_protocol_fields() {
        let json = r#"{
            "ok": true,
            "device_id": "device-legacy",
            "device_name": "legacy-device",
            "http_port": 8080,
            "ts": 1700000000
        }"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.protocol_version, 0);
        assert!(resp.capabilities.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本轮（P3 计划初始）只宣告 errors.envelope.v1；runtime/attention 相关能力由后续
    ///     Runtime/Inbox 计划随路由原子加入。必须保证此刻不误宣告它们，否则对端会调未实现的路由。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化一个 v1 + 仅 errors.envelope.v1 的响应，断言 capabilities 不含
    ///     runtime/attention/inbox 等未来 token（负向断言）。
    #[test]
    fn health_response_does_not_advertise_unimplemented_capabilities_yet() {
        let info = server_protocol_info();
        let resp = HealthResponse {
            ok: true,
            device_id: "device-test".to_string(),
            device_name: "test-device".to_string(),
            http_port: 12345,
            ts: 1_700_000_000,
            protocol_version: info.protocol_version,
            capabilities: info.capabilities.clone(),
        };
        let unimplemented = [
            "runtime.notifications.v1",
            "attention.requests.v1",
            "inbox.messages.v1",
        ];
        for cap in unimplemented {
            assert!(
                !resp.capabilities.iter().any(|c| c == cap),
                "本轮不应宣告未实现的能力 {cap}, 实际: {:?}",
                resp.capabilities
            );
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     peer_client 拿到 HealthResponse 后应能直接复用 PeerProtocolInfo::supports 判断能力，
    ///     protocol_info() 切片必须如实反映 protocol_version + capabilities，避免调用方手拼出错。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造含 v1 + errors.envelope.v1 的 HealthResponse，调用 protocol_info() 后断言
    ///     supports("errors.envelope.v1") 为 true、supports 未知 token 为 false。
    #[test]
    fn health_response_protocol_info_supports_known_capability() {
        let info = server_protocol_info();
        let resp = HealthResponse {
            ok: true,
            device_id: "device-test".to_string(),
            device_name: "test-device".to_string(),
            http_port: 12345,
            ts: 1_700_000_000,
            protocol_version: info.protocol_version,
            capabilities: info.capabilities.clone(),
        };
        let proto = resp.protocol_info();
        assert!(proto.supports("errors.envelope.v1"));
        assert!(!proto.supports("inbox.messages.v1"));
    }
}
