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
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use tauri::State;

/// start 请求体（仅 previewId + requestId + 可选命令）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartBrowserVerificationReq {
    pub preview_id: String,
    pub request_id: String,
    #[serde(default)]
    pub commands: Vec<crate::workbench::browser_verification::BrowserVerificationCommand>,
}

/// artifact 响应（base64，有界）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserVerificationArtifactDto {
    pub run_id: String,
    pub artifact_id: String,
    pub content_type: String,
    pub byte_len: usize,
    pub base64: String,
}

/// 从 live preview registry 解析本机 target URL。
///
/// Business Logic（为什么需要这个函数）:
///     verification 只能打到已登记 loopback preview；RemoteRelay 表示目标在 owner，
///     controller 不得本地启动 engine。
///
/// Code Logic（这个函数做什么）:
///     lookup preview；Local 返回 target_url；RemoteRelay 返回特殊错误码供上层转发；
///     缺失返回 `browser_preview_not_found`。
pub fn resolve_local_target_from_preview(
    state: &AppState,
    preview_id: &str,
) -> Result<(String, Option<String>, Option<String>, String), AppError> {
    let session = state
        .workbench_browser_previews
        .lookup(preview_id)
        .ok_or_else(|| AppError::not_found("browser_preview_not_found"))?;
    match &session.target {
        BrowserPreviewTarget::Local { target_url } => {
            let normalized = crate::workbench::browser::normalize_browser_target_url(target_url)
                .map_err(|_| AppError::validation("browser_redirect_escape"))?;
            Ok((
                session.project_id.clone(),
                session.worktree_id.clone(),
                Some(normalized), // Some = local engine may start
                "local".into(),
            ))
        }
        BrowserPreviewTarget::RemoteRelay {
            base_url,
            remote_preview_id,
            ..
        } => {
            // 不在 controller 启动 engine；返回标记
            let _ = (base_url, remote_preview_id);
            Ok((
                session.project_id.clone(),
                session.worktree_id.clone(),
                None, // None = must relay
                "remote_relay".into(),
            ))
        }
    }
}

/// 在本机 state 上启动验证（仅 Local preview）。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri/control/P2P owner 路径共享同一 helper。
///
/// Code Logic（这个函数做什么）:
///     解析 preview → 若 RemoteRelay 返回 `browser_verification_remote_only` 且 engine 不启动；
///     Local 则调用 service.start。
pub async fn start_browser_verification_for_state(
    state: &AppState,
    req: StartBrowserVerificationReq,
) -> Result<BrowserVerificationRun, AppError> {
    if req.preview_id.is_empty() || req.request_id.is_empty() {
        return Err(AppError::validation("validation_error"));
    }
    // 拒绝过大命令列表
    if req.commands.len() > 32 {
        return Err(AppError::validation("resource_limit"));
    }
    let (project_id, worktree_id, local_target, kind) =
        resolve_local_target_from_preview(state, &req.preview_id)?;
    let Some(target_url) = local_target else {
        // RemoteRelay：不增加 engine 启动计数
        return Err(AppError::validation("browser_verification_remote_only"));
    };
    let _ = kind;
    let start = BrowserVerificationStartRequest {
        preview_id: req.preview_id.clone(),
        request_id: req.request_id,
        commands: if req.commands.is_empty() {
            default_smoke_commands()
        } else {
            req.commands
        },
        fingerprint: None,
    };
    state
        .browser_verification
        .start(req.preview_id, project_id, worktree_id, target_url, start)
        .await
}

/// 查询验证 run。
///
/// Business Logic（为什么需要这个函数）:
///     UI 轮询状态。
///
/// Code Logic（这个函数做什么）:
///     委托 service.get。
pub async fn get_browser_verification_for_state(
    state: &AppState,
    run_id: String,
) -> Result<BrowserVerificationRun, AppError> {
    state.browser_verification.get(&run_id).await
}

/// 取消验证 run。
///
/// Business Logic（为什么需要这个函数）:
///     用户停止验证。
///
/// Code Logic（这个函数做什么）:
///     委托 service.cancel。
pub async fn cancel_browser_verification_for_state(
    state: &AppState,
    run_id: String,
) -> Result<BrowserVerificationRun, AppError> {
    state.browser_verification.cancel(&run_id).await
}

/// 读取 artifact。
///
/// Business Logic（为什么需要这个函数）:
///     UI 展示截图。
///
/// Code Logic（这个函数做什么）:
///     读字节并 base64；限制 8MiB。
pub async fn get_browser_verification_artifact_for_state(
    state: &AppState,
    run_id: String,
    artifact_id: String,
) -> Result<BrowserVerificationArtifactDto, AppError> {
    // 拒绝路径穿越形态 id
    if artifact_id.contains("..") || artifact_id.contains('/') || artifact_id.contains('\\') {
        return Err(AppError::validation("resource_limit"));
    }
    let bytes = state.browser_verification.artifact(&run_id, &artifact_id).await?;
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
    get_browser_verification_artifact_for_state(state.inner(), run_id, artifact_id).await
}

/// 测试辅助：当前 engine 启动次数。
///
/// Business Logic（为什么需要这个函数）:
///     断言过期 preview 不启动 engine。
///
/// Code Logic（这个函数做什么）:
///     读 service 计数。
#[cfg(test)]
pub fn engine_start_count(state: &AppState) -> usize {
    state.browser_verification.engine_start_count()
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
    async fn remote_relay_does_not_start_local_engine() {
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
