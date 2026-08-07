//! net/routes/workbench_project_order_sync.rs — 项目列表顺序 LWW 同步
//!
//! Business Logic（为什么需要这个模块）:
//!     用户拖拽的侧栏项目顺序是跨设备共享偏好；项目实体本身不跨设备，因此单独同步
//!     一份 `orderedIds` 文档，冲突整表 LWW。
//!
//! Code Logic（这个模块做什么）:
//!     - pull：返回本端顺序文档（可能为 null）
//!     - push：对端推送文档；remote 按 LWW 胜出时覆盖本地

use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::workbench::project_order::{order_document_wins, ProjectOrderDocument};
use axum::extract::{Extension, State};
use axum::Json;
use serde::{Deserialize, Serialize};

/// pull 响应：本端顺序文档（缺省 null）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOrderPullResp {
    pub order: Option<ProjectOrderDocument>,
}

/// push 请求：对端顺序文档。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOrderPushReq {
    pub order: ProjectOrderDocument,
}

/// push 响应：是否实际覆盖本地。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOrderPushResp {
    pub accepted: bool,
}

/// POST /api/sync/workbench-project-order/pull — 返回本端顺序文档。
pub async fn project_order_pull(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
) -> P2pResult<Json<ProjectOrderPullResp>> {
    let order = state
        .workbench_project_repo
        .get_order()
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench_project_order.pull"))?;
    Ok(Json(ProjectOrderPullResp { order }))
}

/// POST /api/sync/workbench-project-order/push — LWW 覆盖本端顺序文档。
pub async fn project_order_push(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<ProjectOrderPushReq>,
) -> P2pResult<Json<ProjectOrderPushResp>> {
    let accepted = project_order_push_impl(&state, req.order)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench_project_order.push"))?;
    Ok(Json(ProjectOrderPushResp { accepted }))
}

async fn project_order_push_impl(
    state: &AppState,
    remote: ProjectOrderDocument,
) -> Result<bool, crate::error::AppError> {
    let local = state.workbench_project_repo.get_order().await?;
    let accept = match local.as_ref() {
        None => true,
        Some(local_doc) => order_document_wins(local_doc, &remote),
    };
    if accept {
        state.workbench_project_repo.set_order(&remote).await?;
    }
    Ok(accept)
}
