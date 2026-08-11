//! 用户级指令 V2 preview/apply 计划。

use super::inventory::{
    inspect_user_instruction_workspace, UserInstructionActivationSupport,
    UserInstructionCapabilityLevel, UserInstructionManagementMode, UserInstructionOwnership,
    UserInstructionTargetDto, UserInstructionWorkspaceDto,
};
use crate::agent_hub::instructions::{compile_render, InstructionDocument};
use crate::agent_hub::migration::{USER_INSTRUCTION_LOGICAL_KEY, USER_INSTRUCTION_NAMESPACE};
use crate::agent_hub::models::{
    AgentTarget, AssetKind, UserInstructionPlanClaim, UserInstructionPlanRecord,
};
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::service::load_instruction_document_for_user_v2;
use crate::agent_hub::targets::InstructionRenderContext;
use crate::error::AppError;
use crate::state::AppState;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_PREVIEW_CONTENT_BYTES: usize = 192 * 1024;
const MAX_DIFF_BYTES_PER_TARGET: usize = 64 * 1024;
const PLAN_TTL_MINUTES: i64 = 10;

/// 用户级目标选择，支持简写 mode 或带 adoption/path 选项的详细形状。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserInstructionTargetSelectionDto {
    /// UI 首次设置/日常编辑的稳定三态。
    Mode(UserInstructionTargetSelectionMode),
    /// 完整选择
    Detailed {
        #[serde(rename = "managementMode")]
        management_mode: UserInstructionManagementMode,
        #[serde(default, rename = "adoptExisting")]
        adopt_existing: bool,
        #[serde(default, rename = "manageOverride")]
        manage_override: bool,
    },
}

/// 用户级编辑器 target 选择 wire token。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserInstructionTargetSelectionMode {
    Managed,
    Unmanaged,
    Inherit,
}

impl UserInstructionTargetSelectionDto {
    /// 返回归一化 management mode。
    fn mode(&self) -> UserInstructionManagementMode {
        match self {
            Self::Mode(UserInstructionTargetSelectionMode::Managed) => {
                UserInstructionManagementMode::ManagedActive
            }
            Self::Mode(
                UserInstructionTargetSelectionMode::Unmanaged
                | UserInstructionTargetSelectionMode::Inherit,
            ) => UserInstructionManagementMode::Unmanaged,
            Self::Detailed {
                management_mode, ..
            } => *management_mode,
        }
    }

    /// 是否显式确认纳管既有文件。
    fn adopt_existing(&self) -> bool {
        matches!(
            self,
            Self::Detailed {
                adopt_existing: true,
                ..
            }
        )
    }

    /// 是否显式选择 Codex override。
    fn manage_override(&self) -> bool {
        matches!(
            self,
            Self::Detailed {
                manage_override: true,
                ..
            }
        )
    }
}

/// setup/update 共用的 preview 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewUserInstructionRequest {
    pub base_revision_id: Option<String>,
    pub inventory_snapshot_hash: String,
    pub common_content: String,
    #[serde(default)]
    pub target_extensions: BTreeMap<AgentTarget, String>,
    #[serde(default)]
    pub target_selections: BTreeMap<AgentTarget, UserInstructionTargetSelectionDto>,
}

/// preview 目标文件操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserInstructionPlanOperation {
    Create,
    Update,
    Delete,
    Leave,
}

/// 单 target preview 变更。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInstructionPlanChangeDto {
    pub target: AgentTarget,
    pub path: String,
    pub operation: UserInstructionPlanOperation,
    pub current_hash: Option<String>,
    pub expected_hash: Option<String>,
    pub rendered_hash: Option<String>,
    pub unified_diff: Option<String>,
    pub ownership_required: bool,
    pub will_shadow_source_path: Option<String>,
    pub will_replace_fallback_source_path: Option<String>,
    pub empty_due_to_target_only: bool,
    pub activation: UserInstructionActivationSupport,
    pub warnings: Vec<String>,
    /// diff 被截断时 apply 必须 fail-closed。
    #[serde(default)]
    pub diff_truncated: bool,
}

/// 短期 preview plan DTO。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInstructionPlanDto {
    pub plan_token: String,
    pub expires_at: String,
    pub base_revision_id: Option<String>,
    pub inventory_snapshot_hash: String,
    pub changes: Vec<UserInstructionPlanChangeDto>,
    pub blocking_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredUserInstructionPlan {
    public: UserInstructionPlanDto,
    request: PreviewUserInstructionRequest,
    owner_fingerprint: String,
}

/// apply 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyUserInstructionPlanRequest {
    pub plan_token: String,
    pub client_request_id: String,
}

/// 单 target apply 真实结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserInstructionTargetApplyState {
    Queued,
    Applied,
    NoChange,
    StalePreview,
    Blocked,
    Conflict,
    Failed,
}

/// 单 target apply 结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInstructionTargetApplyResultDto {
    pub target: AgentTarget,
    pub status: UserInstructionTargetApplyState,
    pub path: String,
    pub error_code: Option<String>,
    pub activation: UserInstructionActivationSupport,
}

/// apply 聚合结果，禁止用单一 success 掩盖逐 target 失败。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyUserInstructionPlanResultDto {
    pub plan_token: String,
    pub setup_state: super::inventory::UserInstructionSetupState,
    pub health_state: super::inventory::UserInstructionHealthState,
    pub targets: Vec<UserInstructionTargetApplyResultDto>,
}

/// 预览首次设置。
///
/// Business Logic: setup 与 update 的安全门闩完全相同，只是用户旅程不同。
/// Code Logic: 委托共用 preview builder。
pub async fn preview_user_instruction_setup(
    state: &AppState,
    request: PreviewUserInstructionRequest,
) -> Result<UserInstructionPlanDto, AppError> {
    preview_user_instruction_plan(state, request).await
}

/// 预览日常更新。
///
/// Business Logic: 更新也必须在 mutation 之前完成 revision/inventory/diff 预览。
/// Code Logic: 委托共用 preview builder。
pub async fn preview_user_instruction_update(
    state: &AppState,
    request: PreviewUserInstructionRequest,
) -> Result<UserInstructionPlanDto, AppError> {
    preview_user_instruction_plan(state, request).await
}

/// 生成并持久化 preview plan。
async fn preview_user_instruction_plan(
    state: &AppState,
    request: PreviewUserInstructionRequest,
) -> Result<UserInstructionPlanDto, AppError> {
    validate_request_content_size(&request)?;
    let workspace = inspect_user_instruction_workspace(state).await?;
    validate_preview_base(&workspace, &request)?;
    let document = load_projection_document(state, &workspace).await?;
    let expires_at = (Utc::now() + Duration::minutes(PLAN_TTL_MINUTES)).to_rfc3339();
    let plan_token = uuid::Uuid::new_v4().to_string();
    let mut changes = Vec::with_capacity(workspace.targets.len());
    let mut blocking_reasons = Vec::new();
    for target in &workspace.targets {
        let selection = request
            .target_selections
            .get(&target.target)
            .cloned()
            .unwrap_or(UserInstructionTargetSelectionDto::Mode(
                if target.management_mode == UserInstructionManagementMode::Unmanaged {
                    UserInstructionTargetSelectionMode::Unmanaged
                } else {
                    UserInstructionTargetSelectionMode::Managed
                },
            ));
        let (change, reasons) = build_change(target, &workspace, &document, &selection)?;
        changes.push(change);
        blocking_reasons.extend(reasons);
    }
    blocking_reasons.sort();
    blocking_reasons.dedup();
    let public = UserInstructionPlanDto {
        plan_token: plan_token.clone(),
        expires_at: expires_at.clone(),
        base_revision_id: request.base_revision_id.clone(),
        inventory_snapshot_hash: request.inventory_snapshot_hash.clone(),
        changes,
        blocking_reasons,
    };
    let owner_fingerprint = owner_fingerprint(state, &workspace);
    let stored = StoredUserInstructionPlan {
        public: public.clone(),
        request,
        owner_fingerprint: owner_fingerprint.clone(),
    };
    let plan_json = serde_json::to_string(&stored)?;
    state
        .agent_hub_repo
        .insert_user_instruction_plan(UserInstructionPlanRecord {
            plan_token,
            owner_fingerprint,
            expires_at,
            base_revision_id: public
                .base_revision_id
                .clone()
                .map(crate::agent_hub::models::RevisionId),
            inventory_snapshot_hash: public.inventory_snapshot_hash.clone(),
            plan_json,
            client_request_id: None,
            claimed_at: None,
            consumed_at: None,
            result_json: None,
            created_at: Utc::now().to_rfc3339(),
        })
        .await?;
    Ok(public)
}

/// 应用已确认计划。
///
/// Business Logic: 当前 support manifest 未完成 L3 写入认证，所有 create/update/delete 必须诚实 blocked。
/// Code Logic: 先原子 claim，再验证 expiry/owner/revision/inventory/hash/ownership/empty/diff；
///     本版未实现可达写路，不会因未来 manifest 误开而跳过事务门闩。
pub async fn apply_user_instruction_plan(
    state: &AppState,
    request: ApplyUserInstructionPlanRequest,
) -> Result<ApplyUserInstructionPlanResultDto, AppError> {
    if request.plan_token.trim().is_empty() || request.client_request_id.trim().is_empty() {
        return Err(AppError::validation("USER_INSTRUCTION_APPLY_ID_REQUIRED"));
    }
    let record = match state
        .agent_hub_repo
        .claim_user_instruction_plan(&request.plan_token, &request.client_request_id)
        .await?
    {
        UserInstructionPlanClaim::Replay(result_json) => {
            return serde_json::from_str(&result_json).map_err(AppError::from);
        }
        UserInstructionPlanClaim::Pending => {
            let pending = state
                .agent_hub_repo
                .get_user_instruction_plan(&request.plan_token)
                .await?
                .ok_or_else(|| AppError::not_found("USER_INSTRUCTION_PLAN_NOT_FOUND"))?;
            let expires_at = chrono::DateTime::parse_from_rfc3339(&pending.expires_at)
                .map_err(|_| AppError::validation("USER_INSTRUCTION_PLAN_EXPIRY_INVALID"))?
                .with_timezone(&Utc);
            if Utc::now() < expires_at {
                return Err(AppError::unavailable("USER_INSTRUCTION_APPLY_PENDING"));
            }
            let stored: StoredUserInstructionPlan = serde_json::from_str(&pending.plan_json)?;
            let workspace = inspect_user_instruction_workspace(state).await?;
            let result = terminal_stale_result(&request.plan_token, &stored, &workspace);
            let result_json = serde_json::to_string(&result)?;
            state
                .agent_hub_repo
                .complete_user_instruction_plan(
                    &request.plan_token,
                    &request.client_request_id,
                    &result_json,
                )
                .await?;
            return Ok(result);
        }
        UserInstructionPlanClaim::Claimed(record) => *record,
    };
    let stored: StoredUserInstructionPlan = serde_json::from_str(&record.plan_json)?;
    let workspace = inspect_user_instruction_workspace(state).await?;
    let now = Utc::now();
    let expires_at = chrono::DateTime::parse_from_rfc3339(&record.expires_at)
        .map_err(|_| AppError::validation("USER_INSTRUCTION_PLAN_EXPIRY_INVALID"))?
        .with_timezone(&Utc);
    let global_stale = now >= expires_at
        || owner_fingerprint(state, &workspace) != record.owner_fingerprint
        || workspace.inventory_snapshot_hash != record.inventory_snapshot_hash
        || workspace
            .canonical
            .as_ref()
            .and_then(|canonical| canonical.head_revision_id.as_deref())
            != record
                .base_revision_id
                .as_ref()
                .map(|revision| revision.as_str());

    // 投影基于持久化 head 块文档（含 per-agent variants），而非前端 flat request 合成。
    let document = load_projection_document(state, &workspace).await?;

    let mut targets = Vec::with_capacity(stored.public.changes.len());
    for change in &stored.public.changes {
        let (target_state, error_code) = if global_stale {
            (
                UserInstructionTargetApplyState::StalePreview,
                Some("USER_INSTRUCTION_PREVIEW_STALE".to_string()),
            )
        } else if change.operation == UserInstructionPlanOperation::Leave {
            (UserInstructionTargetApplyState::NoChange, None)
        } else if change.diff_truncated {
            (
                UserInstructionTargetApplyState::Blocked,
                Some("USER_INSTRUCTION_DIFF_TRUNCATED".to_string()),
            )
        } else if change.empty_due_to_target_only {
            (
                UserInstructionTargetApplyState::Blocked,
                Some("USER_INSTRUCTION_EMPTY_TARGET_RENDER".to_string()),
            )
        } else if current_file_hash(Path::new(&change.path))? != change.expected_hash {
            (
                UserInstructionTargetApplyState::StalePreview,
                Some("USER_INSTRUCTION_SOURCE_CHANGED".to_string()),
            )
        } else if change.ownership_required {
            (
                UserInstructionTargetApplyState::Blocked,
                Some("USER_INSTRUCTION_OWNERSHIP_REQUIRED".to_string()),
            )
        } else if let Some(code) = plan_blocking_code_for_target(&stored.public, change.target) {
            (UserInstructionTargetApplyState::Blocked, Some(code))
        } else if workspace_blocks_current_mutation(&workspace, change) {
            (
                UserInstructionTargetApplyState::Blocked,
                Some("USER_INSTRUCTION_TARGET_SCAN_ONLY".to_string()),
            )
        } else {
            match apply_user_instruction_change_to_disk(change, &document) {
                Ok(state) => (state, None),
                Err(code) => (UserInstructionTargetApplyState::Failed, Some(code)),
            }
        };
        targets.push(UserInstructionTargetApplyResultDto {
            target: change.target,
            status: target_state,
            path: change.path.clone(),
            error_code,
            activation: if matches!(
                target_state,
                UserInstructionTargetApplyState::Blocked
                    | UserInstructionTargetApplyState::Failed
                    | UserInstructionTargetApplyState::StalePreview
            ) {
                UserInstructionActivationSupport::Blocked
            } else {
                plan_activation(change.activation)
            },
        });
    }
    let result = ApplyUserInstructionPlanResultDto {
        plan_token: request.plan_token.clone(),
        setup_state: workspace.setup_state,
        health_state: workspace.health_state,
        targets,
    };
    let result_json = serde_json::to_string(&result)?;
    state
        .agent_hub_repo
        .complete_user_instruction_plan(
            &request.plan_token,
            &request.client_request_id,
            &result_json,
        )
        .await?;
    Ok(result)
}

/// apply 写盘前基于刚刷新的 workspace 再验当前 target capability。
///
/// Business Logic（为什么需要这个函数）:
///     preview 后目标可能变为不可读、路径漂移或删除能力回落；旧 plan 即使没有 blocker，
///     也不能继续创建、覆盖或删除原生指令文件。
///
/// Code Logic（这个函数做什么）:
///     Leave 永不写；Create/Update 要求用户确认写链 write=Supported；Delete 仍要求 remove=Supported；
///     target 缺失时 fail-closed。
fn workspace_blocks_current_mutation(
    workspace: &UserInstructionWorkspaceDto,
    change: &UserInstructionPlanChangeDto,
) -> bool {
    if change.operation == UserInstructionPlanOperation::Leave {
        return false;
    }
    let Some(target) = workspace
        .targets
        .iter()
        .find(|target| target.target == change.target)
    else {
        return true;
    };
    match change.operation {
        UserInstructionPlanOperation::Create | UserInstructionPlanOperation::Update => {
            target.capability.write != UserInstructionCapabilityLevel::Supported
        }
        UserInstructionPlanOperation::Delete => {
            target.capability.remove != UserInstructionCapabilityLevel::Supported
        }
        UserInstructionPlanOperation::Leave => false,
    }
}

/// 从 plan 级 blockingReasons 提取该 target 的错误码（若有）。
///
/// Business Logic: preview 已判定 scan-only / empty / ownership 时 apply 不得绕过。
/// Code Logic: reasons 形如 `claude:USER_INSTRUCTION_TARGET_SCAN_ONLY`。
fn plan_blocking_code_for_target(
    plan: &UserInstructionPlanDto,
    target: AgentTarget,
) -> Option<String> {
    let prefix = format!("{}:", target.as_str());
    plan.blocking_reasons.iter().find_map(|reason| {
        reason
            .strip_prefix(&prefix)
            .map(str::to_string)
            .or_else(|| {
                if reason == "USER_INSTRUCTION_DIFF_TRUNCATED" {
                    Some(reason.clone())
                } else {
                    None
                }
            })
    })
}

/// 将单 target plan change 真实写盘（atomic sibling rename）。
///
/// Business Logic: 阶段一要求 certified target 的 create/update/delete 落到原生指令文件。
/// Code Logic: 从 stored request 重编译正文；Create/Update 走 AtomicProjectionWriter；
///     Delete 仅在 expected_hash 匹配时 unlink；AlreadyRendered → Applied。
fn apply_user_instruction_change_to_disk(
    change: &UserInstructionPlanChangeDto,
    document: &InstructionDocument,
) -> Result<UserInstructionTargetApplyState, String> {
    use crate::agent_hub::projection::{
        AtomicProjectionWriter, AtomicWriteOutcome, FileWriteRequest,
    };

    let path = Path::new(&change.path);
    match change.operation {
        UserInstructionPlanOperation::Leave => Ok(UserInstructionTargetApplyState::NoChange),
        UserInstructionPlanOperation::Delete => {
            if !path.exists() {
                return Ok(UserInstructionTargetApplyState::NoChange);
            }
            let current = current_file_hash(path).map_err(|e| e.to_string())?;
            if current != change.expected_hash {
                return Ok(UserInstructionTargetApplyState::StalePreview);
            }
            std::fs::remove_file(path)
                .map_err(|e| format!("USER_INSTRUCTION_DELETE_FAILED:{}", e))?;
            Ok(UserInstructionTargetApplyState::Applied)
        }
        UserInstructionPlanOperation::Create | UserInstructionPlanOperation::Update => {
            let compiled = compile_render(
                document,
                change.target,
                &InstructionRenderContext::default(),
            );
            let rendered_hash = change
                .rendered_hash
                .as_deref()
                .ok_or_else(|| "USER_INSTRUCTION_RENDERED_HASH_MISSING".to_string())?;
            let computed = sha256_hex(&compiled.bytes);
            if computed != rendered_hash {
                return Err("USER_INSTRUCTION_RENDER_HASH_MISMATCH".into());
            }
            let writer = AtomicProjectionWriter::default();
            let outcome = writer
                .write_file(FileWriteRequest {
                    target: path,
                    rendered_bytes: &compiled.bytes,
                    rendered_hash,
                    expected_external_hash: change.expected_hash.as_deref(),
                })
                .map_err(|e| format!("USER_INSTRUCTION_WRITE_FAILED:{}", e))?;
            match outcome {
                AtomicWriteOutcome::Replaced { .. }
                | AtomicWriteOutcome::AlreadyRendered { .. } => {
                    Ok(UserInstructionTargetApplyState::Applied)
                }
                AtomicWriteOutcome::Drift { .. } => {
                    Ok(UserInstructionTargetApplyState::StalePreview)
                }
                AtomicWriteOutcome::DirectoryUnknownFiles { .. } => {
                    Err("USER_INSTRUCTION_UNEXPECTED_DIRECTORY_OUTCOME".into())
                }
            }
        }
    }
}

/// 验证 preview 正文在 control request 256 KiB 限制之下预留信封开销。
fn validate_request_content_size(request: &PreviewUserInstructionRequest) -> Result<(), AppError> {
    let total = request.common_content.len()
        + request
            .target_extensions
            .values()
            .map(String::len)
            .sum::<usize>();
    if total > MAX_PREVIEW_CONTENT_BYTES {
        return Err(AppError::validation("USER_INSTRUCTION_CONTENT_TOO_LARGE"));
    }
    Ok(())
}

/// 验证 preview 基线与刚刷新的 inventory 一致。
fn validate_preview_base(
    workspace: &UserInstructionWorkspaceDto,
    request: &PreviewUserInstructionRequest,
) -> Result<(), AppError> {
    let current_revision = workspace
        .canonical
        .as_ref()
        .and_then(|canonical| canonical.head_revision_id.as_deref());
    if current_revision != request.base_revision_id.as_deref() {
        return Err(AppError::conflict("USER_INSTRUCTION_REVISION_CHANGED"));
    }
    if workspace.inventory_snapshot_hash != request.inventory_snapshot_hash {
        return Err(AppError::conflict("USER_INSTRUCTION_PREVIEW_STALE"));
    }
    Ok(())
}

/// 加载持久化 head InstructionDocument 供 preview/apply 投影使用。
///
/// Business Logic（为什么需要这个函数）:
///     preview/apply 必须基于 canonical 真实块结构（含 per-agent variants/mode），而非前端
///     flat request 合成（后者把 adapted 块摊平为 shared common，丢失 variants）。前端先
///     save_blocks 推进 head，再 preview/apply；head 即权威投影源。
///
/// Code Logic（这个函数做什么）:
///     workspace → asset → load_instruction_document_for_user_v2（CAS blob，markdown fallback）。
async fn load_projection_document(
    state: &AppState,
    workspace: &UserInstructionWorkspaceDto,
) -> Result<InstructionDocument, AppError> {
    if workspace.canonical.is_none() {
        return Ok(InstructionDocument::default());
    }
    let asset = state
        .agent_hub_repo
        .get_asset_by_unique_key(
            &workspace.scope_id,
            AssetKind::Instruction,
            USER_INSTRUCTION_NAMESPACE,
            USER_INSTRUCTION_LOGICAL_KEY,
        )
        .await?
        .ok_or_else(|| AppError::not_found("USER_INSTRUCTION_ASSET_MISSING"))?;
    let (document, _) = load_instruction_document_for_user_v2(&asset, state).await?;
    Ok(document)
}

/// 生成单 target 路径/操作/diff/优先级影响。
fn build_change(
    target: &UserInstructionTargetDto,
    workspace: &UserInstructionWorkspaceDto,
    document: &InstructionDocument,
    selection: &UserInstructionTargetSelectionDto,
) -> Result<(UserInstructionPlanChangeDto, Vec<String>), AppError> {
    let path = selected_path(target, selection);
    let current_source = target.sources.iter().find(|source| source.path == path);
    let current_hash = current_source.and_then(|source| source.hash.clone());
    let exists = current_source.is_some_and(|source| source.exists);
    let selected_mode = selection.mode();
    let compiled = compile_render(
        document,
        target.target,
        &InstructionRenderContext::default(),
    );
    let rendered_hash = sha256_hex(&compiled.bytes);
    let has_any_content = document.blocks.iter().any(|block| {
        block
            .common_markdown
            .as_ref()
            .is_some_and(|text| !text.is_empty())
            || block.variants.values().any(|text| !text.is_empty())
    });
    let empty_due_to_target_only = has_any_content && compiled.user_body().is_empty();
    let operation = match selected_mode {
        UserInstructionManagementMode::Unmanaged => UserInstructionPlanOperation::Leave,
        UserInstructionManagementMode::ManagedPaused => {
            if exists {
                UserInstructionPlanOperation::Delete
            } else {
                UserInstructionPlanOperation::Leave
            }
        }
        UserInstructionManagementMode::ManagedActive if !exists => {
            UserInstructionPlanOperation::Create
        }
        UserInstructionManagementMode::ManagedActive
            if current_hash.as_deref() == Some(rendered_hash.as_str()) =>
        {
            UserInstructionPlanOperation::Leave
        }
        UserInstructionManagementMode::ManagedActive => UserInstructionPlanOperation::Update,
    };
    // map_or：None → true；MSRV 1.77 无 is_none_or（1.82+）
    let ownership_required = matches!(
        operation,
        UserInstructionPlanOperation::Update | UserInstructionPlanOperation::Delete
    ) && current_source.map_or(true, |source| {
        source.ownership != UserInstructionOwnership::HubManaged && !selection.adopt_existing()
    });
    let (unified_diff, diff_truncated) = if matches!(
        operation,
        UserInstructionPlanOperation::Create | UserInstructionPlanOperation::Update
    ) {
        let before = read_text_bounded(&path)?;
        let diff = render_bounded_diff(before.as_deref().unwrap_or(""), compiled.content_str());
        (Some(diff.0), diff.1)
    } else if operation == UserInstructionPlanOperation::Delete {
        let before = read_text_bounded(&path)?;
        let diff = render_bounded_diff(before.as_deref().unwrap_or(""), "");
        (Some(diff.0), diff.1)
    } else {
        (None, false)
    };
    let will_shadow_source_path =
        if target.target == AgentTarget::Codex && selection.manage_override() {
            target
                .sources
                .iter()
                .find(|source| {
                    source.path.ends_with("AGENTS.md")
                        && !source.path.ends_with("AGENTS.override.md")
                        && source.exists
                })
                .map(|source| source.path.clone())
        } else {
            None
        };
    let will_replace_fallback_source_path = if target.target == AgentTarget::OpenCode
        && matches!(
            operation,
            UserInstructionPlanOperation::Create | UserInstructionPlanOperation::Update
        ) {
        target
            .sources
            .iter()
            .find(|source| source.active && source.path.ends_with("CLAUDE.md"))
            .map(|source| source.path.clone())
    } else {
        None
    };
    let mut warnings = Vec::new();
    let mut blocking = Vec::new();
    if target.target == AgentTarget::Codex
        && !selection.manage_override()
        && target
            .sources
            .iter()
            .any(|source| source.active && source.path.ends_with("AGENTS.override.md"))
        && selected_mode == UserInstructionManagementMode::ManagedActive
    {
        warnings.push("codex_override_active_base_will_remain_shadowed".to_string());
        blocking.push(format!(
            "{}:USER_INSTRUCTION_WOULD_SHADOW_SOURCE",
            target.target.as_str()
        ));
    }
    if target.capability.write != UserInstructionCapabilityLevel::Supported
        && matches!(
            operation,
            UserInstructionPlanOperation::Create | UserInstructionPlanOperation::Update
        )
    {
        blocking.push(format!(
            "{}:USER_INSTRUCTION_TARGET_SCAN_ONLY",
            target.target.as_str()
        ));
    }
    if target.capability.remove != UserInstructionCapabilityLevel::Supported
        && operation == UserInstructionPlanOperation::Delete
    {
        blocking.push(format!(
            "{}:USER_INSTRUCTION_TARGET_SCAN_ONLY",
            target.target.as_str()
        ));
    }
    if ownership_required {
        blocking.push(format!(
            "{}:USER_INSTRUCTION_OWNERSHIP_REQUIRED",
            target.target.as_str()
        ));
    }
    if empty_due_to_target_only
        && matches!(
            operation,
            UserInstructionPlanOperation::Create | UserInstructionPlanOperation::Update
        )
    {
        blocking.push(format!(
            "{}:USER_INSTRUCTION_EMPTY_TARGET_RENDER",
            target.target.as_str()
        ));
    }
    if diff_truncated {
        warnings.push("USER_INSTRUCTION_DIFF_TRUNCATED".to_string());
        blocking.push(format!(
            "{}:USER_INSTRUCTION_DIFF_TRUNCATED",
            target.target.as_str()
        ));
    }
    let _ = workspace;
    Ok((
        UserInstructionPlanChangeDto {
            target: target.target,
            path,
            operation,
            current_hash: current_hash.clone(),
            expected_hash: current_hash,
            rendered_hash: if operation == UserInstructionPlanOperation::Delete {
                None
            } else {
                Some(rendered_hash)
            },
            unified_diff,
            ownership_required,
            will_shadow_source_path,
            will_replace_fallback_source_path,
            empty_due_to_target_only,
            activation: plan_activation(target.capability.activate),
            warnings,
            diff_truncated,
        },
        blocking,
    ))
}

/// plan DTO 不暴露 blocked activation；阻断原因单独位于 blockingReasons。
fn plan_activation(
    activation: UserInstructionActivationSupport,
) -> UserInstructionActivationSupport {
    if activation == UserInstructionActivationSupport::Blocked {
        UserInstructionActivationSupport::Unknown
    } else {
        activation
    }
}

/// 将已过期但曾被同幂等键 claim 的崩溃遗留计划收敛为稳定终态。
fn terminal_stale_result(
    plan_token: &str,
    stored: &StoredUserInstructionPlan,
    workspace: &UserInstructionWorkspaceDto,
) -> ApplyUserInstructionPlanResultDto {
    ApplyUserInstructionPlanResultDto {
        plan_token: plan_token.to_string(),
        setup_state: workspace.setup_state,
        health_state: workspace.health_state,
        targets: stored
            .public
            .changes
            .iter()
            .map(|change| UserInstructionTargetApplyResultDto {
                target: change.target,
                status: UserInstructionTargetApplyState::StalePreview,
                path: change.path.clone(),
                error_code: Some("USER_INSTRUCTION_PREVIEW_STALE".to_string()),
                activation: plan_activation(change.activation),
            })
            .collect(),
    }
}

/// 解析预览和 apply 必须共享的 target 路径。
fn selected_path(
    target: &UserInstructionTargetDto,
    selection: &UserInstructionTargetSelectionDto,
) -> String {
    if target.target == AgentTarget::Codex && selection.manage_override() {
        return PathBuf::from(&target.cli.config_root)
            .join("AGENTS.override.md")
            .to_string_lossy()
            .into_owned();
    }
    target.managed_target_path.clone()
}

/// 读取 diff 所需的有界 UTF-8 正文。
pub(crate) fn read_text_bounded(path: &str) -> Result<Option<String>, AppError> {
    let path = Path::new(path);
    if !path.exists() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_PREVIEW_CONTENT_BYTES as u64 {
        return Err(AppError::validation("USER_INSTRUCTION_CONTENT_TOO_LARGE"));
    }
    let bytes = std::fs::read(path)?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| AppError::validation("USER_INSTRUCTION_SOURCE_NOT_UTF8"))
}

/// 生成有界 unified-like diff，截断时带稳定 marker。
pub(crate) fn render_bounded_diff(before: &str, after: &str) -> (String, bool) {
    let mut diff = String::from("--- before\n+++ after\n@@\n");
    for line in before.lines() {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in after.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    if diff.len() <= MAX_DIFF_BYTES_PER_TARGET {
        return (diff, false);
    }
    let mut end = MAX_DIFF_BYTES_PER_TARGET;
    while end > 0 && !diff.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = diff[..end].to_string();
    truncated.push_str("\n[diff truncated]\n");
    (truncated, true)
}

/// 读取当前文件 hash，超限时 fail-closed。
fn current_file_hash(path: &Path) -> Result<Option<String>, AppError> {
    if !path.exists() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_PREVIEW_CONTENT_BYTES as u64 {
        return Err(AppError::validation("USER_INSTRUCTION_CONTENT_TOO_LARGE"));
    }
    Ok(Some(sha256_hex(&std::fs::read(path)?)))
}

/// 计算 plan 所属 OS 用户/配置根指纹。
fn owner_fingerprint(state: &AppState, workspace: &UserInstructionWorkspaceDto) -> String {
    let roots = workspace
        .targets
        .iter()
        .map(|target| format!("{}={}", target.target.as_str(), target.cli.config_root))
        .collect::<Vec<_>>()
        .join("|");
    sha256_hex(
        format!(
            "{}|{}|{}",
            state.device_id.as_str(),
            workspace.scope_id,
            roots
        )
        .as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic: control request 必须在 transport 失败前返回稳定大小错误。
    /// Code Logic: 超过 192 KiB 正文被拒绝。
    #[test]
    fn preview_content_limit_fails_closed() {
        let request = PreviewUserInstructionRequest {
            base_revision_id: None,
            inventory_snapshot_hash: "hash".into(),
            common_content: "x".repeat(MAX_PREVIEW_CONTENT_BYTES + 1),
            target_extensions: BTreeMap::new(),
            target_selections: BTreeMap::new(),
        };
        let error = validate_request_content_size(&request).unwrap_err();
        assert!(error
            .to_string()
            .contains("USER_INSTRUCTION_CONTENT_TOO_LARGE"));
    }

    /// Business Logic: diff 截断不得被 apply 当成完整确认。
    /// Code Logic: 超限返回 marker + truncated=true。
    #[test]
    fn diff_limit_marks_truncation() {
        let (diff, truncated) = render_bounded_diff("", &"x\n".repeat(100_000));
        assert!(truncated);
        assert!(diff.contains("[diff truncated]"));
        assert!(diff.len() < 70 * 1024);
    }

    /// Business Logic: 普通 Codex 管理必须默认 base AGENTS.md，不得静默创建 override。
    /// Code Logic: 只有 manageOverride=true 才返回 AGENTS.override.md。
    #[test]
    fn codex_path_defaults_to_base_agents() {
        let target = UserInstructionTargetDto {
            target: AgentTarget::Codex,
            cli: super::super::inventory::UserInstructionCliDto {
                installed: true,
                version: Some("1.0.0".into()),
                config_root: "/tmp/codex".into(),
            },
            sources: vec![],
            effective_source_id: None,
            managed_target_path: "/tmp/codex/AGENTS.md".into(),
            management_mode: UserInstructionManagementMode::Unmanaged,
            capability: super::super::inventory::UserInstructionCapabilityDto {
                scan: UserInstructionCapabilityLevel::ReadOnly,
                write: UserInstructionCapabilityLevel::Blocked,
                remove: UserInstructionCapabilityLevel::Blocked,
                activate: UserInstructionActivationSupport::Blocked,
                reason_code: None,
                evidence_ids: vec![],
            },
            projection: super::super::inventory::UserInstructionProjectionDto {
                state: super::super::inventory::UserInstructionProjectionState::None,
                desired_revision_id: None,
                applied_revision_id: None,
                observed_hash: None,
                last_error_code: None,
            },
            available_actions: vec![],
        };
        assert!(selected_path(
            &target,
            &UserInstructionTargetSelectionDto::Mode(UserInstructionTargetSelectionMode::Managed)
        )
        .ends_with("/AGENTS.md"));
        assert!(selected_path(
            &target,
            &UserInstructionTargetSelectionDto::Detailed {
                management_mode: UserInstructionManagementMode::ManagedActive,
                adopt_existing: false,
                manage_override: true,
            }
        )
        .ends_with("/AGENTS.override.md"));
    }

    /// Business Logic: 前端稳定三态必须逐字可反序列化，inherit 只继承外部来源且不写盘。
    /// Code Logic: serde 合同覆盖 managed/unmanaged/inherit 到内部 management mode 的映射。
    #[test]
    fn target_selection_wire_contract_accepts_frontend_tokens() {
        let managed: UserInstructionTargetSelectionDto =
            serde_json::from_str("\"managed\"").expect("managed");
        let unmanaged: UserInstructionTargetSelectionDto =
            serde_json::from_str("\"unmanaged\"").expect("unmanaged");
        let inherit: UserInstructionTargetSelectionDto =
            serde_json::from_str("\"inherit\"").expect("inherit");
        assert_eq!(managed.mode(), UserInstructionManagementMode::ManagedActive);
        assert_eq!(unmanaged.mode(), UserInstructionManagementMode::Unmanaged);
        assert_eq!(inherit.mode(), UserInstructionManagementMode::Unmanaged);
    }

    /// Business Logic: apply wire 必须与前端 decoder 使用 status/setupState/healthState。
    /// Code Logic: 序列化 fixture 断言字段名并禁止遗留 state/clientRequestId/complete。
    #[test]
    fn apply_result_wire_contract_matches_frontend_decoder() {
        let result = ApplyUserInstructionPlanResultDto {
            plan_token: "plan".into(),
            setup_state: super::super::inventory::UserInstructionSetupState::Configured,
            health_state: super::super::inventory::UserInstructionHealthState::Blocked,
            targets: vec![UserInstructionTargetApplyResultDto {
                target: AgentTarget::Claude,
                status: UserInstructionTargetApplyState::Blocked,
                path: "/tmp/CLAUDE.md".into(),
                error_code: Some("USER_INSTRUCTION_TARGET_SCAN_ONLY".into()),
                activation: UserInstructionActivationSupport::Blocked,
            }],
        };
        let value = serde_json::to_value(result).expect("serialize");
        assert_eq!(value["setupState"], "configured");
        assert_eq!(value["healthState"], "blocked");
        assert_eq!(value["targets"][0]["status"], "blocked");
        assert_eq!(value["targets"][0]["activation"], "blocked");
        assert!(value.get("clientRequestId").is_none());
        assert!(value.get("complete").is_none());
        assert!(value["targets"][0].get("state").is_none());
    }

    /// Business Logic: apply 必须在原生 writer 前重新读取并检查当前 capability。
    #[test]
    fn live_workspace_capability_gate_precedes_instruction_writer() {
        let src = include_str!("plan.rs");
        let gate = src
            .find("else if workspace_blocks_current_mutation(&workspace, change)")
            .expect("live capability gate");
        let writer = src
            .find("match apply_user_instruction_change_to_disk(change, &document)")
            .expect("native instruction writer");
        assert!(gate < writer);
    }

    /// Business Logic: certified target create/update 必须真实落盘并与 rescan hash 一致。
    /// Code Logic: 直接调用 apply_user_instruction_change_to_disk 写临时 CLAUDE.md。
    #[test]
    fn apply_change_to_disk_creates_instruction_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let body = "# Hub managed\nAlways use conventional commits.\n";
        let rendered_hash = sha256_hex(body.as_bytes());
        let change = UserInstructionPlanChangeDto {
            target: AgentTarget::Claude,
            path: path.to_string_lossy().into_owned(),
            operation: UserInstructionPlanOperation::Create,
            current_hash: None,
            expected_hash: None,
            rendered_hash: Some(rendered_hash.clone()),
            unified_diff: None,
            ownership_required: false,
            will_shadow_source_path: None,
            will_replace_fallback_source_path: None,
            empty_due_to_target_only: false,
            activation: UserInstructionActivationSupport::NewSession,
            warnings: vec![],
            diff_truncated: false,
        };
        let document = InstructionDocument::from_shared_markdown(String::new(), body.to_string());
        let state = apply_user_instruction_change_to_disk(&change, &document).expect("write");
        assert_eq!(state, UserInstructionTargetApplyState::Applied);
        let on_disk = std::fs::read_to_string(&path).expect("read back");
        assert!(on_disk.contains("conventional commits"));
        assert_eq!(sha256_hex(on_disk.as_bytes()), rendered_hash);
    }
}
