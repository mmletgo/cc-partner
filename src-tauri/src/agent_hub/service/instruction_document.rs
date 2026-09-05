//! agent_hub/service/instruction_document — 指令文档持久化与 DTO 转换
//!
//! Business Logic（为什么需要这个模块）:
//!     Hub 编辑与 legacy / user-v2 push 都必须经 revision CAS + ObjectStore blob
//!     持久化，可崩溃恢复且禁止把指令正文写入日志；块 DTO 与权威文档需可逆转换。
//!
//! Code Logic（这个模块做什么）:
//!     asset 加载/校验（load/require/expected revision CAS）、persist/commit revision、
//!     dual-write legacy 摘要、文档加载视图，以及 InstructionBlockDto 与
//!     InstructionDocument 的双向转换。

use super::dto::InstructionBlockDto;
use crate::agent_hub::instructions::{InstructionBlock, InstructionBlockMode, InstructionDocument};
use crate::agent_hub::models::{
    AgentTarget, AssetKind, LogicalAsset, NewRevision, RevisionId, RevisionOperation,
    RevisionOriginKind,
};
use crate::agent_hub::object_store::ObjectStore;
use crate::error::AppError;
use crate::state::AppState;
use std::collections::BTreeMap;

/// 加载资产，缺失 not_found。
///
/// Business Logic（为什么需要这个函数）:
///     详情/编辑 fail-closed。
///
/// Code Logic（这个函数做什么）:
///     get_asset 展开。
pub(super) async fn load_asset_or_not_found(
    state: &AppState,
    asset_id: &str,
) -> Result<LogicalAsset, AppError> {
    state
        .agent_hub_repo
        .get_asset(asset_id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("agent_hub_asset_not_found:{asset_id}")))
}

/// 要求 Instruction kind。
///
/// Business Logic（为什么需要这个函数）:
///     instruction 编辑不得误伤其它 kind。
///
/// Code Logic（这个函数做什么）:
///     load + kind 校验。
pub(super) async fn require_instruction_asset(
    state: &AppState,
    asset_id: &str,
) -> Result<LogicalAsset, AppError> {
    let asset = load_asset_or_not_found(state, asset_id).await?;
    if asset.kind != AssetKind::Instruction {
        return Err(AppError::validation(format!(
            "agent_hub_asset_not_instruction:{}",
            asset.kind.as_str()
        )));
    }
    Ok(asset)
}

/// CAS expected revision（Ui 路径 fail-closed）。
///
/// Business Logic（为什么需要这个函数）:
///     UI 并发编辑必须携带 expected_revision_id；缺失则拒绝，避免静默覆盖。
///     Migration/Filesystem 等非 Ui 路径不经过本函数。
///
/// Code Logic（这个函数做什么）:
///     expected 为 None/空 → validation `agent_hub_expected_revision_required`；
///     与 current 不等 → conflict `agent_hub_revision_conflict`。
pub(super) fn ensure_expected_revision(
    asset: &LogicalAsset,
    expected: Option<&str>,
) -> Result<(), AppError> {
    let Some(expected) = expected.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(AppError::validation("agent_hub_expected_revision_required"));
    };
    let current = asset
        .current_revision_id
        .as_ref()
        .map(|r| r.as_str())
        .unwrap_or("");
    if current != expected {
        return Err(AppError::conflict("agent_hub_revision_conflict"));
    }
    Ok(())
}

/// 打开 ObjectStore。
///
/// Business Logic（为什么需要这个函数）:
///     指令 payload 存 CAS。
///
/// Code Logic（这个函数做什么）:
///     data_dir + ObjectStore::open。
pub(super) fn object_store() -> Result<ObjectStore, AppError> {
    ObjectStore::open(crate::config::data_dir()?)
}

/// 持久化 instruction 文档为 Ui revision。
///
/// Business Logic（为什么需要这个函数）:
///     Hub 编辑必须形成可崩溃恢复 revision，且不记正文日志。
///
/// Code Logic（这个函数做什么）:
///     JSON serialize → put_blob → append_revision(Ui)。
pub(super) async fn persist_instruction_document(
    state: &AppState,
    asset: &LogicalAsset,
    document: &InstructionDocument,
) -> Result<(), AppError> {
    let bytes = serde_json::to_vec(document)
        .map_err(|e| AppError::generic(format!("agent_hub_serialize_instruction_failed:{e}")))?;
    let store = object_store()?;
    let stored = store.put_blob(&bytes).await?;
    let now = chrono::Utc::now().to_rfc3339();
    let parents = asset
        .current_revision_id
        .clone()
        .into_iter()
        .collect::<Vec<_>>();
    // Ui 写路径强制 head CAS：expected_parent = 调用前观测到的 current head
    let expected_parent_id = asset.current_revision_id.clone();
    state
        .agent_hub_repo
        .append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset.id.clone(),
            parents,
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: state.device_id.as_str().to_string(),
            payload_hash: Some(stored.hash),
            tree_manifest_hash: None,
            created_at: now,
            expected_parent_id,
        })
        .await?;

    // N/N+1 dual-write：仅用户级 CLAUDE.md 指令摘要写回 legacy 表；目标文件由 projector；失败不阻断 Hub。
    // legacy vector_clock 永不裁决 Hub 冲突。
    if let Err(e) = maybe_dual_write_user_claude_md_summary(state, asset, document).await {
        tracing::warn!("agent_hub dual_write legacy claude_md failed: {e}");
    }
    // 生产投影：ensure enabled + 按 binding 入队（best-effort，不阻断 revision commit）。
    if let Err(e) = crate::agent_hub::projection_ops::ensure_agent_hub_enabled(state).await {
        tracing::warn!(error = %e, "agent_hub persist ensure enabled failed");
    }
    if let Err(e) =
        crate::agent_hub::projection_ops::schedule_asset_projections(state, &asset.id).await
    {
        tracing::warn!(
            asset_id = %asset.id,
            error = %e,
            "agent_hub persist schedule projections failed"
        );
    }
    Ok(())
}

/// 仅 put_blob + append_revision 推进 canonical head，不触发投影/dual-write/ensure-enabled。
///
/// Business Logic（为什么需要这个函数）:
///     「保存块文档」是 cc-partner 内部编辑态持久化，必须独立于 CLI 原生文件投影
///     （后者受 support manifest L3 门禁）。此处只推进 revision head，让下次 inspect
///     读到新块；目标文件投影仍由 apply（受门禁）或 scheduler 独立处理。
///
/// Code Logic（这个函数做什么）:
///     JSON serialize → put_blob → append_revision(Ui, expected_parent=current head CAS)。
pub(crate) async fn commit_user_instruction_document(
    state: &AppState,
    asset: &LogicalAsset,
    document: &InstructionDocument,
) -> Result<RevisionId, AppError> {
    let bytes = serde_json::to_vec(document)
        .map_err(|e| AppError::generic(format!("agent_hub_serialize_instruction_failed:{e}")))?;
    let store = object_store()?;
    let stored = store.put_blob(&bytes).await?;
    let now = chrono::Utc::now().to_rfc3339();
    let parents = asset
        .current_revision_id
        .clone()
        .into_iter()
        .collect::<Vec<_>>();
    // 块保存强制 head CAS：expected_parent = 调用前观测到的 current head
    let expected_parent_id = asset.current_revision_id.clone();
    let new_id = RevisionId::new_v7();
    state
        .agent_hub_repo
        .append_revision(NewRevision {
            id: new_id.clone(),
            asset_lineage_id: asset.id.clone(),
            parents,
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: state.device_id.as_str().to_string(),
            payload_hash: Some(stored.hash),
            tree_manifest_hash: None,
            created_at: now,
            expected_parent_id,
        })
        .await?;
    Ok(new_id)
}

/// 用户级 CLAUDE.md 资产成功写 revision 后 dual-write legacy 摘要 + 文件。
///
/// Business Logic（为什么需要这个函数）:
///     旧 CLAUDE.md 页/P2P 仍读 `claude_md` 表；Hub 写用户指令后需同步摘要与
///     `~/.claude/CLAUDE.md`，且不得让 legacy VC 参与 Hub merge。
///
/// Code Logic（这个函数做什么）:
///     scope=User + Instruction + logical_key=CLAUDE.md → Claude 摘要 → dual_write **仅** legacy 表；
///     不写 `~/.claude/CLAUDE.md`（projector + binding/support 门闸负责目标文件）。
async fn maybe_dual_write_user_claude_md_summary(
    state: &AppState,
    asset: &LogicalAsset,
    document: &InstructionDocument,
) -> Result<(), AppError> {
    if asset.kind != AssetKind::Instruction {
        return Ok(());
    }
    if asset.logical_key != crate::agent_hub::migration::USER_INSTRUCTION_LOGICAL_KEY {
        return Ok(());
    }
    let Some(scope) = state.agent_hub_repo.get_scope(&asset.scope_id).await? else {
        return Ok(());
    };
    if scope.kind != crate::agent_hub::models::ScopeKind::User {
        return Ok(());
    }
    let summary = crate::agent_hub::migration::claude_summary_markdown_from_document(document);
    // 仅 legacy 摘要表；目标文件由 schedule_asset_projections → projector 经 binding/support 写入。
    crate::agent_hub::migration::dual_write_legacy_claude_md_summary(state, &summary).await?;
    Ok(())
}

/// 加载 instruction 视图（markdown 摘要 + blocks）。
///
/// Business Logic（为什么需要这个函数）:
///     详情页展示 contentMarkdown 与块结构。
///
/// Code Logic（这个函数做什么）:
///     读 blob；JSON 文档优先，否则 UTF-8 markdown → from_shared。
pub(super) async fn load_instruction_view(
    state: &AppState,
    asset: &LogicalAsset,
) -> Result<(Option<String>, Vec<InstructionBlockDto>), AppError> {
    if asset.kind != AssetKind::Instruction {
        return Ok((None, Vec::new()));
    }
    let (doc, note) = load_instruction_document(state, asset).await?;
    let _ = note;
    let content = Some(doc.joined_shared_body());
    let blocks = doc.blocks.iter().map(block_to_dto).collect();
    Ok((content, blocks))
}

/// 加载 instruction 文档。
///
/// Business Logic（为什么需要这个函数）:
///     编辑/详情共用 payload 解析。
///
/// Code Logic（这个函数做什么）:
///     current revision → get_blob → JSON 或 markdown。
pub(super) async fn load_instruction_document(
    state: &AppState,
    asset: &LogicalAsset,
) -> Result<(InstructionDocument, Option<String>), AppError> {
    let Some(rev_id) = asset.current_revision_id.as_ref() else {
        return Ok((
            InstructionDocument {
                relative_key: asset.logical_key.clone(),
                blocks: vec![],
            },
            Some("no current revision".to_string()),
        ));
    };
    let revision = state
        .agent_hub_repo
        .get_revision(rev_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found(format!("agent_hub_revision_not_found:{}", rev_id.as_str()))
        })?;
    let Some(hash) = revision.payload_hash.as_ref() else {
        return Ok((
            InstructionDocument {
                relative_key: asset.logical_key.clone(),
                blocks: vec![],
            },
            Some("revision has no payload".to_string()),
        ));
    };
    let store = object_store()?;
    let bytes = store.get_blob(hash).await?;
    // 禁止记录指令内容
    if let Ok(doc) = serde_json::from_slice::<InstructionDocument>(&bytes) {
        return Ok((doc, None));
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok((
        InstructionDocument::from_shared_markdown(asset.logical_key.clone(), text),
        Some("payload treated as shared markdown".to_string()),
    ))
}

/// Legacy push 专用：加载 instruction 文档（不记录正文）。
///
/// Business Logic: hub-on legacy translator 只更新 Claude targetOnly，需读现有块结构。
/// Code Logic: 委托 load_instruction_document。
pub(crate) async fn load_instruction_document_for_legacy(
    asset: &LogicalAsset,
    state: &AppState,
) -> Result<(InstructionDocument, Option<String>), AppError> {
    load_instruction_document(state, asset).await
}

/// User Instruction V2 专用：加载 canonical 块文档且不记录正文。
///
/// Business Logic: inventory/preview 必须复用与 legacy/service 相同的 CAS payload 解析。
/// Code Logic: 仅委托内部 load_instruction_document。
pub(crate) async fn load_instruction_document_for_user_v2(
    asset: &LogicalAsset,
    state: &AppState,
) -> Result<(InstructionDocument, Option<String>), AppError> {
    load_instruction_document(state, asset).await
}

/// Legacy push 专用：在 CAS 下持久化 instruction 文档并调度投影。
///
/// Business Logic: 不得绕过 revision CAS / projection；目标文件仅 projector 写入。
/// Code Logic: ensure_expected_revision → persist_instruction_document。
pub(crate) async fn persist_instruction_document_for_legacy(
    state: &AppState,
    asset: &LogicalAsset,
    document: &InstructionDocument,
    expected_revision_id: Option<&str>,
) -> Result<(), AppError> {
    ensure_expected_revision(asset, expected_revision_id)?;
    persist_instruction_document(state, asset, document).await
}

/// DTO → 块（block_to_dto 的逆映射）。
///
/// Business Logic（为什么需要这个函数）:
///     「保存块文档」命令需把前端编辑的 blocks round-trip 回权威 InstructionDocument，
///     保留 id/mode/common/variants/heading_path/source_target/needs_adaptation。
///
/// Code Logic（这个函数做什么）:
///     校验 mode 与 variant target 合法；structured_intent 不可逆，置 None。
pub(crate) fn block_from_dto(dto: &InstructionBlockDto) -> Result<InstructionBlock, AppError> {
    let mode = parse_block_mode(&dto.mode)?;
    let mut variants = BTreeMap::new();
    if let Some(map) = &dto.variants {
        for (key, value) in map {
            let target = AgentTarget::parse(key)
                .ok_or_else(|| AppError::validation(format!("非法 variant target: {key}")))?;
            variants.insert(target, value.clone());
        }
    }
    let common_markdown = if dto.common_markdown.is_empty() {
        None
    } else {
        Some(dto.common_markdown.clone())
    };
    Ok(InstructionBlock {
        id: dto.id.clone(),
        mode,
        common_markdown,
        structured_intent: None,
        variants,
        heading_path: dto.heading_path.clone().unwrap_or_default(),
        source_target: dto.source_target,
        needs_adaptation: dto.needs_adaptation,
    })
}

/// 块 DTO 列表 → InstructionDocument。
///
/// Business Logic: 「保存块文档」把整份块模型序列化为权威 canonical 文档，并归并为固定三槽。
/// Code Logic: 逐块 reverse；空 id 补 UUIDv7；normalize_to_three_slots；relative_key 留空。
pub(crate) fn instruction_document_from_block_dtos(
    dtos: &[InstructionBlockDto],
) -> Result<InstructionDocument, AppError> {
    let mut blocks = Vec::with_capacity(dtos.len());
    for dto in dtos {
        let mut block = block_from_dto(dto)?;
        if block.id.trim().is_empty() {
            block.id = crate::agent_hub::instructions::new_block_id();
        }
        blocks.push(block);
    }
    let mut document = InstructionDocument {
        relative_key: String::new(),
        blocks,
    };
    document.normalize_to_three_slots();
    Ok(document)
}

/// 块 → DTO。
///
/// Business Logic（为什么需要这个函数）:
///     UI 需要 mode/common/variants；用户级指令 V2 inspect 也要把 canonical 块暴露给三栏。
///
/// Code Logic（这个函数做什么）:
///     common 空串兜底；variants 转 string key map。
pub(crate) fn block_to_dto(block: &InstructionBlock) -> InstructionBlockDto {
    let variants = if block.variants.is_empty() {
        None
    } else {
        Some(
            block
                .variants
                .iter()
                .map(|(t, v)| (t.as_str().to_string(), v.clone()))
                .collect(),
        )
    };
    InstructionBlockDto {
        id: block.id.clone(),
        mode: block.mode.as_str().to_string(),
        common_markdown: block.common_markdown.clone().unwrap_or_default(),
        variants,
        heading_path: if block.heading_path.is_empty() {
            None
        } else {
            Some(block.heading_path.clone())
        },
        source_target: block.source_target,
        needs_adaptation: block.needs_adaptation,
    }
}

/// 解析 block mode token。
///
/// Business Logic（为什么需要这个函数）:
///     IPC 传入 shared/adapted/targetOnly。
///
/// Code Logic（这个函数做什么）:
///     仅匹配合法 token。
pub(crate) fn parse_block_mode(raw: &str) -> Result<InstructionBlockMode, AppError> {
    match raw {
        "shared" => Ok(InstructionBlockMode::Shared),
        "adapted" => Ok(InstructionBlockMode::Adapted),
        "targetOnly" => Ok(InstructionBlockMode::TargetOnly),
        other => Err(AppError::validation(format!("非法 block mode: {other}"))),
    }
}
