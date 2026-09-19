//! net/routes/mobile_push.rs — Flutter App 推送 token 登记
//!
//! Business Logic: 无身份鉴权的 LAN 登记；同 mobileDeviceId upsert。
//! Code Logic: POST register/unregister → MobilePushRepo。

use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::storage::mobile_push_repo::{maybe_queue_notify, MobilePushPayload, MobilePushRepo};
use axum::extract::{Extension, State};
use axum::Json;
use serde::{Deserialize, Serialize};

/// `POST /api/mobile/push/register` 请求体。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterReq {
    pub mobile_device_id: String,
    pub platform: String,
    pub token: String,
    #[serde(default)]
    pub app_build: String,
}

/// `POST /api/mobile/push/unregister` 请求体。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnregisterReq {
    pub mobile_device_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OkResponse {
    pub ok: bool,
}

/// POST /api/mobile/push/register — upsert token。
pub async fn register(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RegisterReq>,
) -> P2pResult<Json<OkResponse>> {
    let repo = MobilePushRepo::new(state.db.clone());
    repo.register(
        &req.mobile_device_id,
        &req.platform,
        &req.token,
        &req.app_build,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.push.register"))?;
    Ok(Json(OkResponse { ok: true }))
}

/// POST /api/mobile/push/unregister — 按设备删除。
pub async fn unregister(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<UnregisterReq>,
) -> P2pResult<Json<OkResponse>> {
    let repo = MobilePushRepo::new(state.db.clone());
    repo.unregister(&req.mobile_device_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.push.unregister"))?;
    Ok(Json(OkResponse { ok: true }))
}

/// 工作台 Attention 变化时调用：无中转配置则跳过。
#[allow(dead_code)]
pub fn skip_or_queue_attention_push(
    relay_url: Option<&str>,
    relay_token: Option<&str>,
    payload: &MobilePushPayload,
) -> P2pResult<crate::storage::mobile_push_repo::MobilePushSendOutcome> {
    let dummy = P2pRequestContext {
        request_id: "mobile-push-local".into(),
    };
    let relay =
        crate::storage::mobile_push_repo::MobilePushRelay::from_optional(relay_url, relay_token);
    maybe_queue_notify(relay.as_ref(), payload)
        .map_err(|e| P2pError::from_app_error(e, &dummy, "mobile.push.notify"))
}
