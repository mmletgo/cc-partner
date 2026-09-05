//! agent_hub/service — Multi-CLI Agent Hub 服务门面（目录模块）
//!
//! Business Logic（为什么需要这个模块）:
//!     Tauri command / loopback control 需要对 status/list/get/mutate/preview 提供统一 owner 实现，
//!     且禁止把指令正文写入日志。
//!
//! Code Logic（这个模块做什么）:
//!     原单文件 service.rs 的结构性拆分：dto（DTO/Request 定义）、target_intent
//!     （target binding 意图执行）、summary（summary/probe 构建与单元格判定）、
//!     instruction_document（指令文档持久化/DTO 转换）与 tests（测试）。
//!     mod.rs 保留 AgentHubService 门面方法、`*_for_state` 自由函数族与
//!     owner/write-compat 解析，并 re-export 子模块公开项以保持
//!     `agent_hub::service::*` 对外路径不变。

mod dto;
mod instruction_document;
mod summary;
mod target_intent;

pub use dto::*;
pub(crate) use instruction_document::{
    block_to_dto, commit_user_instruction_document, instruction_document_from_block_dtos,
    load_instruction_document_for_legacy, load_instruction_document_for_user_v2,
    persist_instruction_document_for_legacy,
};

use instruction_document::{
    ensure_expected_revision, load_asset_or_not_found, load_instruction_document,
    load_instruction_view, parse_block_mode, persist_instruction_document,
    require_instruction_asset,
};
use summary::{
    build_summaries_for_assets, build_summary, detail_from_summary, probe_all_targets_best_effort,
};
use target_intent::apply_target_intent;

use crate::agent_hub::instructions::{InstructionBlock, InstructionBlockMode, InstructionDocument};
use crate::agent_hub::models::{
    AgentTarget, AssetKind, DesiredPresence, NewTargetBinding, TargetBindingIntent,
};
use crate::agent_hub::project_scope::{
    build_project_enable_preview, enable_project_scope, AgentHubProjectPreview,
    AgentHubProjectStatus, EnableAgentHubProjectRequest,
};
use crate::agent_hub::targets::TargetEnvironment;
use crate::backend::authority::RuntimeRole;
use crate::backend::control::{self, AGENT_HUB_API_VERSION};
use crate::error::AppError;
use crate::state::AppState;
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

/// Agent Hub 服务门面（无状态单元类型）。
///
/// Business Logic（为什么需要这个结构体）:
///     command/control 层通过 `AgentHubService::method` 调用 owner 实现。
///
/// Code Logic（这个结构体做什么）:
///     空单元类型；方法全部接收 `&AppState`。
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentHubService;

impl AgentHubService {
    /// Business Logic: 用户级指令首页必须展示三个 CLI 的真实 source chain 与管理状态。
    /// Code Logic: 委托 V2 inventory owner 实现，不读取 GUI 本地空状态。
    pub async fn inspect_user_instruction_workspace(
        state: &AppState,
    ) -> Result<crate::agent_hub::user_instructions::UserInstructionWorkspaceDto, AppError> {
        crate::agent_hub::user_instructions::inspect_user_instruction_workspace(state).await
    }

    /// Business Logic: 首次设置在任何 mutation 前生成可审阅、短期有效的计划。
    /// Code Logic: 委托 V2 setup preview 并持久化 plan token。
    pub async fn preview_user_instruction_setup(
        state: &AppState,
        req: crate::agent_hub::user_instructions::PreviewUserInstructionRequest,
    ) -> Result<crate::agent_hub::user_instructions::UserInstructionPlanDto, AppError> {
        crate::agent_hub::user_instructions::preview_user_instruction_setup(state, req).await
    }

    /// Business Logic: 日常更新也必须经过 revision/inventory/diff 预览。
    /// Code Logic: 委托 V2 update preview 并持久化 plan token。
    pub async fn preview_user_instruction_update(
        state: &AppState,
        req: crate::agent_hub::user_instructions::PreviewUserInstructionRequest,
    ) -> Result<crate::agent_hub::user_instructions::UserInstructionPlanDto, AppError> {
        crate::agent_hub::user_instructions::preview_user_instruction_update(state, req).await
    }

    /// Business Logic: 仅允许应用已确认且仍新鲜的计划；当前 target 写能力未认证时 fail-closed。
    /// Code Logic: 委托 V2 apply 的原子 claim/CAS/ownership 验证。
    pub async fn apply_user_instruction_plan(
        state: &AppState,
        req: crate::agent_hub::user_instructions::ApplyUserInstructionPlanRequest,
    ) -> Result<crate::agent_hub::user_instructions::ApplyUserInstructionPlanResultDto, AppError>
    {
        crate::agent_hub::user_instructions::apply_user_instruction_plan(state, req).await
    }

    /// Business Logic: 保存块文档是 cc-partner 内部编辑态，独立于 CLI 写入门禁。
    /// Code Logic: 委托 V2 save_user_instruction_blocks（baseRevisionId CAS + put_blob + append_revision）。
    pub async fn save_user_instruction_blocks(
        state: &AppState,
        req: crate::agent_hub::user_instructions::SaveUserInstructionBlocksRequest,
    ) -> Result<crate::agent_hub::user_instructions::UserInstructionCanonicalDto, AppError> {
        crate::agent_hub::user_instructions::save_user_instruction_blocks(state, req).await
    }

    /// Business Logic: 用户级提示词编辑真实配置目录文件，不是 Hub 三槽投影。
    /// Code Logic: 白名单路径 + 有界读；不查 support manifest。
    pub fn read_user_native_instruction_file(
        req: crate::agent_hub::user_instructions::ReadUserNativeInstructionFileRequest,
    ) -> Result<crate::agent_hub::user_instructions::UserNativeInstructionFileDto, AppError> {
        let env = TargetEnvironment::from_process();
        crate::agent_hub::user_instructions::read_user_native_instruction_file(&env, &req)
    }

    /// Business Logic: 用户保存自己的 AGENTS.md / CLAUDE.md / GEMINI.md。
    /// Code Logic: 白名单路径 + CAS 原子写；不查 support manifest L3。
    pub fn write_user_native_instruction_file(
        req: crate::agent_hub::user_instructions::WriteUserNativeInstructionFileRequest,
    ) -> Result<crate::agent_hub::user_instructions::UserNativeInstructionFileDto, AppError> {
        let env = TargetEnvironment::from_process();
        crate::agent_hub::user_instructions::write_user_native_instruction_file(&env, &req)
    }

    /// Business Logic: 三槽历史列表只读查询。
    /// Code Logic: 委托 inventory 层 list_user_instruction_slot_versions。
    pub async fn list_user_instruction_slot_versions(
        state: &AppState,
        asset_id: String,
        slot: crate::agent_hub::user_instructions::InstructionSlotKey,
    ) -> Result<Vec<crate::storage::content_version_repo::ContentVersion>, AppError> {
        crate::agent_hub::user_instructions::list_user_instruction_slot_versions(
            state, asset_id, slot,
        )
        .await
    }

    /// Business Logic: 三槽历史恢复 = 把目标版本正文替换当前槽，写一条新 head。
    /// Code Logic: 委托 inventory 层 restore_user_instruction_slot_version（含 CAS + pre-restore baseline + commit）。
    pub async fn restore_user_instruction_slot_version(
        state: &AppState,
        req: crate::agent_hub::user_instructions::RestoreUserInstructionSlotRequest,
    ) -> Result<crate::agent_hub::user_instructions::UserInstructionCanonicalDto, AppError> {
        crate::agent_hub::user_instructions::restore_user_instruction_slot_version(state, req).await
    }

    /// Business Logic: 首屏 status。
    /// Code Logic: config + owner/writeCompatible + probes + counts。
    pub async fn get_status(state: &AppState) -> Result<AgentHubStatusDto, AppError> {
        get_status_for_state(state).await
    }

    /// Business Logic: 列表过滤。
    /// Code Logic: 解析 kind 后 list_assets + summary。
    pub async fn list_assets(
        state: &AppState,
        req: ListAssetsRequest,
    ) -> Result<Vec<AgentHubAssetSummaryDto>, AppError> {
        let kind = match req.kind.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => Some(
                AssetKind::parse(raw)
                    .ok_or_else(|| AppError::validation(format!("非法 kind: {raw}")))?,
            ),
            None => None,
        };
        let scope = req
            .scope_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        list_assets_for_state(state, scope, kind).await
    }

    /// Business Logic: 资产详情。
    /// Code Logic: summary + blocks + conflicts。
    pub async fn get_asset(
        state: &AppState,
        asset_id: &str,
    ) -> Result<AgentHubAssetDetailDto, AppError> {
        get_asset_for_state(state, asset_id).await
    }

    /// Business Logic: 保存整份指令。
    /// Code Logic: expected revision CAS + from_shared_markdown + Ui revision。
    pub async fn update_instruction(
        state: &AppState,
        req: UpdateInstructionRequest,
    ) -> Result<AgentHubAssetDetailDto, AppError> {
        let asset = require_instruction_asset(state, &req.asset_id).await?;
        ensure_expected_revision(&asset, req.expected_revision_id.as_deref())?;
        let doc = InstructionDocument::from_shared_markdown(
            asset.logical_key.clone(),
            req.content_markdown,
        );
        persist_instruction_document(state, &asset, &doc).await?;
        get_asset_for_state(state, &req.asset_id).await
    }

    /// Business Logic: 更新单块。
    /// Code Logic: load/edit/write；返回块 DTO。
    pub async fn update_instruction_block(
        state: &AppState,
        req: UpdateInstructionBlockRequest,
    ) -> Result<InstructionBlockDto, AppError> {
        let asset = require_instruction_asset(state, &req.asset_id).await?;
        ensure_expected_revision(&asset, req.expected_revision_id.as_deref())?;
        let (mut doc, _) = load_instruction_document(state, &asset).await?;
        let block = doc
            .blocks
            .iter_mut()
            .find(|b| b.id == req.block_id)
            .ok_or_else(|| {
                AppError::not_found(format!("agent_hub_block_not_found:{}", req.block_id))
            })?;
        if let Some(mode) = req.mode.as_deref() {
            block.mode = parse_block_mode(mode)?;
        }
        if let Some(md) = req.common_markdown {
            block.common_markdown = Some(md);
        }
        if let Some(variants) = req.variants {
            block.variants = variants
                .into_iter()
                .filter_map(|(k, v)| AgentTarget::parse(&k).map(|t| (t, v)))
                .collect();
        }
        let dto = block_to_dto(block);
        persist_instruction_document(state, &asset, &doc).await?;
        Ok(dto)
    }

    /// Business Logic: 配对 adapted 变体。
    /// Code Logic: 合并 block_ids → Adapted → 落盘。
    pub async fn pair_instruction_variants(
        state: &AppState,
        req: PairInstructionVariantsRequest,
    ) -> Result<AgentHubAssetDetailDto, AppError> {
        if req.block_ids.len() < 2 {
            return Err(AppError::validation(
                "pair_instruction_variants 至少需要两个 blockId",
            ));
        }
        let asset = require_instruction_asset(state, &req.asset_id).await?;
        ensure_expected_revision(&asset, req.expected_revision_id.as_deref())?;
        let (mut doc, _) = load_instruction_document(state, &asset).await?;
        let mut selected = Vec::new();
        for id in &req.block_ids {
            let block = doc
                .blocks
                .iter()
                .find(|b| &b.id == id)
                .cloned()
                .ok_or_else(|| AppError::not_found(format!("agent_hub_block_not_found:{id}")))?;
            selected.push(block);
        }
        let keep_id = selected[0].id.clone();
        let mut variants = BTreeMap::new();
        let mut common = req
            .common_markdown
            .clone()
            .or_else(|| selected[0].common_markdown.clone())
            .filter(|s| !s.is_empty());
        for block in &selected {
            if let Some(src) = block.source_target {
                if let Some(text) = block.variants.get(&src) {
                    variants.insert(src, text.clone());
                } else if let Some(c) = &block.common_markdown {
                    variants.insert(src, c.clone());
                }
            }
            for (t, body) in &block.variants {
                variants.insert(*t, body.clone());
            }
            if block.mode == InstructionBlockMode::Shared {
                if let Some(c) = &block.common_markdown {
                    if !c.is_empty() {
                        common = Some(c.clone());
                    }
                }
            }
        }
        doc.blocks.retain(|b| !req.block_ids.contains(&b.id));
        doc.blocks.push(InstructionBlock::adapted(
            keep_id,
            common,
            None,
            variants,
            selected[0].heading_path.clone(),
        ));
        persist_instruction_document(state, &asset, &doc).await?;
        get_asset_for_state(state, &req.asset_id).await
    }

    /// Business Logic: 项目预览。
    /// Code Logic: project_scope preview。
    pub async fn preview_project(
        state: &AppState,
        project_id: &str,
    ) -> Result<AgentHubProjectPreview, AppError> {
        build_project_enable_preview(state, project_id).await
    }

    /// Business Logic: 启用项目。
    /// Code Logic: enable confirm=true → ensure enabled + schedule project projections。
    pub async fn enable_project(
        state: &AppState,
        project_id: &str,
    ) -> Result<AgentHubProjectStatus, AppError> {
        let status = enable_project_scope(
            state,
            EnableAgentHubProjectRequest {
                project_id: project_id.to_string(),
                confirm: true,
            },
        )
        .await?;
        if let Err(e) = crate::agent_hub::projection_ops::ensure_agent_hub_enabled(state).await {
            tracing::warn!(error = %e, "agent_hub enable_project ensure enabled failed");
        }
        if let Err(e) =
            crate::agent_hub::projection_ops::schedule_project_projections(state, project_id).await
        {
            tracing::warn!(
                project_id = %project_id,
                error = %e,
                "agent_hub enable_project schedule projections failed"
            );
        }
        Ok(status)
    }

    /// Business Logic: 解决冲突。
    /// Code Logic: keepExternal/manual 要求 content；resolve 后 best-effort 调度投影。
    pub async fn resolve_conflict(
        state: &AppState,
        req: ResolveConflictRequest,
    ) -> Result<AgentHubAssetDetailDto, AppError> {
        let asset = require_instruction_asset(state, &req.asset_id).await?;
        let conflict = state
            .agent_hub_repo
            .get_conflict(&req.conflict_id)
            .await?
            .ok_or_else(|| {
                AppError::not_found(format!("agent_hub_conflict_not_found:{}", req.conflict_id))
            })?;
        if conflict.asset_id != asset.id {
            return Err(AppError::validation("conflict 与 assetId 不匹配"));
        }
        match req.resolution.as_str() {
            "keepHub" => {}
            "keepExternal" | "manual" => {
                let md = req
                    .content_markdown
                    .as_deref()
                    .filter(|s| !s.trim().is_empty());
                let Some(md) = md else {
                    return Err(AppError::validation("agent_hub_resolve_content_required"));
                };
                let doc = InstructionDocument::from_shared_markdown(asset.logical_key.clone(), md);
                persist_instruction_document(state, &asset, &doc).await?;
            }
            other => {
                return Err(AppError::validation(format!(
                    "非法 resolution: {other}（keepHub|keepExternal|manual）"
                )));
            }
        }
        let now = chrono::Utc::now().to_rfc3339();
        state
            .agent_hub_repo
            .resolve_conflict(&req.conflict_id, &now)
            .await?;
        if let Err(e) = crate::agent_hub::projection_ops::ensure_agent_hub_enabled(state).await {
            tracing::warn!(error = %e, "agent_hub resolve_conflict ensure enabled failed");
        }
        if let Err(e) =
            crate::agent_hub::projection_ops::schedule_asset_projections(state, &req.asset_id).await
        {
            tracing::warn!(
                asset_id = %req.asset_id,
                error = %e,
                "agent_hub resolve_conflict schedule projections failed"
            );
        }
        get_asset_for_state(state, &req.asset_id).await
    }

    /// Business Logic: 设置 target binding。
    /// Code Logic: upsert + ensure enabled + schedule projections + 返回 summary。
    pub async fn set_target_binding(
        state: &AppState,
        req: SetTargetBindingRequest,
    ) -> Result<AgentHubAssetSummaryDto, AppError> {
        let asset = load_asset_or_not_found(state, &req.asset_id).await?;
        state
            .agent_hub_repo
            .upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target: req.target,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: req.desired_presence,
                desired_enabled: req.desired_enabled,
            })
            .await?;
        if let Err(e) = crate::agent_hub::projection_ops::ensure_agent_hub_enabled(state).await {
            tracing::warn!(error = %e, "agent_hub set_target_binding ensure enabled failed");
        }
        if let Err(e) =
            crate::agent_hub::projection_ops::schedule_asset_projections(state, &req.asset_id).await
        {
            tracing::warn!(
                asset_id = %req.asset_id,
                error = %e,
                "agent_hub set_target_binding schedule projections failed"
            );
        }
        // re-read asset for current head
        let asset = load_asset_or_not_found(state, &req.asset_id).await?;
        build_summary(state, &asset).await
    }

    /// Business Logic: 设置 target-local desiredPresence。
    /// Code Logic: apply_intent → upsert binding → 可选 schedule。
    pub async fn set_target_presence(
        state: &AppState,
        req: SetTargetPresenceRequest,
    ) -> Result<AgentHubAssetSummaryDto, AppError> {
        apply_target_intent(
            state,
            &req.asset_id,
            req.target,
            TargetBindingIntent::SetPresence(req.desired_presence),
        )
        .await
    }

    /// Business Logic: 设置 target-local desiredEnabled。
    /// Code Logic: apply_intent → upsert enabled → schedule。
    pub async fn set_target_enabled(
        state: &AppState,
        req: SetTargetEnabledRequest,
    ) -> Result<AgentHubAssetSummaryDto, AppError> {
        apply_target_intent(
            state,
            &req.asset_id,
            req.target,
            TargetBindingIntent::SetEnabled(req.desired_enabled),
        )
        .await
    }

    /// Business Logic: 恢复 detached materialization 并调度投影。
    /// Code Logic: apply_intent RestoreDetached → clear Detached → Present schedule。
    pub async fn restore_detached_target(
        state: &AppState,
        req: RestoreDetachedTargetRequest,
    ) -> Result<AgentHubAssetSummaryDto, AppError> {
        apply_target_intent(
            state,
            &req.asset_id,
            req.target,
            TargetBindingIntent::RestoreDetached,
        )
        .await
    }

    /// Business Logic: 一条 canonical tombstone + 全部 target fan-out Absent。
    /// Code Logic: apply_intent DeleteEverywhere（target 参数仅用于定位策略入口）。
    pub async fn delete_asset_everywhere(
        state: &AppState,
        req: DeleteAssetEverywhereRequest,
    ) -> Result<AgentHubAssetSummaryDto, AppError> {
        let asset = load_asset_or_not_found(state, &req.asset_id).await?;
        let bindings = state
            .agent_hub_repo
            .list_target_bindings_for_asset(&asset.id)
            .await?;
        // 任选一个 present binding 或首个 binding 作为意图入口
        let entry_target = bindings
            .iter()
            .find(|b| b.desired_presence == DesiredPresence::Present)
            .or(bindings.first())
            .map(|b| b.target)
            .unwrap_or(AgentTarget::Claude);
        apply_target_intent(
            state,
            &req.asset_id,
            entry_target,
            TargetBindingIntent::DeleteEverywhere,
        )
        .await
    }
}

/// 读取 Agent Hub 运行时状态。
///
/// Business Logic（为什么需要这个函数）:
///     自由函数入口与 service 方法共用实现。
///
/// Code Logic（这个函数做什么）:
///     读 config/owner/writeCompatible/probes/counts。
pub async fn get_status_for_state(state: &AppState) -> Result<AgentHubStatusDto, AppError> {
    let (enabled, background_enabled) = {
        let cfg = state
            .config
            .read()
            .map_err(|_| AppError::generic("agent_hub_config_lock_poisoned"))?;
        (cfg.agent_hub.enabled, cfg.agent_hub.background_enabled)
    };
    let (owner_instance_id, write_compatible) = resolve_owner_and_write_compat(state);
    Ok(AgentHubStatusDto {
        enabled,
        background_enabled,
        agent_hub_api_version: AGENT_HUB_API_VERSION,
        owner_instance_id,
        write_compatible,
        probes: probe_all_targets_best_effort(),
        conflict_count: state
            .agent_hub_repo
            .list_unresolved_conflicts()
            .await?
            .len() as u32,
        blocked_materialization_count: state
            .agent_hub_repo
            .list_blocked_materializations()
            .await?
            .len() as u32,
    })
}

/// 列出资产摘要。
///
/// Business Logic（为什么需要这个函数）:
///     Hub 列表按 scope/kind 过滤。
///
/// Code Logic（这个函数做什么）:
///     repo.list_assets + **一次** shared probe/mats/conflicts 批量化 build_summary。
pub async fn list_assets_for_state(
    state: &AppState,
    scope_id: Option<&str>,
    kind: Option<AssetKind>,
) -> Result<Vec<AgentHubAssetSummaryDto>, AppError> {
    let assets = state.agent_hub_repo.list_assets(scope_id, kind).await?;
    build_summaries_for_assets(state, &assets).await
}

/// 读取资产详情。
///
/// Business Logic（为什么需要这个函数）:
///     选中资产后展示 blocks/content/conflicts。
///
/// Code Logic（这个函数做什么）:
///     summary + load document + unresolved conflicts for asset。
pub async fn get_asset_for_state(
    state: &AppState,
    asset_id: &str,
) -> Result<AgentHubAssetDetailDto, AppError> {
    let asset = load_asset_or_not_found(state, asset_id).await?;
    let summary = build_summary(state, &asset).await?;
    let (content_markdown, blocks) = load_instruction_view(state, &asset).await?;
    let conflicts = state
        .agent_hub_repo
        .list_unresolved_conflicts()
        .await?
        .into_iter()
        .filter(|c| c.asset_id == asset.id)
        .map(|c| AgentHubConflictDto {
            id: c.id,
            target: c.target,
            detail_json: Some(c.detail_json),
            created_at: c.created_at,
        })
        .collect();
    Ok(detail_from_summary(
        summary,
        blocks,
        content_markdown,
        conflicts,
    ))
}

/// 用整篇 Markdown 覆盖 instruction。
///
/// Business Logic（为什么需要这个函数）:
///     自由函数 API 与 service 方法对齐。
///
/// Code Logic（这个函数做什么）:
///     构造 UpdateInstructionRequest 委托方法。
pub async fn update_instruction_for_state(
    state: &AppState,
    asset_id: &str,
    content_markdown: &str,
) -> Result<AgentHubAssetDetailDto, AppError> {
    AgentHubService::update_instruction(
        state,
        UpdateInstructionRequest {
            asset_id: asset_id.to_string(),
            content_markdown: content_markdown.to_string(),
            expected_revision_id: None,
        },
    )
    .await
}

/// 更新单个 instruction 块。
///
/// Business Logic（为什么需要这个函数）:
///     自由函数 API。
///
/// Code Logic（这个函数做什么）:
///     委托 update_instruction_block 后重新 get_asset。
pub async fn update_instruction_block_for_state(
    state: &AppState,
    asset_id: &str,
    block_id: &str,
    mode: Option<InstructionBlockMode>,
    common_markdown: Option<String>,
    variant_target: Option<AgentTarget>,
    variant_markdown: Option<String>,
) -> Result<AgentHubAssetDetailDto, AppError> {
    let mut variants = None;
    if let (Some(t), Some(md)) = (variant_target, variant_markdown) {
        let mut map = BTreeMap::new();
        map.insert(t.as_str().to_string(), md);
        variants = Some(map);
    }
    let _ = AgentHubService::update_instruction_block(
        state,
        UpdateInstructionBlockRequest {
            asset_id: asset_id.to_string(),
            block_id: block_id.to_string(),
            mode: mode.map(|m| m.as_str().to_string()),
            common_markdown,
            variants,
            expected_revision_id: None,
        },
    )
    .await?;
    get_asset_for_state(state, asset_id).await
}

/// 配对两个块为 adapted 变体。
///
/// Business Logic（为什么需要这个函数）:
///     自由函数 API（target_a/target_b 透传为 variants 来源）。
///
/// Code Logic（这个函数做什么）:
///     委托 pair_instruction_variants。
pub async fn pair_instruction_variants_for_state(
    state: &AppState,
    asset_id: &str,
    block_ids: &[String],
    _target_a: AgentTarget,
    _target_b: AgentTarget,
) -> Result<AgentHubAssetDetailDto, AppError> {
    AgentHubService::pair_instruction_variants(
        state,
        PairInstructionVariantsRequest {
            asset_id: asset_id.to_string(),
            block_ids: block_ids.to_vec(),
            common_markdown: None,
            expected_revision_id: None,
        },
    )
    .await
}

/// 项目启用预览。
///
/// Business Logic（为什么需要这个函数）:
///     自由函数 API。
///
/// Code Logic（这个函数做什么）:
///     委托 project_scope。
pub async fn preview_project_for_state(
    state: &AppState,
    project_id: &str,
) -> Result<AgentHubProjectPreview, AppError> {
    AgentHubService::preview_project(state, project_id).await
}

/// 启用项目。
///
/// Business Logic（为什么需要这个函数）:
///     自由函数 API，接收完整 request。
///
/// Code Logic（这个函数做什么）:
///     委托 enable_project_scope。
pub async fn enable_project_for_state(
    state: &AppState,
    request: EnableAgentHubProjectRequest,
) -> Result<AgentHubProjectStatus, AppError> {
    enable_project_scope(state, request).await
}

/// 解决 conflict（自由函数）。
///
/// Business Logic（为什么需要这个函数）:
///     返回 ConflictDto 形状；内部仍写库。
///
/// Code Logic（这个函数做什么）:
///     转字符串 resolution 后 resolve；再 get_conflict 映射 DTO。
pub async fn resolve_conflict_for_state(
    state: &AppState,
    conflict_id: &str,
    resolution: AgentHubConflictResolution,
    content_markdown: Option<&str>,
) -> Result<AgentHubConflictDto, AppError> {
    let conflict = state
        .agent_hub_repo
        .get_conflict(conflict_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found(format!("agent_hub_conflict_not_found:{conflict_id}"))
        })?;
    let resolution_s = match resolution {
        AgentHubConflictResolution::KeepHub => "keepHub",
        AgentHubConflictResolution::KeepExternal => "keepExternal",
        AgentHubConflictResolution::Manual => "manual",
    };
    let _ = AgentHubService::resolve_conflict(
        state,
        ResolveConflictRequest {
            asset_id: conflict.asset_id.clone(),
            conflict_id: conflict_id.to_string(),
            resolution: resolution_s.to_string(),
            content_markdown: content_markdown.map(|s| s.to_string()),
        },
    )
    .await?;
    let resolved = state
        .agent_hub_repo
        .get_conflict(conflict_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found(format!("agent_hub_conflict_not_found:{conflict_id}"))
        })?;
    Ok(AgentHubConflictDto {
        id: resolved.id,
        target: resolved.target,
        detail_json: Some(resolved.detail_json),
        created_at: resolved.created_at,
    })
}

/// 设置 target binding（自由函数）。
///
/// Business Logic（为什么需要这个函数）:
///     返回带 id 的 TargetBindingDto。
///
/// Code Logic（这个函数做什么）:
///     upsert 后读 materialization。
pub async fn set_target_binding_for_state(
    state: &AppState,
    asset_id: &str,
    target: AgentTarget,
    desired_presence: DesiredPresence,
    desired_enabled: bool,
    checkout_binding_id: Option<String>,
) -> Result<AgentHubTargetBindingDto, AppError> {
    let _ = load_asset_or_not_found(state, asset_id).await?;
    let binding = state
        .agent_hub_repo
        .upsert_target_binding(NewTargetBinding {
            asset_id: asset_id.to_string(),
            target,
            local_scope_mapping_id: None,
            checkout_binding_id,
            desired_presence,
            desired_enabled,
        })
        .await?;
    let mat = state
        .agent_hub_repo
        .get_materialization_by_binding(&binding.id)
        .await?;
    Ok(AgentHubTargetBindingDto {
        target: binding.target,
        desired_presence: binding.desired_presence,
        desired_enabled: binding.desired_enabled,
        materialization_status: mat.as_ref().map(|m| m.status.as_str().to_string()),
        last_error: mat.as_ref().and_then(|m| m.last_error.clone()),
        binding_id: Some(binding.id),
        materialization_id: mat.map(|m| m.id),
    })
}

/// 解析 owner 与写兼容性。
///
/// Business Logic（为什么需要这个函数）:
///     GuiClient 对照 control agentHubApiVersion；HeadlessOwner 以本机常量为准。
///
/// Code Logic（这个函数做什么）:
///     HeadlessOwner: owner=config_runtime，writeCompatible=true。
///     GuiClient: control client cache，失败再读 control file。
fn resolve_owner_and_write_compat(state: &AppState) -> (Option<String>, bool) {
    match state.runtime_role {
        RuntimeRole::HeadlessOwner => (
            Some(state.config_runtime.owner_instance_id().to_string()),
            true,
        ),
        RuntimeRole::GuiClient => {
            if let Ok(client) = state.backend_control_client_runtime.client() {
                let owner = client.owner_instance_id().map(|s| s.to_string());
                let compatible = client.agent_hub_api_version() == AGENT_HUB_API_VERSION;
                return (owner, compatible);
            }
            match control::read_control_file() {
                Ok(Some(file)) => {
                    let compatible = file.agent_hub_api_version == AGENT_HUB_API_VERSION;
                    (file.owner_instance_id, compatible)
                }
                _ => (None, false),
            }
        }
    }
}
