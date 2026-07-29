//! agent_hub/projection_ops — 生产路径 projection 调度与 hub 启用
//!
//! Business Logic（为什么需要这个模块）:
//!     Gate A 修复：UI/启用项目/绑定变更后必须真正入队 projection job，并在首次写路径
//!     上将 `agent_hub.enabled` 置 true，让 owner runtime 武装 watcher。
//!
//! Code Logic（这个模块做什么）:
//!     `ensure_agent_hub_enabled`、`schedule_asset_projections`、`schedule_project_projections`；
//!     解析 user/project 目标路径，compile_render + ProjectionScheduler::enqueue_projection；
//!     blocked checkout 跳过 Present 写；best-effort 单 target 失败不拖垮整资产。

use crate::agent_hub::instructions::{compile_render, InstructionDocument};
use crate::agent_hub::models::{
    AgentTarget, AssetKind, DesiredPresence, LogicalAsset, ProjectionPayloadKind, ScopeKind,
    ScopeNode, TargetBinding,
};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::projection::{ProjectionRequest, ProjectionScheduler};
use crate::agent_hub::targets::{InstructionRenderContext, TargetEnvironment, TargetPathResolver};
use crate::config_runtime::update_config_transactionally;
use crate::error::AppError;
use crate::state::AppState;
use crate::storage::agent_hub_repo::AgentHubCheckoutBindingRow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 确保 Agent Hub 配置启用（不触碰 background_enabled）。
///
/// Business Logic（为什么需要这个函数）:
///     用户首次保存指令/启用项目后，owner runtime 需看到 `enabled=true` 才能武装
///     watch/ticker；未成功 install 登录项前不得改 `background_enabled`。
///
/// Code Logic（这个函数做什么）:
///     已 enabled → Ok；否则 `update_config_transactionally` 置 `cfg.agent_hub.enabled=true`。
///     失败仅 warn，成功保存后返回 Ok 以便 owner 循环可 arm。
pub async fn ensure_agent_hub_enabled(state: &AppState) -> Result<(), AppError> {
    let already = {
        let cfg = state
            .config
            .read()
            .map_err(|_| AppError::generic("agent_hub_config_lock_poisoned"))?;
        cfg.agent_hub.enabled
    };
    if already {
        return Ok(());
    }
    match update_config_transactionally(&state.config_runtime, |cfg| {
        cfg.agent_hub.enabled = true;
        Ok(())
    })
    .await
    {
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "agent_hub ensure_agent_hub_enabled failed (best-effort)"
            );
            // 偏好在成功保存后 Ok；失败时仍返回 Err 让调用方决定是否 continue
            Err(e)
        }
    }
}

/// 为 package 资产调度 deactivation（desiredEnabled=false）。
///
/// Business Logic（为什么需要这个函数）:
///     disable 不得只 flip DB；package 资产需 remove-with-binding-retained 语义，
///     将 materialization 标 Pending + 策略 token，供 runtime/activator 消费。
///
/// Code Logic（这个函数做什么）:
///     非 package kind → 0；列出 Present 且 disabled 的 binding；materialization 标 Pending；
///     返回处理条数（best-effort，无 CLI 执行，真实 uninstall 由后续 activator 路径完成）。
pub async fn schedule_package_deactivation(
    state: &AppState,
    asset_id: &str,
) -> Result<u32, AppError> {
    let asset = state
        .agent_hub_repo
        .get_asset(asset_id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("agent_hub_asset_not_found:{asset_id}")))?;
    if !matches!(
        asset.kind,
        AssetKind::Skill
            | AssetKind::Command
            | AssetKind::Agent
            | AssetKind::Plugin
            | AssetKind::Mcp
    ) {
        return Ok(0);
    }
    let bindings = state
        .agent_hub_repo
        .list_target_bindings_for_asset(&asset.id)
        .await?;
    let mut n = 0u32;
    for b in bindings {
        if b.desired_presence != DesiredPresence::Present || b.desired_enabled {
            continue;
        }
        let mat = state
            .agent_hub_repo
            .get_materialization_by_binding(&b.id)
            .await?;
        let strategy = crate::agent_hub::models::TargetDisableStrategy::for_target(b.target);
        state
            .agent_hub_repo
            .upsert_materialization(crate::agent_hub::models::NewMaterialization {
                asset_id: asset.id.clone(),
                target: b.target,
                target_binding_id: b.id.clone(),
                native_path: mat.as_ref().and_then(|m| m.native_path.clone()),
                last_projected_revision_id: mat
                    .as_ref()
                    .and_then(|m| m.last_projected_revision_id.clone()),
                rendered_hash: mat.as_ref().and_then(|m| m.rendered_hash.clone()),
                observed_external_hash: mat.as_ref().and_then(|m| m.observed_external_hash.clone()),
                status: crate::agent_hub::models::MaterializationStatus::Pending,
                last_error: Some(format!(
                    "disable_strategy:{}:deactivation_scheduled",
                    strategy.as_str()
                )),
            })
            .await?;
        n = n.saturating_add(1);
    }
    Ok(n)
}

/// 为单个 instruction 资产调度全部 target 投影 job。
///
/// Business Logic（为什么需要这个函数）:
///     Hub revision 或 binding 变化后，需按 desired_presence 把当前 head 投影到各 CLI 文件。
///
/// Code Logic（这个函数做什么）:
///     加载 asset/document/scope/bindings；Present 时 compile_render 并入队；Absent 入队删除；
///     blocked checkout 跳过 Present；单 target 失败 warn 并继续；返回成功入队数。
pub async fn schedule_asset_projections(state: &AppState, asset_id: &str) -> Result<u32, AppError> {
    let asset = state
        .agent_hub_repo
        .get_asset(asset_id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("agent_hub_asset_not_found:{asset_id}")))?;
    if asset.kind != AssetKind::Instruction {
        // package disable 路径走 schedule_package_deactivation；其它 kind 当前 0
        return schedule_package_deactivation(state, asset_id).await;
    }
    let Some(rev_id) = asset.current_revision_id.clone() else {
        return Ok(0);
    };

    let (document, _) = load_instruction_document(state, &asset).await?;
    let scope = state
        .agent_hub_repo
        .get_scope(&asset.scope_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found(format!("agent_hub_scope_not_found:{}", asset.scope_id))
        })?;

    let bindings = state
        .agent_hub_repo
        .list_target_bindings_for_asset(&asset.id)
        .await?;
    if bindings.is_empty() {
        return Ok(0);
    }

    let data_dir = crate::config::data_dir()?;
    let object_store = ObjectStore::open(&data_dir)?;
    let scheduler = ProjectionScheduler::new(state.agent_hub_repo.as_ref().clone(), object_store);

    let hub_project_id = match scope.kind {
        ScopeKind::User => None,
        ScopeKind::Project | ScopeKind::Directory => scope.hub_project_id.clone(),
    };

    let env = current_target_environment();
    let mut successes: u32 = 0;

    for binding in bindings {
        match schedule_one_binding(
            state,
            &scheduler,
            &asset,
            &document,
            &scope,
            &binding,
            hub_project_id.as_deref(),
            &rev_id,
            &env,
        )
        .await
        {
            Ok(true) => successes += 1,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    asset_id = %asset.id,
                    target = %binding.target.as_str(),
                    error = %e,
                    "agent_hub schedule_asset_projections target failed (best-effort)"
                );
            }
        }
    }
    Ok(successes)
}

/// 为已 opt-in 的 Workbench 项目调度其下全部 instruction 投影。
///
/// Business Logic（为什么需要这个函数）:
///     enable/refresh checkout 后需要把项目 scope 内指令投影到各 checkout 目标文件。
///
/// Code Logic（这个函数做什么）:
///     mapping 未 opt-in → 0；按 hub_project_id 过滤 scopes → list instruction assets →
///     逐个 `schedule_asset_projections`（单资产失败 best-effort）；仅项目/目录 scope，不含 user。
pub async fn schedule_project_projections(
    state: &AppState,
    workbench_project_id: &str,
) -> Result<u32, AppError> {
    let mapping = state
        .agent_hub_repo
        .get_project_mapping_by_local_workbench_id(workbench_project_id)
        .await?;
    let Some(mapping) = mapping.filter(|m| m.opted_in) else {
        return Ok(0);
    };

    let scopes = state.agent_hub_repo.list_scopes().await?;
    let project_scopes: Vec<ScopeNode> = scopes
        .into_iter()
        .filter(|s| {
            matches!(s.kind, ScopeKind::Project | ScopeKind::Directory)
                && s.hub_project_id.as_deref() == Some(mapping.hub_project_id.as_str())
        })
        .collect();

    let mut total: u32 = 0;
    for scope in project_scopes {
        let assets = state
            .agent_hub_repo
            .list_assets(Some(&scope.id), Some(AssetKind::Instruction))
            .await?;
        for asset in assets {
            match schedule_asset_projections(state, &asset.id).await {
                Ok(n) => total = total.saturating_add(n),
                Err(e) => {
                    tracing::warn!(
                        asset_id = %asset.id,
                        project_id = %workbench_project_id,
                        error = %e,
                        "agent_hub schedule_project_projections asset failed (best-effort)"
                    );
                }
            }
        }
    }
    Ok(total)
}

/// 为单条 binding 构建并入队 projection request。
///
/// Business Logic（为什么需要这个函数）:
///     每个 target 的路径、blocked 门闸与 rendered hash 独立；Present 需编译，Absent 可安全调度。
///
/// Code Logic（这个函数做什么）:
///     blocked+Present → skip；解析路径 → compile_render(Present) 或空字节(Absent) → enqueue。
///     成功入队返回 true，skip 返回 false。
#[allow(clippy::too_many_arguments)]
async fn schedule_one_binding(
    state: &AppState,
    scheduler: &ProjectionScheduler,
    asset: &LogicalAsset,
    document: &InstructionDocument,
    scope: &ScopeNode,
    binding: &TargetBinding,
    hub_project_id: Option<&str>,
    rev_id: &crate::agent_hub::models::RevisionId,
    env: &TargetEnvironment,
) -> Result<bool, AppError> {
    let checkout = if let Some(cb_id) = binding.checkout_binding_id.as_deref() {
        state.agent_hub_repo.get_checkout_binding(cb_id).await?
    } else {
        None
    };

    // blocked checkout：跳过 Present 写，避免覆盖冲突文件；Absent 仍可安全调度。
    if binding.desired_presence == DesiredPresence::Present {
        if let Some(cb) = checkout.as_ref() {
            if cb.status == "blocked" {
                tracing::warn!(
                    asset_id = %asset.id,
                    target = %binding.target.as_str(),
                    checkout_id = %cb.id,
                    "agent_hub skip Present projection: checkout blocked"
                );
                return Ok(false);
            }
        }
    }

    let target_path =
        resolve_target_path(state, binding.target, scope, checkout.as_ref(), env).await?;
    let target_path_str = target_path.to_string_lossy().into_owned();

    let (rendered_bytes, rendered_hash) = match binding.desired_presence {
        DesiredPresence::Present => {
            let ctx = build_render_context(scope, checkout.as_ref()).await?;
            let compiled = compile_render(document, binding.target, &ctx);
            let hash = sha256_hex(&compiled.bytes);
            (compiled.bytes, hash)
        }
        DesiredPresence::Absent => {
            let bytes = Vec::new();
            let hash = sha256_hex(&bytes);
            (bytes, hash)
        }
    };

    let current_hash = read_path_hash(&target_path).await;

    let request = ProjectionRequest {
        asset_id: asset.id.clone(),
        target: binding.target,
        target_binding_id: binding.id.clone(),
        desired_revision_id: Some(rev_id.clone()),
        target_path: target_path_str,
        expected_external_hash: current_hash.clone(),
        rendered_hash,
        rendered_bytes,
        desired_presence: binding.desired_presence,
        desired_enabled: binding.desired_enabled,
        payload_kind: ProjectionPayloadKind::File,
        directory_entries: None,
        managed_paths: None,
        hub_project_id: hub_project_id.map(|s| s.to_string()),
        base_hash: current_hash,
    };

    scheduler.enqueue_projection(request).await?;
    Ok(true)
}

/// 解析目标 CLI 指令文件绝对路径。
///
/// Business Logic（为什么需要这个函数）:
///     user 与 project/directory 的路径根不同；必须复用 TargetPathResolver 与 mapping 绝对路径。
///
/// Code Logic（这个函数做什么）:
///     User: resolve_all → Claude CLAUDE.md / Codex AGENTS.override.md / OpenCode AGENTS.md；
///     Project/Directory: mapping.local_absolute_path + scope.relative_path + 文件名。
async fn resolve_target_path(
    state: &AppState,
    target: AgentTarget,
    scope: &ScopeNode,
    checkout: Option<&AgentHubCheckoutBindingRow>,
    env: &TargetEnvironment,
) -> Result<PathBuf, AppError> {
    let file_name = instruction_file_name(target);
    match scope.kind {
        ScopeKind::User => {
            let homes = TargetPathResolver::resolve_all(env);
            let root = match target {
                AgentTarget::Claude => homes.claude.config_root,
                AgentTarget::Codex => homes.codex.config_root,
                AgentTarget::OpenCode => homes.opencode.config_root,
            };
            Ok(root.join(file_name))
        }
        ScopeKind::Project | ScopeKind::Directory => {
            let project_root = resolve_project_root(state, scope, checkout).await?;
            let rel = scope.relative_path.as_deref().unwrap_or("").trim();
            let dir = if rel.is_empty() {
                project_root
            } else {
                project_root.join(rel)
            };
            Ok(dir.join(file_name))
        }
    }
}

/// 解析项目/目录 scope 的 checkout 绝对根。
///
/// Business Logic（为什么需要这个函数）:
///     portable scope 不存绝对路径；投影必须从 mapping 或 checkout binding 取本机路径。
///
/// Code Logic（这个函数做什么）:
///     优先 checkout.local_absolute_path；否则 mapping.local_absolute_path by hub_project_id。
async fn resolve_project_root(
    state: &AppState,
    scope: &ScopeNode,
    checkout: Option<&AgentHubCheckoutBindingRow>,
) -> Result<PathBuf, AppError> {
    if let Some(cb) = checkout {
        if let Some(p) = cb
            .local_absolute_path
            .as_ref()
            .filter(|s| !s.trim().is_empty())
        {
            return Ok(PathBuf::from(p));
        }
    }
    let hub_id = scope
        .hub_project_id
        .as_deref()
        .ok_or_else(|| AppError::validation("agent_hub_project_scope_missing_hub_project_id"))?;
    let mapping = state
        .agent_hub_repo
        .get_project_mapping_by_hub_project_id(hub_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found(format!("agent_hub_project_mapping_not_found:{hub_id}"))
        })?;
    let path = mapping
        .local_absolute_path
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            AppError::validation(format!(
                "agent_hub_project_mapping_missing_local_path:{hub_id}"
            ))
        })?;
    Ok(PathBuf::from(path))
}

/// 构建 compile_render 上下文。
///
/// Business Logic（为什么需要这个函数）:
///     OpenCode prelude 与相对目录依赖 project_root / directory_relative。
///
/// Code Logic（这个函数做什么）:
///     User scope → 空上下文；Project/Directory → relative_path + project_root（来自 checkout/mapping）。
async fn build_render_context(
    scope: &ScopeNode,
    checkout: Option<&AgentHubCheckoutBindingRow>,
) -> Result<InstructionRenderContext, AppError> {
    match scope.kind {
        ScopeKind::User => Ok(InstructionRenderContext::default()),
        ScopeKind::Project | ScopeKind::Directory => {
            let project_root = checkout
                .and_then(|c| c.local_absolute_path.as_ref())
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from);
            let directory_relative = scope
                .relative_path
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            Ok(InstructionRenderContext {
                project_root,
                directory_relative,
                ancestor_agent_paths: Vec::new(),
            })
        }
    }
}

/// 目标指令文件名。
///
/// Business Logic（为什么需要这个函数）:
///     三 CLI 文件名固定，调度与 adapter 必须一致。
///
/// Code Logic（这个函数做什么）:
///     Claude→CLAUDE.md；Codex→AGENTS.override.md；OpenCode→AGENTS.md。
fn instruction_file_name(target: AgentTarget) -> &'static str {
    match target {
        AgentTarget::Claude => "CLAUDE.md",
        AgentTarget::Codex => "AGENTS.override.md",
        AgentTarget::OpenCode => "AGENTS.md",
    }
}

/// 读取路径当前内容 SHA-256（文件不存在返回 None）。
///
/// Business Logic（为什么需要这个函数）:
///     enqueue 需要 expected_external_hash / base_hash 做 drift 检测。
///
/// Code Logic（这个函数做什么）:
///     异步读文件 → sha256_hex；NotFound → None；其它 IO 错误 → None（best-effort）。
async fn read_path_hash(path: &Path) -> Option<String> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Some(sha256_hex(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "agent_hub read target path hash failed"
            );
            None
        }
    }
}

/// 从 CAS 加载 instruction 文档（与 service 同语义，本模块独立最小版）。
///
/// Business Logic（为什么需要这个函数）:
///     projection 调度需在 service 外复用 payload 解析，禁止把正文写入日志。
///
/// Code Logic（这个函数做什么）:
///     current revision → get_blob → JSON InstructionDocument 或 UTF-8 markdown shared。
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
    let store = ObjectStore::open(crate::config::data_dir()?)?;
    let bytes = store.get_blob(hash).await?;
    if let Ok(doc) = serde_json::from_slice::<InstructionDocument>(&bytes) {
        return Ok((doc, None));
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok((
        InstructionDocument::from_shared_markdown(asset.logical_key.clone(), text),
        Some("payload treated as shared markdown".to_string()),
    ))
}

/// 构造当前进程注入环境（不改 process env）。
///
/// Business Logic（为什么需要这个函数）:
///     user 级路径解析依赖 CLAUDE_CONFIG_DIR/CODEX_HOME/OPENCODE_* 与 home。
///
/// Code Logic（这个函数做什么）:
///     dirs::home_dir + 关注 env 变量 + PATH 切分。
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
