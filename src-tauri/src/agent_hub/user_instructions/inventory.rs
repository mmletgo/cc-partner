//! 用户级指令独立 inventory。

use crate::agent_hub::instructions::{InstructionBlockMode, InstructionDocument};
use crate::agent_hub::migration::{
    USER_INSTRUCTION_LOGICAL_KEY, USER_INSTRUCTION_NAMESPACE, USER_SCOPE_STABLE_ID,
};
use crate::agent_hub::models::{
    AgentTarget, AssetKind, DesiredPresence, LogicalAsset, Materialization, MaterializationStatus,
    TargetBinding, UserInstructionOwnershipRecord,
};
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::service::load_instruction_document_for_user_v2;
use crate::agent_hub::support::{
    builtin_support_manifest, evaluate_target_support, find_target_record, CapabilitySupport,
    RuntimeProbeSnapshot, TargetCapability,
};
use crate::agent_hub::targets::{
    AssetAdapter, ClaudeInstructionAdapter, CodexInstructionAdapter, InstructionSource,
    InstructionSourceRole, LocalScopeMapping, OpenCodeInstructionAdapter, TargetEnvironment,
    TargetPathResolver,
};
use crate::error::AppError;
use crate::state::AppState;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const MAX_SOURCE_HASH_BYTES: u64 = 1024 * 1024;
const MAX_CANONICAL_CONTENT_BYTES: usize = 256 * 1024;

/// 用户级指令设置阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserInstructionSetupState {
    /// 无 canonical 且无外部源
    Unconfigured,
    /// 有可导入内容但尚未管理 target
    ReadyToReview,
    /// 至少一个 target 已显式管理
    Configured,
}

/// 用户级指令健康状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserInstructionHealthState {
    /// 已选目标均无待处理事项
    Healthy,
    /// 存在 drift/detached/collision 等可处理状态
    ActionRequired,
    /// 已管理目标被 capability 或冲突阻断
    Blocked,
}

/// V2 用户态管理模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserInstructionManagementMode {
    /// Hub 不拥有该 target 文件
    Unmanaged,
    /// Hub 拥有并期望投影生效
    ManagedActive,
    /// Hub 保留 target 意图，当前无生效投影
    ManagedPaused,
}

/// 用户级 source 展示角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserInstructionSourceRole {
    /// CLI 原生主文件
    Native,
    /// Codex override
    Override,
    /// 兼容回退源
    Fallback,
    /// 被更高优先级源遮蔽
    Shadowed,
}

/// 文件 ownership 事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserInstructionOwnership {
    /// 外部原生文件，未经 Hub 纳管
    External,
    /// 路径有 Hub materialization 所有权记录
    HubManaged,
    /// 路径不存在或无法确定
    Unknown,
}

/// 分项 capability 级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserInstructionCapabilityLevel {
    /// 已认证支持
    Supported,
    /// 可读但不可写
    ReadOnly,
    /// 被 manifest/evidence 阻断
    Blocked,
}

/// 指令激活方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserInstructionActivationSupport {
    /// 立即生效
    Immediate,
    /// 新会话生效
    NewSession,
    /// 重启后生效
    Restart,
    /// adapter 无法证明
    Unknown,
    /// 当前不可激活
    Blocked,
}

/// V2 投影状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserInstructionProjectionState {
    /// 无投影记录
    None,
    /// 等待执行
    Pending,
    /// hash 与 Hub 一致
    InSync,
    /// 外部修改
    Drift,
    /// 外部删除
    Detached,
    /// 内容冲突
    Conflict,
    /// 未纳管同名内容
    Collision,
    /// 需要手工激活
    ActivationRequired,
    /// 执行失败
    Failed,
    /// capability/checkout 阻断
    Blocked,
}

/// 后端计算的 target 可用动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserInstructionAction {
    /// 开始管理
    Manage,
    /// 暂停目标
    Pause,
    /// 恢复目标
    Resume,
    /// 停止管理并保留文件
    StopManaging,
    /// 从单 target 移除投影
    Remove,
    /// 纳管当前外部文件
    Adopt,
    /// 比较外部变更
    Compare,
    /// 恢复外部删除的文件
    Restore,
    /// 删除 canonical 资产
    DeleteAsset,
    /// 打开当前生效文件
    OpenFile,
}

/// CLI 探测信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInstructionCliDto {
    pub installed: bool,
    pub version: Option<String>,
    pub config_root: String,
}

/// 只读 source 事实：metadata + 可选有界磁盘正文（供原始栏自动加载）。
///
/// Business Logic: 打开用户级提示词必须能直接展示/编辑本机已有文件，不能只给 path。
/// Code Logic: content 仅对 active（或无 active 时的首个现存文件）填充；超限截断并标 content_truncated。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInstructionSourceDto {
    pub source_id: String,
    pub path: String,
    pub role: UserInstructionSourceRole,
    pub active: bool,
    pub exists: bool,
    pub non_empty: bool,
    pub hash: Option<String>,
    pub modified_at: Option<String>,
    pub ownership: UserInstructionOwnership,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// 磁盘 UTF-8 正文（有界）；缺失表示未读/过大/非 UTF-8。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// 正文因控制面预算被截断时为 true，不得用截断内容覆盖全文件。
    #[serde(default)]
    pub content_truncated: bool,
}

/// target 分项能力。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInstructionCapabilityDto {
    pub scan: UserInstructionCapabilityLevel,
    pub write: UserInstructionCapabilityLevel,
    pub remove: UserInstructionCapabilityLevel,
    pub activate: UserInstructionActivationSupport,
    pub reason_code: Option<String>,
    pub evidence_ids: Vec<String>,
}

/// target 投影事实。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInstructionProjectionDto {
    pub state: UserInstructionProjectionState,
    pub desired_revision_id: Option<String>,
    pub applied_revision_id: Option<String>,
    pub observed_hash: Option<String>,
    pub last_error_code: Option<String>,
}

/// 单 target 用户级指令事实。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInstructionTargetDto {
    pub target: AgentTarget,
    pub cli: UserInstructionCliDto,
    pub sources: Vec<UserInstructionSourceDto>,
    pub effective_source_id: Option<String>,
    pub managed_target_path: String,
    pub management_mode: UserInstructionManagementMode,
    pub capability: UserInstructionCapabilityDto,
    pub projection: UserInstructionProjectionDto,
    pub available_actions: Vec<UserInstructionAction>,
}

/// canonical 编辑面。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInstructionCanonicalDto {
    pub asset_id: String,
    pub display_name: String,
    pub head_revision_id: Option<String>,
    pub common_content: String,
    pub target_extensions: BTreeMap<AgentTarget, String>,
    pub deleted: bool,
    /// 超出 control 有界响应预算时输出 true，完整正文应走既有 get_asset。
    #[serde(default)]
    pub content_truncated: bool,
}

/// 用户级指令工作区。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInstructionWorkspaceDto {
    pub scope_id: String,
    pub setup_state: UserInstructionSetupState,
    pub health_state: UserInstructionHealthState,
    pub canonical: Option<UserInstructionCanonicalDto>,
    pub targets: Vec<UserInstructionTargetDto>,
    pub refreshed_at: String,
    pub inventory_snapshot_hash: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InventorySnapshotMaterial<'a> {
    scope_id: &'a str,
    canonical_head: Option<&'a str>,
    targets: Vec<InventoryTargetMaterial<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InventoryTargetMaterial<'a> {
    target: AgentTarget,
    config_root: &'a str,
    cli_version: Option<&'a str>,
    managed_target_path: &'a str,
    management_mode: UserInstructionManagementMode,
    scan_capability: UserInstructionCapabilityLevel,
    write_capability: UserInstructionCapabilityLevel,
    remove_capability: UserInstructionCapabilityLevel,
    activation: UserInstructionActivationSupport,
    capability_reason_code: Option<&'a str>,
    evidence_ids: &'a [String],
    sources: Vec<InventorySourceMaterial<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InventorySourceMaterial<'a> {
    path: &'a str,
    role: UserInstructionSourceRole,
    active: bool,
    exists: bool,
    non_empty: bool,
    hash: Option<&'a str>,
    ownership: UserInstructionOwnership,
}

/// 使用当前进程环境扫描用户级指令。
///
/// Business Logic: 显式刷新必须是零写入 inventory，不创建 binding/materialization。
/// Code Logic: 构造注入环境后委托可测入口。
pub async fn inspect_user_instruction_workspace(
    state: &AppState,
) -> Result<UserInstructionWorkspaceDto, AppError> {
    let env = current_target_environment();
    inspect_user_instruction_workspace_with_env(state, &env).await
}

/// 使用注入环境扫描用户级指令。
///
/// Business Logic: 隔离 HOME 测试和生产扫描必须共用同一路径/优先级逻辑。
/// Code Logic: 读 canonical/bindings/materializations，调 adapter 枚举三 target source chain 并组装 DTO。
pub async fn inspect_user_instruction_workspace_with_env(
    state: &AppState,
    env: &TargetEnvironment,
) -> Result<UserInstructionWorkspaceDto, AppError> {
    let scope_id = state
        .agent_hub_repo
        .resolve_user_scope_id()
        .await?
        .unwrap_or_else(|| USER_SCOPE_STABLE_ID.to_string());
    let asset = state
        .agent_hub_repo
        .get_asset_by_unique_key(
            &scope_id,
            AssetKind::Instruction,
            USER_INSTRUCTION_NAMESPACE,
            USER_INSTRUCTION_LOGICAL_KEY,
        )
        .await?;
    let canonical = load_canonical(state, asset.as_ref()).await?;
    let bindings = if let Some(asset) = asset.as_ref() {
        state
            .agent_hub_repo
            .list_target_bindings_for_asset(&asset.id)
            .await?
    } else {
        vec![]
    };
    let mut materializations = BTreeMap::new();
    for binding in &bindings {
        if let Some(mat) = state
            .agent_hub_repo
            .get_materialization_by_binding(&binding.id)
            .await?
        {
            materializations.insert(binding.id.clone(), mat);
        }
    }
    let ownerships: BTreeMap<AgentTarget, UserInstructionOwnershipRecord> =
        if let Some(asset) = asset.as_ref() {
            state
                .agent_hub_repo
                .list_user_instruction_ownerships(&asset.id)
                .await?
                .into_iter()
                .map(|record| (record.target, record))
                .collect()
        } else {
            BTreeMap::new()
        };

    let homes = TargetPathResolver::resolve_all(env);
    let user_scope = LocalScopeMapping {
        scope_kind: crate::agent_hub::models::ScopeKind::User,
        absolute_path: env.home.clone(),
        project_root: None,
        relative_root: None,
        codex_fallback_filenames: vec![],
    };
    let adapters: Vec<Box<dyn AssetAdapter>> = vec![
        Box::new(ClaudeInstructionAdapter),
        Box::new(CodexInstructionAdapter),
        Box::new(OpenCodeInstructionAdapter),
    ];
    let mut targets = Vec::with_capacity(adapters.len());
    for adapter in adapters {
        let target = adapter.target();
        let binding = bindings.iter().find(|binding| binding.target == target);
        let mat = binding.and_then(|binding| materializations.get(&binding.id));
        let mut scanned = adapter
            .scan_instruction_sources(&user_scope, env)
            .unwrap_or_default();
        add_declared_candidates(target, &homes, &mut scanned);
        let ownership = ownerships.get(&target);
        let sources = build_source_dtos(target, scanned, ownership)?;
        let effective_source_id = sources
            .iter()
            .find(|source| source.active)
            .map(|source| source.source_id.clone());
        let management_mode = normalize_management_mode(binding, mat);
        let managed_target_path = resolve_managed_target_path(target, &homes, ownership, mat);
        let (cli, capability) = evaluate_capability(adapter.as_ref(), env)?;
        let projection = build_projection(asset.as_ref(), binding, mat);
        let available_actions = available_actions(
            canonical.is_some(),
            effective_source_id.is_some(),
            management_mode,
            &capability,
            projection.state,
        );
        targets.push(UserInstructionTargetDto {
            target,
            cli,
            sources,
            effective_source_id,
            managed_target_path: managed_target_path.to_string_lossy().into_owned(),
            management_mode,
            capability,
            projection,
            available_actions,
        });
    }

    let any_managed = targets
        .iter()
        .any(|target| target.management_mode != UserInstructionManagementMode::Unmanaged);
    let any_source = targets
        .iter()
        .flat_map(|target| &target.sources)
        .any(|source| source.exists && source.non_empty);
    let setup_state = if any_managed {
        UserInstructionSetupState::Configured
    } else if canonical.is_some() || any_source {
        UserInstructionSetupState::ReadyToReview
    } else {
        UserInstructionSetupState::Unconfigured
    };
    let health_state = aggregate_health(&targets);
    let inventory_snapshot_hash = inventory_snapshot_hash(&scope_id, canonical.as_ref(), &targets)?;

    Ok(UserInstructionWorkspaceDto {
        scope_id,
        setup_state,
        health_state,
        canonical,
        targets,
        refreshed_at: Utc::now().to_rfc3339(),
        inventory_snapshot_hash,
    })
}

/// 加载 canonical 内容并按 control 响应预算有界截断。
async fn load_canonical(
    state: &AppState,
    asset: Option<&LogicalAsset>,
) -> Result<Option<UserInstructionCanonicalDto>, AppError> {
    let Some(asset) = asset else {
        return Ok(None);
    };
    let (document, _) = load_instruction_document_for_user_v2(asset, state).await?;
    let (mut common_content, mut target_extensions) = split_document_content(&document);
    let mut content_truncated = false;
    common_content = truncate_utf8(
        &common_content,
        MAX_CANONICAL_CONTENT_BYTES,
        &mut content_truncated,
    );
    let mut remaining = MAX_CANONICAL_CONTENT_BYTES.saturating_sub(common_content.len());
    for value in target_extensions.values_mut() {
        *value = truncate_utf8(value, remaining, &mut content_truncated);
        remaining = remaining.saturating_sub(value.len());
    }
    Ok(Some(UserInstructionCanonicalDto {
        asset_id: asset.id.clone(),
        display_name: asset.display_name.clone(),
        head_revision_id: asset
            .current_revision_id
            .as_ref()
            .map(|revision| revision.as_str().to_string()),
        common_content,
        target_extensions,
        deleted: asset.deleted_at.is_some(),
        content_truncated,
    }))
}

/// 将块文档转为“公共 + 目标补充”编辑面。
fn split_document_content(
    document: &InstructionDocument,
) -> (String, BTreeMap<AgentTarget, String>) {
    let mut common = Vec::new();
    let mut extensions: BTreeMap<AgentTarget, Vec<String>> = BTreeMap::new();
    for block in &document.blocks {
        if matches!(
            block.mode,
            InstructionBlockMode::Shared | InstructionBlockMode::Adapted
        ) {
            if let Some(text) = block
                .common_markdown
                .as_ref()
                .filter(|text| !text.is_empty())
            {
                common.push(text.clone());
            }
        }
        for (target, text) in &block.variants {
            if !text.is_empty() {
                extensions.entry(*target).or_default().push(text.clone());
            }
        }
    }
    (
        common.join("\n\n"),
        extensions
            .into_iter()
            .map(|(target, parts)| (target, parts.join("\n\n")))
            .collect(),
    )
}

/// 补全缺失候选，使 inventory 能显示“尚未创建”的管理路径。
fn add_declared_candidates(
    target: AgentTarget,
    homes: &crate::agent_hub::targets::TargetHomes,
    scanned: &mut Vec<InstructionSource>,
) {
    let candidates: Vec<(PathBuf, InstructionSourceRole)> = match target {
        AgentTarget::Claude => vec![(
            homes.claude.config_root.join("CLAUDE.md"),
            InstructionSourceRole::NativePrimary,
        )],
        AgentTarget::Codex => vec![
            (
                homes.codex.config_root.join("AGENTS.override.md"),
                InstructionSourceRole::ManagedProjection,
            ),
            (
                homes.codex.config_root.join("AGENTS.md"),
                InstructionSourceRole::NativePrimary,
            ),
        ],
        AgentTarget::OpenCode => vec![
            (
                homes.opencode.config_root.join("AGENTS.md"),
                InstructionSourceRole::NativePrimary,
            ),
            (
                homes.claude.config_root.join("CLAUDE.md"),
                InstructionSourceRole::Fallback,
            ),
        ],
    };
    let existing: BTreeSet<PathBuf> = scanned.iter().map(|source| source.path.clone()).collect();
    for (path, role) in candidates {
        if existing.contains(&path) {
            continue;
        }
        scanned.push(InstructionSource {
            target,
            path,
            scope_kind: crate::agent_hub::models::ScopeKind::User,
            role,
            active: false,
            native_active: false,
            non_empty: false,
            relative_path: None,
            diagnostics: vec![],
        });
    }
}

/// 将 adapter source 转换为有界 DTO（metadata + 可选正文）。
fn build_source_dtos(
    target: AgentTarget,
    mut sources: Vec<InstructionSource>,
    ownership: Option<&UserInstructionOwnershipRecord>,
) -> Result<Vec<UserInstructionSourceDto>, AppError> {
    sources.sort_by_key(source_sort_key);
    let active_path = sources
        .iter()
        .find(|source| source.active)
        .map(|source| source.path.clone());
    // 无 active 时：为首个现存文件附带正文，保证未纳管也能打开编辑。
    let fallback_body_path = if active_path.is_none() {
        sources
            .iter()
            .find(|source| source.path.is_file())
            .map(|source| source.path.clone())
    } else {
        None
    };
    let mut out = Vec::with_capacity(sources.len());
    for source in sources {
        let metadata = std::fs::metadata(&source.path).ok();
        let exists = metadata.as_ref().is_some_and(|metadata| metadata.is_file());
        let include_body = exists
            && (source.active
                || fallback_body_path
                    .as_ref()
                    .is_some_and(|path| path == &source.path));
        let (hash, size_reason, content, content_truncated) =
            read_source_file_bounded(&source.path, metadata.as_ref(), include_body)?;
        let ownership = ownership_for_path(&source.path, exists, ownership);
        let role = map_source_role(target, &source, active_path.as_deref());
        let reason_code = source.diagnostics.first().cloned().or(size_reason);
        let path = source.path.to_string_lossy().into_owned();
        out.push(UserInstructionSourceDto {
            source_id: sha256_hex(format!("{}|{path}", target.as_str()).as_bytes()),
            path,
            role,
            active: source.active,
            exists,
            non_empty: source.non_empty,
            hash,
            modified_at: metadata
                .and_then(|metadata| metadata.modified().ok())
                .map(system_time_to_rfc3339),
            ownership,
            reason_code,
            content,
            content_truncated,
        });
    }
    Ok(out)
}

/// 定义 source 稳定排序，优先 active，再按 adapter 优先级和路径。
fn source_sort_key(source: &InstructionSource) -> (u8, u8, String) {
    let role = match source.role {
        InstructionSourceRole::ManagedProjection => 0,
        InstructionSourceRole::NativePrimary => 1,
        InstructionSourceRole::Fallback => 2,
        InstructionSourceRole::AncestorPrelude => 3,
    };
    (
        if source.active { 0 } else { 1 },
        role,
        source.path.to_string_lossy().into_owned(),
    )
}

/// 映射 adapter 角色为 V2 用户角色。
fn map_source_role(
    target: AgentTarget,
    source: &InstructionSource,
    active_path: Option<&Path>,
) -> UserInstructionSourceRole {
    if source.path.exists() && !source.active && active_path.is_some() {
        return UserInstructionSourceRole::Shadowed;
    }
    match source.role {
        InstructionSourceRole::ManagedProjection if target == AgentTarget::Codex => {
            UserInstructionSourceRole::Override
        }
        InstructionSourceRole::Fallback | InstructionSourceRole::AncestorPrelude => {
            UserInstructionSourceRole::Fallback
        }
        InstructionSourceRole::ManagedProjection | InstructionSourceRole::NativePrimary => {
            UserInstructionSourceRole::Native
        }
    }
}

/// 有界读文件：hash 始终尽力计算；正文仅在 include_body 时填充。
///
/// Business Logic: inspect 一次读盘即可同时服务 inventory hash 与原始栏加载。
/// Code Logic: 返回 (hash, size_reason, content, content_truncated)。
fn read_source_file_bounded(
    path: &Path,
    metadata: Option<&std::fs::Metadata>,
    include_body: bool,
) -> Result<(Option<String>, Option<String>, Option<String>, bool), AppError> {
    let Some(metadata) = metadata.filter(|metadata| metadata.is_file()) else {
        return Ok((None, None, None, false));
    };
    if metadata.len() > MAX_SOURCE_HASH_BYTES {
        return Ok((
            None,
            Some("user_instruction_source_too_large".to_string()),
            None,
            false,
        ));
    }
    let bytes = std::fs::read(path)?;
    let hash = Some(sha256_hex(&bytes));
    if !include_body {
        return Ok((hash, None, None, false));
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok((
            hash,
            Some("user_instruction_source_not_utf8".to_string()),
            None,
            false,
        ));
    };
    let mut truncated = false;
    let content = truncate_utf8(&text, MAX_CANONICAL_CONTENT_BYTES, &mut truncated);
    Ok((hash, None, Some(content), truncated))
}

/// 根据 materialization 路径判定 ownership，observed hash 不作为所有权证据。
fn ownership_for_path(
    path: &Path,
    exists: bool,
    ownership: Option<&UserInstructionOwnershipRecord>,
) -> UserInstructionOwnership {
    if ownership.is_some_and(|record| Path::new(&record.resolved_path) == path) {
        UserInstructionOwnership::HubManaged
    } else if exists {
        UserInstructionOwnership::External
    } else {
        UserInstructionOwnership::Unknown
    }
}

/// 归一化 legacy binding 为 V2 management mode。
pub(crate) fn normalize_management_mode(
    binding: Option<&TargetBinding>,
    materialization: Option<&Materialization>,
) -> UserInstructionManagementMode {
    let Some(binding) = binding else {
        return UserInstructionManagementMode::Unmanaged;
    };
    if binding.desired_presence == DesiredPresence::Absent
        && !binding.desired_enabled
        && materialization.is_none()
    {
        return UserInstructionManagementMode::Unmanaged;
    }
    if binding.desired_presence == DesiredPresence::Present && binding.desired_enabled {
        UserInstructionManagementMode::ManagedActive
    } else {
        UserInstructionManagementMode::ManagedPaused
    }
}

/// 解析 Hub 将管理的用户路径，Codex 默认使用 base AGENTS.md。
fn resolve_managed_target_path(
    target: AgentTarget,
    homes: &crate::agent_hub::targets::TargetHomes,
    ownership: Option<&UserInstructionOwnershipRecord>,
    materialization: Option<&Materialization>,
) -> PathBuf {
    if let Some(path) = ownership.map(|ownership| ownership.resolved_path.as_str()) {
        return PathBuf::from(path);
    }
    if let Some(path) =
        materialization.and_then(|materialization| materialization.native_path.as_ref())
    {
        return PathBuf::from(path);
    }
    match target {
        AgentTarget::Claude => homes.claude.config_root.join("CLAUDE.md"),
        AgentTarget::Codex => homes.codex.config_root.join("AGENTS.md"),
        AgentTarget::OpenCode => homes.opencode.config_root.join("AGENTS.md"),
    }
}

/// 评估单 target CLI 和分项 capability。
fn evaluate_capability(
    adapter: &dyn AssetAdapter,
    env: &TargetEnvironment,
) -> Result<(UserInstructionCliDto, UserInstructionCapabilityDto), AppError> {
    let probe = adapter.probe(env)?;
    let manifest = builtin_support_manifest()?;
    let evaluated = evaluate_target_support(
        &manifest,
        &RuntimeProbeSnapshot {
            target: probe.target,
            executable: probe.executable.clone(),
            version: probe.version.clone(),
            config_root: probe.config_root.clone(),
            fingerprint: probe.fingerprint,
            help_fingerprint: None,
        },
    );
    let record = find_target_record(&manifest, probe.target);
    let scan = capability_level(
        evaluated.capability(TargetCapability::ScanInstruction),
        true,
    );
    let write_support = evaluated.capability(TargetCapability::RenderInstruction);
    let write = capability_level(write_support, false);
    let remove = write;
    let activate = if write == UserInstructionCapabilityLevel::Blocked {
        UserInstructionActivationSupport::Blocked
    } else if write_support == CapabilitySupport::SupportedAfterRestart {
        UserInstructionActivationSupport::Restart
    } else if evaluated
        .capability(TargetCapability::LiveReload)
        .is_supported_family()
    {
        UserInstructionActivationSupport::Immediate
    } else {
        UserInstructionActivationSupport::NewSession
    };
    Ok((
        UserInstructionCliDto {
            installed: probe.executable.is_some(),
            version: probe.version,
            config_root: probe.config_root.to_string_lossy().into_owned(),
        },
        UserInstructionCapabilityDto {
            scan,
            write,
            remove,
            activate,
            reason_code: evaluated.reasons.first().cloned(),
            evidence_ids: record
                .map(|record| record.evidence_ids.clone())
                .unwrap_or_default(),
        },
    ))
}

/// 将 support manifest 能力映射为 V2 级别，显式保留 ReadOnly。
fn capability_level(
    support: CapabilitySupport,
    scan_capability: bool,
) -> UserInstructionCapabilityLevel {
    match support {
        CapabilitySupport::ReadOnly if scan_capability => UserInstructionCapabilityLevel::ReadOnly,
        CapabilitySupport::Supported
        | CapabilitySupport::SupportedAfterRestart
        | CapabilitySupport::ActivationRequired => UserInstructionCapabilityLevel::Supported,
        CapabilitySupport::Blocked | CapabilitySupport::ReadOnly => {
            UserInstructionCapabilityLevel::Blocked
        }
    }
}

/// 将 legacy materialization 映射为单一 V2 projection state。
fn build_projection(
    asset: Option<&LogicalAsset>,
    binding: Option<&TargetBinding>,
    materialization: Option<&Materialization>,
) -> UserInstructionProjectionDto {
    let state = match materialization.map(|materialization| materialization.status) {
        None => UserInstructionProjectionState::None,
        Some(MaterializationStatus::Pending) => UserInstructionProjectionState::Pending,
        Some(MaterializationStatus::Synced) => UserInstructionProjectionState::InSync,
        Some(MaterializationStatus::Drift) => UserInstructionProjectionState::Drift,
        Some(MaterializationStatus::Detached) => UserInstructionProjectionState::Detached,
        Some(MaterializationStatus::Conflict) => UserInstructionProjectionState::Conflict,
        Some(MaterializationStatus::ExternalCollision) => UserInstructionProjectionState::Collision,
        Some(MaterializationStatus::ActivationRequired) => {
            UserInstructionProjectionState::ActivationRequired
        }
        Some(MaterializationStatus::Blocked | MaterializationStatus::Unsupported) => {
            UserInstructionProjectionState::Blocked
        }
    };
    UserInstructionProjectionDto {
        state,
        desired_revision_id: binding.and_then(|_| {
            asset
                .and_then(|asset| asset.current_revision_id.as_ref())
                .map(|revision| revision.as_str().to_string())
        }),
        applied_revision_id: materialization
            .and_then(|materialization| materialization.last_projected_revision_id.as_ref())
            .map(|revision| revision.as_str().to_string()),
        observed_hash: materialization
            .and_then(|materialization| materialization.observed_external_hash.clone()),
        last_error_code: materialization
            .and_then(|materialization| materialization.last_error.clone()),
    }
}

/// 根据后端 invariant 生成安全动作，写能力 blocked 时不返回执行型管理动作。
fn available_actions(
    has_canonical: bool,
    has_effective_source: bool,
    mode: UserInstructionManagementMode,
    capability: &UserInstructionCapabilityDto,
    projection: UserInstructionProjectionState,
) -> Vec<UserInstructionAction> {
    let mut actions = Vec::new();
    if has_effective_source {
        actions.push(UserInstructionAction::OpenFile);
    }
    let write_allowed = capability.write == UserInstructionCapabilityLevel::Supported;
    match mode {
        UserInstructionManagementMode::Unmanaged if write_allowed => {
            actions.push(UserInstructionAction::Manage);
            if has_effective_source {
                actions.push(UserInstructionAction::Adopt);
            }
        }
        UserInstructionManagementMode::ManagedActive => {
            if write_allowed {
                actions.push(UserInstructionAction::Pause);
                actions.push(UserInstructionAction::Remove);
            }
            if capability.remove == UserInstructionCapabilityLevel::Supported {
                actions.push(UserInstructionAction::StopManaging);
            }
        }
        UserInstructionManagementMode::ManagedPaused => {
            if write_allowed {
                actions.push(UserInstructionAction::Resume);
            }
            if capability.remove == UserInstructionCapabilityLevel::Supported {
                actions.push(UserInstructionAction::StopManaging);
            }
        }
        UserInstructionManagementMode::Unmanaged => {}
    }
    if matches!(
        projection,
        UserInstructionProjectionState::Drift
            | UserInstructionProjectionState::Detached
            | UserInstructionProjectionState::Collision
            | UserInstructionProjectionState::Conflict
    ) {
        actions.push(UserInstructionAction::Compare);
        if projection == UserInstructionProjectionState::Detached && write_allowed {
            actions.push(UserInstructionAction::Restore);
        }
    }
    if has_canonical
        && write_allowed
        && capability.remove == UserInstructionCapabilityLevel::Supported
    {
        actions.push(UserInstructionAction::DeleteAsset);
    }
    actions
}

/// 仅已管理 target 参与健康聚合，unmanaged/paused 不产生 partial。
fn aggregate_health(targets: &[UserInstructionTargetDto]) -> UserInstructionHealthState {
    let active: Vec<&UserInstructionTargetDto> = targets
        .iter()
        .filter(|target| target.management_mode == UserInstructionManagementMode::ManagedActive)
        .collect();
    if active.is_empty() {
        return UserInstructionHealthState::Healthy;
    }
    if active.iter().any(|target| {
        target.capability.write == UserInstructionCapabilityLevel::Blocked
            || matches!(
                target.projection.state,
                UserInstructionProjectionState::Blocked | UserInstructionProjectionState::Conflict
            )
    }) {
        return UserInstructionHealthState::Blocked;
    }
    if active
        .iter()
        .any(|target| target.projection.state != UserInstructionProjectionState::InSync)
    {
        return UserInstructionHealthState::ActionRequired;
    }
    UserInstructionHealthState::Healthy
}

/// 生成不含刷新时间和正文的 inventory 快照 hash。
fn inventory_snapshot_hash(
    scope_id: &str,
    canonical: Option<&UserInstructionCanonicalDto>,
    targets: &[UserInstructionTargetDto],
) -> Result<String, AppError> {
    let material = InventorySnapshotMaterial {
        scope_id,
        canonical_head: canonical.and_then(|canonical| canonical.head_revision_id.as_deref()),
        targets: targets
            .iter()
            .map(|target| InventoryTargetMaterial {
                target: target.target,
                config_root: &target.cli.config_root,
                cli_version: target.cli.version.as_deref(),
                managed_target_path: &target.managed_target_path,
                management_mode: target.management_mode,
                scan_capability: target.capability.scan,
                write_capability: target.capability.write,
                remove_capability: target.capability.remove,
                activation: target.capability.activate,
                capability_reason_code: target.capability.reason_code.as_deref(),
                evidence_ids: &target.capability.evidence_ids,
                sources: target
                    .sources
                    .iter()
                    .map(|source| InventorySourceMaterial {
                        path: &source.path,
                        role: source.role,
                        active: source.active,
                        exists: source.exists,
                        non_empty: source.non_empty,
                        hash: source.hash.as_deref(),
                        ownership: source.ownership,
                    })
                    .collect(),
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&material)?;
    Ok(sha256_hex(&bytes))
}

/// 按 UTF-8 边界有界截断正文。
fn truncate_utf8(value: &str, max_bytes: usize, truncated: &mut bool) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    *truncated = true;
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

/// 将文件修改时间转 RFC3339。
fn system_time_to_rfc3339(value: SystemTime) -> String {
    DateTime::<Utc>::from(value).to_rfc3339()
}

/// 构造当前进程的只读 target 环境。
fn current_target_environment() -> TargetEnvironment {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let interest = [
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "OPENCODE_CONFIG_DIR",
        "OPENCODE_CONFIG",
        "OPENCODE_DISABLE_CLAUDE_CODE",
        "OPENCODE_DISABLE_CLAUDE_CODE_PROMPT",
        "XDG_CONFIG_HOME",
        "HOME",
        "USERPROFILE",
    ];
    let mut vars = BTreeMap::new();
    for key in interest {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                vars.insert(key.to_string(), value);
            }
        }
    }
    let path_entries = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    TargetEnvironment {
        home,
        vars,
        path_entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic: legacy 自动 absent binding 必须回到 unmanaged。
    /// Code Logic: Absent+disabled+no materialization 归一化为 Unmanaged。
    #[test]
    fn legacy_absent_binding_is_unmanaged() {
        let binding = TargetBinding {
            id: "binding".into(),
            asset_id: "asset".into(),
            target: AgentTarget::Claude,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Absent,
            desired_enabled: false,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        };
        assert_eq!(
            normalize_management_mode(Some(&binding), None),
            UserInstructionManagementMode::Unmanaged
        );
    }

    /// Business Logic: readOnly 是可读能力，不能再显示 unsupported。
    /// Code Logic: scan 维度保留 ReadOnly，write 维度 fail-closed。
    #[test]
    fn read_only_capability_is_preserved_for_scan() {
        assert_eq!(
            capability_level(CapabilitySupport::ReadOnly, true),
            UserInstructionCapabilityLevel::ReadOnly
        );
        assert_eq!(
            capability_level(CapabilitySupport::ReadOnly, false),
            UserInstructionCapabilityLevel::Blocked
        );
    }

    /// Business Logic: control 响应中 canonical 正文不得无界增长。
    /// Code Logic: 中文 UTF-8 截断不会切断字符。
    #[test]
    fn truncate_utf8_respects_byte_budget_and_boundary() {
        let mut truncated = false;
        let value = truncate_utf8("中文规则", 5, &mut truncated);
        assert_eq!(value, "中");
        assert!(truncated);
        assert!(value.len() <= 5);
    }

    /// Business Logic: 本机已有用户级文件时，inspect source 必须带可编辑正文。
    /// Code Logic: 临时 CLAUDE.md + active source → content 等于磁盘原文。
    #[test]
    fn build_source_dtos_includes_active_disk_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, "## Rules\n\nAlways test.\n").expect("write");
        let sources = vec![InstructionSource {
            target: AgentTarget::Claude,
            path: path.clone(),
            scope_kind: crate::agent_hub::models::ScopeKind::User,
            role: InstructionSourceRole::NativePrimary,
            active: true,
            native_active: true,
            non_empty: true,
            relative_path: None,
            diagnostics: vec![],
        }];
        let dtos = build_source_dtos(AgentTarget::Claude, sources, None).expect("dto");
        assert_eq!(dtos.len(), 1);
        assert_eq!(
            dtos[0].content.as_deref(),
            Some("## Rules\n\nAlways test.\n")
        );
        assert!(!dtos[0].content_truncated);
        assert!(dtos[0].hash.is_some());
    }

    /// Business Logic: 非 active 源默认不附正文，避免 shadow 源撑爆 IPC。
    /// Code Logic: active=false 且存在 fallback 路径时仅 fallback 带 content。
    #[test]
    fn build_source_dtos_skips_inactive_body_when_active_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let active_path = dir.path().join("AGENTS.override.md");
        let shadow_path = dir.path().join("AGENTS.md");
        std::fs::write(&active_path, "override body\n").expect("write active");
        std::fs::write(&shadow_path, "shadow body\n").expect("write shadow");
        let sources = vec![
            InstructionSource {
                target: AgentTarget::Codex,
                path: active_path,
                scope_kind: crate::agent_hub::models::ScopeKind::User,
                role: InstructionSourceRole::ManagedProjection,
                active: true,
                native_active: false,
                non_empty: true,
                relative_path: None,
                diagnostics: vec![],
            },
            InstructionSource {
                target: AgentTarget::Codex,
                path: shadow_path,
                scope_kind: crate::agent_hub::models::ScopeKind::User,
                role: InstructionSourceRole::NativePrimary,
                active: false,
                native_active: true,
                non_empty: true,
                relative_path: None,
                diagnostics: vec![],
            },
        ];
        let dtos = build_source_dtos(AgentTarget::Codex, sources, None).expect("dto");
        let active = dtos.iter().find(|s| s.active).expect("active");
        let shadow = dtos.iter().find(|s| !s.active).expect("shadow");
        assert_eq!(active.content.as_deref(), Some("override body\n"));
        assert!(shadow.content.is_none());
    }
}
