//! net/routes/transfer.rs — /api/transfer/{init,chunk,status} handler（供对端 P2P 调用）
//!
//! Business Logic（为什么需要这个模块）:
//!     对端发送文件时调用这三个端点：init 协商元数据与续传 offset；chunk 逐块写入；
//!     status 查询接收进度。对照 Python `protocol.py` 的三个 handler。
//!     字段命名与 Python 逐字一致（transfer_id/accepted/resume_offset/success/received_bytes 等），
//!     保证迁移期 Rust↔Python 互通。
//!
//! Code Logic（这个模块做什么）:
//!     - POST /api/transfer/init：body `{transfer_id, filename, size, sha256, chunk_size}` →
//!       `{transfer_id, accepted, resume_offset}`
//!     - POST /api/transfer/chunk/:id：body=Bytes，header `X-Chunk-Offset` → `{success, received_bytes}`
//!     - GET /api/transfer/status/:id → `{transfer_id, status, progress, transferred_bytes, size, filename}`
//!
//! header X-Chunk-Offset 是关键契约（对照 Python handle_transfer_chunk）。

use crate::error::AppError;
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::transfer::receiver;
use axum::extract::{Extension, Path, State};
use axum::http::HeaderMap;
use axum::Json;

/// POST /api/transfer/init：接收对端传输元数据，创建接收任务并返回断点续传 offset。
pub async fn transfer_init(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(meta): Json<receiver::InitMeta>,
) -> P2pResult<Json<receiver::InitResp>> {
    let resp = receiver::handle_init(&state, meta)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "transfer.init"))?;
    Ok(Json(resp))
}

/// POST /api/transfer/chunk/:id：接收一个数据块，写入临时文件指定 offset。
///
/// Code Logic: 从 `X-Chunk-Offset` header 取 offset（缺省 0），body 为原始 bytes；
///     调 receiver::handle_chunk 写入并在收齐时自动 finalize；事件通过 AppState 后端 UI adapter 发布。
pub async fn transfer_chunk(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> P2pResult<Json<receiver::ChunkResp>> {
    // 解析 X-Chunk-Offset header（缺省 0，对照 Python）
    let offset: u64 = headers
        .get("X-Chunk-Offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let data = body.to_vec();
    let resp = receiver::handle_chunk(&state, &id, offset, data)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "transfer.chunk"))?;
    Ok(Json(resp))
}

/// GET /api/transfer/status/:id：查询某接收任务状态。
///
/// Code Logic: handle_status 不返回错误（始终 200），因此直接构造响应；
///     保留 P2pResult 返回类型以保持 transfer 域信封契约一致。
pub async fn transfer_status(
    State(state): State<AppState>,
    Extension(_ctx): Extension<P2pRequestContext>,
    Path(id): Path<String>,
) -> P2pResult<Json<receiver::StatusResp>> {
    let resp = receiver::handle_status(&state, &id).await;
    Ok(Json(resp))
}
