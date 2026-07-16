//! net/routes/browser_verification — 浏览器验证 P2P/mobile 路由
//!
//! Business Logic（为什么需要这个模块）:
//!     remote/mobile 需要在 owning device 上启动/查询/取消验证并拉取 artifact；
//!     controller 只代理，engine 仅在 owner 执行。
//!
//! Code Logic（这个模块做什么）:
//!     挂载 create/get/cancel/artifact 路由，委托 commands helper。

use crate::commands::workbench::{
    cancel_browser_verification_for_state, get_browser_verification_artifact_for_state,
    get_browser_verification_for_state, start_browser_verification_for_state,
    StartBrowserVerificationReq,
};
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::workbench::browser_verification::models::BrowserVerificationRun;
use axum::extract::State;
use axum::Extension;
use axum::Json;
use serde::{Deserialize, Serialize};

/// 创建/启动验证请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserVerificationCreateReq {
    pub preview_id: String,
    /// 幂等键（必填）。
    pub request_id: String,
    #[serde(default)]
    pub commands: Vec<crate::workbench::browser_verification::BrowserVerificationCommand>,
}

/// 按 id 查询/取消。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserVerificationIdReq {
    pub run_id: String,
}

/// artifact 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserVerificationArtifactReq {
    pub run_id: String,
    pub artifact_id: String,
}

/// 创建验证 run（owner 执行；需 idempotency key=requestId）。
///
/// Business Logic（为什么需要这个函数）:
///     remote peer/mobile 在 owner 上绑定 live preview 启动验证。
///
/// Code Logic（这个函数做什么）:
///     校验 request_id 非空后委托 start helper。
pub async fn create_browser_verification(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<BrowserVerificationCreateReq>,
) -> P2pResult<Json<BrowserVerificationRun>> {
    if req.request_id.trim().is_empty() {
        return Err(P2pError::from_app_error(
            crate::error::AppError::validation("validation_error"),
            &ctx,
            "workbench.browser_verification.create",
        ));
    }
    let run = start_browser_verification_for_state(
        &state,
        StartBrowserVerificationReq {
            preview_id: req.preview_id,
            request_id: req.request_id,
            commands: req.commands,
        },
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.browser_verification.create"))?;
    Ok(Json(run))
}

/// 查询验证 run。
///
/// Business Logic（为什么需要这个函数）:
///     客户端轮询状态与 evidence。
///
/// Code Logic（这个函数做什么）:
///     委托 get helper。
pub async fn get_browser_verification(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<BrowserVerificationIdReq>,
) -> P2pResult<Json<BrowserVerificationRun>> {
    let run = get_browser_verification_for_state(&state, req.run_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.browser_verification.get"))?;
    Ok(Json(run))
}

/// 取消验证（自然幂等）。
///
/// Business Logic（为什么需要这个函数）:
///     remote 停止 owner 上的验证会话。
///
/// Code Logic（这个函数做什么）:
///     委托 cancel helper。
pub async fn cancel_browser_verification(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<BrowserVerificationIdReq>,
) -> P2pResult<Json<BrowserVerificationRun>> {
    let run = cancel_browser_verification_for_state(&state, req.run_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.browser_verification.cancel"))?;
    Ok(Json(run))
}

/// 拉取 artifact（base64）。
///
/// Business Logic（为什么需要这个函数）:
///     remote UI 展示截图 evidence。
///
/// Code Logic（这个函数做什么）:
///     委托 artifact helper；拒绝路径穿越 id。
pub async fn get_browser_verification_artifact(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<BrowserVerificationArtifactReq>,
) -> P2pResult<Json<crate::commands::workbench::BrowserVerificationArtifactDto>> {
    let dto = get_browser_verification_artifact_for_state(&state, req.run_id, req.artifact_id)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &ctx, "workbench.browser_verification.artifact")
        })?;
    Ok(Json(dto))
}

#[cfg(test)]
mod tests {
    use crate::workbench::browser_verification::FakeEngine;
    use crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry;
    use std::sync::Arc;
    use tempfile::tempdir;

    /// remote 语义：controller 侧 registry 若是 RemoteRelay 则不启 engine。
    #[tokio::test]
    async fn remote_verification_runs_engine_only_on_owner() {
        // 简化：owner Mini 启 engine；controller 用 RemoteRelay 不启
        let dir = tempdir().unwrap();
        let owner_service =
            crate::workbench::browser_verification::BrowserVerificationService::new(
                Arc::new(FakeEngine::succeeds()),
                dir.path().join("owner"),
                "owner".into(),
            )
            .unwrap();
        let controller_service =
            crate::workbench::browser_verification::BrowserVerificationService::new(
                Arc::new(FakeEngine::succeeds()),
                dir.path().join("ctrl"),
                "ctrl".into(),
            )
            .unwrap();
        let owner_previews = WorkbenchBrowserPreviewRegistry::new();
        let controller_previews = WorkbenchBrowserPreviewRegistry::new();

        let owner_preview =
            owner_previews.create_local_for_test("proj", None, "http://127.0.0.1:5173/");
        let ctrl_preview = controller_previews.create_remote_relay(
            "proj".into(),
            None,
            "http://192.168.1.2:62116".into(),
            owner_preview.preview_id.clone(),
            "http://127.0.0.1:5173/".into(),
            62116,
        );

        // controller 解析 RemoteRelay → 不 start
        let session = controller_previews.lookup(&ctrl_preview.preview_id).unwrap();
        assert!(matches!(
            session.target,
            crate::workbench::browser_proxy::BrowserPreviewTarget::RemoteRelay { .. }
        ));
        assert_eq!(controller_service.engine_start_count(), 0);

        // owner start
        let _ = owner_service
            .start(
                owner_preview.preview_id.clone(),
                "proj".into(),
                None,
                "http://127.0.0.1:5173/".into(),
                crate::workbench::browser_verification::BrowserVerificationStartRequest {
                    preview_id: owner_preview.preview_id,
                    request_id: "idem-1".into(),
                    commands: crate::workbench::browser_verification::default_smoke_commands(),
                    fingerprint: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(controller_service.engine_start_count(), 0);
        assert_eq!(owner_service.engine_start_count(), 1);
    }
}
