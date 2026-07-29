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
    AgentTarget, AssetKind, DesiredPresence, LogicalAsset, Materialization, MaterializationStatus,
    NewMaterialization, NewRevision, NewTargetBinding, RevisionId, RevisionOperation,
    RevisionOriginKind, TargetBinding, TargetBindingIntent, TargetBindingTransition,
};
use crate::agent_hub::object_store::ObjectStore;
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
///     列表按 Claude/Codex/OpenCode 三列展示 desired 与 materialization。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 单元格（无 binding id，供前端矩阵）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubTargetCellDto {
    pub target: AgentTarget,
    pub desired_presence: DesiredPresence,
    pub desired_enabled: bool,
    pub materialization_status: Option<String>,
    pub last_error: Option<String>,
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
///     Hub 列表展示 logical asset 与 target 单元格。
///
/// Code Logic（这个结构体做什么）:
///     camelCase summary + hasConflict。
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
///     选中资产后展示矩阵、正文、块与冲突。
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
    let removal_blocked: Vec<String> = Vec::new();
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
            schedule_projection,
            ..
        } => {
            state
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
            if append_canonical_tombstone {
                append_delete_tombstone(state, &asset).await?;
            }
            if fan_out_absent {
                let all = state
                    .agent_hub_repo
                    .list_target_bindings_for_asset(&asset.id)
                    .await?;
                if all.is_empty() {
                    // 确保至少把入口 target 标 absent
                    state
                        .agent_hub_repo
                        .upsert_target_binding(NewTargetBinding {
                            asset_id: asset.id.clone(),
                            target,
                            local_scope_mapping_id: None,
                            checkout_binding_id: None,
                            desired_presence: DesiredPresence::Absent,
                            desired_enabled: false,
                        })
                        .await?;
                } else {
                    for b in all {
                        state
                            .agent_hub_repo
                            .upsert_target_binding(NewTargetBinding {
                                asset_id: asset.id.clone(),
                                target: b.target,
                                local_scope_mapping_id: b.local_scope_mapping_id,
                                checkout_binding_id: b.checkout_binding_id,
                                desired_presence: DesiredPresence::Absent,
                                desired_enabled: false,
                            })
                            .await?;
                    }
                }
                schedule_after_binding_change(state, &asset.id).await;
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

/// 追加一条 delete tombstone revision 并推进 head。
///
/// Business Logic: delete_everywhere 只能生成一条 canonical tombstone。
/// Code Logic: append_revision(Delete, Ui) + expected_parent head CAS。
async fn append_delete_tombstone(state: &AppState, asset: &LogicalAsset) -> Result<(), AppError> {
    let now = chrono::Utc::now().to_rfc3339();
    let parents = asset
        .current_revision_id
        .clone()
        .into_iter()
        .collect::<Vec<_>>();
    let expected_parent_id = asset.current_revision_id.clone();
    state
        .agent_hub_repo
        .append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset.id.clone(),
            parents,
            operation: RevisionOperation::Delete,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: state.device_id.as_str().to_string(),
            payload_hash: None,
            tree_manifest_hash: None,
            created_at: now,
            expected_parent_id,
        })
        .await?;
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
///     repo.list_assets + build_summary。
pub async fn list_assets_for_state(
    state: &AppState,
    scope_id: Option<&str>,
    kind: Option<AssetKind>,
) -> Result<Vec<AgentHubAssetSummaryDto>, AppError> {
    let assets = state.agent_hub_repo.list_assets(scope_id, kind).await?;
    let mut out = Vec::with_capacity(assets.len());
    for asset in assets {
        out.push(build_summary(state, &asset).await?);
    }
    Ok(out)
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

/// best-effort 探测三 CLI。
///
/// Business Logic（为什么需要这个函数）:
///     status 顶部展示本机 Claude/Codex/OpenCode 可用性。
///
/// Code Logic（这个函数做什么）:
///     home+env 构造 TargetEnvironment；adapter.probe 失败返回 unsupported。
fn probe_all_targets_best_effort() -> Vec<AgentHubProbeDto> {
    let env = current_target_environment();
    let adapters: [(&dyn AssetAdapter, AgentTarget); 3] = [
        (&ClaudeInstructionAdapter, AgentTarget::Claude),
        (&CodexInstructionAdapter, AgentTarget::Codex),
        (&OpenCodeInstructionAdapter, AgentTarget::OpenCode),
    ];
    adapters
        .into_iter()
        .map(|(adapter, target)| match adapter.probe(&env) {
            Ok(probe) => AgentHubProbeDto {
                target: probe.target,
                executable: probe.executable.map(|p| p.to_string_lossy().into_owned()),
                version: probe.version,
                support: probe.support.as_str().to_string(),
                config_root: Some(probe.config_root.to_string_lossy().into_owned()),
            },
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

/// 构建资产摘要。
///
/// Business Logic（为什么需要这个函数）:
///     列表/详情/set_binding 共用 summary。
///
/// Code Logic（这个函数做什么）:
///     固定三 target 单元格 + has_conflict 查询。
async fn build_summary(
    state: &AppState,
    asset: &LogicalAsset,
) -> Result<AgentHubAssetSummaryDto, AppError> {
    let bindings = state
        .agent_hub_repo
        .list_target_bindings_for_asset(&asset.id)
        .await?;
    let mats = state.agent_hub_repo.list_materializations().await?;
    let mat_by_binding: BTreeMap<String, &Materialization> = mats
        .iter()
        .map(|m| (m.target_binding_id.clone(), m))
        .collect();
    let mut targets = Vec::new();
    for target in [
        AgentTarget::Claude,
        AgentTarget::Codex,
        AgentTarget::OpenCode,
    ] {
        if let Some(b) = bindings.iter().find(|b| b.target == target) {
            let mat = mat_by_binding.get(&b.id).copied();
            targets.push(AgentHubTargetCellDto {
                target,
                desired_presence: b.desired_presence,
                desired_enabled: b.desired_enabled,
                materialization_status: mat.map(|m| m.status.as_str().to_string()),
                last_error: mat.and_then(|m| m.last_error.clone()),
            });
        } else {
            targets.push(AgentHubTargetCellDto {
                target,
                desired_presence: DesiredPresence::Absent,
                desired_enabled: false,
                materialization_status: None,
                last_error: None,
            });
        }
    }
    let has_conflict = state
        .agent_hub_repo
        .has_unresolved_canonical_conflict(&asset.id)
        .await?
        || state
            .agent_hub_repo
            .has_unresolved_target_conflict(&asset.id, AgentTarget::Claude)
            .await?
        || state
            .agent_hub_repo
            .has_unresolved_target_conflict(&asset.id, AgentTarget::Codex)
            .await?
        || state
            .agent_hub_repo
            .has_unresolved_target_conflict(&asset.id, AgentTarget::OpenCode)
            .await?;
    Ok(AgentHubAssetSummaryDto {
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
    })
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

    // N/N+1 dual-write：仅用户级 CLAUDE.md 指令摘要写回 legacy 表 + 文件；失败不阻断 Hub。
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

/// 用户级 CLAUDE.md 资产成功写 revision 后 dual-write legacy 摘要 + 文件。
///
/// Business Logic（为什么需要这个函数）:
///     旧 CLAUDE.md 页/P2P 仍读 `claude_md` 表；Hub 写用户指令后需同步摘要与
///     `~/.claude/CLAUDE.md`，且不得让 legacy VC 参与 Hub merge。
///
/// Code Logic（这个函数做什么）:
///     scope=User + Instruction + logical_key=CLAUDE.md → Claude 摘要 → dual_write DB
///     + `write_file_if_changed` 落盘（失败上抛，由调用方 warn）。
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
    crate::agent_hub::migration::dual_write_legacy_claude_md_summary(state, &summary).await?;
    crate::sync::claude_md::write_file_if_changed(&summary).await?;
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

/// 块 → DTO。
///
/// Business Logic（为什么需要这个函数）:
///     UI 需要 mode/common/variants。
///
/// Code Logic（这个函数做什么）:
///     common 空串兜底；variants 转 string key map。
fn block_to_dto(block: &InstructionBlock) -> InstructionBlockDto {
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
fn parse_block_mode(raw: &str) -> Result<InstructionBlockMode, AppError> {
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
}
