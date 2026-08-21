//! 浏览器自动验证命令
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面/control/P2P 入口必须只接受 live previewId，从 registry 解析 target，
//!     禁止调用方传入任意 URL/CDP；RemoteRelay 不得在 controller 上启动 engine。
//!
//! Code Logic（这个模块做什么）:
//!     提供 start/get/cancel/artifact helper 与 Tauri command；单元测试覆盖过期 preview。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::browser_proxy::BrowserPreviewTarget;
use crate::workbench::browser_verification::models::{
    default_smoke_commands, BrowserVerificationRun, BrowserVerificationStartRequest,
};
use crate::workbench::remote_client::RemoteWorkbenchClient;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use tauri::State;

pub use crate::workbench::browser_verification::models::BrowserVerificationArtifactDto;

/// start 请求体（仅 previewId + requestId + 可选命令）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartBrowserVerificationReq {
    pub preview_id: String,
    pub request_id: String,
    #[serde(default)]
    pub commands: Vec<crate::workbench::browser_verification::BrowserVerificationCommand>,
}

/// 从 live preview registry 解析目标。
///
/// Business Logic（为什么需要这个函数）:
///     verification 只能打到已登记 loopback preview；RemoteRelay 必须转发到 owner。
///
/// Code Logic（这个函数做什么）:
///     lookup preview；Local → target_url；RemoteRelay → base_url + remote_preview_id。
pub enum ResolvedVerificationTarget {
    /// 本机 engine 可启动。
    Local {
        project_id: String,
        worktree_id: Option<String>,
        target_url: String,
    },
    /// 必须代理到 owner（controller 不启 engine）。
    Remote {
        #[allow(dead_code)]
        project_id: String,
        #[allow(dead_code)]
        worktree_id: Option<String>,
        base_url: String,
        /// owning device id，用于 RemoteWorkbenchClient device bind。
        device_id: String,
        remote_preview_id: String,
    },
}

/// 解析 preview 为 Local 或 Remote 目标。
///
/// Business Logic（为什么需要这个函数）:
///     start/get 路径共享同一 registry 契约；Remote 必须带 owning device_id 做 bind。
///
/// Code Logic（这个函数做什么）:
///     lookup；缺失 `browser_preview_not_found`；RemoteRelay 从项目 row 解析 device_id。
pub async fn resolve_verification_target(
    state: &AppState,
    preview_id: &str,
) -> Result<ResolvedVerificationTarget, AppError> {
    let session = state
        .workbench_browser_previews
        .lookup(preview_id)
        .ok_or_else(|| AppError::not_found("browser_preview_not_found"))?;
    match session.target {
        BrowserPreviewTarget::Local { target_url } => {
            let normalized = crate::workbench::browser::normalize_browser_target_url(&target_url)
                .map_err(|_| AppError::validation("browser_redirect_escape"))?;
            Ok(ResolvedVerificationTarget::Local {
                project_id: session.project_id,
                worktree_id: session.worktree_id,
                target_url: normalized,
            })
        }
        BrowserPreviewTarget::RemoteRelay {
            base_url,
            remote_preview_id,
            ..
        } => {
            let project = state
                .workbench_project_repo
                .get(&session.project_id)
                .await?
                .ok_or_else(|| AppError::not_found("browser_preview_project_not_found"))?;
            let device_id = project.device_id.trim().to_string();
            if device_id.is_empty() {
                return Err(AppError::validation(
                    "browser_verification_remote_missing_device_id",
                ));
            }
            Ok(ResolvedVerificationTarget::Remote {
                project_id: session.project_id,
                worktree_id: session.worktree_id,
                base_url,
                device_id,
                remote_preview_id,
            })
        }
    }
}

/// 在 state 上启动验证：Local 启 engine；RemoteRelay 代理到 owner。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri/control/P2P 共享；controller 永不本地启动 Chromium。
///
/// Code Logic（这个函数做什么）:
///     Local → service.start；Remote → RemoteWorkbenchClient.create（capability 门禁）并记录 proxy。
pub async fn start_browser_verification_for_state(
    state: &AppState,
    req: StartBrowserVerificationReq,
) -> Result<BrowserVerificationRun, AppError> {
    crate::workbench::browser::require_experimental_browser(state)?;
    if req.preview_id.is_empty() || req.request_id.is_empty() {
        return Err(AppError::validation("validation_error"));
    }
    if req.commands.len() > 32 {
        return Err(AppError::validation("resource_limit"));
    }
    let commands = if req.commands.is_empty() {
        default_smoke_commands()
    } else {
        req.commands
    };
    match resolve_verification_target(state, &req.preview_id).await? {
        ResolvedVerificationTarget::Local {
            project_id,
            worktree_id,
            target_url,
        } => {
            let start = BrowserVerificationStartRequest {
                preview_id: req.preview_id.clone(),
                request_id: req.request_id,
                commands,
                fingerprint: None,
            };
            state
                .browser_verification
                .start(req.preview_id, project_id, worktree_id, target_url, start)
                .await
        }
        ResolvedVerificationTarget::Remote {
            base_url,
            device_id,
            remote_preview_id,
            ..
        } => {
            // controller：不增加本地 engine_starts；mutation 必须 bind device_id
            let client = RemoteWorkbenchClient::new().with_expected_device_id(&device_id);
            let run = client
                .create_browser_verification(
                    &base_url,
                    &remote_preview_id,
                    &req.request_id,
                    &commands,
                )
                .await?;
            state
                .browser_verification
                .remember_remote_proxy(run.session.id.clone(), base_url, device_id)
                .await;
            Ok(run)
        }
    }
}

/// 查询验证 run（本地或 remote proxy）。
///
/// Business Logic（为什么需要这个函数）:
///     UI 轮询状态。
///
/// Code Logic（这个函数做什么）:
///     若 run 登记为 remote proxy 则转发 get；否则 service.get。
pub async fn get_browser_verification_for_state(
    state: &AppState,
    run_id: String,
) -> Result<BrowserVerificationRun, AppError> {
    if let Some(proxy) = state
        .browser_verification
        .remote_proxy_endpoint(&run_id)
        .await
    {
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&proxy.device_id)
            .get_browser_verification(&proxy.base_url, &run_id)
            .await;
    }
    state.browser_verification.get(&run_id).await
}

/// 取消验证 run（本地或 remote proxy）。
///
/// Business Logic（为什么需要这个函数）:
///     用户停止验证。
///
/// Code Logic（这个函数做什么）:
///     remote proxy 转发 cancel；否则 service.cancel（await join 后删 profile）。
pub async fn cancel_browser_verification_for_state(
    state: &AppState,
    run_id: String,
) -> Result<BrowserVerificationRun, AppError> {
    if let Some(proxy) = state
        .browser_verification
        .remote_proxy_endpoint(&run_id)
        .await
    {
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&proxy.device_id)
            .cancel_browser_verification(&proxy.base_url, &run_id)
            .await;
    }
    state.browser_verification.cancel(&run_id).await
}

/// 读取 artifact（本地或 remote proxy）。
///
/// Business Logic（为什么需要这个函数）:
///     UI 展示截图。
///
/// Code Logic（这个函数做什么）:
///     remote 转发；本地读字节并 base64；拒绝路径穿越 id。
pub async fn get_browser_verification_artifact_for_state(
    state: &AppState,
    run_id: String,
    artifact_id: String,
) -> Result<BrowserVerificationArtifactDto, AppError> {
    if artifact_id.contains("..") || artifact_id.contains('/') || artifact_id.contains('\\') {
        return Err(AppError::validation("resource_limit"));
    }
    if let Some(proxy) = state
        .browser_verification
        .remote_proxy_endpoint(&run_id)
        .await
    {
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&proxy.device_id)
            .get_browser_verification_artifact(&proxy.base_url, &run_id, &artifact_id)
            .await;
    }
    let bytes = state
        .browser_verification
        .artifact(&run_id, &artifact_id)
        .await?;
    Ok(BrowserVerificationArtifactDto {
        run_id,
        artifact_id,
        content_type: "image/png".into(),
        byte_len: bytes.len(),
        base64: B64.encode(&bytes),
    })
}

/// Tauri：启动浏览器验证。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端一键验证预览。
///
/// Code Logic（这个命令做什么）:
///     解包 State 后委托 helper。
#[tauri::command]
pub async fn start_workbench_browser_verification(
    state: State<'_, AppState>,
    preview_id: String,
    request_id: String,
    commands: Option<Vec<crate::workbench::browser_verification::BrowserVerificationCommand>>,
) -> Result<BrowserVerificationRun, AppError> {
    use super::common::proxy_workbench_if_gui;
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "browser.verification.start",
        serde_json::json!({
            "previewId": preview_id.clone(),
            "requestId": request_id.clone(),
            "commands": commands.clone(),
        }),
    )
    .await?
    {
        return Ok(v);
    }
    start_browser_verification_for_state(
        state.inner(),
        StartBrowserVerificationReq {
            preview_id,
            request_id,
            commands: commands.unwrap_or_default(),
        },
    )
    .await
}

/// Tauri：查询验证 run。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端轮询结果。
///
/// Code Logic（这个命令做什么）:
///     委托 get helper。
#[tauri::command]
pub async fn get_workbench_browser_verification(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<BrowserVerificationRun, AppError> {
    use super::common::proxy_workbench_if_gui;
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "browser.verification.get",
        serde_json::json!({ "runId": run_id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    get_browser_verification_for_state(state.inner(), run_id).await
}

/// Tauri：取消验证。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端停止验证。
///
/// Code Logic（这个命令做什么）:
///     委托 cancel helper。
#[tauri::command]
pub async fn cancel_workbench_browser_verification(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<BrowserVerificationRun, AppError> {
    use super::common::proxy_workbench_if_gui;
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "browser.verification.cancel",
        serde_json::json!({ "runId": run_id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    cancel_browser_verification_for_state(state.inner(), run_id).await
}

/// Tauri：读取验证 artifact。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端展示截图。
///
/// Code Logic（这个命令做什么）:
///     委托 artifact helper。
#[tauri::command]
pub async fn get_workbench_browser_verification_artifact(
    state: State<'_, AppState>,
    run_id: String,
    artifact_id: String,
) -> Result<BrowserVerificationArtifactDto, AppError> {
    use super::common::proxy_workbench_if_gui;
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "browser.verification.artifact",
        serde_json::json!({
            "runId": run_id.clone(),
            "artifactId": artifact_id.clone(),
        }),
    )
    .await?
    {
        return Ok(v);
    }
    get_browser_verification_artifact_for_state(state.inner(), run_id, artifact_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::browser_verification::{BrowserVerificationService, FakeEngine};
    use std::sync::Arc;
    use tempfile::tempdir;

    /// 构造仅含 browser 字段的最小 state 太重；改为直接测 registry + service 组合 helper。
    struct MiniState {
        previews: crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry,
        service: BrowserVerificationService,
    }

    impl MiniState {
        fn engine_start_count(&self) -> usize {
            self.service.engine_start_count()
        }
    }

    /// Mini 仅测「本地 engine 边界」：RemoteRelay 不得在本机 start engine。
    /// 生产路径 `start_browser_verification_for_state` 对 RemoteRelay 会 require_capability 后代理 owner。
    async fn start_on_mini(
        mini: &MiniState,
        req: StartBrowserVerificationReq,
    ) -> Result<BrowserVerificationRun, AppError> {
        let session = mini
            .previews
            .lookup(&req.preview_id)
            .ok_or_else(|| AppError::not_found("browser_preview_not_found"))?;
        let target_url = match session.target {
            BrowserPreviewTarget::Local { target_url } => target_url,
            BrowserPreviewTarget::RemoteRelay { .. } => {
                // 与生产「不启本地 engine」一致；完整 owner 代理见 remote_client + 集成路径
                return Err(AppError::validation("browser_verification_remote_only"));
            }
        };
        let start = BrowserVerificationStartRequest {
            preview_id: req.preview_id.clone(),
            request_id: req.request_id,
            commands: req.commands,
            fingerprint: None,
        };
        mini.service
            .start(
                req.preview_id,
                session.project_id,
                session.worktree_id,
                target_url,
                start,
            )
            .await
    }

    #[tokio::test]
    async fn start_rejects_target_url_and_requires_live_preview_id() {
        let dir = tempdir().unwrap();
        let mini = MiniState {
            previews: crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
            service: BrowserVerificationService::new(
                Arc::new(FakeEngine::succeeds()),
                dir.path().to_path_buf(),
                "o".into(),
            )
            .unwrap(),
        };
        // 过期/不存在 preview
        let err = start_on_mini(
            &mini,
            StartBrowserVerificationReq {
                preview_id: "expired".into(),
                request_id: "r1".into(),
                commands: vec![],
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "browser_preview_not_found");
        assert_eq!(mini.engine_start_count(), 0);

        // live preview 可启动
        let preview = mini
            .previews
            .create_local_for_test("proj", None, "http://127.0.0.1:5173/");
        let run = start_on_mini(
            &mini,
            StartBrowserVerificationReq {
                preview_id: preview.preview_id.clone(),
                request_id: "r2".into(),
                commands: default_smoke_commands(),
            },
        )
        .await
        .unwrap();
        assert_eq!(run.session.preview_id, preview.preview_id);
        assert_eq!(mini.engine_start_count(), 1);
    }

    #[tokio::test]
    async fn remote_relay_preview_never_starts_local_engine_in_mini_boundary() {
        let dir = tempdir().unwrap();
        let mini = MiniState {
            previews: crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
            service: BrowserVerificationService::new(
                Arc::new(FakeEngine::succeeds()),
                dir.path().to_path_buf(),
                "o".into(),
            )
            .unwrap(),
        };
        let preview = mini.previews.create_remote_relay(
            "proj".into(),
            None,
            "http://192.168.1.2:62116".into(),
            "remote-preview".into(),
            "http://127.0.0.1:5173/".into(),
            62116,
        );
        let err = start_on_mini(
            &mini,
            StartBrowserVerificationReq {
                preview_id: preview.preview_id,
                request_id: "r3".into(),
                commands: vec![],
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "browser_verification_remote_only");
        assert_eq!(mini.engine_start_count(), 0);
    }
}
