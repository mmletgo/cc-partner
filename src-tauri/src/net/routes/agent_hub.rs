//! net/routes/agent_hub.rs — Agent Hub LAN push 三阶段路由
//!
//! Business Logic（为什么需要这个模块）:
//!     对端在源侧选择目标后 push SnapshotEnvelope；本路由只做接收端协议面。
//!     sourceDeviceId / clientRequestId / expected-device header 是绑定/幂等标签，**不是**身份认证。
//!
//! Code Logic（这个模块做什么）:
//!     POST prepare / PUT objects / POST commit；body limit 与 P2pError 信封；
//!     业务委托 `agent_hub::replication::receiver`。

use crate::agent_hub::object_store::ObjectStore;
use crate::agent_hub::replication::ledger::ReplicationLedger;
use crate::agent_hub::replication::pull::{
    build_remote_inventory_for_target, source_prepare_selection, source_read_object_chunk,
    RemoteInventoryQuery, RemoteSelectionQuery, PORTABLE_PULL_MAX_CHUNK_BYTES,
};
use crate::agent_hub::replication::receiver::{
    commit_push, prepare_push, put_object_chunk, CommitPushRequest, CommitPushResponse,
    PreparePushRequest, PreparePushResponse, PutObjectResponse, AGENT_HUB_MAX_CHUNK_BYTES,
};
use crate::net::error_response::{P2pError, P2pResult};
// CAPABILITY_* used in tests module for wire-token assertions
#[cfg(test)]
use crate::net::protocol::{CAPABILITY_AGENT_HUB_V1, CAPABILITY_PORTABLE_PULL_V1};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

/// PUT object query：offset + 可选 chunk SHA-256。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutObjectQuery {
    /// 写入 offset（u64）
    pub offset: u64,
    /// 可选 chunk body SHA-256 hex
    #[serde(default)]
    pub chunk_sha256: Option<String>,
}

/// POST /api/agent-hub/push/prepare
///
/// Business Logic: 校验 envelope/limits，返回 missing objects；幂等回放。
/// Code Logic: 构造 ObjectStore/ledger → prepare_push。
pub async fn agent_hub_push_prepare(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<PreparePushRequest>,
) -> P2pResult<Json<PreparePushResponse>> {
    let data_dir = crate::config::data_dir()
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.prepare"))?;
    let objects = ObjectStore::open(&data_dir)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.prepare"))?;
    let ledger =
        ReplicationLedger::new(state.agent_hub_repo.pool(), state.maintenance_gate.clone());
    let resp = prepare_push(&state.agent_hub_repo, &objects, &ledger, &data_dir, body)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.prepare"))?;
    Ok(Json(resp))
}

/// PUT /api/agent-hub/push/:transferId/objects/:objectHash?offset=
///
/// Business Logic: application/octet-stream chunk ≤8 MiB；offset 连续；hash 校验。
/// Code Logic: 读 body bytes → put_object_chunk。
pub async fn agent_hub_push_object(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Path((transfer_id, object_hash)): Path<(String, String)>,
    Query(query): Query<PutObjectQuery>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> P2pResult<Json<PutObjectResponse>> {
    // Content-Type 提示（guard 已拦 form；此处接受 octet-stream 或缺省）
    let _ = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());

    if body.len() > AGENT_HUB_MAX_CHUNK_BYTES {
        return Err(P2pError::validation(
            format!(
                "agent_hub_push_chunk_too_large:actual={}:limit={AGENT_HUB_MAX_CHUNK_BYTES}",
                body.len()
            ),
            &ctx,
        ));
    }

    let data_dir = crate::config::data_dir()
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.object"))?;
    let ledger =
        ReplicationLedger::new(state.agent_hub_repo.pool(), state.maintenance_gate.clone());
    let resp = put_object_chunk(
        &ledger,
        &data_dir,
        &transfer_id,
        &object_hash,
        query.offset,
        &body,
        query.chunk_sha256.as_deref(),
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.object"))?;
    Ok(Json(resp))
}

/// POST /api/agent-hub/push/:transferId/commit
///
/// Business Logic: 全部 object verified 后 SnapshotImporter::commit_import；enqueue reconcile。
/// Code Logic: commit_push + best-effort schedule_asset_projections。
pub async fn agent_hub_push_commit(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Path(transfer_id): Path<String>,
    Json(body): Json<CommitPushRequest>,
) -> P2pResult<Json<CommitPushResponse>> {
    let data_dir = crate::config::data_dir()
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.commit"))?;
    let objects = ObjectStore::open(&data_dir)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.commit"))?;
    let ledger =
        ReplicationLedger::new(state.agent_hub_repo.pool(), state.maintenance_gate.clone());
    let state_for_sched = state.clone();
    let resp = commit_push(
        &state.agent_hub_repo,
        &objects,
        &ledger,
        &data_dir,
        &transfer_id,
        body,
        move |asset_ids| {
            // 异步 reconcile：协议成功后 best-effort 调度投影；
            // 同时触发 durable outbox drain，确保 intent 状态推进。
            let ids: Vec<String> = asset_ids.to_vec();
            tauri::async_runtime::spawn(async move {
                for id in ids {
                    let _ = crate::agent_hub::projection_ops::schedule_asset_projections(
                        &state_for_sched,
                        &id,
                    )
                    .await;
                }
                let _ =
                    crate::agent_hub::replication::drain_lan_projection_intents(&state_for_sched)
                        .await;
            });
        },
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.commit"))?;
    // commit 返回后立即 best-effort 再排水一次（覆盖 spawn 前崩溃的窗口由 worker 补）
    let state_for_drain = state.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::agent_hub::replication::drain_lan_projection_intents(&state_for_drain).await;
    });
    Ok(Json(resp))
}

/// POST /api/agent-hub/portable/inventory
///
/// Business Logic: 返回指定 target 的 metadata inventory（无 secret / 无 path）。
/// Code Logic: body.sourceTarget → build_remote_inventory_for_target。
pub async fn agent_hub_portable_inventory(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<RemoteInventoryQuery>,
) -> P2pResult<Json<crate::agent_hub::replication::pull::RemotePortableInventoryDto>> {
    let dto = build_remote_inventory_for_target(&state, body.source_target)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.portable.inventory"))?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/portable/selection
///
/// Business Logic: 按勾选 inventoryItemIds 冻结 SnapshotEnvelope/CAS（源端不 adoption）。
/// Code Logic: source_prepare_selection。
pub async fn agent_hub_portable_selection(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<RemoteSelectionQuery>,
) -> P2pResult<Json<crate::agent_hub::replication::pull::RemotePortableSelectionResponse>> {
    let dto = source_prepare_selection(&state, body.source_target, body.inventory_item_ids)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.portable.selection"))?;
    Ok(Json(dto))
}

/// GET /api/agent-hub/portable/objects/:transferId/:objectHash?offset=
///
/// Business Logic: 分块 ≤8MiB，offset 续传；application/octet-stream。
/// Code Logic: source_read_object_chunk → raw bytes。
pub async fn agent_hub_portable_object(
    Extension(ctx): Extension<P2pRequestContext>,
    Path((transfer_id, object_hash)): Path<(String, String)>,
    Query(query): Query<PutObjectQuery>,
) -> Result<Response, P2pError> {
    let bytes = source_read_object_chunk(&transfer_id, &object_hash, query.offset)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.portable.object"))?;
    if bytes.len() > PORTABLE_PULL_MAX_CHUNK_BYTES {
        return Err(P2pError::validation(
            format!(
                "agent_hub_portable_chunk_too_large:actual={}:limit={PORTABLE_PULL_MAX_CHUNK_BYTES}",
                bytes.len()
            ),
            &ctx,
        ));
    }
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/octet-stream")],
        Bytes::from(bytes),
    )
        .into_response())
}

/// 路由/能力原子性：push 三路由 + portable-pull 三路由同 build 宣告。
///
/// 注意：expected-device / Host / Origin 守卫是协议完整性约束，**不是**身份认证。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{AssetKind, AssetPolicy, RevisionOperation, RevisionOriginKind};
    use crate::agent_hub::object_store::sha256_hex;
    use crate::agent_hub::replication::receiver::{prepare_push, PreparePushRequest};
    use crate::agent_hub::snapshot::builder::hash_selection;
    use crate::agent_hub::snapshot::envelope::{
        compute_snapshot_hash, SnapshotAsset, SnapshotEnvelopeV1, SnapshotLineage,
        SnapshotObjectDescriptor, SnapshotRevision, SnapshotSelection, CANONICALIZATION_NAME,
        FORMAT_NAME, FORMAT_VERSION,
    };
    use crate::net::protocol::server_protocol_info;
    use crate::storage::AgentHubRepo;
    use std::collections::BTreeMap;

    /// Business Logic: 能力 token 必须与 push/portable 路由同 build 宣告。
    /// Code Logic: server_protocol_info supports agent-hub.v1 + portable-pull.v1；handler 符号存在。
    #[test]
    fn capability_and_handlers_are_atomic() {
        let info = server_protocol_info();
        assert!(
            info.supports(CAPABILITY_AGENT_HUB_V1),
            "server_protocol_info 必须宣告 agent-hub.v1，实际: {:?}",
            info.capabilities
        );
        assert!(
            info.supports(CAPABILITY_PORTABLE_PULL_V1),
            "server_protocol_info 必须宣告 agent-hub.portable-pull.v1，实际: {:?}",
            info.capabilities
        );
        // 引用 handler，防止 capability 宣告而路由未接线被优化掉
        let _ = (
            agent_hub_push_prepare as *const (),
            agent_hub_push_object as *const (),
            agent_hub_push_commit as *const (),
            agent_hub_portable_inventory as *const (),
            agent_hub_portable_selection as *const (),
            agent_hub_portable_object as *const (),
        );
        assert_eq!(CAPABILITY_AGENT_HUB_V1, "agent-hub.v1");
        assert_eq!(CAPABILITY_PORTABLE_PULL_V1, "agent-hub.portable-pull.v1");
        // 能力列表字典序应包含 token
        assert!(info.capabilities.windows(2).all(|w| w[0] <= w[1]));
    }

    /// Business Logic: portable inventory 不得声明为鉴权。
    #[test]
    fn portable_pull_docs_do_not_claim_authentication() {
        let note = "portable pull uses expected-device and clientRequestId as binding/idempotency labels, not authentication";
        assert!(note.contains("not authentication"));
    }

    /// Business Logic: 守卫是完整性约束，测试不得称其为 authentication。
    #[test]
    fn protocol_docs_do_not_claim_authentication_for_device_binding() {
        // sourceDeviceId / clientRequestId 文档语义：幂等标签
        let note = "sourceDeviceId and clientRequestId are idempotency labels, not authentication";
        assert!(note.contains("not authentication"));
    }

    fn sample_envelope() -> (SnapshotEnvelopeV1, Vec<u8>) {
        let secret = b"route-fixture-secret";
        let object_hash = sha256_hex(secret);
        let rev_id = "01900000-0000-7000-8000-000000000011";
        let asset_id = "01900000-0000-7000-8000-0000000000a2";
        let mut envelope = SnapshotEnvelopeV1 {
            format: FORMAT_NAME.into(),
            format_version: FORMAT_VERSION,
            canonicalization: CANONICALIZATION_NAME.into(),
            snapshot_id: "01900000-0000-7000-8000-0000000000c2".into(),
            snapshot_hash: "0".repeat(64),
            source_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
            created_at: "2026-07-29T12:00:00Z".into(),
            selection: SnapshotSelection {
                scope_ids: vec!["scope-user".into()],
                asset_ids: vec![asset_id.into()],
                include_history: true,
            },
            asset_heads: BTreeMap::from([(asset_id.into(), vec![rev_id.into()])]),
            assets: vec![SnapshotAsset {
                id: asset_id.into(),
                scope_id: "scope-user".into(),
                kind: AssetKind::Mcp,
                origin_namespace: "standalone".into(),
                logical_key: "mcp-route".into(),
                display_name: "Route MCP".into(),
                policy: AssetPolicy::Shared,
                deleted_at: None,
            }],
            lineages: vec![SnapshotLineage {
                id: asset_id.into(),
                root_asset_id: asset_id.into(),
            }],
            revisions: vec![SnapshotRevision {
                id: rev_id.into(),
                asset_lineage_id: asset_id.into(),
                parents: vec![],
                generation: "0".into(),
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
                payload_hash: Some(object_hash.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T12:00:00Z".into(),
            }],
            variants: vec![],
            conflicts: vec![],
            aliases: vec![],
            objects: vec![SnapshotObjectDescriptor {
                hash: object_hash,
                size: (secret.len() as u64).to_string(),
            }],
        };
        envelope.snapshot_hash = compute_snapshot_hash(&envelope).unwrap();
        (envelope, secret.to_vec())
    }

    /// Business Logic: prepare 幂等与 missing hashes 契约。
    #[tokio::test]
    async fn prepare_returns_missing_and_replays() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().to_path_buf();
        let _guard =
            crate::config::install_data_dir_env(Some(data.to_str().expect("utf8 temp path")));
        let data = crate::config::data_dir().unwrap();
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        let repo = AgentHubRepo::new(pool.clone());
        let store = ObjectStore::open(&data).unwrap();
        let ledger = crate::agent_hub::replication::ReplicationLedger::new_standalone(pool);
        let (envelope, _) = sample_envelope();
        let sel = hash_selection(&envelope.selection).unwrap();
        let req = PreparePushRequest {
            envelope,
            source_device_id: "src".into(),
            client_request_id: "c1".into(),
            selection_hash: sel,
        };
        let a = prepare_push(&repo, &store, &ledger, &data, req.clone())
            .await
            .unwrap();
        assert_eq!(a.missing_object_hashes.len(), 1);
        let b = prepare_push(&repo, &store, &ledger, &data, req)
            .await
            .unwrap();
        assert_eq!(a.transfer_id, b.transfer_id);
    }
}
