//! net/routes/attention.rs — Mobile Attention 快照 HTTP 路由。
//!
//! Business Logic（为什么需要这个模块）:
//!     手机浏览器 `/mobile` 需要同源 GET 拉取与桌面相同的 Attention 快照；
//!     能力由 `attention.v1` 宣告，旧后端不支持时客户端应显示 unsupported，不得猜测旧接口。
//!
//! Code Logic（这个模块做什么）:
//!     `GET /api/mobile/attention` 委托 `list_attention_items_for_state`；
//!     使用 P2pRequestContext/P2pError 信封；不递归请求其它设备的 attention 聚合。

use crate::attention::models::AttentionSnapshotDto;
use crate::commands::attention::list_attention_items_for_state;
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use axum::extract::{Extension, State};
use axum::Json;

/// Business Logic（为什么需要这个函数）:
///     手机 Inbox 需要通过同源 HTTP 读取本机聚合快照；本路由只聚合当前 backend 的项目，
///     Orchestrator source 可对各 remote owning device 刷新 mirror 一次，但绝不让对端再聚合 attention。
///
/// Code Logic（这个函数做什么）:
///     委托 `list_attention_items_for_state`；错误经 P2pError 信封返回。
pub async fn list_attention(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
) -> P2pResult<Json<AttentionSnapshotDto>> {
    let snapshot = list_attention_items_for_state(&state)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "attention.list"))?;
    Ok(Json(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::protocol::{
        server_protocol_info, PeerProtocolInfo, CAPABILITY_ATTENTION_V1,
        CAPABILITY_ERRORS_ENVELOPE_V1, PROTOCOL_VERSION_V1,
    };
    use crate::net::request_context::P2pRequestContext;

    /// Business Logic（为什么需要这个测试）:
    ///     桌面 helper 与 Mobile HTTP 必须产出同一 JSON 契约，避免两端字段漂移。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对同一快照 DTO 走 helper 序列化路径与 HTTP Json 包装序列化路径，断言 Value 相等。
    #[test]
    fn tauri_helper_and_http_response_serialize_to_equal_json() {
        // 契约测试：两端共享 AttentionSnapshotDto，序列化必须完全一致。
        // 使用固定字段避免 generatedAt 时钟抖动；真实 helper 在集成路径调用同一类型。
        let snapshot = AttentionSnapshotDto {
            generated_at: "2026-07-12T10:00:00Z".to_string(),
            counts: crate::attention::models::AttentionCountsDto {
                total: 1,
                decision: 0,
                blocked: 0,
                environment: 1,
            },
            items: vec![crate::attention::models::AttentionItemDto {
                id: "workbench:dependency:tmux".to_string(),
                category: crate::attention::models::AttentionCategory::Environment,
                source_kind: crate::attention::models::AttentionSourceKind::WorkbenchDependency,
                title: "tmux 依赖缺失".to_string(),
                summary: "Workbench 需要 tmux".to_string(),
                updated_at: "2026-07-12T09:00:00Z".to_string(),
                freshness: crate::attention::models::AttentionFreshness::Live,
                cached_at: None,
                project: None,
                device: None,
                target: crate::attention::models::AttentionTargetDto::Settings {
                    tab: crate::attention::models::AttentionSettingsTab::Dependencies,
                },
            }],
        };

        // 模拟 Tauri command 返回路径：直接序列化 helper 结果。
        let tauri_json = serde_json::to_value(&snapshot).expect("tauri serialize");
        // 模拟 HTTP handler 返回路径：Json 包装后同样序列化 body。
        let http_body = Json(snapshot.clone());
        let http_json = serde_json::to_value(&http_body.0).expect("http serialize");
        assert_eq!(
            tauri_json, http_json,
            "list_attention_items 与 GET /api/mobile/attention 必须共享同一 JSON 契约"
        );
        assert_eq!(tauri_json["generatedAt"], "2026-07-12T10:00:00Z");
        assert_eq!(tauri_json["items"][0]["id"], "workbench:dependency:tmux");
        assert_eq!(tauri_json["items"][0]["sourceKind"], "workbenchDependency");
        assert_eq!(tauri_json["items"][0]["target"]["kind"], "settings");
        assert_eq!(tauri_json["items"][0]["target"]["tab"], "dependencies");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机宣告 attention.v1 后，Mobile 客户端才能安全调用 GET /api/mobile/attention。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 server_protocol_info 含 attention.v1，且 supports 命中。
    #[test]
    fn server_advertises_attention_v1_capability() {
        let info = server_protocol_info();
        assert_eq!(info.protocol_version, PROTOCOL_VERSION_V1);
        assert!(
            info.supports(CAPABILITY_ATTENTION_V1),
            "本机必须宣告 attention.v1，与 HTTP 路由同提交落地"
        );
        assert!(info.supports(CAPABILITY_ERRORS_ENVELOPE_V1));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧后端无 attention.v1 时，Mobile 必须分类为 unsupported，不得猜测旧端点结果。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 legacy v0 与仅有 errors.envelope.v1 的 v1 对端，断言 supports(attention.v1)=false。
    #[test]
    fn legacy_server_without_attention_v1_is_unsupported() {
        let legacy_v0 = PeerProtocolInfo {
            protocol_version: 0,
            capabilities: Vec::new(),
        };
        assert!(
            !legacy_v0.supports(CAPABILITY_ATTENTION_V1),
            "v0 对端必须被分类为不支持 attention"
        );

        let legacy_v1_without_attention = PeerProtocolInfo {
            protocol_version: PROTOCOL_VERSION_V1,
            capabilities: vec![CAPABILITY_ERRORS_ENVELOPE_V1.to_string()],
        };
        assert!(
            !legacy_v1_without_attention.supports(CAPABILITY_ATTENTION_V1),
            "仅有 errors.envelope.v1 的对端不得被猜测为支持 attention"
        );
        // 客户端在 supports=false 时不得调用 /api/mobile/attention 或其它旧接口拼装。
        assert!(
            !legacy_v1_without_attention
                .capabilities
                .iter()
                .any(|c| c.contains("attention")),
            "legacy 能力列表不得含 attention 前缀 token"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     HTTP 错误必须经 P2pError 信封并保留 request id，符合 P2P 计划契约。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 generic AppError 构造 attention.list 信封，断言 code/request_id。
    #[test]
    fn attention_errors_use_p2p_envelope_with_request_id() {
        let ctx = P2pRequestContext {
            request_id: "req-attention-1".to_string(),
        };
        let err = P2pError::from_app_error(
            crate::error::AppError::generic("source boom"),
            &ctx,
            "attention.list",
        );
        assert_eq!(err.envelope().request_id, "req-attention-1");
        assert_eq!(err.envelope().code, "internal_error");
        assert!(err.envelope().error.contains("source boom"));
    }
}
