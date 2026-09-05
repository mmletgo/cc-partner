//! portable_inventory/scanner — 三 target 四类资产库存扫描 + inspect 入口（目录模块）
//!
//! Business Logic（为什么需要这个模块）:
//!     inspect 必须从本机真实路径/配置观测 Skill/Command/Plugin/MCP 事实
//!     （actualEnabled/origin/parentPlugin/hash/capability），ensure 管理账本后与 canonical 对账；
//!     不得静默改目标磁盘资产内容；CAS 原字节与 adoption 移盘不在本路径。
//!
//! Code Logic（这个模块做什么）:
//!     调 AssetAdapter::scan_portable_assets + 只读 Plugin package 发现；
//!     将 `DiscoveredPortableAsset` 转为 `PortableInventoryItemDto`；
//!     inspect：scan → ensure_managed（ledger）→ reconcile；unopted 项目只读 mutation 能力。
//!     本目录按职责拆分：`plugin_roots`（Plugin 包扫描与根候选）、`hashing`（确定性树哈希）、
//!     `items`（发现项→inventory item 组装与能力判定）、`tests`（单元测试）；
//!     对外路径 `agent_hub::portable_inventory::scanner::*` 保持不变。

mod hashing;
mod items;
mod plugin_roots;

pub use hashing::{hash_directory_tree, hash_plugin_root};
pub(crate) use plugin_roots::user_plugin_package_root_paths;

use items::{
    action_support_is_ready, annotate_store_loaded_via_other_path, current_target_environment,
    discovered_to_item, inject_store_catalog_items,
};
use plugin_roots::scan_plugin_packages;

use crate::{
    agent_hub::{
        models::{AgentTarget, ScopeKind},
        portable_actions::targets::has_direct_local_actions,
        portable_inventory::{
            ensure_managed::ensure_discovered_portable_items_managed,
            models::{
                inventory_snapshot_hash, PortableAssetKind, PortableInventoryItemDto,
                PortableInventoryManagementState, PortableInventoryMutationCapability,
                PortableInventoryQuery, PortableInventoryScanCapability,
                PortableInventorySnapshotDto, PortableInventoryTargetDto,
            },
            reconcile::reconcile_portable_inventory,
        },
        support::{
            builtin_support_manifest, evaluate_target_support, find_target_record,
            CapabilitySupport, EvaluatedTargetSupport, RuntimeProbeSnapshot, TargetCapability,
        },
        targets::{
            AssetAdapter, ClaudeInstructionAdapter, CodexInstructionAdapter,
            CursorInstructionAdapter, GeminiInstructionAdapter, GrokInstructionAdapter,
            LocalScopeMapping, OpenCodeInstructionAdapter, PiInstructionAdapter, TargetEnvironment,
            TargetPathResolver, TargetProbe,
        },
    },
    error::AppError,
    state::AppState,
};
use std::{collections::BTreeMap, path::PathBuf};

#[cfg(test)]
mod tests;

/// 扫描用 scope 输入（显式注入路径与 opt-in；不猜测）。
///
/// Business Logic（为什么需要这个结构体）:
///     路径只能来自 user 配置根或已注册 mapping；unopted 仍可扫描但 mutation 只读。
///
/// Code Logic（这个结构体做什么）:
///     scope_id/kind/project 元数据 + absolute_path + project_opted_in。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableScanScope {
    /// 稳定 scope id（user 或 hub project id）
    pub scope_id: String,
    /// user / project / directory
    pub scope_kind: ScopeKind,
    /// 项目 id（user 为 None）
    pub project_id: Option<String>,
    /// 项目是否已 opt-in（user 恒 true）
    pub project_opted_in: bool,
    /// 扫描绝对路径（user scope 通常为 home；adapter 仍按 config_root 解析）
    pub absolute_path: PathBuf,
}

/// 使用当前进程环境刷新本机 portable inventory。
///
/// Business Logic（为什么需要这个函数）:
///     UI/动作 preview 的权威 inspect 入口；扫描后 ensure 管理账本再对账。
///     不写目标磁盘资产内容；不把发现当成 CAS/adoption 移盘。
///
/// Code Logic（这个函数做什么）:
///     进程级 soft-TTL cache → miss 时构造 TargetEnvironment → scan → ensure → reconcile → store。
pub async fn inspect_portable_inventory(
    state: &AppState,
) -> Result<PortableInventorySnapshotDto, AppError> {
    inspect_portable_inventory_query(state, PortableInventoryQuery::default()).await
}

/// 按当前 UI target/kind/scope 过滤扫描；空 query 等价完整权威扫描。
pub async fn inspect_portable_inventory_query(
    state: &AppState,
    query: PortableInventoryQuery,
) -> Result<PortableInventorySnapshotDto, AppError> {
    loop {
        match crate::agent_hub::portable_inventory::cache::begin_scan(query.clone()) {
            crate::agent_hub::portable_inventory::cache::CacheLookup::Hit(snapshot) => {
                return Ok(snapshot)
            }
            crate::agent_hub::portable_inventory::cache::CacheLookup::Wait(notify) => {
                notify.wait().await;
            }
            crate::agent_hub::portable_inventory::cache::CacheLookup::Leader(guard) => {
                let env = current_target_environment();
                let result = inspect_portable_inventory_with_env_query(state, &env, query).await;
                crate::agent_hub::portable_inventory::cache::complete_scan(
                    guard,
                    result.as_ref().ok().cloned(),
                );
                return result;
            }
        }
    }
}

/// 强制执行一次未缓存的本机 inventory 扫描。
///
/// Business Logic（为什么需要这个函数）:
///     mutation / pull install 后必须观察磁盘最新事实，不能命中 mutation 前的 2 秒缓存。
///
/// Code Logic（这个函数做什么）:
///     先递增缓存 generation 使进行中的旧扫描不可回填，再走 single-flight 扫描；
///     成功结果进入新缓存，供后续普通 inspect 复用。
pub async fn inspect_portable_inventory_force(
    state: &AppState,
) -> Result<PortableInventorySnapshotDto, AppError> {
    inspect_portable_inventory_force_query(state, PortableInventoryQuery::default()).await
}

/// 以当前进程环境重新探测单个 target 的 support manifest 结果。
///
/// Business Logic: Pull 等跨模块写入口需要逐动作 capability，不能只消费 inventory 的
/// target 汇总 mutation 状态。
/// Code Logic: fresh target probe → 与 inventory scanner 相同的 manifest evaluator。
pub(crate) fn evaluate_current_portable_target_support(
    target: AgentTarget,
) -> Result<EvaluatedTargetSupport, AppError> {
    let env = current_target_environment();
    let probe = match target {
        AgentTarget::Claude
        | AgentTarget::Codex
        | AgentTarget::OpenCode
        | AgentTarget::Grok
        | AgentTarget::Gemini
        | AgentTarget::Cursor
        | AgentTarget::Pi => crate::agent_hub::targets::probe_target(target, &env),
    }?;
    evaluate_probe_support(target, &probe)
}

/// 强制执行一次指定过滤条件的未缓存扫描。
pub async fn inspect_portable_inventory_force_query(
    state: &AppState,
    query: PortableInventoryQuery,
) -> Result<PortableInventorySnapshotDto, AppError> {
    crate::agent_hub::portable_inventory::cache::invalidate_portable_inventory_cache();
    inspect_portable_inventory_query(state, query).await
}

/// 在注入环境下强制扫描（测试与隔离运行时使用）。
///
/// Business Logic: 与生产 force rescan 使用同一失效语义，避免测试只覆盖 override。
/// Code Logic: 失效全局缓存后直接扫描注入环境；注入环境不写入全局缓存，
/// 避免隔离测试/远端 fixture 与当前进程环境互相污染。
pub async fn inspect_portable_inventory_force_with_env(
    state: &AppState,
    env: &TargetEnvironment,
) -> Result<PortableInventorySnapshotDto, AppError> {
    inspect_portable_inventory_force_with_env_query(state, env, PortableInventoryQuery::default())
        .await
}

/// 在注入环境下强制执行指定过滤条件的扫描。
pub async fn inspect_portable_inventory_force_with_env_query(
    state: &AppState,
    env: &TargetEnvironment,
    query: PortableInventoryQuery,
) -> Result<PortableInventorySnapshotDto, AppError> {
    crate::agent_hub::portable_inventory::cache::invalidate_portable_inventory_cache();
    inspect_portable_inventory_with_env_query(state, env, query).await
}

/// 使用注入环境刷新 portable inventory（可测）。
///
/// Business Logic: 隔离 HOME 与生产扫描共用同一路径/origin 规则；发现即管理。
/// Code Logic: scopes → scan → `ensure_discovered_portable_items_managed` → reconcile。
pub async fn inspect_portable_inventory_with_env(
    state: &AppState,
    env: &TargetEnvironment,
) -> Result<PortableInventorySnapshotDto, AppError> {
    inspect_portable_inventory_with_env_query(state, env, PortableInventoryQuery::default()).await
}

/// 注入环境下的过滤扫描，供 UI 与定向测试共享。
pub async fn inspect_portable_inventory_with_env_query(
    state: &AppState,
    env: &TargetEnvironment,
    query: PortableInventoryQuery,
) -> Result<PortableInventorySnapshotDto, AppError> {
    let scopes = collect_scan_scopes(state, env, &query).await?;
    let scan_query = PortableInventoryQuery {
        target: query.target,
        kind: query.kind,
        scope_kind: query.scope_kind,
        local_project_id: None,
    };
    let scan_env = env.clone();
    let (targets, mut discovered) = tokio::task::spawn_blocking(move || {
        scan_portable_inventory_facts_query(&scan_env, &scopes, scan_query)
    })
    .await
    .map_err(|error| AppError::generic(format!("portable inventory scan task: {error}")))??;
    let ensure_report =
        ensure_discovered_portable_items_managed(&state.agent_hub_repo, &mut discovered).await;
    if !ensure_report.failures.is_empty() {
        tracing::warn!(
            target = "agent_hub.portable_inventory",
            ensured = ensure_report.ensured,
            skipped = ensure_report.skipped,
            failures = ensure_report.failures.len(),
            "portable inventory ensure_managed completed with per-item failures"
        );
    }
    // ensure 失败项已标 unsupported；reconcile 后再贴回，避免被「无 fact → unmanaged」覆盖
    let failed_ids: std::collections::BTreeMap<String, String> = ensure_report
        .failures
        .iter()
        .map(|f| (f.inventory_item_id.clone(), f.reason.clone()))
        .collect();
    let mut snapshot =
        reconcile_portable_inventory(&state.agent_hub_repo, targets, discovered).await?;
    for item in &mut snapshot.items {
        if let Some(reason) = failed_ids.get(&item.inventory_item_id) {
            item.management_state = PortableInventoryManagementState::Unsupported;
            item.capabilities.reason_code = Some("ensure_managed_failed".into());
            item.capabilities.can_confirm_current_version = false;
            let warn = format!("ensure_managed_failed:{reason}");
            if !item.warnings.iter().any(|w| w == &warn) {
                item.warnings.push(warn);
            }
        }
    }
    if !failed_ids.is_empty() {
        snapshot.inventory_snapshot_hash =
            inventory_snapshot_hash(&snapshot.targets, &snapshot.items)?;
    }
    Ok(snapshot)
}

/// 只读扫描三 target 事实（不对账、不访问 DB）。
///
/// Business Logic（为什么需要这个函数）:
///     单元测试可用隔离 fixture 验证 origin/enabled/parentPlugin/hash/capability，
///     无需 SQLite；生产路径也复用同一转换逻辑。
///
/// Code Logic（这个函数做什么）:
///     probe + scan_portable_assets + plugin package roots；转换为 inventory DTO。
pub fn scan_portable_inventory_facts(
    env: &TargetEnvironment,
    scopes: &[PortableScanScope],
) -> Result<
    (
        Vec<PortableInventoryTargetDto>,
        Vec<PortableInventoryItemDto>,
    ),
    AppError,
> {
    scan_portable_inventory_facts_query(env, scopes, PortableInventoryQuery::default())
}

/// 按 target/kind/scope 缩小真实扫描面；过滤在目录遍历前生效。
pub fn scan_portable_inventory_facts_query(
    env: &TargetEnvironment,
    scopes: &[PortableScanScope],
    query: PortableInventoryQuery,
) -> Result<
    (
        Vec<PortableInventoryTargetDto>,
        Vec<PortableInventoryItemDto>,
    ),
    AppError,
> {
    if query.local_project_id.is_some() {
        return Err(AppError::validation(
            "PORTABLE_INVENTORY_LOCAL_PROJECT_ID_UNRESOLVED",
        ));
    }
    let adapters: Vec<Box<dyn AssetAdapter>> = vec![
        Box::new(ClaudeInstructionAdapter),
        Box::new(CodexInstructionAdapter),
        Box::new(OpenCodeInstructionAdapter),
        Box::new(GrokInstructionAdapter),
        Box::new(GeminiInstructionAdapter),
        Box::new(CursorInstructionAdapter),
        Box::new(PiInstructionAdapter),
    ];
    let homes = TargetPathResolver::resolve_all(env);
    let mut target_dtos = Vec::with_capacity(adapters.len());
    let mut items: Vec<PortableInventoryItemDto> = Vec::new();
    let mut seen_ids: BTreeMap<String, usize> = BTreeMap::new();
    let mut probes = BTreeMap::new();
    std::thread::scope(|scope| -> Result<(), AppError> {
        let handles = adapters
            .iter()
            .filter(|adapter| {
                !query
                    .target
                    .is_some_and(|selected| selected != adapter.target())
            })
            .map(|adapter| {
                let target = adapter.target();
                (target, scope.spawn(move || adapter.probe(env)))
            })
            .collect::<Vec<_>>();
        for (target, handle) in handles {
            let probe = handle.join().map_err(|_| {
                AppError::generic(format!(
                    "portable target probe thread panicked:{}",
                    target.as_str()
                ))
            })??;
            probes.insert(target, probe);
        }
        Ok(())
    })?;

    for adapter in &adapters {
        let target = adapter.target();
        if query.target.is_some_and(|selected| selected != target) {
            continue;
        }
        let probe = probes
            .remove(&target)
            .ok_or_else(|| AppError::generic("portable target probe result missing"))?;
        let target_dto = target_dto_from_probe(target, &probe, env)?;
        let evaluated = evaluate_probe_support(target, &probe)?;
        target_dtos.push(target_dto.clone());

        for scope in scopes {
            if query
                .scope_kind
                .is_some_and(|selected| selected != scope.scope_kind)
            {
                continue;
            }
            let mapping = LocalScopeMapping {
                scope_kind: scope.scope_kind,
                absolute_path: scope.absolute_path.clone(),
                project_root: match scope.scope_kind {
                    ScopeKind::User => None,
                    ScopeKind::Project | ScopeKind::Directory => Some(scope.absolute_path.clone()),
                },
                relative_root: None,
                codex_fallback_filenames: vec![],
            };
            let discoveries = if query.kind == Some(PortableAssetKind::Plugin) {
                Vec::new()
            } else {
                match adapter.scan_portable_assets_filtered(
                    &mapping,
                    env,
                    query.kind.map(PortableAssetKind::to_asset_kind),
                ) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            target = "agent_hub.portable_inventory",
                            error = %e,
                            agent = target.as_str(),
                            scope = %scope.scope_id,
                            "portable scan failed; continue other scopes"
                        );
                        Vec::new()
                    }
                }
            };
            // Plugin package roots（package 本体 + 组件 parent 关联）。MCP 不从这些目录发现。
            if query.kind != Some(PortableAssetKind::Mcp) {
                scan_plugin_packages(
                    scope,
                    env,
                    &homes,
                    &target_dto,
                    &evaluated,
                    &mut seen_ids,
                    &mut items,
                    query.kind,
                )?;
            }

            for disc in discoveries {
                // Instruction/Agent/Hook 不进 portable 四类库存
                let Ok(kind) = PortableAssetKind::try_from_asset_kind(disc.kind) else {
                    continue;
                };
                if query.kind.is_some_and(|selected| selected != kind) {
                    continue;
                }
                discovered_to_item(
                    kind,
                    &disc,
                    scope,
                    &target_dto,
                    &evaluated,
                    &mut seen_ids,
                    &mut items,
                );
            }
            inject_store_catalog_items(
                target,
                scope,
                &target_dto,
                &evaluated,
                query.kind,
                &mut seen_ids,
                &mut items,
            );
        }
    }
    annotate_store_loaded_via_other_path(&mut items);

    // 稳定排序，便于断言
    items.sort_by(|a, b| {
        a.target
            .as_str()
            .cmp(b.target.as_str())
            .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
            .then_with(|| a.inventory_item_id.cmp(&b.inventory_item_id))
    });
    target_dtos.sort_by(|a, b| a.target.as_str().cmp(b.target.as_str()));
    Ok((target_dtos, items))
}

/// 从 AppState 收集 user + 已注册 mapping 的 project scopes。
///
/// Business Logic（为什么需要这个函数）:
///     用户级列举不得猜测未映射项目路径；项目 Agent 显式传入本机 workbench id 时
///     必须能只读扫描，不能因尚未 opt-in 而 404。
///
/// Code Logic（这个函数做什么）:
///     有 local_project_id 则校验本机项目并 ensure mapping 身份（新建 opted_in=false）；
///     否则 user home + 已映射 projects。
async fn collect_scan_scopes(
    state: &AppState,
    env: &TargetEnvironment,
    query: &PortableInventoryQuery,
) -> Result<Vec<PortableScanScope>, AppError> {
    if let Some(raw_local_project_id) = query.local_project_id.as_deref() {
        let local_project_id = raw_local_project_id.trim();
        if local_project_id.is_empty() {
            return Err(AppError::validation(
                "PORTABLE_INVENTORY_LOCAL_PROJECT_ID_REQUIRED",
            ));
        }
        if local_project_id.starts_with("remote:") {
            return Err(AppError::validation(
                "PORTABLE_INVENTORY_REMOTE_PROJECT_UNSUPPORTED",
            ));
        }
        if query.scope_kind != Some(ScopeKind::Project) {
            return Err(AppError::validation(
                "PORTABLE_INVENTORY_PROJECT_SCOPE_REQUIRED",
            ));
        }
        let project = state
            .workbench_project_repo
            .get(local_project_id)
            .await?
            .ok_or_else(|| AppError::not_found("PORTABLE_INVENTORY_LOCAL_PROJECT_NOT_FOUND"))?;
        if project.kind != "local" {
            return Err(AppError::validation(
                "PORTABLE_INVENTORY_REMOTE_PROJECT_UNSUPPORTED",
            ));
        }
        let mapping =
            crate::agent_hub::project_scope::ensure_local_project_mapping_identity(state, &project)
                .await?;
        if mapping.local_workbench_project_id.as_deref() != Some(local_project_id) {
            return Err(AppError::validation(
                "PORTABLE_INVENTORY_PROJECT_MAPPING_MISMATCH",
            ));
        }
        if mapping.hub_project_id.trim().is_empty() {
            return Err(AppError::validation(
                "PORTABLE_INVENTORY_PROJECT_MAPPING_INVALID",
            ));
        }
        let absolute_path = mapping
            .local_absolute_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from(&project.path));
        return Ok(vec![PortableScanScope {
            scope_id: format!("project:{}", mapping.hub_project_id),
            scope_kind: ScopeKind::Project,
            project_id: Some(mapping.hub_project_id),
            project_opted_in: mapping.opted_in,
            absolute_path,
        }]);
    }
    let mut scopes = vec![PortableScanScope {
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        project_id: None,
        project_opted_in: true,
        absolute_path: env.home.clone(),
    }];

    // 全量列举：路径只来自已注册 mapping；未映射不猜测。
    // 显式 local_project_id 已在上方 ensure 身份（opted_in 保持原值 / 新建为 false）。
    let projects = state
        .workbench_project_repo
        .list()
        .await
        .unwrap_or_default();
    for project in projects {
        if project.kind != "local" {
            continue;
        }
        let mapping = state
            .agent_hub_repo
            .get_project_mapping_by_local_workbench_id(&project.id)
            .await?;
        let Some(mapping) = mapping else {
            continue;
        };
        let absolute = mapping
            .local_absolute_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from(&project.path));
        scopes.push(PortableScanScope {
            scope_id: format!("project:{}", mapping.hub_project_id),
            scope_kind: ScopeKind::Project,
            project_id: Some(mapping.hub_project_id.clone()),
            project_opted_in: mapping.opted_in,
            absolute_path: absolute,
        });
    }
    Ok(scopes)
}

/// 按当前 probe 求值 support manifest，供 target 汇总状态与逐动作能力共用。
fn evaluate_probe_support(
    target: AgentTarget,
    probe: &TargetProbe,
) -> Result<EvaluatedTargetSupport, AppError> {
    let manifest = builtin_support_manifest()?;
    let snapshot = RuntimeProbeSnapshot {
        target,
        executable: probe.executable.clone(),
        version: probe.version.clone(),
        config_root: probe.config_root.clone(),
        fingerprint: probe.fingerprint.clone(),
        help_fingerprint: None,
    };
    Ok(evaluate_target_support(&manifest, &snapshot))
}

fn target_dto_from_probe(
    target: AgentTarget,
    probe: &TargetProbe,
    env: &TargetEnvironment,
) -> Result<PortableInventoryTargetDto, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let config_root = match target {
        AgentTarget::Claude => homes.claude.config_root.display().to_string(),
        AgentTarget::Codex => homes.codex.config_root.display().to_string(),
        AgentTarget::OpenCode => homes.opencode.config_root.display().to_string(),
        AgentTarget::Grok => homes.grok.config_root.display().to_string(),
        AgentTarget::Gemini => homes.gemini.config_root.display().to_string(),
        AgentTarget::Cursor => homes.cursor.config_root.display().to_string(),
        AgentTarget::Pi => homes.pi.config_root.display().to_string(),
    };
    let installed = probe.executable.is_some();
    let manifest = builtin_support_manifest()?;
    let evaluated = evaluate_probe_support(target, probe)?;
    let scan_cap = match evaluated.capability(TargetCapability::ScanPortableAssets) {
        CapabilitySupport::Supported
        | CapabilitySupport::SupportedAfterRestart
        | CapabilitySupport::ActivationRequired => PortableInventoryScanCapability::Supported,
        CapabilitySupport::ReadOnly => PortableInventoryScanCapability::ReadOnly,
        CapabilitySupport::Blocked => {
            // 版本未知仍可尝试扫描，但 mutation blocked
            if installed {
                PortableInventoryScanCapability::ReadOnly
            } else {
                PortableInventoryScanCapability::Blocked
            }
        }
    };
    let direct_local_management = installed && has_direct_local_actions(target);
    // 直管执行器只是实现能力的 allowlist，不能绕过 support manifest 的认证门。
    // scan-only manifest 下即使 CLI 已安装且 Claude/Codex 具备旧版本机动作，
    // 也必须保持 Blocked，避免 inventory 把旧执行器重新提升成可写。
    let mutation_cap = if [
        TargetCapability::RenderPortableAssets,
        TargetCapability::ActivatePackage,
        TargetCapability::DeactivatePackage,
    ]
    .into_iter()
    .any(|capability| action_support_is_ready(&evaluated, capability))
    {
        PortableInventoryMutationCapability::Supported
    } else if [
        TargetCapability::RenderPortableAssets,
        TargetCapability::ActivatePackage,
        TargetCapability::DeactivatePackage,
    ]
    .into_iter()
    .any(|capability| !matches!(evaluated.capability(capability), CapabilitySupport::Blocked))
    {
        PortableInventoryMutationCapability::PreviewOnly
    } else {
        PortableInventoryMutationCapability::Blocked
    };
    let reason_code = if probe.version.is_none() {
        Some("cli_version_unknown".into())
    } else if !installed {
        Some("cli_not_installed".into())
    } else if mutation_cap == PortableInventoryMutationCapability::PreviewOnly {
        Some("portable_mutation_preview_only".into())
    } else if mutation_cap == PortableInventoryMutationCapability::Blocked {
        Some("portable_mutation_blocked".into())
    } else {
        None
    };
    let mut evidence_ids = Vec::new();
    if let Some(rec) = find_target_record(&manifest, target) {
        evidence_ids.extend(rec.evidence_ids.iter().cloned());
    }
    if evidence_ids.is_empty() {
        evidence_ids.push("L2-PORTABLE-INVENTORY-001".into());
    }
    if direct_local_management
        && !evidence_ids
            .iter()
            .any(|id| id == "L2-AGENT-HUB-PORTABLE-PARITY-001")
    {
        evidence_ids.push("L2-AGENT-HUB-PORTABLE-PARITY-001".into());
    }
    Ok(PortableInventoryTargetDto {
        target,
        installed,
        version: probe.version.clone(),
        executable: probe.executable.as_ref().map(|p| p.display().to_string()),
        config_root,
        scan_capability: scan_cap,
        mutation_capability: mutation_cap,
        reason_code,
        evidence_ids,
    })
}
