//! net/routes/claude_md_sync.rs — /api/sync/claude_md/{pull,push} handler（供对端 P2P 推送 user 级 CLAUDE.md）
//!
//! Business Logic（为什么需要这个模块）:
//!     user 级 CLAUDE.md（~/.claude/CLAUDE.md）只在用户主动点击推送时传播。push 让触发设备
//!     把自己的 CLAUDE.md 推过来，本端必须覆盖为发送方版本；pull 仅保留兼容旧同步协议。
//!     Gate D Task 7：N/N+1 保留本路由；legacy 摘要 dual-write 不裁决 Hub 冲突；
//!     关闭 Hub 时不得清理 CAS / 未知 Hub 表；实际删除受 N+2 门闩约束。
//!
//! Code Logic（这个模块做什么）:
//!     - POST /api/sync/claude_md/pull：body `{vector_clock: {...}}`，比对后若本端领先/并发
//!       则返回本端 CLAUDE.md 完整 ClaudeMdRow，否则 None。
//!     - POST /api/sync/claude_md/push：body `{claude_md: ClaudeMdRow}`，覆盖落库 + 写文件，
//!       返回 `{accepted: bool}`。
//!     字段 snake_case（ClaudeMdRow 默认序列化），与 sync.rs 的 prompts 同步路由一致。

use crate::error::AppError;
use crate::models::claude_md::ClaudeMdRow;
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::sync::vector_clock::{compare, ClockOrder};
use axum::extract::{Extension, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// claude_md/pull 请求体：对端发来的本端向量时钟（本端据此判断是否需要回传）。
#[derive(Debug, Deserialize)]
pub struct ClaudeMdPullReq {
    /// 调用方（对端）当前的 CLAUDE.md 向量时钟；缺省视作空时钟。
    #[serde(default)]
    pub vector_clock: HashMap<String, u64>,
}

/// claude_md/pull 响应体：本端需要下发给对端的 CLAUDE.md（None 表示本端无或无更新）。
#[derive(Debug, Serialize)]
pub struct ClaudeMdPullResp {
    /// 本端领先/并发时为 Some(local_row)，否则 None。
    pub claude_md: Option<ClaudeMdRow>,
}

/// claude_md/push 请求体：对端推送来的 CLAUDE.md 完整行。
#[derive(Debug, Deserialize)]
pub struct ClaudeMdPushReq {
    pub claude_md: ClaudeMdRow,
}

/// claude_md/push 响应体：是否实际接受落库（true=发送方版本与本地有差异并已写入）。
#[derive(Debug, Serialize)]
pub struct ClaudeMdPushResp {
    pub accepted: bool,
}

/// POST /api/sync/claude_md/pull：接收对端向量时钟，若本端领先/并发则回传本端 CLAUDE.md。
///
/// Business Logic: 对端把它的向量时钟发来，本端比对后决定是否下发本端版本。本端 None
///     （无记录）或本端不领先（Before/Equal）时不下发；本端 After/Concurrent 时下发。
///     与 sync::sync_pull 的语义一致，只是 CLAUDE.md 单例退化为 0/1 条。
///
/// Code Logic:
///     1. 读本端 claude_md 单例，None → 响应 claude_md:None；
///     2. compare(local.vc, remote.vc) 返回 local 相对 remote 关系，
///        After/Concurrent（本端领先/并发）→ 回 Some(local)，否则 None。
pub async fn claude_md_pull(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<ClaudeMdPullReq>,
) -> P2pResult<Json<ClaudeMdPullResp>> {
    let claude_md = claude_md_pull_impl(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "claude_md.pull"))?;
    Ok(Json(ClaudeMdPullResp { claude_md }))
}

/// claude_md_pull 业务实现：本端领先/并发时返回本端 CLAUDE.md。
async fn claude_md_pull_impl(
    state: &AppState,
    req: ClaudeMdPullReq,
) -> Result<Option<ClaudeMdRow>, AppError> {
    let local = state.claude_md_repo.get().await?;
    let claude_md = match local {
        None => None,
        Some(local_row) => {
            // compare(local, remote)：After=本端领先，Concurrent=并发 → 需下发
            let relation = compare(&local_row.vector_clock, &req.vector_clock);
            if matches!(relation, ClockOrder::After | ClockOrder::Concurrent) {
                Some(local_row)
            } else {
                None
            }
        }
    };
    Ok(claude_md)
}

/// POST /api/sync/claude_md/push：接收对端推送的 CLAUDE.md，覆盖落库 + 写文件。
///
/// Business Logic: CLAUDE.md 的用户主动推送语义是"接收端变成触发设备这份配置"，
///     因此不能按双向同步 merge，也不能让接收端本地版本因时间戳/向量时钟获胜。
///
/// Code Logic:
///     1. 读取本地行用于判断是否有差异；
///     2. 无论本地是否存在，都 upsert 发送方 row + write_file_if_changed；
///     3. accepted 表示本地同步字段是否发生变化。
pub async fn claude_md_push(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<ClaudeMdPushReq>,
) -> P2pResult<Json<ClaudeMdPushResp>> {
    let accepted = claude_md_push_impl(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "claude_md.push"))?;
    Ok(Json(ClaudeMdPushResp { accepted }))
}

/// claude_md_push 业务实现：覆盖落库 + 写文件，返回是否实际发生变化。
///
/// Business Logic: legacy 摘要路径；不裁决 Hub 冲突、不 GC CAS。
/// Code Logic: upsert + write_file_if_changed；忽略未知 Hub 表。
async fn claude_md_push_impl(state: &AppState, req: ClaudeMdPushReq) -> Result<bool, AppError> {
    // Hub on：禁止直接写旧表/目标文件；必须翻译为 canonical mutation（user instruction）。
    // Hub off：保留 legacy 直接写路径（N/N+1 兼容）。
    let hub_enabled = state
        .config
        .read()
        .map(|c| c.agent_hub.enabled)
        .unwrap_or(false);
    let policy = crate::agent_hub::migration::legacy_facade_policy(hub_enabled);
    if !policy.allow_direct_target_mutation {
        return claude_md_push_via_hub(state, &req).await;
    }

    let local = state.claude_md_repo.get().await?;
    // 用 `Option::map_or` 而非 `Option::is_none_or`（后者 1.82 才 stable），
    // 项目 MSRV 是 1.77.2，clippy 的 `-D warnings` 会阻断。
    let accepted = local.as_ref().map_or(true, |local_row| {
        local_row.content != req.claude_md.content
            || local_row.vector_clock != req.claude_md.vector_clock
            || local_row.updated_at != req.claude_md.updated_at
            || local_row.device_id != req.claude_md.device_id
    });
    state.claude_md_repo.upsert(&req.claude_md).await?;
    crate::sync::claude_md::write_file_if_changed(&req.claude_md.content).await?;
    Ok(accepted)
}

/// Hub 启用时把 legacy CLAUDE.md DTO 翻译为 canonical user instruction mutation。
///
/// Business Logic: Hub 是唯一事实源；N-1 peer 不得绕过 revision/CAS 直接改 CLI 文件。
///     已有 Hub head 时禁止再跑会 reclassify Shared/Adapted 的 migration seed。
/// Code Logic: 仅 asset/head 缺失时 CAS seed；否则基于 expected head 只替换 Claude-owned 块集合。
async fn claude_md_push_via_hub(state: &AppState, req: &ClaudeMdPushReq) -> Result<bool, AppError> {
    use crate::agent_hub::migration::{USER_INSTRUCTION_LOGICAL_KEY, USER_INSTRUCTION_NAMESPACE};
    use crate::agent_hub::models::AssetKind;
    use crate::agent_hub::object_store::ObjectStore;

    let data_dir = crate::config::data_dir()?;
    let claude_path = crate::agent_hub::migration::user_claude_md_file_path()
        .unwrap_or_else(|_| data_dir.join("legacy-claude-md-missing"));
    let _ = ObjectStore::open(&data_dir)?;

    // Codex R4: 单事务 NULL-head CAS seed；CAS miss 时不 reclassify，仅 targetOnly 带 expected parent。
    // 禁止事务外 need_seed 判断后 race 覆盖并发 import 建立的 Shared/Adapted head。
    let seed_outcome = crate::agent_hub::migration::seed_user_instruction_if_head_null(
        state,
        &claude_path,
        &data_dir,
    )
    .await
    .map_err(|e| AppError::generic(format!("agent_hub_legacy_claude_md_seed_failed:{e}")))?;

    let scope_id = state
        .agent_hub_repo
        .resolve_user_scope_id()
        .await?
        .ok_or_else(|| {
            AppError::validation("agent_hub_legacy_claude_md_user_scope_missing".to_string())
        })?;
    let asset = state
        .agent_hub_repo
        .get_asset_by_unique_key(
            &scope_id,
            AssetKind::Instruction,
            USER_INSTRUCTION_NAMESPACE,
            USER_INSTRUCTION_LOGICAL_KEY,
        )
        .await?
        .ok_or_else(|| {
            AppError::validation("agent_hub_legacy_claude_md_user_asset_missing".to_string())
        })?;
    // seed 后或 CAS miss 后必须 reload head；禁止用陈旧 None expected 覆盖
    let before = asset.current_revision_id.clone();
    let _ = seed_outcome;

    // 专用幂等 translator：只替换 Claude-owned targetOnly 块集合，保留 Shared/Adapted/其他 target。
    // 多 Claude 块时收敛为单块，避免重复 push 把完整 incoming 写进每个块。
    let (mut doc, _) =
        crate::agent_hub::service::load_instruction_document_for_legacy(&asset, state).await?;
    let incoming = req.claude_md.content.as_str();
    let current_summary = crate::agent_hub::migration::claude_summary_markdown_from_document(&doc);
    if current_summary == incoming {
        // 内容未变：no-op（不推进 revision、不写文件）
        return Ok(false);
    }

    apply_claude_owned_blocks_from_legacy_incoming(&mut doc, incoming);

    let expected = before
        .as_ref()
        .map(|r| r.as_str().to_string())
        .ok_or_else(|| {
            AppError::validation("agent_hub_legacy_claude_md_asset_head_missing".to_string())
        })?;
    crate::agent_hub::service::persist_instruction_document_for_legacy(
        state,
        &asset,
        &doc,
        Some(expected.as_str()),
    )
    .await
    .map_err(|e| {
        AppError::conflict(format!(
            "agent_hub_legacy_claude_md_hub_mutation_failed:{e}"
        ))
    })?;

    // Hub-on：仅 dual-write legacy 摘要表；目标文件由 projector 经 binding/support 写入。
    let summary = crate::agent_hub::migration::claude_summary_markdown_from_document(&doc);
    let _ = crate::agent_hub::migration::dual_write_legacy_claude_md_summary(state, &summary).await;

    let after = state
        .agent_hub_repo
        .get_asset(&asset.id)
        .await?
        .and_then(|a| a.current_revision_id);
    Ok(before != after)
}

/// 用 legacy incoming 替换文档中 Claude-owned targetOnly 块集合。
///
/// Business Logic（为什么需要这个函数）:
///     Hub-on legacy push 只能覆盖 Claude targetOnly 内容；Shared/Adapted/其他 target 必须保留。
///     多 Claude 块时不得把同一正文重复写入每个块，否则多块文档会膨胀且丢失幂等性。
///
/// Code Logic（这个函数做什么）:
///     1) 删除所有含 Claude variant 的 targetOnly 块；
///     2) 追加单个 Claude targetOnly 块承载完整 incoming；
///     3) Shared/Adapted 与非 Claude targetOnly 原样保留。
fn apply_claude_owned_blocks_from_legacy_incoming(
    doc: &mut crate::agent_hub::instructions::document::InstructionDocument,
    incoming: &str,
) {
    use crate::agent_hub::instructions::document::{InstructionBlock, InstructionBlockMode};
    use crate::agent_hub::models::AgentTarget;

    doc.blocks.retain(|block| {
        if block.mode != InstructionBlockMode::TargetOnly {
            return true;
        }
        // 含 Claude variant 或 source=Claude 的 targetOnly 由本 push 全权替换
        if block.variants.contains_key(&AgentTarget::Claude)
            || block.source_target == Some(AgentTarget::Claude)
        {
            return false;
        }
        true
    });
    doc.blocks.push(InstructionBlock::target_only(
        crate::agent_hub::instructions::document::new_block_id(),
        AgentTarget::Claude,
        incoming.to_string(),
        vec![],
        false,
    ));
}

#[cfg(test)]
mod tests {
    use super::apply_claude_owned_blocks_from_legacy_incoming;
    use crate::agent_hub::instructions::document::{
        InstructionBlock, InstructionBlockMode, InstructionDocument,
    };
    use crate::agent_hub::models::AgentTarget;

    /// Business Logic: multi Claude targetOnly 块不得被完整 incoming 重复写爆。
    /// Code Logic: 替换为单 Claude 块，保留 Shared。
    #[test]
    fn legacy_push_replaces_claude_blocks_idempotently() {
        let mut doc = InstructionDocument {
            relative_key: String::new(),
            blocks: vec![
                InstructionBlock {
                    id: "shared".into(),
                    mode: InstructionBlockMode::Shared,
                    common_markdown: Some("shared body".into()),
                    structured_intent: None,
                    variants: Default::default(),
                    heading_path: vec![],
                    source_target: None,
                    needs_adaptation: false,
                },
                InstructionBlock::target_only("c1", AgentTarget::Claude, "old1", vec![], false),
                InstructionBlock::target_only("c2", AgentTarget::Claude, "old2", vec![], false),
            ],
        };
        apply_claude_owned_blocks_from_legacy_incoming(&mut doc, "incoming once");
        let claude_only: Vec<_> = doc
            .blocks
            .iter()
            .filter(|b| {
                b.mode == InstructionBlockMode::TargetOnly
                    && b.variants.contains_key(&AgentTarget::Claude)
            })
            .collect();
        assert_eq!(claude_only.len(), 1);
        assert_eq!(
            claude_only[0].variants.get(&AgentTarget::Claude).unwrap(),
            "incoming once"
        );
        assert!(doc
            .blocks
            .iter()
            .any(|b| b.mode == InstructionBlockMode::Shared));
        // 二次应用仍单块
        apply_claude_owned_blocks_from_legacy_incoming(&mut doc, "incoming once");
        let again = doc
            .blocks
            .iter()
            .filter(|b| {
                b.mode == InstructionBlockMode::TargetOnly
                    && b.variants.contains_key(&AgentTarget::Claude)
            })
            .count();
        assert_eq!(again, 1);
    }
}
