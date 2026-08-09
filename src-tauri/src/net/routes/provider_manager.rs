//! net/routes/provider_manager.rs — Provider Manager HTTP 路由处理器。
//!
//! Business Logic（为什么需要这个模块）:
//!     移动端 `/mobile` 浏览器无法走 Tauri invoke，需要经 HTTP 切换 cc-switch 已配置的
//!     provider。本模块把无状态的 `provider_manager` 业务逻辑（读 cc-switch SQLite + 委托
//!     CLI 写盘）映射到 `/api/provider-manager/*` 路由；桌面端 invoke 路径保持不变。
//!
//! Code Logic（这个模块做什么）:
//!     三个薄 handler：summary（只读快照）、list（各 agent provider 列表）、switch（切换）。
//!     失败统一经 `P2pError::from_app_error` 走错误信封；DTO 复用 `provider_manager::models`
//!     的 camelCase serde，与桌面 IPC 同源，无新 wire 格式。

use axum::extract::Extension;
use axum::Json;
use serde::Deserialize;

use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::provider_manager::{AgentApp, AppProviders, ProviderManagerSummary};

/// `POST /api/provider-manager/switch` 请求体（camelCase，对齐前端）。
///
/// Business Logic: 前端传目标 agent 与 provider id；`app` 复用 `AgentApp` 的 lowercase serde，
///     与桌面 `provider_manager_switch({app, providerId})` 入参同源。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSwitchReq {
    /// 目标 agent（`claude`/`codex`/...，对齐 cc-switch CLI `--app`）。
    pub app: AgentApp,
    /// 要切换到的 provider id。
    pub provider_id: String,
}

/// `GET /api/provider-manager/summary` — Provider Manager 整体状态快照。
///
/// Business Logic: 移动端首屏展示 DB 是否存在、CLI/GUI 检测与各 agent provider 列表。
///     `summary` 内部对 list_apps 失败容错（warn + 空），绝不返回 Err，故无需错误信封。
pub async fn summary() -> Json<ProviderManagerSummary> {
    Json(crate::provider_manager::summary().await)
}

/// `GET /api/provider-manager/list` — 各受支持 agent 的 provider 列表。
pub async fn list(
    Extension(ctx): Extension<P2pRequestContext>,
) -> P2pResult<Json<Vec<AppProviders>>> {
    let apps = crate::provider_manager::list_apps()
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "provider-manager.list"))?;
    Ok(Json(apps))
}

/// `POST /api/provider-manager/switch` — 切换某 agent 的当前 provider（委托 cc-switch CLI 写盘）。
pub async fn switch(
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<ProviderSwitchReq>,
) -> P2pResult<Json<AppProviders>> {
    let updated = crate::provider_manager::switch(req.app, &req.provider_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "provider-manager.switch"))?;
    Ok(Json(updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    /// 构造测试用 request context（固定 request_id）。
    fn ctx() -> P2pRequestContext {
        P2pRequestContext {
            request_id: "req-pm-test".to_string(),
        }
    }

    /// Business Logic: switch 入参必须 camelCase（`providerId`）且 `app` 用 lowercase agent。
    #[test]
    fn switch_req_decodes_camel_case_and_lowercase_app() {
        let json = serde_json::json!({ "app": "claude", "providerId": "p-1" });
        let req: ProviderSwitchReq = serde_json::from_value(json).expect("反序列化应成功");
        assert_eq!(req.app, AgentApp::Claude);
        assert_eq!(req.provider_id, "p-1");
    }

    /// Business Logic: 未知的 app 字符串必须被拒绝（防止误传 `claude-desktop` 等）。
    #[test]
    fn switch_req_rejects_unknown_app() {
        let json = serde_json::json!({ "app": "claude-desktop", "providerId": "p" });
        let result: Result<ProviderSwitchReq, _> = serde_json::from_value(json);
        assert!(result.is_err(), "claude-desktop 不是合法 --app 目标");
    }

    /// Business Logic: 空 provider id 在 CLI 检测前就被 validation 拒绝（不依赖本机 cc-switch），
    ///     handler 必须把它映射成 400 `validation_error` 信封并保留 request_id。
    #[tokio::test]
    async fn switch_rejects_empty_provider_id_with_validation_envelope() {
        let req = ProviderSwitchReq {
            app: AgentApp::Claude,
            provider_id: "   ".to_string(),
        };
        let result = switch(Extension(ctx()), Json(req)).await;
        let err = result.expect_err("空 provider id 应返回 Err");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.envelope().code, "validation_error");
        assert_eq!(err.envelope().request_id, "req-pm-test");
    }
}
