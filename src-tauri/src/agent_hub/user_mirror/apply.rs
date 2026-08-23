//! user_mirror/apply — 把 preview plan 的提示词槽与原生文件落到 dest
//!
//! Business Logic（为什么需要这个模块）:
//!     镜像确认后必须立刻用源端真实字节覆盖目标 Hub 三槽和白名单原生文件；
//!     单 Agent 失败不得回滚已成功项，也不得把仓库根 `AGENTS.md` 当成 Grok/Cursor 输出。
//!
//! Code Logic（这个模块做什么）:
//!     按 plan.instruction_writes 在 dest process 解析 logical_id → 白名单绝对路径，
//!     Write/Replace 从 CAS 取 UTF-8 字节、Clear 写空串；再按源 slot objects 覆盖三槽。
//!     本 task 不处理 Skill/MCP extras。

use super::models::{
    UserMirrorAgentPlanDto, UserMirrorAgentResultDto, UserMirrorChangeOp, UserMirrorFileChangeDto,
    UserMirrorItemState, UserMirrorPlanDto, USER_MIRROR_NATIVE_PATH_FORBIDDEN,
};
use super::selection::UserMirrorObjectBinding;
use crate::agent_hub::migration::{
    USER_INSTRUCTION_DISPLAY_NAME, USER_INSTRUCTION_LOGICAL_KEY, USER_INSTRUCTION_NAMESPACE,
    USER_SCOPE_STABLE_ID,
};
use crate::agent_hub::models::{
    AgentTarget, AssetKind, AssetPolicy, LogicalAsset, NewLogicalAsset, NewScopeNode, ScopeKind,
};
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::projection::{AtomicProjectionWriter, AtomicWriteOutcome, FileWriteRequest};
use crate::agent_hub::service::{
    commit_user_instruction_document, load_instruction_document_for_user_v2,
};
use crate::agent_hub::targets::{TargetEnvironment, TargetHomes, TargetPathResolver};
use crate::agent_hub::user_instructions::{
    replace_slot_text, user_level_mirror_native_paths, write_user_native_instruction_file,
    InstructionSlotKey, WriteUserNativeInstructionFileRequest, MAX_NATIVE_FILE_BYTES,
};
use crate::error::AppError;
use crate::state::AppState;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 在 dest owning process 应用镜像的提示词槽与原生文件。
///
/// Business Logic（为什么需要这个函数）:
///     Pull/Push 的 apply 端必须把源端冻结字节写到本机白名单路径和 Hub 三槽；
///     失败按 Agent 记录，已成功文件保留。
///
/// Code Logic（这个函数做什么）:
///     使用当前进程 `TargetEnvironment` 解析 dest 路径；portable extras 本 task 跳过。
pub async fn apply_user_mirror_instructions(
    dest_state: &AppState,
    plan: &UserMirrorPlanDto,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<Vec<UserMirrorAgentResultDto>, AppError> {
    let env = TargetEnvironment::from_process();
    apply_user_mirror_instructions_with_env(dest_state, plan, objects, bindings, &env).await
}

/// 注入 dest 环境下的指令/原生文件 apply（测试与生产共用规则）。
///
/// Business Logic: DualEnv 隔离 HOME 必须与生产走同一白名单，禁止信任 LAN 路径。
/// Code Logic: 按 Agent 写 native → 同步三槽；单 Agent 失败继续其余 Agent。
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

/// 落地单个 Agent 的 instruction_writes 与 Hub 三槽。
///
/// Business Logic: 该 Agent 任一步失败则 Failed，不回滚已写文件，不影响其他 Agent。
/// Code Logic: 先逐条 native；全成功后再覆盖 common/adapted/exclusive。
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
    use crate::agent_hub::targets::{TargetEnvironment, TargetPathResolver};
    use crate::agent_hub::user_mirror::inventory::build_local_user_mirror_inventory_with_env;
    use crate::agent_hub::user_mirror::models::{
        UserMirrorChangeOp, UserMirrorDirection, UserMirrorFileChangeDto, UserMirrorItemState,
        USER_MIRROR_NATIVE_PATH_FORBIDDEN,
    };
    use crate::agent_hub::user_mirror::preview::preview_from_two_inventories;
    use crate::agent_hub::user_mirror::selection::freeze_user_mirror_selection_with_env;
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
}
