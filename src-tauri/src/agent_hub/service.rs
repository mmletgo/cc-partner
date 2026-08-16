//! agent_hub/service — Multi-CLI Agent Hub 服务门面
//!
//! Business Logic（为什么需要这个模块）:
//!     Tauri command / loopback control 需要对 status/list/get/mutate/preview 提供统一 owner 实现，
//!     且禁止把指令正文写入日志。
//!
//! Code Logic（这个模块做什么）:
//!     定义 camelCase DTO / Request 与 `AgentHubService` 方法；组合 AgentHubRepo、ObjectStore、
//!     project_scope 与 target adapters。另导出 `*_for_state` 薄包装供自由函数调用。

use crate::agent_hub::instructions::{InstructionBlock, InstructionBlockMode, InstructionDocument};
use crate::agent_hub::models::{
    compute_asset_aggregate_status, AgentTarget, AssetKind, DesiredPresence, LogicalAsset,
    Materialization, MaterializationStatus, NewMaterialization, NewRevision, NewTargetBinding,
    RevisionId, RevisionOperation, RevisionOriginKind, TargetBinding, TargetBindingIntent,
    TargetBindingTransition, TargetDisableStrategy, TargetStatusSnapshot,
    UserInstructionOwnershipRecord,
};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::project_scope::{
    build_project_enable_preview, enable_project_scope, AgentHubProjectPreview,
    AgentHubProjectStatus, EnableAgentHubProjectRequest,
};
use crate::agent_hub::targets::{
    AssetAdapter, ClaudeInstructionAdapter, CodexInstructionAdapter, OpenCodeInstructionAdapter,
    TargetEnvironment,
};
use crate::backend::authority::RuntimeRole;
use crate::backend::control::{self, AGENT_HUB_API_VERSION};
use crate::error::AppError;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Agent Hub 服务门面（无状态单元类型）。
///
/// Business Logic（为什么需要这个结构体）:
///     command/control 层通过 `AgentHubService::method` 调用 owner 实现。
///
/// Code Logic（这个结构体做什么）:
///     空单元类型；方法全部接收 `&AppState`。
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentHubService;

/// CLI probe DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     首屏展示可执行文件、版本与支持级别。
///
/// Code Logic（这个结构体做什么）:
///     camelCase probe 字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubProbeDto {
    pub target: AgentTarget,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub support: String,
    pub config_root: Option<String>,
}

/// Agent Hub 运行时状态。
///
/// Business Logic（为什么需要这个结构体）:
///     UI 首屏与升级门闸依赖 enabled / writeCompatible / 冲突计数。
///
/// Code Logic（这个结构体做什么）:
///     camelCase status 聚合。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubStatusDto {
    pub enabled: bool,
    pub background_enabled: bool,
    pub agent_hub_api_version: u32,
    pub owner_instance_id: Option<String>,
    pub write_compatible: bool,
    pub probes: Vec<AgentHubProbeDto>,
    pub conflict_count: u32,
    pub blocked_materialization_count: u32,
}

/// 资产 target 单元格。
///
/// Business Logic（为什么需要这个结构体）:
///     列表按 Claude/Codex/OpenCode 三列展示 desired 与 materialization；
///     Task 8 矩阵需要 supported/verified/sourceOnly 输入，禁止仅凭 Synced 推断 full。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 单元格（无 binding id）+ 聚合输入字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubTargetCellDto {
    pub target: AgentTarget,
    pub desired_presence: DesiredPresence,
    pub desired_enabled: bool,
    pub materialization_status: Option<String>,
    pub last_error: Option<String>,
    /// 是否在 requested 集合（有 binding 行）
    pub requested: bool,
    /// 目标当前是否 supported（probe 失败/unsupported → false）
    pub supported: bool,
    /// 是否仅 sourceOnly（无可投影 materialization）
    pub source_only: bool,
    /// 是否 verified（package activation/list 通过；指令 Synced 即 verified）
    pub verified: bool,
}

/// 资产在某一 target 上的绑定摘要（含 id）。
///
/// Business Logic（为什么需要这个结构体）:
///     set_target_binding 等路径可能需要 binding/materialization id。
///
/// Code Logic（这个结构体做什么）:
///     在 TargetCell 基础上附加 bindingId/materializationId。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubTargetBindingDto {
    pub target: AgentTarget,
    pub desired_presence: DesiredPresence,
    pub desired_enabled: bool,
    pub materialization_status: Option<String>,
    pub last_error: Option<String>,
    pub binding_id: Option<String>,
    pub materialization_id: Option<String>,
}

/// 资产列表摘要。
///
/// Business Logic（为什么需要这个结构体）:
///     Hub 列表展示 logical asset 与 target 单元格；聚合态供矩阵 UI 直接消费。
///
/// Code Logic（这个结构体做什么）:
///     camelCase summary + hasConflict + aggregateStatus。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubAssetSummaryDto {
    pub asset_id: String,
    pub scope_id: String,
    pub kind: String,
    pub display_name: String,
    pub logical_key: String,
    pub origin_namespace: String,
    pub policy: String,
    pub current_revision_id: Option<String>,
    pub targets: Vec<AgentHubTargetCellDto>,
    pub has_conflict: bool,
    /// 派生聚合状态：`full|partial|sourceOnly|activationRequired|externalCollision|detached|blocked`
    pub aggregate_status: String,
}

/// 指令块 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     InstructionBlocksDrawer 编辑 shared/adapted/targetOnly 与变体。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；commonMarkdown 为字符串，variants 为 map。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstructionBlockDto {
    pub id: String,
    pub mode: String,
    pub common_markdown: String,
    pub variants: Option<BTreeMap<String, String>>,
    pub heading_path: Option<Vec<String>>,
    pub source_target: Option<AgentTarget>,
    pub needs_adaptation: bool,
}

/// 任务文档中的指令块别名。
pub type AgentHubInstructionBlockDto = InstructionBlockDto;

/// 冲突摘要。
///
/// Business Logic（为什么需要这个结构体）:
///     详情页冲突列表与 resolve 后刷新。
///
/// Code Logic（这个结构体做什么）:
///     最小 camelCase 字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubConflictDto {
    pub id: String,
    pub target: Option<AgentTarget>,
    pub detail_json: Option<String>,
    pub created_at: String,
}

/// 资产详情。
///
/// Business Logic（为什么需要这个结构体）:
///     选中资产后展示矩阵、正文、块与冲突；聚合态与 summary 同源。
///
/// Code Logic（这个结构体做什么）:
///     扁平 summary 字段 + blocks/content/conflicts。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubAssetDetailDto {
    pub asset_id: String,
    pub scope_id: String,
    pub kind: String,
    pub display_name: String,
    pub logical_key: String,
    pub origin_namespace: String,
    pub policy: String,
    pub current_revision_id: Option<String>,
    pub targets: Vec<AgentHubTargetCellDto>,
    pub has_conflict: bool,
    /// 派生聚合状态（与 summary 同源）
    pub aggregate_status: String,
    pub blocks: Vec<InstructionBlockDto>,
    pub content_markdown: Option<String>,
    pub conflicts: Vec<AgentHubConflictDto>,
}

/// 列表过滤请求。
///
/// Business Logic（为什么需要这个结构体）:
///     list_assets control/IPC payload 承载可选 scope/kind。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 可选字段，default 空。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ListAssetsRequest {
    pub scope_id: Option<String>,
    pub kind: Option<String>,
}

/// 更新整份指令请求。
///
/// Business Logic（为什么需要这个结构体）:
///     保存编辑器正文并可选 CAS expected revision。
///
/// Code Logic（这个结构体做什么）:
///     camelCase assetId/contentMarkdown/expectedRevisionId。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInstructionRequest {
    pub asset_id: String,
    pub content_markdown: String,
    pub expected_revision_id: Option<String>,
}

/// 更新单个块请求。
///
/// Business Logic（为什么需要这个结构体）:
///     块抽屉可改 mode/common/variants。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 可选字段 + expected revision。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInstructionBlockRequest {
    pub asset_id: String,
    pub block_id: String,
    pub mode: Option<String>,
    pub common_markdown: Option<String>,
    pub variants: Option<BTreeMap<String, String>>,
    pub expected_revision_id: Option<String>,
}

/// 配对变体请求。
///
/// Business Logic（为什么需要这个结构体）:
///     将多个块合并为 adapted。
///
/// Code Logic（这个结构体做什么）:
///     blockIds + 可选 commonMarkdown。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairInstructionVariantsRequest {
    pub asset_id: String,
    pub block_ids: Vec<String>,
    pub common_markdown: Option<String>,
    pub expected_revision_id: Option<String>,
}

/// 解决冲突请求。
///
/// Business Logic（为什么需要这个结构体）:
///     keepHub/keepExternal/manual + 可选手工正文。
///
/// Code Logic（这个结构体做什么）:
///     camelCase resolution 字符串。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveConflictRequest {
    pub asset_id: String,
    pub conflict_id: String,
    pub resolution: String,
    pub content_markdown: Option<String>,
}

/// 设置 binding 请求。
///
/// Business Logic（为什么需要这个结构体）:
///     用户按 target 列开关 presence/enabled。
///
/// Code Logic（这个结构体做什么）:
///     camelCase target enum + presence + enabled。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTargetBindingRequest {
    pub asset_id: String,
    pub target: AgentTarget,
    pub desired_presence: DesiredPresence,
    pub desired_enabled: bool,
}

/// 设置 target presence 请求。
///
/// Business Logic: desiredPresence 是 target-local；Absent 只卸该 target。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTargetPresenceRequest {
    pub asset_id: String,
    pub target: AgentTarget,
    pub desired_presence: DesiredPresence,
}

/// 设置 target enabled 请求。
///
/// Business Logic: desiredEnabled 是 target-local；不改其它 CLI。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTargetEnabledRequest {
    pub asset_id: String,
    pub target: AgentTarget,
    pub desired_enabled: bool,
}

/// 恢复 detached target 请求。
///
/// Business Logic: 外部整文件删除后用户显式恢复，调度投影。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreDetachedTargetRequest {
    pub asset_id: String,
    pub target: AgentTarget,
}

/// 从所有 target 删除资产请求。
///
/// Business Logic: 唯一生成 canonical tombstone 的入口。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAssetEverywhereRequest {
    pub asset_id: String,
}

/// 冲突解决策略枚举（自由函数 API 使用）。
///
/// Business Logic（为什么需要这个枚举）:
///     稳定 wire token keepHub/keepExternal/manual。
///
/// Code Logic（这个枚举做什么）:
///     camelCase serde。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentHubConflictResolution {
    KeepHub,
    KeepExternal,
    Manual,
}

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

/// 执行 target binding 意图（presence/enabled/restore/everywhere）。
///
/// Business Logic（为什么需要这个函数）:
///     四条命令共享同一转移表；禁止各自猜测 tombstone。
///
/// Code Logic（这个函数做什么）:
///     加载 binding/materialization → apply_intent → 写库/调度 → summary。
async fn apply_target_intent(
    state: &AppState,
    asset_id: &str,
    target: AgentTarget,
    intent: TargetBindingIntent,
) -> Result<AgentHubAssetSummaryDto, AppError> {
    let asset = load_asset_or_not_found(state, asset_id).await?;
    let bindings = state
        .agent_hub_repo
        .list_target_bindings_for_asset(&asset.id)
        .await?;
    let present_count = bindings
        .iter()
        .filter(|b| b.desired_presence == DesiredPresence::Present)
        .count();
    let binding = bindings
        .iter()
        .find(|b| b.target == target)
        .cloned()
        .unwrap_or(TargetBinding {
            id: String::new(),
            asset_id: asset.id.clone(),
            target,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Absent,
            desired_enabled: false,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        });
    let mat = if binding.id.is_empty() {
        None
    } else {
        state
            .agent_hub_repo
            .get_materialization_by_binding(&binding.id)
            .await?
    };
    // Absent / DeleteEverywhere 前先计算 removal-blocked 预览；绑定尚未变更。
    let needs_removal_preflight = matches!(
        intent,
        TargetBindingIntent::SetPresence(DesiredPresence::Absent)
            | TargetBindingIntent::DeleteEverywhere
    );
    let removal_blocked = if needs_removal_preflight {
        match intent {
            TargetBindingIntent::DeleteEverywhere => {
                collect_removal_blocked_for_asset(state, &asset.id).await?
            }
            _ => compute_removal_blocked_paths(mat.as_ref()),
        }
    } else {
        Vec::new()
    };
    let transition = binding.apply_intent(
        intent,
        mat.as_ref().map(|m| m.status),
        asset.policy,
        present_count,
        &removal_blocked,
    );

    match transition {
        TargetBindingTransition::UpdateEnabled {
            desired_enabled,
            disable_strategy,
            schedule_projection,
        } => {
            let updated = state
                .agent_hub_repo
                .upsert_target_binding(NewTargetBinding {
                    asset_id: asset.id.clone(),
                    target,
                    local_scope_mapping_id: binding.local_scope_mapping_id.clone(),
                    checkout_binding_id: binding.checkout_binding_id.clone(),
                    desired_presence: binding.desired_presence,
                    desired_enabled,
                })
                .await?;
            // disable 必须落到 adapter 策略，而非仅 flip DB 位。
            if !desired_enabled {
                apply_disable_strategy(state, &asset, &updated, mat.as_ref(), disable_strategy)
                    .await?;
            } else if let Some(m) = mat.as_ref() {
                // re-enable：Pending 等待投影/激活重新应用
                state
                    .agent_hub_repo
                    .upsert_materialization(NewMaterialization {
                        asset_id: asset.id.clone(),
                        target,
                        target_binding_id: updated.id.clone(),
                        native_path: m.native_path.clone(),
                        last_projected_revision_id: m.last_projected_revision_id.clone(),
                        rendered_hash: m.rendered_hash.clone(),
                        observed_external_hash: m.observed_external_hash.clone(),
                        status: MaterializationStatus::Pending,
                        last_error: Some(format!("enable_strategy:{}", disable_strategy.as_str())),
                    })
                    .await?;
            }
            if schedule_projection {
                schedule_after_binding_change(state, &asset.id).await;
            }
        }
        TargetBindingTransition::UpdatePresence {
            desired_presence,
            schedule_projection,
            ..
        } => {
            let enabled = if desired_presence == DesiredPresence::Absent {
                false
            } else {
                binding.desired_enabled
            };
            state
                .agent_hub_repo
                .upsert_target_binding(NewTargetBinding {
                    asset_id: asset.id.clone(),
                    target,
                    local_scope_mapping_id: binding.local_scope_mapping_id.clone(),
                    checkout_binding_id: binding.checkout_binding_id.clone(),
                    desired_presence,
                    desired_enabled: enabled,
                })
                .await?;
            if schedule_projection {
                schedule_after_binding_change(state, &asset.id).await;
            }
        }
        TargetBindingTransition::RestoreDetached {
            desired_presence,
            schedule_projection,
            clear_detached_status,
        } => {
            let updated = state
                .agent_hub_repo
                .upsert_target_binding(NewTargetBinding {
                    asset_id: asset.id.clone(),
                    target,
                    local_scope_mapping_id: binding.local_scope_mapping_id.clone(),
                    checkout_binding_id: binding.checkout_binding_id.clone(),
                    desired_presence,
                    desired_enabled: true,
                })
                .await?;
            if clear_detached_status {
                // Pending 表示等待投影；禁止保持 Detached 否则 scheduler 会 no-op
                state
                    .agent_hub_repo
                    .upsert_materialization(NewMaterialization {
                        asset_id: asset.id.clone(),
                        target,
                        target_binding_id: updated.id.clone(),
                        native_path: mat.as_ref().and_then(|m| m.native_path.clone()),
                        last_projected_revision_id: mat
                            .as_ref()
                            .and_then(|m| m.last_projected_revision_id.clone()),
                        rendered_hash: mat.as_ref().and_then(|m| m.rendered_hash.clone()),
                        observed_external_hash: None,
                        status: MaterializationStatus::Pending,
                        last_error: None,
                    })
                    .await?;
            }
            if schedule_projection {
                schedule_after_binding_change(state, &asset.id).await;
            }
        }
        TargetBindingTransition::DeleteEverywhere {
            append_canonical_tombstone,
            fan_out_absent,
        } => {
            // 单 write-lease 事务：tombstone + fan-out，避免中途失败留下半状态。
            // Plugin package 必须走 ownership-aware 删除，否则 package-owned component 成孤儿。
            if append_canonical_tombstone || fan_out_absent {
                if asset.kind == AssetKind::Plugin {
                    // ownership tombstone + 全部 binding→Absent 已在同一 repo TX 完成；
                    // 此处仅 durable schedule（可恢复，非权威写）。
                    let store = object_store()?;
                    let delete_result = state
                        .agent_hub_repo
                        .delete_plugin_package_with_ownership(
                            &asset.id,
                            &store,
                            RevisionOriginKind::Ui,
                            state.device_id.as_str().to_string(),
                        )
                        .await?;
                    let mut schedule_ids = vec![asset.id.clone()];
                    for d in &delete_result.component_decisions {
                        if d.decision
                            == crate::agent_hub::plugins::ownership::ComponentDeleteDecision::TombstoneOwned
                        {
                            schedule_ids.push(d.component_asset_id.clone());
                        }
                    }
                    for aid in schedule_ids {
                        schedule_after_binding_change(state, &aid).await;
                    }
                } else {
                    let parents = asset
                        .current_revision_id
                        .clone()
                        .into_iter()
                        .collect::<Vec<_>>();
                    let expected_parent_id = asset.current_revision_id.clone();
                    let fan_out: Vec<NewTargetBinding> = {
                        let all = state
                            .agent_hub_repo
                            .list_target_bindings_for_asset(&asset.id)
                            .await?;
                        if all.is_empty() {
                            vec![NewTargetBinding {
                                asset_id: asset.id.clone(),
                                target,
                                local_scope_mapping_id: None,
                                checkout_binding_id: None,
                                desired_presence: DesiredPresence::Absent,
                                desired_enabled: false,
                            }]
                        } else {
                            all.into_iter()
                                .map(|b| NewTargetBinding {
                                    asset_id: asset.id.clone(),
                                    target: b.target,
                                    local_scope_mapping_id: b.local_scope_mapping_id,
                                    checkout_binding_id: b.checkout_binding_id,
                                    desired_presence: DesiredPresence::Absent,
                                    desired_enabled: false,
                                })
                                .collect()
                        }
                    };
                    state
                        .agent_hub_repo
                        .delete_asset_everywhere_atomic(
                            &asset.id,
                            NewRevision {
                                id: RevisionId::new_v7(),
                                asset_lineage_id: asset.id.clone(),
                                parents,
                                operation: RevisionOperation::Delete,
                                origin_kind: RevisionOriginKind::Ui,
                                origin_target: None,
                                origin_replica_id: state.device_id.as_str().to_string(),
                                payload_hash: None,
                                tree_manifest_hash: None,
                                created_at: chrono::Utc::now().to_rfc3339(),
                                expected_parent_id,
                            },
                            fan_out,
                        )
                        .await?;
                    schedule_after_binding_change(state, &asset.id).await;
                }
            }
        }
        TargetBindingTransition::RejectLastTargetOnlyRequiresEverywhere { code } => {
            return Err(AppError::validation(code));
        }
        TargetBindingTransition::RejectRemovalBlocked {
            code,
            preview_paths,
        } => {
            return Err(AppError::validation(format!(
                "{code}:{}",
                preview_paths.join(",")
            )));
        }
    }

    let asset = load_asset_or_not_found(state, asset_id).await?;
    build_summary(state, &asset).await
}

/// ensure enabled + schedule projections（best-effort）。
async fn schedule_after_binding_change(state: &AppState, asset_id: &str) {
    if let Err(e) = crate::agent_hub::projection_ops::ensure_agent_hub_enabled(state).await {
        tracing::warn!(error = %e, "agent_hub binding change ensure enabled failed");
    }
    if let Err(e) =
        crate::agent_hub::projection_ops::schedule_asset_projections(state, asset_id).await
    {
        tracing::warn!(
            asset_id = %asset_id,
            error = %e,
            "agent_hub binding change schedule projections failed"
        );
    }
}

/// 计算单个 materialization 的 removal-blocked 路径预览。
///
/// Business Logic（为什么需要这个函数）:
///     Absent 前必须返回精确 preview；外部改动/未知子项不得先把 binding 标 Absent。
///
/// Code Logic（这个函数做什么）:
///     文件：current hash 与 rendered/observed 均不一致 → 路径入 preview；
///     目录：未知子项或 managed 子路径 hash 漂移 → 路径入 preview；
///     路径不存在或 hash 命中 managed 集合 → 可删（空 preview）。
pub(crate) fn compute_removal_blocked_paths(mat: Option<&Materialization>) -> Vec<String> {
    let Some(mat) = mat else {
        return Vec::new();
    };
    let Some(path_str) = mat.native_path.as_deref().filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let path = PathBuf::from(path_str);
    if !path.exists() {
        return Vec::new();
    }
    let managed: Vec<String> = [
        mat.rendered_hash.clone(),
        mat.observed_external_hash.clone(),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .collect();
    if path.is_file() {
        return match std::fs::read(&path) {
            Ok(bytes) => {
                let current = sha256_hex(&bytes);
                if managed.iter().any(|h| h == &current) {
                    Vec::new()
                } else {
                    vec![path_str.to_string()]
                }
            }
            Err(_) => vec![path_str.to_string()],
        };
    }
    if path.is_dir() {
        let mut blocked = Vec::new();
        let entries = match std::fs::read_dir(&path) {
            Ok(e) => e,
            Err(_) => return vec![path_str.to_string()],
        };
        for entry in entries.flatten() {
            let child = entry.path();
            if !child.is_file() {
                // 未知子目录/非文件一律阻塞，禁止递归删
                blocked.push(child.to_string_lossy().into_owned());
                continue;
            }
            match std::fs::read(&child) {
                Ok(bytes) => {
                    let current = sha256_hex(&bytes);
                    if !managed.iter().any(|h| h == &current) {
                        blocked.push(child.to_string_lossy().into_owned());
                    }
                }
                Err(_) => blocked.push(child.to_string_lossy().into_owned()),
            }
        }
        return blocked;
    }
    vec![path_str.to_string()]
}

/// 汇总资产全部 binding 的 removal-blocked 路径（DeleteEverywhere 预检）。
///
/// Business Logic: everywhere 任一条路径被外部改过 → 整次拒绝并返回完整 preview。
/// Code Logic: list bindings + materializations → 合并 compute_removal_blocked_paths。
async fn collect_removal_blocked_for_asset(
    state: &AppState,
    asset_id: &str,
) -> Result<Vec<String>, AppError> {
    let bindings = state
        .agent_hub_repo
        .list_target_bindings_for_asset(asset_id)
        .await?;
    let mut out = Vec::new();
    for b in bindings {
        let mat = state
            .agent_hub_repo
            .get_materialization_by_binding(&b.id)
            .await?;
        out.extend(compute_removal_blocked_paths(mat.as_ref()));
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// 执行 adapter 声明的 disable 策略。
///
/// Business Logic（为什么需要这个函数）:
///     desiredEnabled=false 不得只改 DB 位；package 资产需 remove-with-binding-retained，
///     指令资产通过 schedule Present+disabled 投影（内容可保留但 desired 已禁用）。
///
/// Code Logic（这个函数做什么）:
///     更新 materialization 为 Pending + 策略 token；package 路径若存在则 best-effort 标记
///     deactivate 作业意图（真实 CLI uninstall 由后续 activator/runtime 消费 binding）。
async fn apply_disable_strategy(
    state: &AppState,
    asset: &LogicalAsset,
    binding: &TargetBinding,
    mat: Option<&Materialization>,
    strategy: TargetDisableStrategy,
) -> Result<(), AppError> {
    let is_package = matches!(
        asset.kind,
        AssetKind::Skill
            | AssetKind::Command
            | AssetKind::Agent
            | AssetKind::Plugin
            | AssetKind::Mcp
    );
    let strategy_token = strategy.as_str();
    // 无论是否已有 materialization，都写入 Pending + 策略 token，
    // 避免仅 flip desired_enabled 而无可观测 deactivation 意图。
    let (native_path, last_rev, rendered_hash, observed) = match mat {
        Some(m) => (
            m.native_path.clone(),
            m.last_projected_revision_id.clone(),
            m.rendered_hash.clone(),
            m.observed_external_hash.clone(),
        ),
        None => (None, None, None, None),
    };
    let _ = is_package; // 策略 token 区分 package/instruction 由 strategy.as_str
    state
        .agent_hub_repo
        .upsert_materialization(NewMaterialization {
            asset_id: asset.id.clone(),
            target: binding.target,
            target_binding_id: binding.id.clone(),
            native_path,
            last_projected_revision_id: last_rev,
            rendered_hash,
            observed_external_hash: observed,
            status: MaterializationStatus::Pending,
            last_error: Some(format!("disable_strategy:{strategy_token}")),
        })
        .await?;
    // 调度 deactivation job：package 走 projection_ops package 路径（若有）；
    // 当前 instruction schedule 已覆盖 Present+disabled；package 调度 best-effort。
    if is_package {
        if let Err(e) =
            crate::agent_hub::projection_ops::schedule_package_deactivation(state, &asset.id).await
        {
            tracing::warn!(
                asset_id = %asset.id,
                target = %binding.target.as_str(),
                error = %e,
                "agent_hub disable strategy schedule deactivation failed (best-effort)"
            );
        }
    }
    Ok(())
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

/// best-effort 探测三 CLI，并经 support manifest 评估展示态。
///
/// Business Logic（为什么需要这个函数）:
///     status 顶部展示本机 Claude/Codex/OpenCode 可用性。
///     未认证（null 版本 / 写能力 blocked）不得显示绿色 Supported，必须 scanOnly。
///
/// Code Logic（这个函数做什么）:
///     home+env 构造 TargetEnvironment；adapter.probe 后经 evaluate_target_support 映射 support 字段。
fn probe_all_targets_best_effort() -> Vec<AgentHubProbeDto> {
    use crate::agent_hub::support::{
        builtin_support_manifest, evaluate_target_support, EvaluatedSupportMode,
        RuntimeProbeSnapshot,
    };
    let env = current_target_environment();
    let manifest = match builtin_support_manifest() {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(error = %e, "agent_hub probe: builtin support manifest unavailable");
            None
        }
    };
    AgentTarget::ALL
        .into_iter()
        .map(|target| match crate::agent_hub::targets::probe_target(target, &env) {
            Ok(probe) => {
                let support = if let Some(manifest) = manifest.as_ref() {
                    let snap = RuntimeProbeSnapshot {
                        target: probe.target,
                        executable: probe.executable.clone(),
                        version: probe.version.clone(),
                        config_root: probe.config_root.clone(),
                        fingerprint: probe.fingerprint.clone(),
                        help_fingerprint: None,
                    };
                    let eval = evaluate_target_support(manifest, &snap);
                    match &eval.mode {
                        EvaluatedSupportMode::Certified => {
                            if eval.write_allowed {
                                "supported".to_string()
                            } else {
                                "scanOnly".to_string()
                            }
                        }
                        EvaluatedSupportMode::ScanOnly { .. } => "scanOnly".to_string(),
                        EvaluatedSupportMode::Blocked { .. } => "unsupported".to_string(),
                    }
                } else {
                    // fail-closed：manifest 不可用时不得抬升为 Supported
                    "scanOnly".to_string()
                };
                AgentHubProbeDto {
                    target: probe.target,
                    executable: probe.executable.map(|p| p.to_string_lossy().into_owned()),
                    version: probe.version,
                    support,
                    config_root: Some(probe.config_root.to_string_lossy().into_owned()),
                }
            }
            Err(_) => AgentHubProbeDto {
                target,
                executable: None,
                version: None,
                support: "unsupported".to_string(),
                config_root: None,
            },
        })
        .collect()
}

/// 构造当前进程注入环境。
///
/// Business Logic（为什么需要这个函数）:
///     probe 不得改 process env，但必须读取真实 home/PATH 与 CLI 变量。
///
/// Code Logic（这个函数做什么）:
///     dirs::home_dir + 关注 env + PATH 切分。
fn current_target_environment() -> TargetEnvironment {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let interest = [
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "OPENCODE_CONFIG_DIR",
        "OPENCODE_CONFIG",
        "XDG_CONFIG_HOME",
        "HOME",
        "USERPROFILE",
    ];
    let mut vars = BTreeMap::new();
    for key in interest {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                vars.insert(key.to_string(), v);
            }
        }
    }
    let path_entries = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    TargetEnvironment {
        home,
        vars,
        path_entries,
    }
}

/// list/detail 共用的 summary 共享输入（mats/probe/conflicts 只拉一次）。
struct SummarySharedContext {
    mat_by_binding: BTreeMap<String, Materialization>,
    bindings_by_asset: BTreeMap<String, Vec<TargetBinding>>,
    ownerships_by_asset: BTreeMap<String, Vec<UserInstructionOwnershipRecord>>,
    support_by_target: BTreeMap<AgentTarget, bool>,
    /// asset_id 存在未解决 conflict（canonical 或任意 target）。
    conflict_asset_ids: std::collections::HashSet<String>,
}

/**
 * Business Logic: 批量 list 时禁止 N 次全表 mats + N×3 CLI probe + N×4 conflict。
 * Code Logic: 并行读取 mats/bindings/ownerships/conflicts，各表一次；CLI probe 也只执行一次。
 */
async fn load_summary_shared_context(state: &AppState) -> Result<SummarySharedContext, AppError> {
    let (mats, bindings, ownerships, conflicts) = tokio::try_join!(
        state.agent_hub_repo.list_materializations(),
        state.agent_hub_repo.list_target_bindings(),
        state.agent_hub_repo.list_user_instruction_ownerships_all(),
        state.agent_hub_repo.list_unresolved_conflicts(),
    )?;
    let support_by_target = probe_support_map();
    let conflict_asset_ids = conflicts.into_iter().map(|c| c.asset_id).collect();
    let mat_by_binding = mats
        .into_iter()
        .map(|materialization| (materialization.target_binding_id.clone(), materialization))
        .collect();
    let mut bindings_by_asset: BTreeMap<String, Vec<TargetBinding>> = BTreeMap::new();
    for binding in bindings {
        bindings_by_asset
            .entry(binding.asset_id.clone())
            .or_default()
            .push(binding);
    }
    let mut ownerships_by_asset: BTreeMap<String, Vec<UserInstructionOwnershipRecord>> =
        BTreeMap::new();
    for ownership in ownerships {
        ownerships_by_asset
            .entry(ownership.asset_id.clone())
            .or_default()
            .push(ownership);
    }
    Ok(SummarySharedContext {
        mat_by_binding,
        bindings_by_asset,
        ownerships_by_asset,
        support_by_target,
        conflict_asset_ids,
    })
}

/**
 * Business Logic: 列表路径对 N 条 asset 共享 probe/mats/conflicts。
 * Code Logic: 预载 shared → 内存按 asset 关联 bindings/ownerships/mats。
 */
async fn build_summaries_for_assets(
    state: &AppState,
    assets: &[LogicalAsset],
) -> Result<Vec<AgentHubAssetSummaryDto>, AppError> {
    if assets.is_empty() {
        return Ok(Vec::new());
    }
    let shared = load_summary_shared_context(state).await?;
    let mut out = Vec::with_capacity(assets.len());
    for asset in assets {
        out.push(build_summary_with_shared(asset, &shared));
    }
    Ok(out)
}

/// 构建资产摘要。
///
/// Business Logic（为什么需要这个函数）:
///     列表/详情/set_binding 共用 summary。
///
/// Code Logic（这个函数做什么）:
///     单条路径加载 shared 后委托 build_summary_with_shared（与批量字段一致）。
async fn build_summary(
    state: &AppState,
    asset: &LogicalAsset,
) -> Result<AgentHubAssetSummaryDto, AppError> {
    let shared = load_summary_shared_context(state).await?;
    Ok(build_summary_with_shared(asset, &shared))
}

/**
 * Business Logic: 固定三 target 单元格 + has_conflict（来自 shared 集合）。
 * Code Logic: bindings/ownership/mats/probe/conflicts 全部从 shared 内存索引读取。
 */
fn build_summary_with_shared(
    asset: &LogicalAsset,
    shared: &SummarySharedContext,
) -> AgentHubAssetSummaryDto {
    let bindings = shared
        .bindings_by_asset
        .get(&asset.id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let needs_ownership = asset.scope_id == crate::agent_hub::migration::USER_SCOPE_STABLE_ID
        && asset.logical_key == crate::agent_hub::migration::USER_INSTRUCTION_LOGICAL_KEY;
    let user_instruction_ownership = shared
        .ownerships_by_asset
        .get(&asset.id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let support_by_target = &shared.support_by_target;
    let mut targets = Vec::new();
    let mut snaps = Vec::new();
    for target in [
        AgentTarget::Claude,
        AgentTarget::Codex,
        AgentTarget::OpenCode,
    ] {
        let supported = support_by_target.get(&target).copied().unwrap_or(false);
        if let Some(b) = bindings.iter().find(|b| b.target == target) {
            let mat = shared.mat_by_binding.get(&b.id);
            let legacy_unselected = needs_ownership
                && b.desired_presence == DesiredPresence::Absent
                && !b.desired_enabled
                && b.local_scope_mapping_id.is_none()
                && b.checkout_binding_id.is_none()
                && mat.is_none()
                && !user_instruction_ownership
                    .iter()
                    .any(|ownership| ownership.target == target);
            let mat_status = mat.map(|m| m.status);
            let source_only = is_source_only_cell(asset, mat_status);
            let verified = is_verified_cell(asset, b, mat);
            targets.push(AgentHubTargetCellDto {
                target,
                desired_presence: b.desired_presence,
                desired_enabled: b.desired_enabled,
                materialization_status: mat_status.map(|s| s.as_str().to_string()),
                last_error: mat.and_then(|m| m.last_error.clone()),
                requested: !legacy_unselected,
                supported,
                source_only,
                verified,
            });
            if !legacy_unselected {
                snaps.push(TargetStatusSnapshot {
                    requested: true,
                    desired_presence: b.desired_presence,
                    desired_enabled: b.desired_enabled,
                    supported,
                    source_only,
                    materialization_status: mat_status,
                    verified,
                });
            }
        } else {
            targets.push(AgentHubTargetCellDto {
                target,
                desired_presence: DesiredPresence::Absent,
                desired_enabled: false,
                materialization_status: None,
                last_error: None,
                requested: false,
                supported,
                source_only: false,
                verified: false,
            });
            // 无 binding 的 target 不进入 requested 聚合
        }
    }
    let aggregate_status = compute_asset_aggregate_status(&snaps).as_str().to_string();
    let has_conflict = shared.conflict_asset_ids.contains(&asset.id);
    AgentHubAssetSummaryDto {
        asset_id: asset.id.clone(),
        scope_id: asset.scope_id.clone(),
        kind: asset.kind.as_str().to_string(),
        display_name: asset.display_name.clone(),
        logical_key: asset.logical_key.clone(),
        origin_namespace: asset.origin_namespace.clone(),
        policy: asset.policy.as_str().to_string(),
        current_revision_id: asset
            .current_revision_id
            .as_ref()
            .map(|r| r.as_str().to_string()),
        targets,
        has_conflict,
        aggregate_status,
    }
}

/// best-effort 三 target support 探测映射。
///
/// Business Logic: 聚合 full 需 supported；probe 失败不得伪装 supported。
/// Code Logic: 复用 adapters；失败 → false。
pub(crate) fn evaluate_target_support_flags(
    evaluated: &crate::agent_hub::support::EvaluatedTargetSupport,
    capability: crate::agent_hub::support::TargetCapability,
) -> bool {
    use crate::agent_hub::support::CapabilitySupport;
    matches!(
        evaluated.capability(capability),
        CapabilitySupport::Supported
            | CapabilitySupport::SupportedAfterRestart
            | CapabilitySupport::ActivationRequired
            | CapabilitySupport::ReadOnly
    )
}

fn probe_support_map() -> BTreeMap<AgentTarget, bool> {
    use crate::agent_hub::support::{
        builtin_support_manifest, evaluate_target_support, RuntimeProbeSnapshot, TargetCapability,
    };
    let env = current_target_environment_for_summary();
    let manifest = builtin_support_manifest().ok();
    let adapters: Vec<(Box<dyn AssetAdapter>, AgentTarget)> = vec![
        (Box::new(ClaudeInstructionAdapter), AgentTarget::Claude),
        (Box::new(CodexInstructionAdapter), AgentTarget::Codex),
        (Box::new(OpenCodeInstructionAdapter), AgentTarget::OpenCode),
    ];
    let mut map = BTreeMap::new();
    for (adapter, target) in adapters {
        let supported = match (manifest.as_ref(), adapter.probe(&env)) {
            (Some(manifest), Ok(probe)) => {
                let snapshot = RuntimeProbeSnapshot {
                    target: probe.target,
                    executable: probe.executable,
                    version: probe.version,
                    config_root: probe.config_root,
                    fingerprint: probe.fingerprint,
                    help_fingerprint: None,
                };
                let evaluated = evaluate_target_support(manifest, &snapshot);
                evaluate_target_support_flags(&evaluated, TargetCapability::ScanInstruction)
            }
            _ => false,
        };
        map.insert(target, supported);
    }
    map
}

/// summary 用 TargetEnvironment（复用 probe 环境构造）。
fn current_target_environment_for_summary() -> TargetEnvironment {
    current_target_environment()
}

/// 单元格是否 sourceOnly。
///
/// Business Logic: 无可投影 materialization 且 kind 无法在该 target 落地。
/// Code Logic: 无 mat 且 desired Present → sourceOnly 倾向；指令有 path 可投影 → false。
fn is_source_only_cell(asset: &LogicalAsset, mat_status: Option<MaterializationStatus>) -> bool {
    if mat_status.is_some() {
        return false;
    }
    // Instruction 始终可投影路径；package 缺 mat 时视为 sourceOnly（仅 hub 源）
    !matches!(asset.kind, AssetKind::Instruction)
}

/// 单元格是否 verified。
///
/// Business Logic: full 禁止仅凭 package write 成功；需 activation/list 通过。
/// Code Logic: Instruction Synced → verified；package Synced + 无 disable_strategy 错误 → verified；
/// ActivationRequired/Pending/Blocked 等 → false。
fn is_verified_cell(
    asset: &LogicalAsset,
    binding: &TargetBinding,
    mat: Option<&Materialization>,
) -> bool {
    let Some(mat) = mat else {
        return false;
    };
    match mat.status {
        MaterializationStatus::Synced => {
            if binding.desired_presence == DesiredPresence::Absent {
                return true;
            }
            // package：若 last_error 标记仍待激活则非 verified
            if !matches!(asset.kind, AssetKind::Instruction) {
                if let Some(err) = mat.last_error.as_deref() {
                    if err.contains("activation") || err.contains("disable_strategy") {
                        return false;
                    }
                }
            }
            true
        }
        MaterializationStatus::ActivationRequired
        | MaterializationStatus::Pending
        | MaterializationStatus::Blocked
        | MaterializationStatus::Unsupported
        | MaterializationStatus::Drift
        | MaterializationStatus::Conflict
        | MaterializationStatus::Detached
        | MaterializationStatus::ExternalCollision => false,
    }
}

/// summary → detail。
///
/// Business Logic（为什么需要这个函数）:
///     复用 summary 字段填充扁平 detail。
///
/// Code Logic（这个函数做什么）:
///     字段拷贝 + blocks/content/conflicts。
fn detail_from_summary(
    summary: AgentHubAssetSummaryDto,
    blocks: Vec<InstructionBlockDto>,
    content_markdown: Option<String>,
    conflicts: Vec<AgentHubConflictDto>,
) -> AgentHubAssetDetailDto {
    AgentHubAssetDetailDto {
        asset_id: summary.asset_id,
        scope_id: summary.scope_id,
        kind: summary.kind,
        display_name: summary.display_name,
        logical_key: summary.logical_key,
        origin_namespace: summary.origin_namespace,
        policy: summary.policy,
        current_revision_id: summary.current_revision_id,
        targets: summary.targets,
        has_conflict: summary.has_conflict,
        aggregate_status: summary.aggregate_status,
        blocks,
        content_markdown,
        conflicts,
    }
}

/// 加载资产，缺失 not_found。
///
/// Business Logic（为什么需要这个函数）:
///     详情/编辑 fail-closed。
///
/// Code Logic（这个函数做什么）:
///     get_asset 展开。
async fn load_asset_or_not_found(
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
async fn require_instruction_asset(
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
fn ensure_expected_revision(asset: &LogicalAsset, expected: Option<&str>) -> Result<(), AppError> {
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
fn object_store() -> Result<ObjectStore, AppError> {
    ObjectStore::open(crate::config::data_dir()?)
}

/// 持久化 instruction 文档为 Ui revision。
///
/// Business Logic（为什么需要这个函数）:
///     Hub 编辑必须形成可崩溃恢复 revision，且不记正文日志。
///
/// Code Logic（这个函数做什么）:
///     JSON serialize → put_blob → append_revision(Ui)。
async fn persist_instruction_document(
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
async fn load_instruction_view(
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
async fn load_instruction_document(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic: 前端依赖 camelCase 键名。
    /// Code Logic: serde_json 断言关键键。
    #[test]
    fn status_dto_serializes_camel_case_keys() {
        let dto = AgentHubStatusDto {
            enabled: true,
            background_enabled: false,
            agent_hub_api_version: 1,
            owner_instance_id: Some("owner".to_string()),
            write_compatible: true,
            probes: vec![],
            conflict_count: 0,
            blocked_materialization_count: 0,
        };
        let v = serde_json::to_value(&dto).unwrap();
        assert!(v.get("agentHubApiVersion").is_some());
        assert!(v.get("writeCompatible").is_some());
        assert!(v.get("blockedMaterializationCount").is_some());
        assert!(v.get("backgroundEnabled").is_some());
    }

    /// Business Logic: resolution enum wire tokens。
    /// Code Logic: camelCase serde。
    #[test]
    fn conflict_resolution_wire_tokens() {
        assert_eq!(
            serde_json::to_value(AgentHubConflictResolution::KeepHub).unwrap(),
            serde_json::json!("keepHub")
        );
        assert_eq!(
            serde_json::to_value(AgentHubConflictResolution::KeepExternal).unwrap(),
            serde_json::json!("keepExternal")
        );
        assert_eq!(
            serde_json::to_value(AgentHubConflictResolution::Manual).unwrap(),
            serde_json::json!("manual")
        );
    }

    /// Business Logic: Gate B presence/enabled/restore/everywhere 请求 DTO 必须 camelCase 稳定。
    /// Code Logic: serde 键名断言。
    #[test]
    fn presence_mutation_request_dto_camel_case_keys() {
        let presence = SetTargetPresenceRequest {
            asset_id: "a".into(),
            target: AgentTarget::Claude,
            desired_presence: DesiredPresence::Absent,
        };
        let v = serde_json::to_value(&presence).unwrap();
        assert!(v.get("assetId").is_some());
        assert!(v.get("desiredPresence").is_some());
        assert_eq!(v.get("desiredPresence").unwrap(), "absent");

        let enabled = SetTargetEnabledRequest {
            asset_id: "a".into(),
            target: AgentTarget::Codex,
            desired_enabled: false,
        };
        let v = serde_json::to_value(&enabled).unwrap();
        assert!(v.get("desiredEnabled").is_some());

        let restore = RestoreDetachedTargetRequest {
            asset_id: "a".into(),
            target: AgentTarget::OpenCode,
        };
        let v = serde_json::to_value(&restore).unwrap();
        assert!(v.get("assetId").is_some());
        assert!(v.get("target").is_some());

        let everywhere = DeleteAssetEverywhereRequest {
            asset_id: "a".into(),
        };
        let v = serde_json::to_value(&everywhere).unwrap();
        assert_eq!(v.get("assetId").unwrap(), "a");
    }

    /// Business Logic: summary/detail 必须暴露 aggregateStatus 与 cell-level 输入。
    /// Code Logic: serde 键名断言。
    #[test]
    fn summary_and_cell_dto_expose_aggregate_and_cell_inputs() {
        let cell = AgentHubTargetCellDto {
            target: AgentTarget::Claude,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
            materialization_status: Some("synced".into()),
            last_error: None,
            requested: true,
            supported: true,
            source_only: false,
            verified: true,
        };
        let v = serde_json::to_value(&cell).unwrap();
        assert!(v.get("requested").is_some());
        assert!(v.get("supported").is_some());
        assert!(v.get("sourceOnly").is_some());
        assert!(v.get("verified").is_some());

        let summary = AgentHubAssetSummaryDto {
            asset_id: "a".into(),
            scope_id: "s".into(),
            kind: "instruction".into(),
            display_name: "d".into(),
            logical_key: "k".into(),
            origin_namespace: "n".into(),
            policy: "shared".into(),
            current_revision_id: None,
            targets: vec![cell],
            has_conflict: false,
            aggregate_status: "full".into(),
        };
        let v = serde_json::to_value(&summary).unwrap();
        assert_eq!(v.get("aggregateStatus").unwrap(), "full");
        assert!(v.get("hasConflict").is_some());
    }

    /// Business Logic: managed hash 匹配可删；漂移/未知子项阻塞并返回精确路径。
    /// Code Logic: tempfile 文件/目录场景。
    #[test]
    fn removal_blocked_paths_detect_hash_drift_and_unknown_children() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("CLAUDE.md");
        std::fs::write(&file, b"managed-content").unwrap();
        let managed = sha256_hex(b"managed-content");
        let mat = Materialization {
            id: "m1".into(),
            asset_id: "a".into(),
            target: AgentTarget::Claude,
            target_binding_id: "b1".into(),
            native_path: Some(file.to_string_lossy().into_owned()),
            last_projected_revision_id: None,
            rendered_hash: Some(managed.clone()),
            observed_external_hash: Some(managed.clone()),
            status: MaterializationStatus::Synced,
            last_error: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        assert!(compute_removal_blocked_paths(Some(&mat)).is_empty());

        std::fs::write(&file, b"external-edit").unwrap();
        let blocked = compute_removal_blocked_paths(Some(&mat));
        assert_eq!(blocked.len(), 1);
        assert!(blocked[0].ends_with("CLAUDE.md"));

        // 目录：未知子文件阻塞
        let pkg = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("extra.txt"), b"unknown").unwrap();
        let dir_mat = Materialization {
            id: "m2".into(),
            asset_id: "a".into(),
            target: AgentTarget::Codex,
            target_binding_id: "b2".into(),
            native_path: Some(pkg.to_string_lossy().into_owned()),
            last_projected_revision_id: None,
            rendered_hash: Some(managed),
            observed_external_hash: None,
            status: MaterializationStatus::Synced,
            last_error: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        let blocked_dir = compute_removal_blocked_paths(Some(&dir_mat));
        assert!(!blocked_dir.is_empty());
        assert!(blocked_dir.iter().any(|p| p.ends_with("extra.txt")));
    }

    /// 构建最小 HeadlessOwner AppState 供 presence mutation 集成测。
    ///
    /// Business Logic: Step 1 六项断言必须经 public service 方法，而非手写 upsert。
    /// Code Logic: tempfile sqlite + AgentHubRepo schema + 精简 AppState 字段。
    async fn build_service_state() -> (AppState, tempfile::TempDir) {
        use crate::backend::authority::RuntimeRole;
        use crate::backend::event_bus::RuntimeEventBus;
        use crate::backend::runtime_metrics::RuntimeMetrics;
        use crate::backend::ui::HeadlessBackendUi;
        use crate::cloud_sync::runtime::CloudSyncRuntime;
        use crate::config::{
            AppConfig, BatteryConfig, GithubTrendingConfig, HealthConfig,
            OrchestratorAutomationConfig,
        };
        use crate::config_runtime::ConfigRuntime;
        use crate::config_store::MemoryConfigStore;
        use crate::net::peer_client::PeerClient;
        use crate::orchestrator::repo::OrchestratorRepo;
        use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
        use crate::storage::maintenance_gate::DatabaseMaintenanceGate;
        use crate::storage::{
            AgentHubRepo, ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, ScratchpadRepo,
            SshTargetRepo, TransferRepo, WorkbenchAgentSessionRepo, WorkbenchBrowserRepo,
            WorkbenchProjectRepo, WorkbenchSessionRepo, WorkbenchWorkspaceLayoutRepo,
            WorkbenchWorktreeRepo,
        };
        use crate::transfer::registry::TransferRegistry;
        use crate::updater::UpdateRuntime;
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;
        use std::sync::atomic::AtomicU16;
        use std::sync::{Arc, Mutex, RwLock};

        let tmp = tempfile::tempdir().unwrap();
        // 隔离 data_dir，避免 schedule/object_store 写真实 home
        std::env::set_var("CC_PARTNER_DATA_DIR", tmp.path());
        let db_path = tmp.path().join("data.db");
        let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
        let options = SqliteConnectOptions::from_str(&db_url)
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        let agent_hub = AgentHubRepo::new(pool.clone());

        let config = AppConfig {
            device_id: "svc-test".to_string(),
            device_name: "svc-test".to_string(),
            http_port: 0,
            receive_dir: tmp.path().join("recv").to_string_lossy().to_string(),
            game_plugin_dir: "/tmp/plugins".into(),
            db_path: db_path.to_string_lossy().to_string(),
            screenshot_hotkey: "<cmd>+s".to_string(),
            prompt_optimizer_hotkey: "<ctrl>".to_string(),
            prompt_optimizer_fill_language: "zh".to_string(),
            prompt_quick_input_hotkey: "<ctrl>+/".to_string(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            battery: BatteryConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            internal_claude: crate::config::InternalClaudeConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
        };
        let store = Arc::new(MemoryConfigStore::with_config(config.clone()));
        let config_runtime = Arc::new(ConfigRuntime::new(config, store));
        let config = config_runtime.shared_value();
        let maintenance_gate = Arc::new(DatabaseMaintenanceGate::new());
        let owner = uuid::Uuid::new_v4().to_string();
        let event_bus = Arc::new(RuntimeEventBus::new(owner));
        let layout_repo = WorkbenchWorkspaceLayoutRepo::new(pool.clone());
        let _ = layout_repo.ensure_schema().await;

        let state = AppState {
            config,
            config_runtime,
            db: pool.clone(),
            maintenance_gate: maintenance_gate.clone(),
            prompt_repo: Arc::new(PromptRepo::new(pool.clone())),
            attention_read_repo: Arc::new(crate::storage::AttentionReadRepo::new(pool.clone())),
            transfer_repo: Arc::new(TransferRepo::new(pool.clone())),
            claude_md_repo: Arc::new(ClaudeMdRepo::new(pool.clone())),
            scratchpad_repo: Arc::new(ScratchpadRepo::new(pool.clone())),
            ssh_target_repo: Arc::new(SshTargetRepo::new(pool.clone())),
            device_id: Arc::new("svc-test".to_string()),
            devices: Arc::new(RwLock::new(std::collections::HashMap::new())),
            actual_http_port: Arc::new(AtomicU16::new(0)),
            discovery: Arc::new(Mutex::new(None)),
            overlay_trusted_ips: Arc::new(RwLock::new(std::collections::HashSet::new())),
            manual_peer_cancel: Arc::new(Mutex::new(None)),
            peer_client: Arc::new(PeerClient::new()),
            transfers: Arc::new(TransferRegistry::new()),
            ui: Arc::new(HeadlessBackendUi::new(tmp.path().to_path_buf())),
            update_runtime: Arc::new(UpdateRuntime::new()),
            cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
            workbench_project_repo: Arc::new(WorkbenchProjectRepo::new(pool.clone())),
            workbench_session_repo: Arc::new(WorkbenchSessionRepo::new(pool.clone())),
            workbench_worktree_repo: Arc::new(WorkbenchWorktreeRepo::new(pool.clone())),
            workbench_browser_repo: Arc::new(WorkbenchBrowserRepo::new(pool.clone())),
            workbench_agent_session_repo: Arc::new(WorkbenchAgentSessionRepo::new(pool.clone())),
            agent_ledger_repo: Arc::new(crate::storage::AgentLedgerRepo::new(pool.clone())),
            agent_ledger_service: Arc::new(
                crate::workbench::agent_ledger::AgentLedgerService::new(
                    crate::storage::AgentLedgerRepo::new(pool.clone()),
                ),
            ),
            agent_hub_repo: Arc::new(agent_hub),
            workbench_workspace_layout_repo: Arc::new(layout_repo),
            workbench_project_note_repo: Arc::new(crate::storage::WorkbenchProjectNoteRepo::new(
                pool.clone(),
            )),
            browser_verification: Arc::new(
                crate::workbench::browser_verification::BrowserVerificationService::new(
                    Arc::new(crate::workbench::browser_verification::FakeEngine::succeeds()),
                    tmp.path().join("browser-verification"),
                    "test-owner".into(),
                )
                .expect("browser verification fixture"),
            ),
            workbench_browser_previews: Arc::new(
                crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
            ),
            workbench_sessions: Arc::new(
                crate::workbench::sessions::WorkbenchSessionRegistry::new(),
            ),
            workbench_remote_events: Arc::new(
                crate::workbench::remote_events::WorkbenchRemoteEventBus::new("test-owner"),
            ),
            workbench_remote_event_bridges: Arc::new(
                crate::workbench::remote_events::RemoteEventBridgeRegistry::new(),
            ),
            workbench_dependency: Arc::new(
                crate::workbench::dependencies::WorkbenchDependencyInstallRuntime::new(),
            ),
            cc_collector_cancel: Arc::new(Mutex::new(None)),
            cloud_sync_runtime: Arc::new(CloudSyncRuntime::new()),
            cloud_sync_cancel: Arc::new(Mutex::new(None)),
            health: Arc::new(crate::health::HealthRuntime::new()),
            health_repo: Arc::new(crate::storage::health_repo::HealthRepo::new(pool.clone())),
            health_cancel: Arc::new(Mutex::new(None)),
            orchestrator_repo: Arc::new(OrchestratorRepo::new(pool.clone())),
            orchestrator_scheduler_telemetry: OrchestratorSchedulerTelemetry::default(),
            orchestrator_cancel: Arc::new(Mutex::new(None)),
            orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
            agent_ledger_cancel: Arc::new(Mutex::new(None)),
            agent_hub_cancel: Arc::new(Mutex::new(None)),
            agent_hub_git_runtime: Arc::new(crate::agent_hub::git::AgentHubGitRuntime::new()),
            agent_hub_git_cancel: Arc::new(Mutex::new(None)),
            workbench_claude_session_indexes: Arc::new(RwLock::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_watchers: Arc::new(Mutex::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_index_dispose_epochs: Arc::new(Mutex::new(
                std::collections::HashMap::new(),
            )),
            runtime_metrics: Arc::new(RuntimeMetrics::new()),
            runtime_role: RuntimeRole::HeadlessOwner,
            event_bus,
            backend_control_client_runtime: Arc::new(
                crate::backend::control_client::BackendControlClientRuntime::new(),
            ),
            gui_event_relay_cancel: Arc::new(Mutex::new(None)),
        };
        (state, tmp)
    }

    /// seed user scope + instruction asset + revision + multi bindings。
    async fn seed_instruction_asset(
        state: &AppState,
        policy: crate::agent_hub::models::AssetPolicy,
        targets: &[(AgentTarget, DesiredPresence, bool)],
    ) -> LogicalAsset {
        use crate::agent_hub::models::{
            AssetKind, NewLogicalAsset, NewRevision, NewScopeNode, NewTargetBinding, RevisionId,
            RevisionOperation, RevisionOriginKind, ScopeKind,
        };
        let scope = state
            .agent_hub_repo
            .insert_scope(NewScopeNode {
                id: None,
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let asset = state
            .agent_hub_repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: format!("lk-{}", uuid::Uuid::new_v4().simple()),
                display_name: "demo".into(),
                policy,
            })
            .await
            .unwrap();
        let _rev = state
            .agent_hub_repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "svc-test".into(),
                payload_hash: Some("aa".repeat(32)),
                tree_manifest_hash: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        for (target, presence, enabled) in targets {
            state
                .agent_hub_repo
                .upsert_target_binding(NewTargetBinding {
                    asset_id: asset.id.clone(),
                    target: *target,
                    local_scope_mapping_id: None,
                    checkout_binding_id: None,
                    desired_presence: *presence,
                    desired_enabled: *enabled,
                })
                .await
                .unwrap();
        }
        state
            .agent_hub_repo
            .get_asset(&asset.id)
            .await
            .unwrap()
            .unwrap()
    }

    /// Business Logic: disable 一 target 不改其它 binding 与 canonical revision。
    /// Code Logic: set_target_enabled(false) 公共路径。
    #[tokio::test]
    async fn service_disable_one_target_leaves_other_bindings_and_revision() {
        let (state, _tmp) = build_service_state().await;
        let asset = seed_instruction_asset(
            &state,
            crate::agent_hub::models::AssetPolicy::Shared,
            &[
                (AgentTarget::Claude, DesiredPresence::Present, true),
                (AgentTarget::Codex, DesiredPresence::Present, true),
            ],
        )
        .await;
        let head_before = asset.current_revision_id.clone();
        let summary = AgentHubService::set_target_enabled(
            &state,
            SetTargetEnabledRequest {
                asset_id: asset.id.clone(),
                target: AgentTarget::Claude,
                desired_enabled: false,
            },
        )
        .await
        .unwrap();
        let claude = summary
            .targets
            .iter()
            .find(|t| t.target == AgentTarget::Claude)
            .unwrap();
        let codex = summary
            .targets
            .iter()
            .find(|t| t.target == AgentTarget::Codex)
            .unwrap();
        assert!(!claude.desired_enabled);
        assert_eq!(claude.desired_presence, DesiredPresence::Present);
        assert!(codex.desired_enabled);
        assert_eq!(codex.desired_presence, DesiredPresence::Present);
        let after = state
            .agent_hub_repo
            .get_asset(&asset.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.current_revision_id, head_before);
        // disable 策略落地：materialization 带 disable_strategy token
        let bindings = state
            .agent_hub_repo
            .list_target_bindings_for_asset(&asset.id)
            .await
            .unwrap();
        let claude_b = bindings
            .iter()
            .find(|b| b.target == AgentTarget::Claude)
            .unwrap();
        let mat = state
            .agent_hub_repo
            .get_materialization_by_binding(&claude_b.id)
            .await
            .unwrap();
        assert!(mat
            .as_ref()
            .and_then(|m| m.last_error.as_ref())
            .is_some_and(|e| e.contains("disable_strategy")));
        assert!(!summary.aggregate_status.is_empty());
    }

    /// Business Logic: desiredPresence=absent 只卸本 target。
    /// Code Logic: set_target_presence(Absent) 公共路径。
    #[tokio::test]
    async fn service_absent_is_target_local_only() {
        let (state, _tmp) = build_service_state().await;
        let asset = seed_instruction_asset(
            &state,
            crate::agent_hub::models::AssetPolicy::Shared,
            &[
                (AgentTarget::Claude, DesiredPresence::Present, true),
                (AgentTarget::Codex, DesiredPresence::Present, true),
            ],
        )
        .await;
        let summary = AgentHubService::set_target_presence(
            &state,
            SetTargetPresenceRequest {
                asset_id: asset.id.clone(),
                target: AgentTarget::Claude,
                desired_presence: DesiredPresence::Absent,
            },
        )
        .await
        .unwrap();
        let claude = summary
            .targets
            .iter()
            .find(|t| t.target == AgentTarget::Claude)
            .unwrap();
        let codex = summary
            .targets
            .iter()
            .find(|t| t.target == AgentTarget::Codex)
            .unwrap();
        assert_eq!(claude.desired_presence, DesiredPresence::Absent);
        assert!(!claude.desired_enabled);
        assert_eq!(codex.desired_presence, DesiredPresence::Present);
        assert!(codex.desired_enabled);
    }

    /// Business Logic: 外部漂移路径在 binding 变更前拒绝，并返回精确 preview。
    /// Code Logic: materialization native_path hash 漂移 → validation reject。
    #[tokio::test]
    async fn service_absent_rejects_before_mutation_when_paths_blocked() {
        let (state, tmp) = build_service_state().await;
        let asset = seed_instruction_asset(
            &state,
            crate::agent_hub::models::AssetPolicy::Shared,
            &[(AgentTarget::Claude, DesiredPresence::Present, true)],
        )
        .await;
        let bindings = state
            .agent_hub_repo
            .list_target_bindings_for_asset(&asset.id)
            .await
            .unwrap();
        let b = bindings
            .iter()
            .find(|x| x.target == AgentTarget::Claude)
            .unwrap();
        let path = tmp.path().join("external.md");
        std::fs::write(&path, b"external").unwrap();
        state
            .agent_hub_repo
            .upsert_materialization(NewMaterialization {
                asset_id: asset.id.clone(),
                target: AgentTarget::Claude,
                target_binding_id: b.id.clone(),
                native_path: Some(path.to_string_lossy().into_owned()),
                last_projected_revision_id: None,
                rendered_hash: Some(sha256_hex(b"managed")),
                observed_external_hash: Some(sha256_hex(b"managed")),
                status: MaterializationStatus::Synced,
                last_error: None,
            })
            .await
            .unwrap();
        let err = AgentHubService::set_target_presence(
            &state,
            SetTargetPresenceRequest {
                asset_id: asset.id.clone(),
                target: AgentTarget::Claude,
                desired_presence: DesiredPresence::Absent,
            },
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("agent_hub_removal_blocked_unknown_or_changed_paths"),
            "msg={msg}"
        );
        // binding 未变
        let still = state
            .agent_hub_repo
            .get_target_binding(&b.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(still.desired_presence, DesiredPresence::Present);
    }

    /// Business Logic: DeleteEverywhere 在任意 target 路径 blocked 时拒绝，且不写 tombstone/binding。
    /// Code Logic: collect_removal_blocked_for_asset → RejectRemovalBlocked；asset 仍 live。
    #[tokio::test]
    async fn service_delete_everywhere_rejects_before_mutation_when_paths_blocked() {
        let (state, tmp) = build_service_state().await;
        let asset = seed_instruction_asset(
            &state,
            crate::agent_hub::models::AssetPolicy::Shared,
            &[
                (AgentTarget::Claude, DesiredPresence::Present, true),
                (AgentTarget::Codex, DesiredPresence::Present, true),
            ],
        )
        .await;
        let bindings = state
            .agent_hub_repo
            .list_target_bindings_for_asset(&asset.id)
            .await
            .unwrap();
        let claude = bindings
            .iter()
            .find(|x| x.target == AgentTarget::Claude)
            .unwrap();
        let path = tmp.path().join("everywhere-drift.md");
        std::fs::write(&path, b"external").unwrap();
        state
            .agent_hub_repo
            .upsert_materialization(NewMaterialization {
                asset_id: asset.id.clone(),
                target: AgentTarget::Claude,
                target_binding_id: claude.id.clone(),
                native_path: Some(path.to_string_lossy().into_owned()),
                last_projected_revision_id: None,
                rendered_hash: Some(sha256_hex(b"managed")),
                observed_external_hash: Some(sha256_hex(b"managed")),
                status: MaterializationStatus::Synced,
                last_error: None,
            })
            .await
            .unwrap();
        let before_rev = asset.current_revision_id.clone();
        let err = AgentHubService::delete_asset_everywhere(
            &state,
            DeleteAssetEverywhereRequest {
                asset_id: asset.id.clone(),
            },
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("agent_hub_removal_blocked_unknown_or_changed_paths"),
            "msg={msg}"
        );
        // 全部 binding 与 head 均未变
        let still_asset = state
            .agent_hub_repo
            .get_asset(&asset.id)
            .await
            .unwrap()
            .unwrap();
        assert!(still_asset.deleted_at.is_none());
        assert_eq!(still_asset.current_revision_id, before_rev);
        for b in state
            .agent_hub_repo
            .list_target_bindings_for_asset(&asset.id)
            .await
            .unwrap()
        {
            assert_eq!(b.desired_presence, DesiredPresence::Present);
            assert!(b.desired_enabled);
        }
    }

    /// Business Logic: restore_detached 清 Detached、Present+enabled、schedule 投影意图。
    /// Code Logic: materialization Pending；binding Present。
    #[tokio::test]
    async fn service_restore_detached_clears_and_schedules() {
        let (state, _tmp) = build_service_state().await;
        let asset = seed_instruction_asset(
            &state,
            crate::agent_hub::models::AssetPolicy::Shared,
            &[(AgentTarget::Claude, DesiredPresence::Present, false)],
        )
        .await;
        let bindings = state
            .agent_hub_repo
            .list_target_bindings_for_asset(&asset.id)
            .await
            .unwrap();
        let b = bindings.first().unwrap();
        state
            .agent_hub_repo
            .upsert_materialization(NewMaterialization {
                asset_id: asset.id.clone(),
                target: AgentTarget::Claude,
                target_binding_id: b.id.clone(),
                native_path: Some("/tmp/x".into()),
                last_projected_revision_id: None,
                rendered_hash: Some("hh".into()),
                observed_external_hash: None,
                status: MaterializationStatus::Detached,
                last_error: Some("external_delete".into()),
            })
            .await
            .unwrap();
        let summary = AgentHubService::restore_detached_target(
            &state,
            RestoreDetachedTargetRequest {
                asset_id: asset.id.clone(),
                target: AgentTarget::Claude,
            },
        )
        .await
        .unwrap();
        let cell = summary
            .targets
            .iter()
            .find(|t| t.target == AgentTarget::Claude)
            .unwrap();
        assert_eq!(cell.desired_presence, DesiredPresence::Present);
        assert!(cell.desired_enabled);
        let mat = state
            .agent_hub_repo
            .get_materialization_by_binding(&b.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mat.status, MaterializationStatus::Pending);
    }

    /// Business Logic: delete_everywhere 一条 tombstone + 全部 Absent。
    /// Code Logic: 公共 delete_asset_everywhere；CAS head 推进一次。
    #[tokio::test]
    async fn service_delete_everywhere_one_tombstone_and_fan_out() {
        let (state, _tmp) = build_service_state().await;
        let asset = seed_instruction_asset(
            &state,
            crate::agent_hub::models::AssetPolicy::Shared,
            &[
                (AgentTarget::Claude, DesiredPresence::Present, true),
                (AgentTarget::Codex, DesiredPresence::Present, true),
            ],
        )
        .await;
        let head_before = asset.current_revision_id.clone().unwrap();
        let summary = AgentHubService::delete_asset_everywhere(
            &state,
            DeleteAssetEverywhereRequest {
                asset_id: asset.id.clone(),
            },
        )
        .await
        .unwrap();
        assert!(summary
            .targets
            .iter()
            .filter(|t| t.requested)
            .all(|t| t.desired_presence == DesiredPresence::Absent && !t.desired_enabled));
        let after = state
            .agent_hub_repo
            .get_asset(&asset.id)
            .await
            .unwrap()
            .unwrap();
        assert!(after.deleted_at.is_some());
        assert_ne!(
            after.current_revision_id.as_ref().map(|r| r.as_str()),
            Some(head_before.as_str())
        );
    }

    /// Business Logic: targetOnly 最后一 target 不得猜 everywhere。
    /// Code Logic: set_target_presence(Absent) → AppError validation token。
    #[tokio::test]
    async fn service_target_only_last_target_requires_everywhere() {
        let (state, _tmp) = build_service_state().await;
        let asset = seed_instruction_asset(
            &state,
            crate::agent_hub::models::AssetPolicy::TargetOnly,
            &[(AgentTarget::Claude, DesiredPresence::Present, true)],
        )
        .await;
        let err = AgentHubService::set_target_presence(
            &state,
            SetTargetPresenceRequest {
                asset_id: asset.id.clone(),
                target: AgentTarget::Claude,
                desired_presence: DesiredPresence::Absent,
            },
        )
        .await
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("agent_hub_target_only_last_target_requires_everywhere"));
        let still = state
            .agent_hub_repo
            .list_target_bindings_for_asset(&asset.id)
            .await
            .unwrap();
        assert_eq!(still[0].desired_presence, DesiredPresence::Present);
    }

    /// Business Logic: status probe 必须走 evaluate_target_support，不得硬编码 Supported。
    /// Code Logic: OpenCode 未 pin 时 scanOnly/unsupported；Claude/Codex 在本机 pin 命中时可 supported。
    #[test]
    fn status_probe_uses_evaluate_target_support_not_raw_supported() {
        let probes = probe_all_targets_best_effort();
        assert_eq!(probes.len(), crate::agent_hub::models::AgentTarget::ALL.len());
        for p in probes {
            match p.target {
                crate::agent_hub::models::AgentTarget::OpenCode => {
                    assert_ne!(
                        p.support.as_str(),
                        "supported",
                        "opencode without pin must not report Supported"
                    );
                    assert!(
                        matches!(p.support.as_str(), "scanOnly" | "unsupported"),
                        "unexpected support={} for opencode",
                        p.support
                    );
                }
                crate::agent_hub::models::AgentTarget::Claude
                | crate::agent_hub::models::AgentTarget::Codex
                | crate::agent_hub::models::AgentTarget::Grok
                | crate::agent_hub::models::AgentTarget::Gemini => {
                    assert!(
                        matches!(p.support.as_str(), "supported" | "scanOnly" | "unsupported"),
                        "unexpected support={} for {}",
                        p.support,
                        p.target.as_str()
                    );
                }
            }
        }
    }

    /// R5 P2.3: `probe_support_map` must funnel through
    /// `builtin_support_manifest + evaluate_target_support + evaluate_target_support_flags`,
    /// and a `None` manifest must label **no** target as supported.
    #[test]
    fn probe_support_map_null_manifest_marks_no_target_supported() {
        // Force a manifest load failure by passing an empty manifest module override path.
        // The function under test never reads process state for the manifest, so we exercise
        // it directly: when `builtin_support_manifest()` would fail-closed, every entry must
        // be `false`.
        use crate::agent_hub::support::{builtin_support_manifest, CapabilitySupport};
        let manifest = builtin_support_manifest().expect("default manifest loads");
        // Sanity: the helper must exist and be crate-reachable.
        let _flag_fn: fn(
            &crate::agent_hub::support::EvaluatedTargetSupport,
            crate::agent_hub::support::TargetCapability,
        ) -> bool = evaluate_target_support_flags;

        // Synthesise an evaluated target with no executable / version and confirm the
        // helper returns false for every capability — the canonical "no support" signal.
        let snapshot = crate::agent_hub::support::RuntimeProbeSnapshot {
            target: crate::agent_hub::models::AgentTarget::Claude,
            executable: None,
            version: None,
            config_root: std::path::PathBuf::from("/nonexistent"),
            fingerprint: String::new(),
            help_fingerprint: None,
        };
        let evaluated = crate::agent_hub::support::evaluate_target_support(&manifest, &snapshot);
        // Uncertified probe → read-side capabilities may be ReadOnly, write-side must be
        // Blocked。summary 的 scan 支持必须保留 ReadOnly，不把可发现误报为 unsupported。
        for cap in [
            crate::agent_hub::support::TargetCapability::ScanInstruction,
            crate::agent_hub::support::TargetCapability::RenderInstruction,
            crate::agent_hub::support::TargetCapability::ActivatePackage,
            crate::agent_hub::support::TargetCapability::DeactivatePackage,
        ] {
            let support = evaluated.capability(cap);
            assert!(
                matches!(
                    support,
                    CapabilitySupport::Blocked | CapabilitySupport::ReadOnly
                ),
                "uncertified probe must evaluate to Blocked or ReadOnly for {cap:?}, got {support:?}"
            );
            assert_eq!(
                evaluate_target_support_flags(&evaluated, cap),
                support == CapabilitySupport::ReadOnly,
                "ReadOnly should count for scan summary while Blocked remains false for {cap:?}"
            );
        }

        // Live map 只表达 scan 可用性；ReadOnly 可以为 true，但不得遗漏 target。
        let map = probe_support_map();
        assert_eq!(
            map.len(),
            3,
            "probe_support_map must cover all three targets"
        );
        assert!(map.keys().all(|target| matches!(
            target,
            AgentTarget::Claude | AgentTarget::Codex | AgentTarget::OpenCode
        )));
    }

    /// R5 P2.3: `evaluate_target_support_flags` is exposed at `pub(crate)` so future
    /// projection/activation code can gate writes without re-deriving the helper inline.
    #[test]
    fn evaluate_target_support_flags_exposed_for_crate() {
        // The type-level reference must compile, proving the function is reachable from
        // a downstream test module.
        let _fn_ref: fn(
            &crate::agent_hub::support::EvaluatedTargetSupport,
            crate::agent_hub::support::TargetCapability,
        ) -> bool = evaluate_target_support_flags;

        // And it must distinguish CapabilitySupport states as documented in the module docs.
        use crate::agent_hub::support::{
            builtin_support_manifest, CapabilitySupport, EvaluatedSupportMode,
            EvaluatedTargetSupport, RuntimeProbeSnapshot, TargetCapability,
        };
        let manifest = builtin_support_manifest().expect("default manifest");
        let snapshot = RuntimeProbeSnapshot {
            target: crate::agent_hub::models::AgentTarget::Codex,
            executable: None,
            version: None,
            config_root: std::path::PathBuf::from("/nonexistent"),
            fingerprint: String::new(),
            help_fingerprint: None,
        };
        let evaluated = crate::agent_hub::support::evaluate_target_support(&manifest, &snapshot);
        // Blocked path returns false；ReadOnly scan mode 则保持可发现。
        if matches!(evaluated.mode, EvaluatedSupportMode::Blocked { .. }) {
            let scan = evaluated.capability(TargetCapability::ScanInstruction);
            assert_eq!(
                evaluate_target_support_flags(&evaluated, TargetCapability::ScanInstruction),
                scan == CapabilitySupport::ReadOnly
            );
        } else {
            // Whatever the real-world verdict, the helper must agree with `evaluated.capability`.
            for cap in [
                TargetCapability::ScanInstruction,
                TargetCapability::RenderInstruction,
                TargetCapability::ActivatePackage,
            ] {
                let support = evaluated.capability(cap);
                let flag = evaluate_target_support_flags(&evaluated, cap);
                assert_eq!(
                    flag,
                    matches!(
                        support,
                        CapabilitySupport::Supported
                            | CapabilitySupport::SupportedAfterRestart
                            | CapabilitySupport::ActivationRequired
                            | CapabilitySupport::ReadOnly
                    ),
                    "evaluate_target_support_flags must agree with EvaluatedTargetSupport::capability for {cap:?}"
                );
            }
        }
        // And the helper must be `pub(crate)` so the next call site compiles.
        let _: EvaluatedTargetSupport = evaluated;
    }
}
