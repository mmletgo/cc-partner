//! user_mirror/apply — 把 preview plan 的提示词槽、原生文件与 portable 资产落到 dest
//!
//! Business Logic（为什么需要这个模块）:
//!     镜像确认后必须立刻用源端真实字节覆盖目标 Hub 三槽、白名单原生文件和 portable 资产；
//!     单 Agent 失败不得回滚已成功项，也不得把仓库根 `AGENTS.md` 当成 Grok/Cursor 输出。
//!
//! Code Logic（这个模块做什么）:
//!     按 plan.instruction_writes 在 dest process 解析 logical_id → 白名单绝对路径，
//!     Write/Replace 从 CAS 取 UTF-8 字节、Clear 写空串；再按源 slot objects 覆盖三槽。
//!     随后按 Agent 做 portable upsert（replaceAfterPreview）与 dest extras：
//!     Skill/Command detach、Plugin viewing Disable、MCP leaf 删除；不 spawn 未认证 CLI。

use super::models::{
    UserMirrorAgentPlanDto, UserMirrorAgentResultDto, UserMirrorChangeOp, UserMirrorFileChangeDto,
    UserMirrorItemState, UserMirrorPlanDto, UserMirrorPortableChangeDto,
    USER_MIRROR_LEGACY_LOSSY_BLOCKED, USER_MIRROR_NATIVE_PATH_FORBIDDEN,
};
use super::selection::UserMirrorObjectBinding;
use crate::agent_hub::assets::{from_canonical_bytes, PortableAssetPayload};
use crate::agent_hub::config_patch::{
    apply_config_patch_atomically, ConfigPatchOutcome, JsoncConfigPatcher, ManagedConfigPatch,
    SemanticConfigPatcher, TomlConfigPatcher,
};
use crate::agent_hub::migration::{
    USER_INSTRUCTION_DISPLAY_NAME, USER_INSTRUCTION_LOGICAL_KEY, USER_INSTRUCTION_NAMESPACE,
    USER_SCOPE_STABLE_ID,
};
use crate::agent_hub::models::{
    AgentTarget, AssetKind, AssetPolicy, LogicalAsset, NewLogicalAsset, NewScopeNode, ScopeKind,
};
use crate::agent_hub::object_store::{sha256_hex, TreeEntryType, TreeManifest};
use crate::agent_hub::portable_actions::models::PortableAssetActionKind;
use crate::agent_hub::portable_actions::targets::TargetActionRawOutcome;
use crate::agent_hub::portable_inventory::{
    inspect_portable_inventory_force_with_env_query, invalidate_portable_inventory_cache,
    PortableAssetKind, PortableInventoryItemDto, PortableInventoryQuery,
};
use crate::agent_hub::portable_store::{
    ensure_portable_store_layout, execute_skill_or_command_store, store_command_file,
    store_skill_dir,
};
use crate::agent_hub::projection::{AtomicProjectionWriter, AtomicWriteOutcome, FileWriteRequest};
use crate::agent_hub::service::{
    commit_user_instruction_document, load_instruction_document_for_user_v2,
};
use crate::agent_hub::snapshot::portable_builder::bytes_are_legacy_lossy;
use crate::agent_hub::targets::portable::{claude_user_mcp_config_path, render_mcp_projection};
use crate::agent_hub::targets::{TargetEnvironment, TargetHomes, TargetPathResolver};
use crate::agent_hub::user_instructions::{
    replace_slot_text, user_level_mirror_native_paths, write_user_native_instruction_file,
    InstructionSlotKey, WriteUserNativeInstructionFileRequest, MAX_NATIVE_FILE_BYTES,
};
use crate::claude_code_assets::portable_remove_path;
use crate::error::AppError;
use crate::state::AppState;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// 在 dest owning process 应用镜像的提示词槽、原生文件与 portable 资产。
///
/// Business Logic（为什么需要这个函数）:
///     Pull/Push 的 apply 端必须把源端冻结字节写到本机白名单路径、Hub 三槽和 portable 落点；
///     失败按 Agent 记录，已成功文件保留。
///
/// Code Logic（这个函数做什么）:
///     使用当前进程 `TargetEnvironment` 解析 dest 路径；再按 Agent 做 portable upsert/extras。
pub async fn apply_user_mirror_instructions(
    dest_state: &AppState,
    plan: &UserMirrorPlanDto,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<Vec<UserMirrorAgentResultDto>, AppError> {
    let env = TargetEnvironment::from_process();
    apply_user_mirror_instructions_with_env(dest_state, plan, objects, bindings, &env).await
}

/// 注入 dest 环境下的指令/原生文件与 portable apply（测试与生产共用规则）。
///
/// Business Logic: DualEnv 隔离 HOME 必须与生产走同一白名单，禁止信任 LAN 路径。
/// Code Logic: 按 Agent 写 native → 同步三槽 → portable upsert/extras；单 Agent 失败继续其余 Agent。
pub(crate) async fn apply_user_mirror_instructions_with_env(
    dest_state: &AppState,
    plan: &UserMirrorPlanDto,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
    env: &TargetEnvironment,
) -> Result<Vec<UserMirrorAgentResultDto>, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let mut results = Vec::with_capacity(plan.agents.len());
    for agent_plan in &plan.agents {
        let result = apply_one_agent(dest_state, env, &homes, agent_plan, objects, bindings).await;
        results.push(result);
    }
    Ok(results)
}

/// 落地单个 Agent 的 instruction_writes、Hub 三槽与 portable 资产。
///
/// Business Logic: 该 Agent 任一步失败则 Failed，不回滚已写文件，不影响其他 Agent。
/// Code Logic: 先逐条 native；全成功后再覆盖三槽；portable 按条目继续，收集首个失败。
async fn apply_one_agent(
    dest_state: &AppState,
    env: &TargetEnvironment,
    homes: &TargetHomes,
    agent_plan: &UserMirrorAgentPlanDto,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> UserMirrorAgentResultDto {
    for change in &agent_plan.instruction_writes {
        if let Err(error) =
            write_one_native(env, homes, agent_plan.target, change, objects, bindings)
        {
            return failed_agent(agent_plan.target, &error);
        }
    }
    if let Err(error) =
        sync_hub_slots_for_agent(dest_state, agent_plan.target, objects, bindings).await
    {
        return failed_agent(agent_plan.target, &error);
    }
    if let Err(error) =
        apply_agent_portables(dest_state, env, homes, agent_plan, objects, bindings).await
    {
        return failed_agent(agent_plan.target, &error);
    }
    UserMirrorAgentResultDto {
        target: agent_plan.target,
        state: UserMirrorItemState::Succeeded,
        error_code: None,
        message: None,
    }
}

/// 把一条 native instruction_write 写到 dest 白名单路径。
///
/// Business Logic: logical_id 必须在 dest 进程白名单内；仓库根 AGENTS.md 不得作为 Grok 输出。
/// Code Logic: 查 `user_level_mirror_native_paths`；Write/Replace 取 CAS UTF-8；Clear 写空串。
pub(crate) fn write_one_native(
    env: &TargetEnvironment,
    homes: &TargetHomes,
    target: AgentTarget,
    change: &UserMirrorFileChangeDto,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<(), AppError> {
    let path = dest_path_for_logical_id(homes, target, &change.logical_id)?;
    let content = native_content_for_change(target, change, objects, bindings)?;
    if content.len() > MAX_NATIVE_FILE_BYTES {
        return Err(AppError::validation(
            "USER_NATIVE_INSTRUCTION_CONTENT_TOO_LARGE".to_string(),
        ));
    }
    write_dest_native_file(env, &path, &content, change.dest_hash.as_deref())
}

/// dest 进程把 logical_id 映射为白名单绝对路径。
///
/// Business Logic: 禁止信任 LAN 传来的 path；未登记 id 视为逃逸。
/// Code Logic: 仅匹配 `(target, logical_id)`；miss → `USER_MIRROR_NATIVE_PATH_FORBIDDEN`。
fn dest_path_for_logical_id(
    homes: &TargetHomes,
    target: AgentTarget,
    logical_id: &str,
) -> Result<PathBuf, AppError> {
    user_level_mirror_native_paths(homes)
        .into_iter()
        .find(|(mapped_target, mapped_id, _)| *mapped_target == target && mapped_id == logical_id)
        .map(|(_, _, path)| path)
        .ok_or_else(|| AppError::validation(USER_MIRROR_NATIVE_PATH_FORBIDDEN.to_string()))
}

/// 从 CAS / Clear 取出要写入的 UTF-8 正文。
fn native_content_for_change(
    target: AgentTarget,
    change: &UserMirrorFileChangeDto,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<String, AppError> {
    match change.op {
        UserMirrorChangeOp::Clear => Ok(String::new()),
        UserMirrorChangeOp::Write | UserMirrorChangeOp::Replace => {
            let bytes = object_bytes_for_logical_id(target, &change.logical_id, objects, bindings)?;
            String::from_utf8(bytes).map_err(|_| {
                AppError::validation(format!("USER_MIRROR_NATIVE_NOT_UTF8:{}", change.logical_id))
            })
        }
        UserMirrorChangeOp::Delete | UserMirrorChangeOp::Disable => {
            Err(AppError::validation(format!(
                "USER_MIRROR_INSTRUCTION_OP_UNSUPPORTED:{}",
                change.logical_id
            )))
        }
    }
}

/// 按 target+logical_id 取冻结对象字节。
fn object_bytes_for_logical_id(
    target: AgentTarget,
    logical_id: &str,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<Vec<u8>, AppError> {
    let binding = bindings
        .iter()
        .find(|binding| {
            binding.target == target && binding.logical_id.as_deref() == Some(logical_id)
        })
        .ok_or_else(|| AppError::not_found(format!("USER_MIRROR_OBJECT_NOT_FOUND:{logical_id}")))?;
    if binding.blocked || binding.object_hash.is_empty() {
        return Err(AppError::not_found(format!(
            "USER_MIRROR_OBJECT_NOT_FOUND:{logical_id}"
        )));
    }
    objects
        .get(&binding.object_hash)
        .cloned()
        .ok_or_else(|| AppError::not_found(format!("USER_MIRROR_OBJECT_NOT_FOUND:{logical_id}")))
}

/// 写入 dest 白名单文件：优先复用 `write_user_native_instruction_file`，槽落点走同一 CAS writer。
///
/// Business Logic: declared native 与 adapted/exclusive 槽文件都在镜像白名单内，必须能写。
/// Code Logic: 先走用户指令 writer；PATH_NOT_ALLOWED 且路径已在镜像白名单时改 AtomicProjectionWriter。
fn write_dest_native_file(
    env: &TargetEnvironment,
    path: &Path,
    content: &str,
    expected_hash: Option<&str>,
) -> Result<(), AppError> {
    let request = WriteUserNativeInstructionFileRequest {
        path: path.to_string_lossy().into_owned(),
        content: content.to_string(),
        expected_hash: expected_hash.map(str::to_string),
    };
    match write_user_native_instruction_file(env, &request) {
        Ok(_) => Ok(()),
        Err(error) if is_native_path_not_allowed(&error) => {
            write_whitelisted_native_bytes(path, content.as_bytes(), expected_hash)
        }
        Err(error) => Err(error),
    }
}

fn is_native_path_not_allowed(error: &AppError) -> bool {
    error
        .to_string()
        .contains("USER_NATIVE_INSTRUCTION_PATH_NOT_ALLOWED")
}

/// 对镜像白名单内、但用户指令 editor 未收录的槽文件做 CAS 原子写。
fn write_whitelisted_native_bytes(
    path: &Path,
    bytes: &[u8],
    expected_hash: Option<&str>,
) -> Result<(), AppError> {
    let rendered_hash = sha256_hex(bytes);
    let outcome = AtomicProjectionWriter::default()
        .write_file(FileWriteRequest {
            target: path,
            rendered_bytes: bytes,
            rendered_hash: &rendered_hash,
            expected_external_hash: expected_hash,
        })
        .map_err(|error| {
            AppError::generic(format!("USER_NATIVE_INSTRUCTION_WRITE_FAILED:{error}"))
        })?;
    match outcome {
        AtomicWriteOutcome::Replaced { .. } | AtomicWriteOutcome::AlreadyRendered { .. } => Ok(()),
        AtomicWriteOutcome::Drift { .. } => Err(AppError::conflict(
            "USER_NATIVE_INSTRUCTION_STALE".to_string(),
        )),
        AtomicWriteOutcome::DirectoryUnknownFiles { .. } => Err(AppError::generic(
            "USER_NATIVE_INSTRUCTION_UNEXPECTED_DIRECTORY_OUTCOME".to_string(),
        )),
    }
}

/// 用源端 `{target}.hub.{common|adapted|exclusive}` 对象覆盖 dest 三槽。
///
/// Business Logic: 空源槽必须写成空块，不能留下 dest 旧 canonical。
/// Code Logic: 确保 user instruction asset → replace_slot_text → commit（等价 inspect/save）。
async fn sync_hub_slots_for_agent(
    dest_state: &AppState,
    target: AgentTarget,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<(), AppError> {
    let common = hub_slot_text(target, "common", objects, bindings)?;
    let adapted = hub_slot_text(target, "adapted", objects, bindings)?;
    let exclusive = hub_slot_text(target, "exclusive", objects, bindings)?;
    let asset = ensure_dest_instruction_asset(dest_state).await?;
    let (document, _) = load_instruction_document_for_user_v2(&asset, dest_state).await?;
    let document = replace_slot_text(&document, InstructionSlotKey::Shared, &common);
    let document = replace_slot_text(
        &document,
        InstructionSlotKey::Adapted { agent: target },
        &adapted,
    );
    let document = replace_slot_text(
        &document,
        InstructionSlotKey::TargetOnly { agent: target },
        &exclusive,
    );
    let asset = reload_instruction_asset(dest_state, &asset.scope_id).await?;
    commit_user_instruction_document(dest_state, &asset, &document)
        .await
        .map(|_| ())
}

/// 读取源冻结的 Hub 槽正文；缺对象视为空槽。
fn hub_slot_text(
    target: AgentTarget,
    slot: &str,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<String, AppError> {
    let logical_id = format!("{}.hub.{slot}", target.as_str());
    let Some(binding) = bindings.iter().find(|binding| {
        binding.target == target && binding.logical_id.as_deref() == Some(logical_id.as_str())
    }) else {
        return Ok(String::new());
    };
    if binding.blocked || binding.object_hash.is_empty() {
        return Ok(String::new());
    }
    let Some(bytes) = objects.get(&binding.object_hash) else {
        return Ok(String::new());
    };
    String::from_utf8(bytes.clone())
        .map_err(|_| AppError::validation(format!("USER_MIRROR_NATIVE_NOT_UTF8:{logical_id}")))
}

/// 确保 dest 存在 user scope 与 instruction asset。
async fn ensure_dest_instruction_asset(state: &AppState) -> Result<LogicalAsset, AppError> {
    let scope = if let Some(existing) = state.agent_hub_repo.get_scope(USER_SCOPE_STABLE_ID).await?
    {
        existing
    } else if let Some(id) = state.agent_hub_repo.resolve_user_scope_id().await? {
        state
            .agent_hub_repo
            .get_scope(&id)
            .await?
            .ok_or_else(|| AppError::not_found("USER_INSTRUCTION_SCOPE_MISSING".to_string()))?
    } else {
        state
            .agent_hub_repo
            .insert_scope(NewScopeNode {
                id: Some(USER_SCOPE_STABLE_ID.to_string()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await?
    };
    if let Some(asset) = state
        .agent_hub_repo
        .get_asset_by_unique_key(
            &scope.id,
            AssetKind::Instruction,
            USER_INSTRUCTION_NAMESPACE,
            USER_INSTRUCTION_LOGICAL_KEY,
        )
        .await?
    {
        return Ok(asset);
    }
    state
        .agent_hub_repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id,
            kind: AssetKind::Instruction,
            origin_namespace: USER_INSTRUCTION_NAMESPACE.to_string(),
            logical_key: USER_INSTRUCTION_LOGICAL_KEY.to_string(),
            display_name: USER_INSTRUCTION_DISPLAY_NAME.to_string(),
            policy: AssetPolicy::TargetOnly,
        })
        .await
}

async fn reload_instruction_asset(
    state: &AppState,
    scope_id: &str,
) -> Result<LogicalAsset, AppError> {
    state
        .agent_hub_repo
        .get_asset_by_unique_key(
            scope_id,
            AssetKind::Instruction,
            USER_INSTRUCTION_NAMESPACE,
            USER_INSTRUCTION_LOGICAL_KEY,
        )
        .await?
        .ok_or_else(|| AppError::not_found("USER_INSTRUCTION_ASSET_MISSING".to_string()))
}

/// 落地单个 Agent 的 portable upsert 与 dest extras。
///
/// Business Logic: 源有的覆盖安装；目标多出的 Skill/Command 卸下、Plugin Disable、MCP 删 leaf。
///     单条失败不回滚，继续其余条目；不 spawn 未认证 CLI、不 destroyStore 共享包、不删 `~/.agents`。
/// Code Logic: Skill/Command → Plugin → MCP upsert，再 dest extras；收集首个失败。
async fn apply_agent_portables(
    dest_state: &AppState,
    env: &TargetEnvironment,
    homes: &TargetHomes,
    agent_plan: &UserMirrorAgentPlanDto,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<(), AppError> {
    let live = dest_live_items(dest_state, env, agent_plan.target).await?;
    let mut first_error: Option<AppError> = None;
    for change in agent_plan.portable_upserts.iter().filter(|change| {
        matches!(
            change.kind,
            PortableAssetKind::Skill | PortableAssetKind::Command
        )
    }) {
        if let Err(error) =
            upsert_skill_or_command(homes, agent_plan.target, change, objects, bindings)
        {
            first_error.get_or_insert(error);
        }
    }
    for change in agent_plan
        .portable_upserts
        .iter()
        .filter(|change| change.kind == PortableAssetKind::Plugin)
    {
        if let Err(error) = upsert_plugin(homes, agent_plan.target, change, objects, bindings) {
            first_error.get_or_insert(error);
        }
    }
    for change in agent_plan
        .portable_upserts
        .iter()
        .filter(|change| change.kind == PortableAssetKind::Mcp)
    {
        if let Err(error) = upsert_mcp(env, homes, agent_plan.target, change, objects, bindings) {
            first_error.get_or_insert(error);
        }
    }
    for change in &agent_plan.portable_deletes {
        if let Err(error) = extra_skill_or_command(env, homes, agent_plan.target, change, &live) {
            first_error.get_or_insert(error);
        }
    }
    for change in &agent_plan.plugin_disables {
        if let Err(error) = extra_plugin_disable(homes, agent_plan.target, change) {
            first_error.get_or_insert(error);
        }
    }
    for change in &agent_plan.mcp_deletes {
        if let Err(error) = extra_mcp_delete(env, homes, agent_plan.target, change) {
            first_error.get_or_insert(error);
        }
    }
    invalidate_portable_inventory_cache();
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// 扫描 dest 当前 user-scope portable 库存，供 extras 解析 native 路径。
async fn dest_live_items(
    dest_state: &AppState,
    env: &TargetEnvironment,
    target: AgentTarget,
) -> Result<Vec<PortableInventoryItemDto>, AppError> {
    invalidate_portable_inventory_cache();
    let snapshot = inspect_portable_inventory_force_with_env_query(
        dest_state,
        env,
        PortableInventoryQuery {
            target: Some(target),
            kind: None,
            scope_kind: Some(ScopeKind::User),
            local_project_id: None,
        },
    )
    .await?;
    Ok(snapshot
        .items
        .into_iter()
        .filter(|item| item.target == target && item.scope_kind == ScopeKind::User)
        .collect())
}

/// 把源 Skill/Command 写入 portable-store 并 attach 到 viewing Agent native 根。
///
/// Business Logic: 冲突一律替换；真树不得与软链并存。
/// Code Logic: CAS 树/正文落到 store，必要时先卸下 dest 真树，再 `Attach`。
fn upsert_skill_or_command(
    homes: &TargetHomes,
    target: AgentTarget,
    change: &UserMirrorPortableChangeDto,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<(), AppError> {
    let bytes = portable_object_bytes(target, change.kind, &change.native_id, objects, bindings)?;
    let data_dir = crate::config::data_dir()?;
    let store_root = ensure_portable_store_layout(&data_dir)?;
    let native_path = viewing_native_path(homes, target, change.kind, &change.native_id);
    match change.kind {
        PortableAssetKind::Skill => {
            let store_dir = store_skill_dir(&store_root, &change.native_id);
            materialize_skill_into_store(objects, &bytes, &store_dir)?;
        }
        PortableAssetKind::Command => {
            let store_file = store_command_file(&store_root, &change.native_id);
            materialize_command_into_store(&bytes, &store_file)?;
        }
        PortableAssetKind::Plugin | PortableAssetKind::Mcp => {
            return Err(AppError::validation(
                "USER_MIRROR_PORTABLE_KIND_UNSUPPORTED".to_string(),
            ));
        }
    }
    replace_real_tree_if_needed(&native_path)?;
    store_outcome(execute_skill_or_command_store(
        target,
        PortableAssetActionKind::Attach,
        change.kind,
        &change.native_id,
        &native_path,
        None,
    )?)
}

/// 把源 Plugin 包落到 viewing Agent plugins 根并写启用标记。
///
/// Business Logic: 启停只写 viewing 配置，禁止 spawn CLI。
/// Code Logic: CAS 树还原到 `plugins/<id>`；再 patch enabledPlugins / plugins.enabled。
fn upsert_plugin(
    homes: &TargetHomes,
    target: AgentTarget,
    change: &UserMirrorPortableChangeDto,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<(), AppError> {
    let bytes = portable_object_bytes(target, change.kind, &change.native_id, objects, bindings)?;
    let dest_dir = plugin_package_dir(homes, target, &change.native_id);
    if let Some(tree_hash) = plugin_tree_hash(&bytes) {
        materialize_tree_from_objects(objects, &tree_hash, &dest_dir)?;
    } else {
        if let Some(parent) = dest_dir.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir_all(&dest_dir)?;
        fs::write(dest_dir.join("plugin.json"), &bytes)?;
    }
    set_plugin_viewing_enabled(homes, target, &change.native_id, true)
}

/// 把源 MCP leaf（含凭据）写入 dest 该 Agent 配置；legacyLossy 不得覆盖。
///
/// Business Logic: 凭据只走 CAS；占位符标 `USER_MIRROR_LEGACY_LOSSY_BLOCKED` 并保留 dest 原文。
/// Code Logic: blocked/占位直接失败；否则 semantic patch 该 server key。
fn upsert_mcp(
    env: &TargetEnvironment,
    homes: &TargetHomes,
    target: AgentTarget,
    change: &UserMirrorPortableChangeDto,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<(), AppError> {
    let Some(binding) = portable_binding(bindings, target, change.kind, &change.native_id) else {
        return Err(AppError::not_found(format!(
            "USER_MIRROR_OBJECT_NOT_FOUND:{}",
            change.native_id
        )));
    };
    if binding.blocked {
        return Err(AppError::validation(
            USER_MIRROR_LEGACY_LOSSY_BLOCKED.to_string(),
        ));
    }
    let bytes = objects.get(&binding.object_hash).cloned().ok_or_else(|| {
        AppError::not_found(format!("USER_MIRROR_OBJECT_NOT_FOUND:{}", change.native_id))
    })?;
    if bytes_are_legacy_lossy(&bytes) {
        return Err(AppError::validation(
            USER_MIRROR_LEGACY_LOSSY_BLOCKED.to_string(),
        ));
    }
    let Some((path, table, kind)) = mcp_config_spec(env, homes, target) else {
        return Ok(());
    };
    let value = mcp_leaf_value(target, &bytes)?;
    patch_mcp_leaf(&path, kind, table, &change.native_id, Some(value))
}

/// Skill/Command dest extras：只卸下 viewing Agent，不 destroyStore，不删 `~/.agents`。
fn extra_skill_or_command(
    env: &TargetEnvironment,
    homes: &TargetHomes,
    target: AgentTarget,
    change: &UserMirrorPortableChangeDto,
    live: &[PortableInventoryItemDto],
) -> Result<(), AppError> {
    let viewing = viewing_native_path(homes, target, change.kind, &change.native_id);
    let observed = live
        .iter()
        .find(|item| item.kind == change.kind && item.native_id == change.native_id)
        .and_then(|item| item.source_path.as_deref())
        .map(PathBuf::from);
    let native_path = observed.unwrap_or_else(|| viewing.clone());
    if is_agents_source_tree(&native_path, &env.home) {
        if viewing != native_path && viewing.exists() && !is_agents_source_tree(&viewing, &env.home)
        {
            unload_viewing_skill_or_command(target, change.kind, &change.native_id, &viewing)?;
        }
        return Ok(());
    }
    unload_viewing_skill_or_command(target, change.kind, &change.native_id, &native_path)
}

/// Plugin dest extras：只写 viewing Disable，不 Uninstall。
fn extra_plugin_disable(
    homes: &TargetHomes,
    target: AgentTarget,
    change: &UserMirrorPortableChangeDto,
) -> Result<(), AppError> {
    set_plugin_viewing_enabled(homes, target, &change.native_id, false)
}

/// MCP dest extras：从该 Agent 配置 leaf 删除 server 键（含凭据）。
fn extra_mcp_delete(
    env: &TargetEnvironment,
    homes: &TargetHomes,
    target: AgentTarget,
    change: &UserMirrorPortableChangeDto,
) -> Result<(), AppError> {
    let Some((path, table, kind)) = mcp_config_spec(env, homes, target) else {
        return Ok(());
    };
    if !path.is_file() {
        return Ok(());
    }
    patch_mcp_leaf(&path, kind, table, &change.native_id, None)
}

/// 卸下 viewing native：优先 store detach；真树则删除 viewing 副本。
fn unload_viewing_skill_or_command(
    target: AgentTarget,
    kind: PortableAssetKind,
    native_id: &str,
    native_path: &Path,
) -> Result<(), AppError> {
    match execute_skill_or_command_store(
        target,
        PortableAssetActionKind::Detach,
        kind,
        native_id,
        native_path,
        None,
    ) {
        Ok(TargetActionRawOutcome::Applied | TargetActionRawOutcome::Skipped) => Ok(()),
        Ok(TargetActionRawOutcome::Failed { code, .. })
            if code == "PORTABLE_STORE_DISABLE_NOT_A_LINK" =>
        {
            if native_path.exists() {
                portable_remove_path(native_path)?;
            }
            Ok(())
        }
        Ok(other) => store_outcome(other),
        Err(error) => {
            let text = error.to_string();
            if text.contains("PORTABLE_STORE_DISABLE_NOT_A_LINK")
                || text.contains("PORTABLE_STORE_REFUSE_UNLINK_ESCAPE")
            {
                if native_path.exists() && !path_contains_agents(native_path) {
                    portable_remove_path(native_path)?;
                }
                return Ok(());
            }
            Err(error)
        }
    }
}

fn replace_real_tree_if_needed(path: &Path) -> Result<(), AppError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(AppError::from(error)),
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    portable_remove_path(path)
}

fn materialize_skill_into_store(
    objects: &BTreeMap<String, Vec<u8>>,
    packed: &[u8],
    store_dir: &Path,
) -> Result<(), AppError> {
    if let Ok(PortableAssetPayload::Skill(skill)) = from_canonical_bytes(packed) {
        materialize_tree_from_objects(objects, &skill.tree_manifest_hash, store_dir)?;
        if !store_dir.join("SKILL.md").is_file() {
            return Err(AppError::validation(
                "USER_MIRROR_SKILL_TREE_MISSING_SKILL_MD".to_string(),
            ));
        }
        return Ok(());
    }
    if let Some(parent) = store_dir.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(store_dir)?;
    fs::write(store_dir.join("SKILL.md"), packed)?;
    Ok(())
}

fn materialize_command_into_store(packed: &[u8], store_file: &Path) -> Result<(), AppError> {
    if let Some(parent) = store_file.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Ok(PortableAssetPayload::Command(command)) = from_canonical_bytes(packed) {
        let text = format!(
            "---\nname: {}\ndescription: {}\n---\n{}\n",
            command.name,
            command.description.as_deref().unwrap_or(""),
            command.prompt_template
        );
        fs::write(store_file, text)?;
        return Ok(());
    }
    fs::write(store_file, packed)?;
    Ok(())
}

/// 从内存 CAS 对象还原目录树（replaceAfterPreview，不留旧文件）。
fn materialize_tree_from_objects(
    objects: &BTreeMap<String, Vec<u8>>,
    tree_hash: &str,
    dest: &Path,
) -> Result<(), AppError> {
    let manifest_bytes = objects
        .get(tree_hash)
        .ok_or_else(|| AppError::not_found(format!("USER_MIRROR_OBJECT_NOT_FOUND:{tree_hash}")))?;
    let manifest: TreeManifest = serde_json::from_slice(manifest_bytes).map_err(|error| {
        AppError::validation(format!("USER_MIRROR_TREE_MANIFEST_INVALID:{error}"))
    })?;
    let parent = dest
        .parent()
        .ok_or_else(|| AppError::validation("USER_MIRROR_NATIVE_PATH_FORBIDDEN".to_string()))?;
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(".cc-partner-mirror-staging-{}", Uuid::now_v7()));
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    if let Err(error) = write_tree_entries(objects, &manifest, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if dest.exists() {
        portable_remove_path(dest)?;
    }
    if let Err(error) = fs::rename(&staging, dest) {
        let _ = fs::remove_dir_all(&staging);
        return Err(AppError::from(error));
    }
    Ok(())
}

fn write_tree_entries(
    objects: &BTreeMap<String, Vec<u8>>,
    manifest: &TreeManifest,
    dest: &Path,
) -> Result<(), AppError> {
    fs::create_dir_all(dest)?;
    for entry in &manifest.entries {
        let rel = entry.path.replace('\\', "/");
        if rel
            .split('/')
            .any(|part| part.is_empty() || part == ".." || part == ".")
        {
            return Err(AppError::validation(
                USER_MIRROR_NATIVE_PATH_FORBIDDEN.to_string(),
            ));
        }
        let path = dest.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        match entry.entry_type {
            TreeEntryType::File => {
                let blob = objects.get(&entry.blob_hash).ok_or_else(|| {
                    AppError::not_found(format!("USER_MIRROR_OBJECT_NOT_FOUND:{}", entry.blob_hash))
                })?;
                fs::write(&path, blob)?;
            }
            TreeEntryType::Symlink => {}
        }
    }
    Ok(())
}

fn portable_object_bytes(
    target: AgentTarget,
    kind: PortableAssetKind,
    native_id: &str,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<Vec<u8>, AppError> {
    let binding = portable_binding(bindings, target, kind, native_id)
        .ok_or_else(|| AppError::not_found(format!("USER_MIRROR_OBJECT_NOT_FOUND:{native_id}")))?;
    if binding.blocked || binding.object_hash.is_empty() {
        return Err(AppError::validation(
            USER_MIRROR_LEGACY_LOSSY_BLOCKED.to_string(),
        ));
    }
    objects
        .get(&binding.object_hash)
        .cloned()
        .ok_or_else(|| AppError::not_found(format!("USER_MIRROR_OBJECT_NOT_FOUND:{native_id}")))
}

fn portable_binding<'a>(
    bindings: &'a [UserMirrorObjectBinding],
    target: AgentTarget,
    kind: PortableAssetKind,
    native_id: &str,
) -> Option<&'a UserMirrorObjectBinding> {
    bindings.iter().find(|binding| {
        binding.target == target
            && binding.kind == Some(kind)
            && binding.native_id.as_deref() == Some(native_id)
    })
}

fn viewing_native_path(
    homes: &TargetHomes,
    target: AgentTarget,
    kind: PortableAssetKind,
    native_id: &str,
) -> PathBuf {
    let root = config_root_for(homes, target);
    match kind {
        PortableAssetKind::Command => root.join("commands").join(format!("{native_id}.md")),
        _ => root.join("skills").join(native_id),
    }
}

fn plugin_package_dir(homes: &TargetHomes, target: AgentTarget, native_id: &str) -> PathBuf {
    config_root_for(homes, target)
        .join("plugins")
        .join(native_id)
}

fn config_root_for(homes: &TargetHomes, target: AgentTarget) -> PathBuf {
    match target {
        AgentTarget::Claude => homes.claude.config_root.clone(),
        AgentTarget::Codex => homes.codex.config_root.clone(),
        AgentTarget::OpenCode => homes.opencode.config_root.clone(),
        AgentTarget::Grok => homes.grok.config_root.clone(),
        AgentTarget::Gemini => homes.gemini.config_root.clone(),
        AgentTarget::Cursor => homes.cursor.config_root.clone(),
        AgentTarget::Pi => homes.pi.config_root.clone(),
    }
}

fn is_agents_source_tree(path: &Path, home: &Path) -> bool {
    path.starts_with(home.join(".agents")) || path_contains_agents(path)
}

fn path_contains_agents(path: &Path) -> bool {
    path.to_string_lossy()
        .replace('\\', "/")
        .contains("/.agents/")
}

#[derive(Clone, Copy)]
enum McpPatchKind {
    Jsonc,
    Toml,
}

fn mcp_config_spec(
    env: &TargetEnvironment,
    homes: &TargetHomes,
    target: AgentTarget,
) -> Option<(PathBuf, &'static str, McpPatchKind)> {
    match target {
        AgentTarget::Claude => Some((
            claude_user_mcp_config_path(env),
            "mcpServers",
            McpPatchKind::Jsonc,
        )),
        AgentTarget::Codex => Some((
            homes.codex.config_root.join("config.toml"),
            "mcp_servers",
            McpPatchKind::Toml,
        )),
        AgentTarget::Grok => Some((
            homes.grok.config_root.join("config.toml"),
            "mcp_servers",
            McpPatchKind::Toml,
        )),
        AgentTarget::OpenCode => {
            let jsonc = homes.opencode.config_root.join("opencode.jsonc");
            let path = if jsonc.is_file() {
                jsonc
            } else {
                let json = homes.opencode.config_root.join("opencode.json");
                if json.is_file() {
                    json
                } else {
                    homes.opencode.config_file.clone()
                }
            };
            Some((path, "mcpServers", McpPatchKind::Jsonc))
        }
        AgentTarget::Gemini => Some((
            homes.gemini.config_root.join("settings.json"),
            "mcpServers",
            McpPatchKind::Jsonc,
        )),
        AgentTarget::Cursor => Some((
            homes.cursor.config_root.join("mcp.json"),
            "mcpServers",
            McpPatchKind::Jsonc,
        )),
        AgentTarget::Pi => None,
    }
}

fn mcp_leaf_value(target: AgentTarget, bytes: &[u8]) -> Result<serde_json::Value, AppError> {
    if let Ok(PortableAssetPayload::Mcp(server)) = from_canonical_bytes(bytes) {
        let projection = render_mcp_projection(target, &server)?;
        let leaf = projection
            .files
            .first()
            .map(|file| file.bytes.as_slice())
            .unwrap_or(b"{}");
        return serde_json::from_slice(leaf).map_err(|error| {
            AppError::validation(format!("USER_MIRROR_MCP_NATIVE_RENDER:{error}"))
        });
    }
    serde_json::from_slice(bytes)
        .map_err(|error| AppError::validation(format!("USER_MIRROR_MCP_LEAF_INVALID:{error}")))
}

fn patch_mcp_leaf(
    path: &Path,
    kind: McpPatchKind,
    table: &str,
    server_id: &str,
    value: Option<serde_json::Value>,
) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let current = if path.exists() {
        fs::read(path)?
    } else if value.is_none() {
        return Ok(());
    } else {
        match kind {
            McpPatchKind::Jsonc => b"{}".to_vec(),
            McpPatchKind::Toml => Vec::new(),
        }
    };
    let leaf_path = vec![table.to_string(), server_id.to_string()];
    let existing = match kind {
        McpPatchKind::Jsonc => JsoncConfigPatcher.inspect(&current, &leaf_path).ok(),
        McpPatchKind::Toml => TomlConfigPatcher.inspect(&current, &leaf_path).ok(),
    };
    if value.is_none() && existing.as_ref().map_or(true, |owned| !owned.present) {
        return Ok(());
    }
    let expected = existing.and_then(|owned| {
        if owned.present {
            owned.value_hash
        } else {
            None
        }
    });
    let patches = [ManagedConfigPatch {
        owner_id: format!("user-mirror:{server_id}"),
        path: leaf_path,
        value,
        expected_base_hash: expected,
    }];
    let prepared = match kind {
        McpPatchKind::Jsonc => apply_config_patch_atomically(&JsoncConfigPatcher, path, &patches)?,
        McpPatchKind::Toml => apply_config_patch_atomically(&TomlConfigPatcher, path, &patches)?,
    };
    match prepared.patched.outcome {
        ConfigPatchOutcome::Applied => Ok(()),
        other => Err(AppError::conflict(format!(
            "USER_MIRROR_MCP_PATCH:{other:?}"
        ))),
    }
}

fn plugin_tree_hash(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if value.get("kind").and_then(|kind| kind.as_str()) == Some("portablePluginTreeRef") {
        value
            .get("treeManifestHash")
            .and_then(|hash| hash.as_str())
            .filter(|hash| !hash.is_empty())
            .map(str::to_string)
    } else {
        None
    }
}

/// 写 viewing Agent 的 Plugin 启用标记（文件 patch，不 spawn CLI）。
fn set_plugin_viewing_enabled(
    homes: &TargetHomes,
    target: AgentTarget,
    native_id: &str,
    enabled: bool,
) -> Result<(), AppError> {
    match target {
        AgentTarget::Claude => {
            let path = homes.claude.config_root.join("settings.json");
            patch_jsonc_bool(&path, &["enabledPlugins", native_id], enabled)
        }
        AgentTarget::Codex => {
            let path = homes.codex.config_root.join("config.toml");
            patch_toml_plugin_enabled(&path, native_id, enabled)
        }
        AgentTarget::Grok => {
            let path = homes.grok.config_root.join("config.toml");
            patch_grok_plugin_enabled(&path, native_id, enabled)
        }
        AgentTarget::Gemini => {
            let path = homes.gemini.config_root.join("settings.json");
            patch_jsonc_bool(&path, &["enabledPlugins", native_id], enabled)
        }
        AgentTarget::Cursor | AgentTarget::OpenCode | AgentTarget::Pi => Ok(()),
    }
}

fn patch_jsonc_bool(path: &Path, keys: &[&str], enabled: bool) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let patch_path: Vec<String> = keys.iter().map(|key| (*key).to_string()).collect();
    let current = if path.exists() {
        fs::read(path)?
    } else {
        b"{}".to_vec()
    };
    let existing = JsoncConfigPatcher.inspect(&current, &patch_path).ok();
    if existing
        .as_ref()
        .is_some_and(|owned| owned.present && owned.value.as_bool() == Some(enabled))
    {
        return Ok(());
    }
    let expected = existing.and_then(|owned| {
        if owned.present {
            owned.value_hash
        } else {
            None
        }
    });
    let patches = [ManagedConfigPatch {
        owner_id: format!(
            "user-mirror-plugin:{}",
            keys.last().copied().unwrap_or("plugin")
        ),
        path: patch_path,
        value: Some(serde_json::Value::Bool(enabled)),
        expected_base_hash: expected,
    }];
    let prepared = apply_config_patch_atomically(&JsoncConfigPatcher, path, &patches)?;
    match prepared.patched.outcome {
        ConfigPatchOutcome::Applied => Ok(()),
        other => Err(AppError::conflict(format!(
            "USER_MIRROR_PLUGIN_PATCH:{other:?}"
        ))),
    }
}

fn patch_toml_plugin_enabled(path: &Path, native_id: &str, enabled: bool) -> Result<(), AppError> {
    if !path.is_file() && !enabled {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let patch_path = vec!["plugins".into(), native_id.to_string(), "enabled".into()];
    let current = if path.exists() {
        fs::read(path)?
    } else {
        Vec::new()
    };
    let existing = TomlConfigPatcher.inspect(&current, &patch_path).ok();
    if !enabled && existing.as_ref().map_or(true, |owned| !owned.present) {
        return Ok(());
    }
    if existing
        .as_ref()
        .is_some_and(|owned| owned.present && owned.value.as_bool() == Some(enabled))
    {
        return Ok(());
    }
    let expected = existing.and_then(|owned| {
        if owned.present {
            owned.value_hash
        } else {
            None
        }
    });
    let patches = [ManagedConfigPatch {
        owner_id: format!("user-mirror-plugin:{native_id}"),
        path: patch_path,
        value: Some(serde_json::Value::Bool(enabled)),
        expected_base_hash: expected,
    }];
    let prepared = apply_config_patch_atomically(&TomlConfigPatcher, path, &patches)?;
    match prepared.patched.outcome {
        ConfigPatchOutcome::Applied => Ok(()),
        other => Err(AppError::conflict(format!(
            "USER_MIRROR_PLUGIN_PATCH:{other:?}"
        ))),
    }
}

fn patch_grok_plugin_enabled(path: &Path, native_id: &str, enabled: bool) -> Result<(), AppError> {
    if !path.is_file() && enabled {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let current = if path.exists() {
        fs::read(path)?
    } else {
        Vec::new()
    };
    let disabled_path = vec!["plugins".into(), "disabled".into()];
    let owned = TomlConfigPatcher.inspect(&current, &disabled_path).ok();
    let mut disabled: Vec<serde_json::Value> = owned
        .as_ref()
        .and_then(|value| value.value.as_array())
        .cloned()
        .unwrap_or_default();
    let contains = disabled
        .iter()
        .any(|value| value.as_str() == Some(native_id));
    if enabled {
        if !contains {
            return Ok(());
        }
        disabled.retain(|value| value.as_str() != Some(native_id));
    } else if contains {
        return Ok(());
    } else {
        disabled.push(serde_json::Value::String(native_id.to_string()));
    }
    let expected = owned.and_then(|value| {
        if value.present {
            value.value_hash
        } else {
            None
        }
    });
    let patches = [ManagedConfigPatch {
        owner_id: format!("user-mirror-plugin:{native_id}"),
        path: disabled_path,
        value: Some(serde_json::Value::Array(disabled)),
        expected_base_hash: expected,
    }];
    let prepared = apply_config_patch_atomically(&TomlConfigPatcher, path, &patches)?;
    match prepared.patched.outcome {
        ConfigPatchOutcome::Applied => Ok(()),
        other => Err(AppError::conflict(format!(
            "USER_MIRROR_PLUGIN_PATCH:{other:?}"
        ))),
    }
}

fn store_outcome(outcome: TargetActionRawOutcome) -> Result<(), AppError> {
    match outcome {
        TargetActionRawOutcome::Applied | TargetActionRawOutcome::Skipped => Ok(()),
        TargetActionRawOutcome::Failed { code, message }
        | TargetActionRawOutcome::Blocked { code, message }
        | TargetActionRawOutcome::OutcomeUnknown { code, message } => {
            Err(AppError::validation(format!("{code}:{message}")))
        }
    }
}

fn failed_agent(target: AgentTarget, error: &AppError) -> UserMirrorAgentResultDto {
    UserMirrorAgentResultDto {
        target,
        state: UserMirrorItemState::Failed,
        error_code: Some(mirror_error_code(error)),
        message: Some(error.to_string()),
    }
}

/// 从 AppError 抽出稳定镜像 error code。
fn mirror_error_code(error: &AppError) -> String {
    let text = error.to_string();
    for code in [
        USER_MIRROR_LEGACY_LOSSY_BLOCKED,
        USER_MIRROR_NATIVE_PATH_FORBIDDEN,
        "USER_NATIVE_INSTRUCTION_STALE",
        "USER_NATIVE_INSTRUCTION_CONTENT_TOO_LARGE",
        "USER_MIRROR_NATIVE_NOT_UTF8",
        "USER_MIRROR_OBJECT_NOT_FOUND",
    ] {
        if text.contains(code) {
            return code.to_string();
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{
        apply_user_mirror_instructions_with_env, dest_path_for_logical_id, write_one_native,
    };
    use crate::agent_hub::models::AgentTarget;
    use crate::agent_hub::portable_inventory::PortableAssetKind;
    use crate::agent_hub::snapshot::portable_builder::LEGACY_LOSSY_PLACEHOLDER;
    use crate::agent_hub::targets::{TargetEnvironment, TargetPathResolver};
    use crate::agent_hub::user_mirror::inventory::build_local_user_mirror_inventory_with_env;
    use crate::agent_hub::user_mirror::models::{
        UserMirrorAgentResultDto, UserMirrorChangeOp, UserMirrorDirection, UserMirrorFileChangeDto,
        UserMirrorItemState, UserMirrorPlanDto, USER_MIRROR_LEGACY_LOSSY_BLOCKED,
        USER_MIRROR_NATIVE_PATH_FORBIDDEN,
    };
    use crate::agent_hub::user_mirror::preview::preview_from_two_inventories;
    use crate::agent_hub::user_mirror::selection::{
        freeze_user_mirror_selection_with_env, BuiltUserMirrorSelection,
    };
    use crate::backend::runtime::build_app_state;
    use crate::backend::ui::RecordingBackendUi;
    use crate::config::{install_data_dir_env, install_env_var};
    use crate::state::AppState;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct DualEnv {
        _tmp: tempfile::TempDir,
        _guards: Vec<Box<dyn std::any::Any>>,
        source_state: AppState,
        dest_state: AppState,
        source_home: PathBuf,
        dest_home: PathBuf,
        source_env: TargetEnvironment,
        dest_env: TargetEnvironment,
    }

    /// Business Logic（为什么需要这个函数）:
    ///     apply 测试必须同时隔离源/目标 HOME 与 data_dir，避免扫到或改写开发者真实配置。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先构建 source AppState 并释放其 env 锁，再安装 dest HOME/data_dir 构建 dest_state。
    async fn seed_dual_env() -> DualEnv {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source_home = tmp.path().join("source-home");
        let dest_home = tmp.path().join("dest-home");
        let source_data = tmp.path().join("source-data");
        let dest_data = tmp.path().join("dest-data");
        for path in [&source_home, &dest_home, &source_data, &dest_data] {
            fs::create_dir_all(path).expect("mkdir");
        }
        let source_env = isolated_target_env(&source_home);
        let dest_env = isolated_target_env(&dest_home);

        let source_state = {
            let _data = install_data_dir_env(Some(source_data.to_str().expect("utf8 source data")));
            let _home = install_env_var(
                "HOME",
                Some(source_home.to_str().expect("utf8 source home")),
            );
            let ui = Arc::new(RecordingBackendUi::default());
            build_app_state(ui).await.expect("source state")
        };
        let dest_data_guard =
            install_data_dir_env(Some(dest_data.to_str().expect("utf8 dest data")));
        let dest_home_guard =
            install_env_var("HOME", Some(dest_home.to_str().expect("utf8 dest home")));
        let dest_state = {
            let ui = Arc::new(RecordingBackendUi::default());
            build_app_state(ui).await.expect("dest state")
        };
        DualEnv {
            _tmp: tmp,
            _guards: vec![Box::new(dest_data_guard), Box::new(dest_home_guard)],
            source_state,
            dest_state,
            source_home,
            dest_home,
            source_env,
            dest_env,
        }
    }

    fn isolated_target_env(home: &Path) -> TargetEnvironment {
        TargetEnvironment {
            home: home.to_path_buf(),
            vars: BTreeMap::new(),
            path_entries: Vec::new(),
        }
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, text).expect("write");
    }

    /// DualEnv 上跑 preview → freeze → apply，供 portable extras 回归复用。
    async fn run_apply_mirror(
        env: &DualEnv,
    ) -> (
        UserMirrorPlanDto,
        BuiltUserMirrorSelection,
        Vec<UserMirrorAgentResultDto>,
    ) {
        let source_inventory = build_local_user_mirror_inventory_with_env(
            &env.source_state,
            "src-dev",
            &env.source_env,
        )
        .await
        .expect("source inventory");
        let dest_inventory =
            build_local_user_mirror_inventory_with_env(&env.dest_state, "dst-dev", &env.dest_env)
                .await
                .expect("dest inventory");
        let plan = preview_from_two_inventories(
            &source_inventory,
            &dest_inventory,
            "src-dev",
            "dst-dev",
            UserMirrorDirection::Pull,
        );
        let built = freeze_user_mirror_selection_with_env(
            &env.source_state,
            &source_inventory,
            &env.source_env,
        )
        .await
        .expect("freeze");
        let results = apply_user_mirror_instructions_with_env(
            &env.dest_state,
            &plan,
            &built.object_bytes,
            &built.item_bindings,
            &env.dest_env,
        )
        .await
        .expect("apply");
        (plan, built, results)
    }

    fn claude_result(results: &[UserMirrorAgentResultDto]) -> &UserMirrorAgentResultDto {
        results
            .iter()
            .find(|result| result.target == AgentTarget::Claude)
            .expect("claude result")
    }

    async fn rescan_dest_claude_items(
        env: &DualEnv,
        kind: PortableAssetKind,
    ) -> Vec<crate::agent_hub::user_mirror::models::UserMirrorPortableItemDto> {
        crate::agent_hub::portable_inventory::invalidate_portable_inventory_cache();
        let inventory =
            build_local_user_mirror_inventory_with_env(&env.dest_state, "dst-dev", &env.dest_env)
                .await
                .expect("dest rescan");
        inventory
            .agents
            .iter()
            .find(|agent| agent.target == AgentTarget::Claude)
            .expect("claude dest agent")
            .items
            .iter()
            .filter(|item| item.kind == kind)
            .cloned()
            .collect()
    }

    /// Business Logic（为什么需要这个测试）:
    ///     dest CLAUDE.md 必须被源端字节覆盖；源缺失的白名单文件要 Clear；
    ///     Grok 不得把源正文写进仓库/工作区 AGENTS.md 夹具。
    ///
    /// Code Logic（这个测试做什么）:
    ///     DualEnv：源 CLAUDE.md=FROM-SRC、Grok AGENTS.md=FROM-SRC；dest CLAUDE.md=OLD-DEST、
    ///     Codex AGENTS.md 待清、仓库 AGENTS.md 夹具。apply 后断言覆盖/清空/夹具未改。
    #[tokio::test]
    async fn apply_instruction_mirror_overwrites_native_bytes_and_clears_missing() {
        let env = seed_dual_env().await;
        write(
            env.source_home.join(".claude/CLAUDE.md").as_path(),
            "FROM-SRC",
        );
        write(
            env.source_home.join(".grok/AGENTS.md").as_path(),
            "FROM-SRC",
        );
        write(
            env.dest_home.join(".claude/CLAUDE.md").as_path(),
            "OLD-DEST",
        );
        write(
            env.dest_home.join(".codex/AGENTS.md").as_path(),
            "DEST-ONLY-CODEX",
        );
        let repo_agents = env.dest_home.join("proj-not-config/AGENTS.md");
        write(&repo_agents, "REPO-AGENTS-MUST-STAY");

        let source_inventory = build_local_user_mirror_inventory_with_env(
            &env.source_state,
            "src-dev",
            &env.source_env,
        )
        .await
        .expect("source inventory");
        let dest_inventory =
            build_local_user_mirror_inventory_with_env(&env.dest_state, "dst-dev", &env.dest_env)
                .await
                .expect("dest inventory");
        let plan = preview_from_two_inventories(
            &source_inventory,
            &dest_inventory,
            "src-dev",
            "dst-dev",
            UserMirrorDirection::Pull,
        );
        let built = freeze_user_mirror_selection_with_env(
            &env.source_state,
            &source_inventory,
            &env.source_env,
        )
        .await
        .expect("freeze");

        let results = apply_user_mirror_instructions_with_env(
            &env.dest_state,
            &plan,
            &built.object_bytes,
            &built.item_bindings,
            &env.dest_env,
        )
        .await
        .expect("apply");
        let claude = results
            .iter()
            .find(|result| result.target == AgentTarget::Claude)
            .expect("claude result");
        assert_eq!(claude.state, UserMirrorItemState::Succeeded, "{claude:?}");
        let dest_claude = fs::read_to_string(env.dest_home.join(".claude/CLAUDE.md")).unwrap();
        assert_eq!(dest_claude, "FROM-SRC");
        let dest_codex = fs::read_to_string(env.dest_home.join(".codex/AGENTS.md")).unwrap();
        assert_eq!(dest_codex, "");
        let dest_grok = fs::read_to_string(env.dest_home.join(".grok/AGENTS.md")).unwrap();
        assert_eq!(dest_grok, "FROM-SRC");
        assert!(
            !fs::read_to_string(&repo_agents)
                .unwrap_or_default()
                .contains("FROM-SRC"),
            "Grok must not write the repo/workspace AGENTS.md fixture"
        );
        assert_eq!(
            fs::read_to_string(&repo_agents).unwrap(),
            "REPO-AGENTS-MUST-STAY"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     解析到白名单外的 logical_id 必须只让该 Agent 失败，其它 Agent 继续写盘。
    ///
    /// Code Logic（这个测试做什么）:
    ///     给 Grok 追加不在白名单的 instruction_write；Claude 仍 Succeeded 且 CLAUDE.md 已覆盖。
    #[tokio::test]
    async fn whitelist_miss_fails_that_agent_and_continues_others() {
        let env = seed_dual_env().await;
        write(
            env.source_home.join(".claude/CLAUDE.md").as_path(),
            "FROM-SRC",
        );
        write(
            env.dest_home.join(".claude/CLAUDE.md").as_path(),
            "OLD-DEST",
        );

        let source_inventory = build_local_user_mirror_inventory_with_env(
            &env.source_state,
            "src-dev",
            &env.source_env,
        )
        .await
        .expect("source inventory");
        let dest_inventory =
            build_local_user_mirror_inventory_with_env(&env.dest_state, "dst-dev", &env.dest_env)
                .await
                .expect("dest inventory");
        let mut plan = preview_from_two_inventories(
            &source_inventory,
            &dest_inventory,
            "src-dev",
            "dst-dev",
            UserMirrorDirection::Pull,
        );
        let grok = plan
            .agents
            .iter_mut()
            .find(|agent| agent.target == AgentTarget::Grok)
            .expect("grok plan");
        grok.instruction_writes.push(UserMirrorFileChangeDto {
            logical_id: "grok.native.repo-AGENTS.md".into(),
            op: UserMirrorChangeOp::Write,
            source_hash: Some("deadbeef".into()),
            dest_hash: None,
        });
        let built = freeze_user_mirror_selection_with_env(
            &env.source_state,
            &source_inventory,
            &env.source_env,
        )
        .await
        .expect("freeze");

        let results = apply_user_mirror_instructions_with_env(
            &env.dest_state,
            &plan,
            &built.object_bytes,
            &built.item_bindings,
            &env.dest_env,
        )
        .await
        .expect("apply");
        let claude = results
            .iter()
            .find(|result| result.target == AgentTarget::Claude)
            .expect("claude");
        assert_eq!(claude.state, UserMirrorItemState::Succeeded, "{claude:?}");
        assert_eq!(
            fs::read_to_string(env.dest_home.join(".claude/CLAUDE.md")).unwrap(),
            "FROM-SRC"
        );
        let grok = results
            .iter()
            .find(|result| result.target == AgentTarget::Grok)
            .expect("grok");
        assert_eq!(grok.state, UserMirrorItemState::Failed);
        assert_eq!(
            grok.error_code.as_deref(),
            Some(USER_MIRROR_NATIVE_PATH_FORBIDDEN)
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     write_one_native 必须在 logical_id 未登记时 fail-closed，不能猜测路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     孤立 env 上对未知 logical_id 调用 write_one_native，断言 FORBIDDEN。
    #[test]
    fn write_one_native_rejects_unknown_logical_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env = isolated_target_env(tmp.path());
        let homes = TargetPathResolver::resolve_all(&env);
        let change = UserMirrorFileChangeDto {
            logical_id: "claude.native.EVIL.md".into(),
            op: UserMirrorChangeOp::Write,
            source_hash: Some("abc".into()),
            dest_hash: None,
        };
        let err = write_one_native(
            &env,
            &homes,
            AgentTarget::Claude,
            &change,
            &BTreeMap::new(),
            &[],
        )
        .expect_err("forbidden");
        assert!(
            err.to_string().contains(USER_MIRROR_NATIVE_PATH_FORBIDDEN),
            "{err}"
        );
        assert!(
            dest_path_for_logical_id(&homes, AgentTarget::Claude, "claude.native.EVIL.md").is_err()
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     源有的 Skill 必须覆盖安装；目标多出的 Skill 要从 viewing Agent 卸下，
    ///     且不得为了列表干净去删 `~/.agents` 源树。
    ///
    /// Code Logic（这个测试做什么）:
    ///     DualEnv：源 `keep`、dest `dest-only` + `~/.agents` 夹具；apply 后 dest
    ///     库存无 dest-only、有 keep，夹具仍在。
    #[tokio::test]
    async fn apply_portable_upserts_skill_and_unloads_dest_only_without_touching_agents() {
        let env = seed_dual_env().await;
        write(
            env.source_home
                .join(".claude/skills/keep/SKILL.md")
                .as_path(),
            "---\nname: keep\ndescription: keep skill\n---\nKEEP-SKILL-BODY\n",
        );
        write(
            env.dest_home
                .join(".claude/skills/dest-only/SKILL.md")
                .as_path(),
            "---\nname: dest-only\ndescription: dest only\n---\nDEST-ONLY-BODY\n",
        );
        let agents_fixture = env.dest_home.join(".agents/skills/agents-fixture/SKILL.md");
        write(
            &agents_fixture,
            "---\nname: agents-fixture\n---\nAGENTS-FIXTURE\n",
        );

        let (_, _, results) = run_apply_mirror(&env).await;
        assert_eq!(
            claude_result(&results).state,
            UserMirrorItemState::Succeeded,
            "{results:?}"
        );

        let skills = rescan_dest_claude_items(&env, PortableAssetKind::Skill).await;
        assert!(
            skills.iter().any(|item| item.native_id == "keep"),
            "keep skill must be present after upsert: {skills:?}"
        );
        assert!(
            !skills.iter().any(|item| item.native_id == "dest-only"),
            "dest-only skill must be gone from dest inventory: {skills:?}"
        );
        assert!(agents_fixture.is_file(), "~/.agents fixture must remain");
        assert!(
            fs::read_to_string(&agents_fixture)
                .unwrap()
                .contains("AGENTS-FIXTURE"),
            "~/.agents source tree must not be destroyed"
        );
        let keep_body = fs::read_to_string(env.dest_home.join(".claude/skills/keep/SKILL.md"))
            .unwrap_or_default();
        assert!(
            keep_body.contains("KEEP-SKILL-BODY"),
            "source keep skill body must land on dest native root, got {keep_body:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     目标多出的 Plugin 只 Disable viewing 标记，不得 Uninstall，包目录必须留下。
    ///
    /// Code Logic（这个测试做什么）:
    ///     dest 写入 dest-only plugin 包 + enabledPlugins=true；apply 后 actual_enabled=false
    ///     且 package 目录仍在。
    #[tokio::test]
    async fn apply_disables_dest_only_plugin_without_uninstalling_package() {
        let env = seed_dual_env().await;
        let plugin_root = env.dest_home.join(".claude/plugins/dest-only");
        write(
            plugin_root.join(".claude-plugin/plugin.json").as_path(),
            r#"{"name":"dest-only","version":"1.0.0"}"#,
        );
        write(
            env.dest_home.join(".claude/settings.json").as_path(),
            r#"{"enabledPlugins":{"dest-only":true}}"#,
        );

        let (_, _, results) = run_apply_mirror(&env).await;
        assert_eq!(
            claude_result(&results).state,
            UserMirrorItemState::Succeeded,
            "{results:?}"
        );
        assert!(
            plugin_root.join(".claude-plugin/plugin.json").is_file(),
            "plugin package dir must remain after disable"
        );
        let plugins = rescan_dest_claude_items(&env, PortableAssetKind::Plugin).await;
        let dest_only = plugins
            .iter()
            .find(|item| item.native_id == "dest-only" || item.native_id.starts_with("dest-only"))
            .expect("dest-only plugin still inventoried");
        assert_eq!(
            dest_only.actual_enabled,
            Some(false),
            "dest-only plugin must be viewing-disabled: {dest_only:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     源 MCP 凭据必须写入 dest 配置文件；dest 多出的 server 整项删除；
    ///     result JSON 不得回显 secret。
    ///
    /// Code Logic（这个测试做什么）:
    ///     源 `src-api` 含 TOKEN，dest 另有 `dest-only`；apply 后 dest json 含源 secret、
    ///     无 dest-only，结果序列化不含 secret。
    #[tokio::test]
    async fn apply_writes_source_mcp_secret_to_dest_file_and_deletes_dest_only_server() {
        let env = seed_dual_env().await;
        const SRC_SECRET: &str = "src-secret-alpha";
        write(
            env.source_home.join(".claude.json").as_path(),
            &format!(
                r#"{{"mcpServers":{{"src-api":{{"command":"uvx","args":["srv"],"env":{{"TOKEN":"{SRC_SECRET}"}},"enabled":true}}}}}}"#
            ),
        );
        write(
            env.dest_home.join(".claude.json").as_path(),
            r#"{"mcpServers":{"dest-only":{"command":"uvx","args":["gone"],"env":{"TOKEN":"dest-only-secret-beta"},"enabled":true}}}"#,
        );

        let (_, _, results) = run_apply_mirror(&env).await;
        assert_eq!(
            claude_result(&results).state,
            UserMirrorItemState::Succeeded,
            "{results:?}"
        );
        let dest_json = fs::read_to_string(env.dest_home.join(".claude.json")).unwrap();
        assert!(
            dest_json.contains(SRC_SECRET),
            "source MCP secret must be written into dest config file"
        );
        assert!(
            !dest_json.contains("dest-only"),
            "dest-only MCP server key must be removed from dest json: {dest_json}"
        );
        let result_json = serde_json::to_string(&results).unwrap();
        assert!(
            !result_json.contains(SRC_SECRET),
            "apply result JSON must not echo MCP secret"
        );
        assert!(
            !result_json.contains("dest-only-secret-beta"),
            "apply result JSON must not echo dest-only secret"
        );
        let mcps = rescan_dest_claude_items(&env, PortableAssetKind::Mcp).await;
        assert!(
            mcps.iter().any(|item| item.native_id == "src-api"),
            "src-api must be present after upsert: {mcps:?}"
        );
        assert!(
            !mcps.iter().any(|item| item.native_id == "dest-only"),
            "dest-only MCP must be gone from dest inventory: {mcps:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `legacyLossy` 占位不得覆盖 dest 已有真凭据；该 server 标失败并继续其他项。
    ///
    /// Code Logic（这个测试做什么）:
    ///     源 lossy 为 placeholder；dest lossy 为真凭据且有 dest-only。
    ///     apply 后 Claude Failed + LEGACY_LOSSY_BLOCKED，lossy 原凭据保留，dest-only 仍删。
    #[tokio::test]
    async fn apply_legacy_lossy_mcp_fails_that_server_and_keeps_dest_credential() {
        let env = seed_dual_env().await;
        const DEST_SECRET: &str = "dest-real-secret-keep";
        write(
            env.source_home.join(".claude.json").as_path(),
            &format!(
                r#"{{"mcpServers":{{"lossy":{{"command":"uvx","args":["srv"],"env":{{"TOKEN":"{LEGACY_LOSSY_PLACEHOLDER}"}},"enabled":true}}}}}}"#
            ),
        );
        write(
            env.dest_home.join(".claude.json").as_path(),
            &format!(
                r#"{{"mcpServers":{{"lossy":{{"command":"uvx","args":["old"],"env":{{"TOKEN":"{DEST_SECRET}"}},"enabled":true}},"dest-only":{{"command":"uvx","args":["gone"],"enabled":true}}}}}}"#
            ),
        );

        let (_, _, results) = run_apply_mirror(&env).await;
        let claude = claude_result(&results);
        assert_eq!(claude.state, UserMirrorItemState::Failed, "{claude:?}");
        assert_eq!(
            claude.error_code.as_deref(),
            Some(USER_MIRROR_LEGACY_LOSSY_BLOCKED)
        );
        let dest_json = fs::read_to_string(env.dest_home.join(".claude.json")).unwrap();
        assert!(
            dest_json.contains(DEST_SECRET),
            "dest original credential must be kept: {dest_json}"
        );
        assert!(
            !dest_json.contains(LEGACY_LOSSY_PLACEHOLDER),
            "legacyLossy placeholder must not overwrite dest leaf"
        );
        assert!(
            !dest_json.contains("dest-only"),
            "other MCP dest extras must continue: {dest_json}"
        );
        let result_json = serde_json::to_string(&results).unwrap();
        assert!(!result_json.contains(DEST_SECRET));
    }
}
