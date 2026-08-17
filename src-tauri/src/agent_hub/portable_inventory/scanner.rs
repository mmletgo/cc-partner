//! portable_inventory/scanner — 三 target 四类资产库存扫描 + inspect 入口
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

use crate::agent_hub::assets::{McpTransport, PortableAssetPayload};
use crate::agent_hub::models::{AgentTarget, ScopeKind};
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::plugins::decompose::discover_plugin_source_for_target;
use crate::agent_hub::portable_actions::models::PortableAssetActionKind;
use crate::agent_hub::portable_actions::targets::{
    has_direct_local_actions, supports_direct_local_action,
};
use crate::agent_hub::portable_inventory::ensure_managed::ensure_discovered_portable_items_managed;
use crate::agent_hub::portable_inventory::models::{
    inventory_item_id, PortableAssetKind, PortableInventoryItemCapabilitiesDto,
    PortableInventoryItemDto, PortableInventoryManagementState,
    PortableInventoryMutationCapability, PortableInventoryQuery, PortableInventoryScanCapability,
    PortableInventorySnapshotDto, PortableInventorySourceOrigin, PortableInventoryTargetDto,
    PortableMcpCredentialFactDto,
};
use crate::agent_hub::portable_inventory::reconcile::reconcile_portable_inventory;
use crate::agent_hub::support::{
    builtin_support_manifest, evaluate_target_support, find_target_record, CapabilitySupport,
    EvaluatedTargetSupport, RuntimeProbeSnapshot, TargetCapability,
};
use crate::agent_hub::targets::portable::{
    DiscoveredPortableAsset, PortableDiscoveryStatus, PortableOriginKind,
};
use crate::agent_hub::targets::{
    AssetAdapter, ClaudeInstructionAdapter, CodexInstructionAdapter, CursorInstructionAdapter,
    GeminiInstructionAdapter, GrokInstructionAdapter, LocalScopeMapping,
    OpenCodeInstructionAdapter, TargetEnvironment, TargetPathResolver, TargetProbe,
};
use crate::error::AppError;
use crate::state::AppState;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

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
        | AgentTarget::Cursor => crate::agent_hub::targets::probe_target(target, &env),
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
            let warn = format!("ensure_managed_failed:{reason}");
            if !item.warnings.iter().any(|w| w == &warn) {
                item.warnings.push(warn);
            }
        }
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
        }
    }

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
        let mapping = state
            .agent_hub_repo
            .get_project_mapping_by_local_workbench_id(local_project_id)
            .await?
            .ok_or_else(|| AppError::not_found("PORTABLE_INVENTORY_PROJECT_MAPPING_NOT_FOUND"))?;
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

    // 路径只来自已注册 mapping（经 workbench local project 关联）；未映射不猜测。
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

/// 只读发现 Plugin 包本体（不写 CAS）。
///
/// Business Logic（为什么需要这个函数）:
///     Plugin package 是顶层 Standalone 资产，需要单独发现并生成 inventory 行；
///     同时遍历包内组件（skills/commands/...）的 discovery 并复用 discovered_to_item。
///
/// Code Logic（这个函数做什么）:
///     遍历 plugin roots，对每个 package 算 inv_id（source_identity = "standalone"），
///     按 seen 去重后写入共享 items；组件 discovery 经 discovered_to_item 写入同一 items，
///     并把 parent_plugin_inventory_item_id 指向当前 package 的 inv_id。
#[allow(clippy::too_many_arguments)] // 内部 helper：scope/env/homes/target/evaluated/seen/items/kind 8 段语义独立
fn scan_plugin_packages(
    scope: &PortableScanScope,
    env: &TargetEnvironment,
    homes: &crate::agent_hub::targets::paths::TargetHomes,
    target_dto: &PortableInventoryTargetDto,
    evaluated: &EvaluatedTargetSupport,
    seen: &mut BTreeMap<String, usize>,
    items: &mut Vec<PortableInventoryItemDto>,
    selected_kind: Option<PortableAssetKind>,
) -> Result<(), AppError> {
    let target = target_dto.target;
    let roots = plugin_roots_for(target, scope, env, homes);
    // Codex：config.toml [plugins] 启用表；Claude/OpenCode 不用。
    let codex_enablement = if target == AgentTarget::Codex && scope.scope_kind == ScopeKind::User {
        load_codex_plugin_enablement(&homes.codex.config_root)
    } else if target == AgentTarget::Codex {
        load_codex_plugin_enablement(&scope.absolute_path.join(".codex"))
    } else {
        BTreeMap::new()
    };
    for candidate in roots {
        let root = candidate.path;
        if !root.is_dir() {
            continue;
        }
        // 基础设施目录永不作为 package
        if crate::agent_hub::portable_inventory::plugin_paths::is_plugin_infrastructure_path(&root)
        {
            continue;
        }
        let mut source = match discover_plugin_source_for_target(
            target,
            &root,
            scope.scope_id.clone(),
            scope.scope_kind,
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // 稳定身份：registry key > 路径布局 id > manifest/目录名
        if let Some(reg) = candidate.registry_plugin_id.as_ref() {
            if !reg.trim().is_empty() {
                source.plugin_id = reg.clone();
                // 无 manifest 时目录名常是版本号；展示名也用 registry id
                if source.name == root.file_name().and_then(|s| s.to_str()).unwrap_or("") {
                    source.name = reg.clone();
                }
            }
        } else if let Some(path_id) =
            crate::agent_hub::portable_inventory::plugin_paths::plugin_id_from_path(Some(
                &root.display().to_string(),
            ))
        {
            // cache 布局下目录名/manifest 可能是版本号；以路径 id 为准
            let dir_name = root
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if source.plugin_id == dir_name || source.plugin_id.is_empty() {
                source.plugin_id = path_id.clone();
                if source.name == dir_name {
                    source.name = path_id;
                }
            }
        }
        if crate::agent_hub::portable_inventory::plugin_paths::is_plugin_infrastructure_name(
            &source.plugin_id,
        ) {
            continue;
        }
        // plugin package 是顶层 Standalone 资产：source_identity = "standalone"，native_id = plugin_id。
        // 绝对路径只保留在 source_path 字段供 UI 显示与 executor 物理移动。
        let source_path = root.display().to_string();
        let inv_id = inventory_item_id(target, &scope.scope_id, "standalone", &source.plugin_id);
        if selected_kind.is_none() || selected_kind == Some(PortableAssetKind::Plugin) {
            let (content_hash, tree_hash) = if selected_kind == Some(PortableAssetKind::Plugin) {
                // 列表首屏只需 manifest 身份；递归 tree hash 在选中项 preview 时精确计算，
                // apply 前仍会再次未缓存校验，避免为十个大包读取数百 MB。
                (hash_plugin_manifest(&root)?, None)
            } else {
                let (content_hash, tree_hash) = hash_plugin_root_cached(&root)?;
                (content_hash, Some(tree_hash))
            };
            let mut warnings = Vec::new();
            // 组件同名 skill 路径存在时不合并——仅作 package 行
            let component_skill = root.join("skills");
            if component_skill.is_dir() {
                warnings.push("plugin_has_components".into());
            }
            let actual_enabled = if target == AgentTarget::Codex {
                let (enabled, warn) =
                    codex_plugin_actual_enabled(&source.plugin_id, &codex_enablement);
                if let Some(w) = warn {
                    warnings.push(w);
                }
                Some(enabled)
            } else {
                // Claude/OpenCode：目录存在即 installed；无统一 disabled 语义时 true
                Some(true)
            };
            let can_mutate_scope =
                scope.project_opted_in && scope.scope_kind != ScopeKind::Directory;
            let can_enable = can_mutate_scope
                && action_capability_supported(
                    evaluated,
                    target,
                    PortableAssetKind::Plugin,
                    PortableAssetActionKind::Enable,
                );
            let can_deactivate = can_mutate_scope
                && action_capability_supported(
                    evaluated,
                    target,
                    PortableAssetKind::Plugin,
                    PortableAssetActionKind::Disable,
                );
            let reason = if !scope.project_opted_in && scope.scope_kind != ScopeKind::User {
                Some("project_not_opted_in".into())
            } else {
                action_capability_reason(target_dto, evaluated, target, PortableAssetKind::Plugin)
            };
            let item = PortableInventoryItemDto {
                inventory_item_id: inv_id.clone(),
                target,
                kind: PortableAssetKind::Plugin,
                native_id: source.plugin_id.clone(),
                display_name: source.name.clone(),
                description: source.description.clone(),
                version: source.version.clone(),
                scope_id: scope.scope_id.clone(),
                scope_kind: scope.scope_kind,
                project_id: scope.project_id.clone(),
                project_opted_in: scope.project_opted_in,
                source_path: Some(source_path.clone()),
                source_origin: PortableInventorySourceOrigin::Standalone,
                parent_plugin_inventory_item_id: None,
                actual_enabled,
                content_hash: Some(content_hash),
                tree_hash,
                canonical_asset_id: None,
                canonical_revision_id: None,
                management_state: PortableInventoryManagementState::Unmanaged,
                desired_presence: None,
                desired_enabled: None,
                materialization_status: None,
                capabilities: item_capabilities(
                    target,
                    PortableAssetKind::Plugin,
                    actual_enabled,
                    can_enable,
                    can_deactivate,
                    true,
                    reason,
                ),
                warnings,
                mcp_credential: None,
            };
            // seen 去重：与 discovered_to_item 同一套 disabled-wins 合并策略。
            match seen.get(&inv_id).copied() {
                None => {
                    seen.insert(inv_id.clone(), items.len());
                    items.push(item);
                }
                Some(idx) => {
                    if should_replace_with(&item, &items[idx]) {
                        items[idx] = item;
                    }
                    // 否则丢弃（保留已存在的）
                }
            }
        }

        // Installed plugin roots may live below cache/<marketplace>/<id>/<version> rather than
        // directly below config_root/plugins. Scan components from the same authoritative roots
        // so nested installed plugins are visible without treating cache infrastructure as a
        // package of its own.
        if selected_kind == Some(PortableAssetKind::Plugin) {
            continue;
        }
        for discovery in
            crate::agent_hub::targets::portable::scan_plugin_components_readonly_filtered(
                target,
                scope.scope_kind,
                &root,
                &source.plugin_id,
                selected_kind.map(PortableAssetKind::to_asset_kind),
            )?
        {
            let Ok(kind) = PortableAssetKind::try_from_asset_kind(discovery.kind) else {
                continue;
            };
            if selected_kind.is_some_and(|selected| selected != kind) {
                continue;
            }
            // 记录写入前 items 长度，判断 discovered_to_item 是否实际新增了一条；
            // 若新增，再回填 parent_plugin_inventory_item_id。
            let before = items.len();
            discovered_to_item(kind, &discovery, scope, target_dto, evaluated, seen, items);
            if items.len() > before {
                if let Some(last) = items.last_mut() {
                    last.parent_plugin_inventory_item_id = Some(inv_id.clone());
                }
            }
        }
    }
    Ok(())
}

/// 可扫描的 plugin package 根（含可选 registry 身份）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PluginRootCandidate {
    path: PathBuf,
    /// installed_plugins.json 的 key 前缀（`pyright-lsp@market` → `pyright-lsp`）
    registry_plugin_id: Option<String>,
}

impl PluginRootCandidate {
    fn path_only(path: PathBuf) -> Self {
        Self {
            path,
            registry_plugin_id: None,
        }
    }
}

/// 当前 scope 下应扫描的 plugin package 根（权威入口）。
///
/// Business Logic: adapters 与 inventory 必须共享同一根集合，禁止一级 read_dir(plugins)。
/// Code Logic: user 走 registry/cache manifest；project 只认直装 manifest。
fn plugin_roots_for(
    target: AgentTarget,
    scope: &PortableScanScope,
    env: &TargetEnvironment,
    homes: &crate::agent_hub::targets::paths::TargetHomes,
) -> Vec<PluginRootCandidate> {
    let mut roots = match (target, scope.scope_kind) {
        (AgentTarget::Claude, ScopeKind::User) => {
            claude_user_plugin_roots(&homes.claude.config_root)
        }
        (AgentTarget::Codex, ScopeKind::User) => codex_user_plugin_roots(&homes.codex.config_root),
        (AgentTarget::OpenCode, ScopeKind::User) => [
            homes.opencode.config_root.join("plugins"),
            env.home.join(".opencode").join("plugins"),
        ]
        .into_iter()
        .flat_map(|base| direct_manifest_plugin_roots(&base, target))
        .map(PluginRootCandidate::path_only)
        .collect(),
        (AgentTarget::Claude, _) => direct_manifest_plugin_roots(
            &scope.absolute_path.join(".claude").join("plugins"),
            target,
        )
        .into_iter()
        .map(PluginRootCandidate::path_only)
        .collect(),
        (AgentTarget::Codex, _) => direct_manifest_plugin_roots(
            &scope.absolute_path.join(".codex").join("plugins"),
            target,
        )
        .into_iter()
        .map(PluginRootCandidate::path_only)
        .collect(),
        (AgentTarget::OpenCode, _) => [
            scope.absolute_path.join(".opencode").join("plugins"),
            scope.absolute_path.join("plugins"),
        ]
        .into_iter()
        .flat_map(|base| direct_manifest_plugin_roots(&base, target))
        .map(PluginRootCandidate::path_only)
        .collect(),
        (AgentTarget::Grok, ScopeKind::User) => {
            direct_manifest_plugin_roots(&homes.grok.config_root.join("plugins"), target)
                .into_iter()
                .map(PluginRootCandidate::path_only)
                .collect()
        }
        (AgentTarget::Gemini, ScopeKind::User) => {
            direct_manifest_plugin_roots(&homes.gemini.config_root.join("plugins"), target)
                .into_iter()
                .map(PluginRootCandidate::path_only)
                .collect()
        }
        (AgentTarget::Cursor, ScopeKind::User) => {
            direct_manifest_plugin_roots(&homes.cursor.config_root.join("plugins"), target)
                .into_iter()
                .map(PluginRootCandidate::path_only)
                .collect()
        }
        (AgentTarget::Grok, _) => {
            direct_manifest_plugin_roots(&scope.absolute_path.join(".grok").join("plugins"), target)
                .into_iter()
                .map(PluginRootCandidate::path_only)
                .collect()
        }
        (AgentTarget::Gemini, _) => direct_manifest_plugin_roots(
            &scope.absolute_path.join(".gemini").join("extensions"),
            target,
        )
        .into_iter()
        .map(PluginRootCandidate::path_only)
        .collect(),
        (AgentTarget::Cursor, _) => direct_manifest_plugin_roots(
            &scope.absolute_path.join(".cursor").join("plugins"),
            target,
        )
        .into_iter()
        .map(PluginRootCandidate::path_only)
        .collect(),
    };
    roots.sort_by(|a, b| a.path.cmp(&b.path));
    roots.dedup_by(|a, b| a.path == b.path);
    roots
}

/// 供 target adapter 复用的 user-scope package 根路径。
///
/// Business Logic: 禁止 adapters 自行 `read_dir(plugins)` 把 cache/data 当 package。
/// Code Logic: 仅返回 path 列表。
pub(crate) fn user_plugin_package_root_paths(
    target: AgentTarget,
    config_root: &Path,
) -> Vec<PathBuf> {
    let candidates = match target {
        AgentTarget::Claude => claude_user_plugin_roots(config_root),
        AgentTarget::Codex => codex_user_plugin_roots(config_root),
        AgentTarget::OpenCode => {
            direct_manifest_plugin_roots(&config_root.join("plugins"), AgentTarget::OpenCode)
                .into_iter()
                .map(PluginRootCandidate::path_only)
                .collect()
        }
        AgentTarget::Grok | AgentTarget::Gemini | AgentTarget::Cursor => {
            direct_manifest_plugin_roots(&config_root.join("plugins"), target)
                .into_iter()
                .map(PluginRootCandidate::path_only)
                .collect()
        }
    };
    candidates.into_iter().map(|c| c.path).collect()
}

/// Claude 的 installed_plugins.json 是安装状态权威；cache/marketplaces/data 本身不是插件。
fn claude_user_plugin_roots(config_root: &Path) -> Vec<PluginRootCandidate> {
    let plugins_root = config_root.join("plugins");
    let cache_root = plugins_root.join("cache");
    let mut roots = Vec::new();
    if let Ok(raw) = fs::read_to_string(plugins_root.join("installed_plugins.json")) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(plugins) = value.get("plugins").and_then(|v| v.as_object()) {
                for (registry_key, installs) in plugins {
                    let registry_plugin_id = registry_key
                        .split('@')
                        .next()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string());
                    let Some(installs) = installs.as_array() else {
                        continue;
                    };
                    for install in installs {
                        if install
                            .get("scope")
                            .and_then(|v| v.as_str())
                            .is_some_and(|scope| scope != "user")
                        {
                            continue;
                        }
                        let Some(path) = install.get("installPath").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        let path = PathBuf::from(path);
                        if !path.is_dir() || !path.starts_with(&cache_root) {
                            continue;
                        }
                        if crate::agent_hub::portable_inventory::plugin_paths::is_plugin_infrastructure_path(
                            &path,
                        ) {
                            continue;
                        }
                        roots.push(PluginRootCandidate {
                            path,
                            registry_plugin_id: registry_plugin_id.clone(),
                        });
                    }
                }
            }
        }
    }
    // Development/direct installs remain supported, but only with a target manifest.
    roots.extend(
        direct_manifest_plugin_roots(&plugins_root, AgentTarget::Claude)
            .into_iter()
            .map(PluginRootCandidate::path_only),
    );
    roots
}

/// Codex 当前没有稳定 registry 文件；只认 cache 中精确 `.codex-plugin/plugin.json` 根。
///
/// Business Logic: 启用权威在 `config.toml` 的 `[plugins."id@market"]`；本函数只列 package 根。
/// Code Logic: walk cache 找 `.codex-plugin/plugin.json`；registry_plugin_id 填 path id。
fn codex_user_plugin_roots(config_root: &Path) -> Vec<PluginRootCandidate> {
    let plugins_root = config_root.join("plugins");
    let cache_root = plugins_root.join("cache");
    let mut roots: Vec<PluginRootCandidate> =
        direct_manifest_plugin_roots(&plugins_root, AgentTarget::Codex)
            .into_iter()
            .map(PluginRootCandidate::path_only)
            .collect();
    if cache_root.is_dir() {
        for entry in walkdir::WalkDir::new(&cache_root)
            .follow_links(false)
            .max_depth(5)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() || entry.file_name() != "plugin.json" {
                continue;
            }
            let Some(manifest_dir) = entry.path().parent() else {
                continue;
            };
            if manifest_dir.file_name().and_then(|v| v.to_str()) != Some(".codex-plugin") {
                continue;
            }
            let Some(root) = manifest_dir.parent() else {
                continue;
            };
            if root.starts_with(&cache_root)
                && !crate::agent_hub::portable_inventory::plugin_paths::is_plugin_infrastructure_path(
                    root,
                )
            {
                let registry_plugin_id =
                    crate::agent_hub::portable_inventory::plugin_paths::plugin_id_from_path(Some(
                        &root.display().to_string(),
                    ));
                roots.push(PluginRootCandidate {
                    path: root.to_path_buf(),
                    registry_plugin_id,
                });
            }
        }
    }
    roots
}

/// 解析 Codex `config.toml` 中 `[plugins."id@market"] enabled` 映射。
///
/// Business Logic: 表存在即安装；`enabled` 缺省 true；`enabled=false` 为禁用。
/// Code Logic: toml_edit 扫描 `plugins` 表；key 保留完整 `id@market` 字符串。
pub(crate) fn parse_codex_plugin_enablement_from_toml(text: &str) -> BTreeMap<String, bool> {
    let mut out = BTreeMap::new();
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return out;
    };
    let Some(plugins) = doc.get("plugins").and_then(|i| i.as_table()) else {
        return out;
    };
    for (key, item) in plugins.iter() {
        let Some(table) = item.as_table() else {
            continue;
        };
        let enabled = table
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        out.insert(key.to_string(), enabled);
    }
    out
}

/// 从 CODEX_HOME/config.toml 加载 plugin 启用表。
fn load_codex_plugin_enablement(config_root: &Path) -> BTreeMap<String, bool> {
    let path = config_root.join("config.toml");
    match fs::read_to_string(&path) {
        Ok(text) => parse_codex_plugin_enablement_from_toml(&text),
        Err(_) => BTreeMap::new(),
    }
}

/// 解析 package 在 Codex config 中的启用态。
///
/// Business Logic: 优先精确 `id@market`；否则任意同 short id 的 key；
/// cache 残留且 config 无记录 → false（不得硬编码 true）。
/// Code Logic: BTreeMap 查找 + short-id 前缀匹配。
fn codex_plugin_actual_enabled(
    plugin_id: &str,
    enablement: &BTreeMap<String, bool>,
) -> (bool, Option<String>) {
    if enablement.is_empty() {
        // 无 config 表时保持 installed=true 兼容旧 fixture（无 plugins 段）
        return (true, None);
    }
    if let Some(v) = enablement.get(plugin_id) {
        return (*v, None);
    }
    // short id `browser` 匹配 `browser@openai-bundled`
    let prefix = format!("{plugin_id}@");
    let mut matched: Option<bool> = None;
    for (key, enabled) in enablement {
        if key == plugin_id || key.starts_with(&prefix) {
            matched = Some(matched.map(|m| m || *enabled).unwrap_or(*enabled));
        }
    }
    match matched {
        Some(v) => (v, None),
        None => (false, Some("codex_plugin_not_in_config".into())),
    }
}

/// 只返回含目标 manifest 的一级插件目录，拒绝 cache/data/staging 等基础设施目录。
fn direct_manifest_plugin_roots(base: &Path, target: AgentTarget) -> Vec<PathBuf> {
    let manifest = match target {
        AgentTarget::Claude => ".claude-plugin/plugin.json",
        AgentTarget::Codex => ".codex-plugin/plugin.json",
        AgentTarget::OpenCode => "package.json",
        AgentTarget::Grok | AgentTarget::Gemini | AgentTarget::Cursor => "plugin.json",
    };
    let Ok(read) = fs::read_dir(base) else {
        return Vec::new();
    };
    read.filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path.join(manifest).is_file()
                && !crate::agent_hub::portable_inventory::plugin_paths::is_plugin_infrastructure_path(
                    path,
                )
        })
        .collect()
}

/// 目录树单一确定性 hash（相对路径/类型/内容；不跟随 symlink）。
///
/// Business Logic（为什么需要这个函数）:
///     planner 在 mutation 前需要能够发现嵌套文件、空目录或 symlink 目标变化，
///     仅比较根目录名会把 stale plan 错当成当前事实。
///
/// Code Logic（这个函数做什么）:
///     递归枚举目录，按规范化 `/` 相对路径排序；记录 directory/file/symlink 类型、
///     内容 SHA-256 与平台 executable 位，再对确定性 JSON 求 SHA-256。
pub fn hash_directory_tree(root: &Path) -> Result<String, AppError> {
    if !root.is_dir() {
        return Err(AppError::not_found("PORTABLE_ASSET_ACTION_SOURCE_MISSING"));
    }
    let mut entries = Vec::new();
    collect_deterministic_tree_entries(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let bytes = serde_json::to_vec(&entries)
        .map_err(|e| AppError::generic(format!("portable tree hash serialize: {e}")))?;
    Ok(sha256_hex(&bytes))
}

#[derive(Debug, Serialize)]
struct DeterministicTreeEntry {
    path: String,
    entry_type: &'static str,
    content_hash: String,
    executable: bool,
}

fn collect_deterministic_tree_entries(
    root: &Path,
    current: &Path,
    entries: &mut Vec<DeterministicTreeEntry>,
) -> Result<(), AppError> {
    let mut children: Vec<_> = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name().to_string_lossy().into_owned());
    for entry in children {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        let relative = deterministic_relative_posix(root, &path);
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            let target = fs::read_link(&path)?;
            let target_text = target.to_string_lossy().replace('\\', "/");
            entries.push(DeterministicTreeEntry {
                path: relative,
                entry_type: "symlink",
                content_hash: sha256_hex(target_text.as_bytes()),
                executable: false,
            });
        } else if file_type.is_dir() {
            entries.push(DeterministicTreeEntry {
                path: relative.clone(),
                entry_type: "directory",
                content_hash: sha256_hex(&[]),
                executable: false,
            });
            collect_deterministic_tree_entries(root, &path, entries)?;
        } else if file_type.is_file() {
            let bytes = fs::read(&path)?;
            entries.push(DeterministicTreeEntry {
                path: relative,
                entry_type: "file",
                content_hash: sha256_hex(&bytes),
                executable: deterministic_is_executable(&metadata),
            });
        } else {
            return Err(AppError::validation(format!(
                "PORTABLE_ASSET_ACTION_UNSUPPORTED_TREE_ENTRY:{relative}"
            )));
        }
    }
    Ok(())
}

fn deterministic_relative_posix(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

fn deterministic_is_executable(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

/// Plugin 根目录 content_hash + tree_hash（与 inventory 行同源）。
///
/// Business Logic: planner `expected_source_hash` 与 apply recheck 必须共享同一 material 域，
/// 禁止路径字符串 sha 与 manifest 字节 hash 混用导致生产 plugin 永远 SOURCE_HASH_CHANGED。
/// Code Logic: 优先 manifest 文件字节；无 manifest 才回落 path display（与历史 inventory 一致）；
/// tree_hash 为递归相对路径/类型/内容 hash。
pub fn hash_plugin_root(root: &Path) -> Result<(String, String), AppError> {
    let content_hash = hash_plugin_manifest(root)?;
    let tree_hash = hash_directory_tree(root)?;
    Ok((content_hash, tree_hash))
}

/// Plugin manifest 身份 hash；列表扫描使用，完整 tree 延迟到动作 preview。
fn hash_plugin_manifest(root: &Path) -> Result<String, AppError> {
    let mut hasher_material = Vec::new();
    for rel in [
        ".claude-plugin/plugin.json",
        ".codex-plugin/plugin.json",
        "package.json",
    ] {
        let p = root.join(rel);
        if p.is_file() {
            let bytes = fs::read(&p)?;
            hasher_material.extend_from_slice(&bytes);
            break;
        }
    }
    Ok(if hasher_material.is_empty() {
        sha256_hex(root.display().to_string().as_bytes())
    } else {
        sha256_hex(&hasher_material)
    })
}

#[derive(Clone)]
struct CachedPluginHash {
    metadata_fingerprint: String,
    hashes: (String, String),
}

/// 只读 inventory 专用增量 hash；mutation 校验继续调用未缓存的 `hash_plugin_root`。
fn hash_plugin_root_cached(root: &Path) -> Result<(String, String), AppError> {
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, CachedPluginHash>>> = OnceLock::new();
    let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let metadata_fingerprint =
        crate::agent_hub::targets::tree_metadata::tree_metadata_fingerprint(root)?;
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
        .filter(|entry| entry.metadata_fingerprint == metadata_fingerprint)
        .cloned()
    {
        return Ok(hit.hashes);
    }
    let hashes = hash_plugin_root(root)?;
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.len() >= 512 && !guard.contains_key(&key) {
        guard.clear();
    }
    guard.insert(
        key,
        CachedPluginHash {
            metadata_fingerprint,
            hashes: hashes.clone(),
        },
    );
    Ok(hashes)
}

#[allow(clippy::too_many_arguments)] // 内部 helper：kind/disc/scope/target/evaluated/seen/items 7 段语义独立
fn discovered_to_item(
    kind: PortableAssetKind,
    disc: &DiscoveredPortableAsset,
    scope: &PortableScanScope,
    target_dto: &PortableInventoryTargetDto,
    evaluated: &EvaluatedTargetSupport,
    seen: &mut BTreeMap<String, usize>,
    items: &mut Vec<PortableInventoryItemDto>,
) {
    let source_path = disc.origin.path.display().to_string();
    let source_origin = origin_to_inventory_origin(disc.origin.origin_kind, kind);

    // 路径无关的 source_identity：与 ensure_managed canonical 跟踪同一套 origin_namespace
    // 语义。Plugin origin 走 plugin_id（优先用 adapter 已填的 parent_plugin_id，
    // 否则用路径启发式）；其余映射为 "standalone"。
    let source_identity = match source_origin {
        PortableInventorySourceOrigin::PluginComponent => {
            let plugin_id = disc
                .origin
                .parent_plugin_id
                .clone()
                .or_else(|| {
                    crate::agent_hub::portable_inventory::plugin_paths::plugin_id_from_path(Some(
                        &disc.origin.path.to_string_lossy(),
                    ))
                })
                .unwrap_or_else(|| "plugin".into());
            format!("plugin:{plugin_id}")
        }
        PortableInventorySourceOrigin::Standalone | PortableInventorySourceOrigin::NativeConfig => {
            "standalone".into()
        }
    };

    let inv_id = inventory_item_id(
        disc.origin.target,
        &scope.scope_id,
        &source_identity,
        &disc.origin.native_id,
    );

    // parent plugin package 的 inventory_item_id 也走路径无关语义：
    // plugin 包本体 source_identity = "standalone"（plugin package 是顶层 Standalone 资产）。
    let parent_plugin_inventory_item_id = disc
        .origin
        .parent_plugin_id
        .as_ref()
        .map(|plugin_id| {
            inventory_item_id(disc.origin.target, &scope.scope_id, "standalone", plugin_id)
        })
        .or_else(|| {
            // 路径启发式：.../plugins/<id>/skills|commands|...
            infer_parent_plugin_from_path(disc.origin.target, &scope.scope_id, &disc.origin.path)
        });

    let actual_enabled = actual_enabled_for(kind, disc);
    let (content_hash, tree_hash) = match kind {
        PortableAssetKind::Skill => (
            Some(disc.origin.content_hash.clone()),
            disc.origin.tree_hash.clone(),
        ),
        PortableAssetKind::Command | PortableAssetKind::Mcp | PortableAssetKind::Plugin => (
            Some(disc.origin.content_hash.clone()),
            disc.origin.tree_hash.clone(),
        ),
    };

    let mut warnings: Vec<String> = disc.diagnostics.iter().map(|d| d.code.clone()).collect();
    if matches!(disc.origin.status, PortableDiscoveryStatus::Blocked) {
        warnings.push("source_blocked".into());
    }

    let mcp_credential = if kind == PortableAssetKind::Mcp {
        Some(mcp_credential_fact(disc))
    } else {
        None
    };

    let can_mutate_scope = scope.project_opted_in;
    let can_enable = can_mutate_scope
        && action_capability_supported(
            evaluated,
            disc.origin.target,
            kind,
            PortableAssetActionKind::Enable,
        );
    let can_deactivate = can_mutate_scope
        && action_capability_supported(
            evaluated,
            disc.origin.target,
            kind,
            PortableAssetActionKind::Disable,
        );
    let enable_semantics = enable_semantics_supported(kind, disc.origin.target);
    let reason = if !scope.project_opted_in && scope.scope_kind != ScopeKind::User {
        Some("project_not_opted_in".into())
    } else if !can_enable || !can_deactivate {
        action_capability_reason(target_dto, evaluated, disc.origin.target, kind)
    } else if !enable_semantics {
        Some("enable_semantics_unsupported".into())
    } else if matches!(disc.origin.status, PortableDiscoveryStatus::Blocked) {
        Some("source_blocked".into())
    } else {
        None
    };

    let description = match &disc.payload {
        PortableAssetPayload::Skill(s) => {
            if s.description.trim().is_empty() {
                None
            } else {
                Some(s.description.clone())
            }
        }
        PortableAssetPayload::Command(c) => c.description.clone(),
        PortableAssetPayload::Mcp(_) => None,
        PortableAssetPayload::Agent(a) => a.description.clone(),
    };

    let item = PortableInventoryItemDto {
        inventory_item_id: inv_id.clone(),
        target: disc.origin.target,
        kind,
        native_id: disc.origin.native_id.clone(),
        display_name: disc.semantic_name.clone(),
        description,
        version: None,
        scope_id: scope.scope_id.clone(),
        scope_kind: scope.scope_kind,
        project_id: scope.project_id.clone(),
        project_opted_in: scope.project_opted_in,
        source_path: Some(source_path),
        source_origin,
        parent_plugin_inventory_item_id,
        actual_enabled,
        content_hash,
        tree_hash,
        canonical_asset_id: None,
        canonical_revision_id: None,
        management_state: if matches!(disc.origin.status, PortableDiscoveryStatus::Blocked) {
            PortableInventoryManagementState::Unsupported
        } else {
            PortableInventoryManagementState::Unmanaged
        },
        desired_presence: None,
        desired_enabled: None,
        materialization_status: None,
        capabilities: item_capabilities(
            disc.origin.target,
            kind,
            actual_enabled,
            can_enable,
            can_deactivate,
            enable_semantics,
            reason,
        ),
        warnings,
        mcp_credential,
    };

    // seen 去重：按 inv_id 索引已存在 item。
    // 同一逻辑资产在 active/disabled 路径下产出相同 id（路径无关 source_identity）；
    // claude.rs adapter 是 active 先扫、disabled 后扫，若用"先到先得"会让 disabled 版本
    // 被丢弃、UI 永远显示 enabled——这是严重 bug。所以这里用"disabled 赢"合并策略：
    // 当新 item 是 disabled（actual_enabled == Some(false)）而已存在不是时，替换为新 item。
    match seen.get(&inv_id).copied() {
        None => {
            seen.insert(inv_id, items.len());
            items.push(item);
        }
        Some(idx) => {
            if should_replace_with(&item, &items[idx]) {
                items[idx] = item;
                // seen 不变：index 仍指向同一槽位
            }
            // 否则丢弃（保留已存在的）
        }
    }
}

/// 判断同一 inv_id 的新 discovery 是否应替换已存在的 inventory item。
///
/// Business Logic（为什么需要这个函数）:
///     inventory_item_id 现在路径无关，同一逻辑资产在 active 与 disabled 路径下产出
///     相同 id；claude.rs adapter 先扫 active 后扫 disabled，"先到先得"会让 disabled
///     版本被丢弃，UI 永远显示 enabled。本函数实现"disabled 赢"策略：disabled 表示
///     用户最近主动操作过，是更可信的当前意图；active+disabled 共存本身是异常态
///     （正常 disable 流程会清空 active），此时保留 disabled 即反映"用户已禁用"。
///
/// Code Logic（这个函数做什么）:
///     若新 item actual_enabled == Some(false) 且已存在 item 不是 disabled → true（替换）。
///     若已存在 item 是 disabled 而新 item 不是 → false（保留已存在的 disabled）。
///     其他情况（都是 active/都是 disabled/一方 None）→ false（保留已存在，避免抖动）。
fn should_replace_with(
    new_item: &PortableInventoryItemDto,
    existing: &PortableInventoryItemDto,
) -> bool {
    if new_item.actual_enabled == Some(false) && existing.actual_enabled != Some(false) {
        return true;
    }
    false
}

fn origin_to_inventory_origin(
    origin: PortableOriginKind,
    kind: PortableAssetKind,
) -> PortableInventorySourceOrigin {
    match origin {
        PortableOriginKind::Plugin => PortableInventorySourceOrigin::PluginComponent,
        PortableOriginKind::Native
        | PortableOriginKind::Compatibility
        | PortableOriginKind::LegacyStandalone => {
            if kind == PortableAssetKind::Mcp {
                PortableInventorySourceOrigin::NativeConfig
            } else {
                PortableInventorySourceOrigin::Standalone
            }
        }
    }
}

fn actual_enabled_for(kind: PortableAssetKind, disc: &DiscoveredPortableAsset) -> Option<bool> {
    match disc.origin.status {
        PortableDiscoveryStatus::Active => Some(true),
        PortableDiscoveryStatus::Disabled => Some(false),
        PortableDiscoveryStatus::Blocked => Some(false),
        PortableDiscoveryStatus::Discovered => {
            // Command 无原生 enable 语义时返回 null；当前 discovery 统一不推断 enabled
            let _ = (kind, disc.origin.target);
            None
        }
    }
}

fn enable_semantics_supported(kind: PortableAssetKind, target: AgentTarget) -> bool {
    match kind {
        PortableAssetKind::Skill | PortableAssetKind::Plugin | PortableAssetKind::Mcp => true,
        PortableAssetKind::Command => {
            // Codex 无独立 command enable 语义；Claude/OpenCode 有 disabled 路径
            matches!(target, AgentTarget::Claude | AgentTarget::OpenCode)
        }
    }
}

/// 为 item capability 提供稳定的 target mutation 限制原因。
fn mutation_capability_reason(target: &PortableInventoryTargetDto) -> Option<String> {
    match target.mutation_capability {
        PortableInventoryMutationCapability::Supported => None,
        PortableInventoryMutationCapability::PreviewOnly => target
            .reason_code
            .clone()
            .or_else(|| Some("portable_mutation_preview_only".into())),
        PortableInventoryMutationCapability::Blocked => target.reason_code.clone(),
    }
}

/// 返回 portable action 对应的唯一 support capability。
fn action_required_capability(
    kind: PortableAssetKind,
    action: PortableAssetActionKind,
) -> Option<TargetCapability> {
    match action {
        PortableAssetActionKind::Enable => Some(if kind == PortableAssetKind::Plugin {
            TargetCapability::ActivatePackage
        } else {
            TargetCapability::RenderPortableAssets
        }),
        PortableAssetActionKind::Disable | PortableAssetActionKind::Uninstall => {
            Some(if kind == PortableAssetKind::Plugin {
                TargetCapability::DeactivatePackage
            } else {
                TargetCapability::RenderPortableAssets
            })
        }
        PortableAssetActionKind::Adopt | PortableAssetActionKind::InstallToSourceTarget => None,
    }
}

/// 判断已求值的单项写能力能否立即执行。
fn action_support_is_ready(
    evaluated: &EvaluatedTargetSupport,
    capability: TargetCapability,
) -> bool {
    let support = evaluated.capability(capability);
    let support_ready = if capability == TargetCapability::DeactivatePackage {
        support == CapabilitySupport::Supported
    } else {
        matches!(
            support,
            CapabilitySupport::Supported | CapabilitySupport::SupportedAfterRestart
        )
    };
    support_ready && evaluated.allows_write_capability(capability)
}

/// 精确检查某个 target × kind × action 是否具备写能力。
fn action_capability_supported(
    evaluated: &EvaluatedTargetSupport,
    target: AgentTarget,
    kind: PortableAssetKind,
    action: PortableAssetActionKind,
) -> bool {
    supports_direct_local_action(target, kind, action)
        && action_required_capability(kind, action)
            .is_some_and(|capability| action_support_is_ready(evaluated, capability))
}

/// 为逐动作 capability 返回稳定限制原因。
fn action_capability_reason(
    target_dto: &PortableInventoryTargetDto,
    evaluated: &EvaluatedTargetSupport,
    target: AgentTarget,
    kind: PortableAssetKind,
) -> Option<String> {
    if target_dto.mutation_capability != PortableInventoryMutationCapability::Supported {
        if let Some(reason) = mutation_capability_reason(target_dto) {
            return Some(reason);
        }
    }
    if !supports_direct_local_action(target, kind, PortableAssetActionKind::Enable)
        || !supports_direct_local_action(target, kind, PortableAssetActionKind::Uninstall)
    {
        return Some("portable_direct_action_unavailable".into());
    }
    if action_required_capability(kind, PortableAssetActionKind::Uninstall)
        == Some(TargetCapability::DeactivatePackage)
        && !action_capability_supported(evaluated, target, kind, PortableAssetActionKind::Uninstall)
    {
        return Some("deactivate_package_not_supported".into());
    }
    mutation_capability_reason(target_dto).or_else(|| {
        action_required_capability(kind, PortableAssetActionKind::Enable).and_then(|capability| {
            if action_capability_supported(evaluated, target, kind, PortableAssetActionKind::Enable)
            {
                None
            } else {
                Some(format!("{}_not_supported", capability.as_str()))
            }
        })
    })
}

fn item_capabilities(
    target: AgentTarget,
    kind: PortableAssetKind,
    actual_enabled: Option<bool>,
    can_enable_mutation: bool,
    can_deactivate_mutation: bool,
    enable_semantics: bool,
    reason: Option<String>,
) -> PortableInventoryItemCapabilitiesDto {
    let can_toggle_enable = can_enable_mutation && enable_semantics && actual_enabled.is_some();
    let can_toggle_deactivate =
        can_deactivate_mutation && enable_semantics && actual_enabled.is_some();
    let can_enable = can_toggle_enable
        && actual_enabled == Some(false)
        && supports_direct_local_action(target, kind, PortableAssetActionKind::Enable);
    let can_disable = can_toggle_deactivate
        && actual_enabled == Some(true)
        && supports_direct_local_action(target, kind, PortableAssetActionKind::Disable);
    PortableInventoryItemCapabilitiesDto {
        can_enable,
        can_disable,
        can_uninstall: can_deactivate_mutation
            && supports_direct_local_action(target, kind, PortableAssetActionKind::Uninstall),
        // Adopt ownership write is not wired (PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED).
        // Never advertise canAdopt=true — UI prioritizes Adopt as primary action otherwise.
        can_adopt: false,
        can_install_to_source_target: false,
        reason_code: reason,
        evidence_ids: vec![format!("L2-PORTABLE-{}-SCAN", kind.as_str().to_uppercase())],
    }
}

fn mcp_credential_fact(disc: &DiscoveredPortableAsset) -> PortableMcpCredentialFactDto {
    let PortableAssetPayload::Mcp(server) = &disc.payload else {
        return PortableMcpCredentialFactDto {
            present: false,
            hash: None,
        };
    };
    let mut material = String::new();
    for (k, v) in &server.env {
        material.push_str(k);
        material.push('=');
        material.push_str(v);
        material.push('\n');
    }
    match &server.transport {
        McpTransport::Http { url, headers } => {
            material.push_str(url);
            material.push('\n');
            for (k, v) in headers {
                material.push_str(k);
                material.push('=');
                material.push_str(v);
                material.push('\n');
            }
        }
        McpTransport::Stdio { .. } => {}
    }
    let present = !server.env.is_empty()
        || matches!(
            &server.transport,
            McpTransport::Http { headers, url }
                if !headers.is_empty() || url.contains("token=") || url.contains("key=")
        );
    let hash = if present {
        Some(sha256_hex(material.as_bytes()))
    } else {
        None
    };
    PortableMcpCredentialFactDto { present, hash }
}

fn infer_plugin_root(path: &Path) -> Option<PathBuf> {
    crate::agent_hub::portable_inventory::plugin_paths::infer_plugin_package_root(path)
}

fn infer_parent_plugin_from_path(
    target: AgentTarget,
    scope_id: &str,
    path: &Path,
) -> Option<String> {
    let root = infer_plugin_root(path)?;
    let plugin_id = crate::agent_hub::portable_inventory::plugin_paths::plugin_id_from_path(Some(
        &path.to_string_lossy(),
    ))
    .or_else(|| {
        root.file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    })?;
    if crate::agent_hub::portable_inventory::plugin_paths::is_plugin_infrastructure_name(&plugin_id)
    {
        return None;
    }
    Some(inventory_item_id(
        target,
        scope_id,
        "standalone",
        &plugin_id,
    ))
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
        "GROK_HOME",
        "GEMINI_HOME",
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
    use crate::agent_hub::portable_inventory::reconcile::reconcile_portable_inventory_with_facts;
    use crate::agent_hub::targets::{
        AdapterSupportLevel, ClaudeInstructionAdapter, CodexInstructionAdapter,
        CursorInstructionAdapter, GeminiInstructionAdapter, GrokInstructionAdapter,
        OpenCodeInstructionAdapter,
    };
    use std::collections::BTreeMap as Map;

    fn write(path: &Path, text: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn seed_all_targets_fixture() -> (tempfile::TempDir, TargetEnvironment) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();

        // --- Claude user ---
        write(
            &home.join(".claude/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review\n---\n# Review\n",
        );
        write(
            &home.join(".claude/disabled/skills/old-review/SKILL.md"),
            "---\nname: old-review\ndescription: Disabled\n---\n# Old\n",
        );
        write(
            &home.join(".claude/commands/ship.md"),
            "---\nname: ship\n---\nShip it\n",
        );
        write(
            &home.join(".claude/disabled/commands/legacy.md"),
            "---\nname: legacy\n---\nLegacy\n",
        );
        // standalone + plugin same-name skill
        write(
            &home.join(".claude/skills/shared-name/SKILL.md"),
            "---\nname: shared-name\n---\n# Standalone\n",
        );
        write(
            &home.join(".claude/plugins/demo-plugin/.claude-plugin/plugin.json"),
            r#"{"name":"demo-plugin","version":"1.0.0","description":"Demo"}"#,
        );
        write(
            &home.join(".claude/plugins/demo-plugin/skills/shared-name/SKILL.md"),
            "---\nname: shared-name\n---\n# Plugin component\n",
        );
        write(
            &home.join(".claude/.claude.json"),
            r#"{
  "mcpServers": {
    "good-api": {
      "command": "uvx",
      "args": ["srv"],
      "env": { "API_TOKEN": "plain-fixture" },
      "enabled": true
    },
    "off-api": {
      "command": "uvx",
      "args": ["off"],
      "enabled": false
    }
  }
}"#,
        );
        // corrupt MCP sibling file for blocked diagnostic path (settings)
        write(&home.join(".claude/broken-mcp.json"), "{ not json !!");

        // --- Codex user ---
        write(
            &home.join(".codex/config.toml"),
            r#"
[mcp_servers.good-api]
command = "uvx"
args = ["srv"]
enabled = true
env = { API_TOKEN = "plain-fixture" }

[mcp_servers.off-api]
command = "uvx"
args = ["off"]
enabled = false
"#,
        );
        write(
            &home.join(".agents/skills/review/SKILL.md"),
            "---\nname: review\n---\n# Codex review\n",
        );
        write(
            &home.join(".codex/plugins/demo-plugin/.codex-plugin/plugin.json"),
            r#"{"name":"demo-plugin","version":"0.2.0"}"#,
        );
        write(
            &home.join(".codex/plugins/demo-plugin/skills/shared-name/SKILL.md"),
            "---\nname: shared-name\n---\n# Codex plugin skill\n",
        );

        // --- OpenCode user ---
        write(
            &home.join(".opencode/skills/review/SKILL.md"),
            "---\nname: review\n---\n# OC\n",
        );
        write(
            &home.join(".opencode/disabled/skills/old/SKILL.md"),
            "---\nname: old\n---\n# old\n",
        );
        write(
            &home.join(".opencode/commands/ship.md"),
            "---\nname: ship\n---\nOC ship\n",
        );
        write(
            &home.join(".opencode/plugins/demo-plugin/package.json"),
            r#"{"name":"demo-plugin","version":"3.0.0"}"#,
        );
        write(
            &home.join(".opencode/plugins/demo-plugin/skills/shared-name/SKILL.md"),
            "---\nname: shared-name\n---\n# OC plugin\n",
        );
        write(
            &home.join("opencode.jsonc"),
            r#"{
  "mcpServers": {
    "good-api": {
      "command": "uvx",
      "args": ["oc"],
      "env": { "API_TOKEN": "plain-fixture" },
      "enabled": true
    }
  }
}
"#,
        );

        // Project fixtures
        let opted = home.join("proj-opted");
        let unopted = home.join("proj-unopted");
        write(
            &opted.join(".claude/skills/proj-skill/SKILL.md"),
            "---\nname: proj-skill\n---\n# P\n",
        );
        write(
            &unopted.join(".claude/skills/hidden/SKILL.md"),
            "---\nname: hidden\n---\n# H\n",
        );

        let mut vars = Map::new();
        vars.insert(
            "CLAUDE_CONFIG_DIR".into(),
            home.join(".claude").to_string_lossy().into(),
        );
        vars.insert(
            "CODEX_HOME".into(),
            home.join(".codex").to_string_lossy().into(),
        );
        vars.insert(
            "OPENCODE_CONFIG_DIR".into(),
            home.join(".opencode").to_string_lossy().into(),
        );
        vars.insert(
            "OPENCODE_CONFIG".into(),
            home.join("opencode.jsonc").to_string_lossy().into(),
        );
        let env = TargetEnvironment {
            home: home.clone(),
            vars,
            path_entries: vec![],
        };
        (dir, env)
    }

    fn user_and_projects(home: &Path) -> Vec<PortableScanScope> {
        vec![
            PortableScanScope {
                scope_id: "user".into(),
                scope_kind: ScopeKind::User,
                project_id: None,
                project_opted_in: true,
                absolute_path: home.to_path_buf(),
            },
            PortableScanScope {
                scope_id: "project:opted".into(),
                scope_kind: ScopeKind::Project,
                project_id: Some("opted".into()),
                project_opted_in: true,
                absolute_path: home.join("proj-opted"),
            },
            PortableScanScope {
                scope_id: "project:unopted".into(),
                scope_kind: ScopeKind::Project,
                project_id: Some("unopted".into()),
                project_opted_in: false,
                absolute_path: home.join("proj-unopted"),
            },
        ]
    }

    #[test]
    fn scan_only_manifest_cannot_be_promoted_by_direct_local_allowlist() {
        let (_tmp, env) = seed_all_targets_fixture();
        let probe = TargetProbe {
            target: AgentTarget::Claude,
            executable: Some(env.home.join("bin/claude")),
            // 缺失 runtime version 会使 manifest 求值进入 scan-only；旧直管 allowlist
            // 仍然存在，但不得因此把 mutation capability 提升为 Supported。
            version: None,
            config_root: env.home.join(".claude"),
            support: AdapterSupportLevel::Supported,
            fingerprint: "fixture-fingerprint".into(),
        };

        let target = target_dto_from_probe(AgentTarget::Claude, &probe, &env).unwrap();

        assert_eq!(
            target.mutation_capability,
            PortableInventoryMutationCapability::Blocked
        );
        assert_eq!(target.reason_code.as_deref(), Some("cli_version_unknown"));
    }

    #[test]
    fn preview_only_target_exposes_zero_mutation_affordances_and_reason() {
        let target = PortableInventoryTargetDto {
            target: AgentTarget::Claude,
            installed: true,
            version: Some("1.0.0".into()),
            executable: Some("/bin/claude".into()),
            config_root: "/cfg/claude".into(),
            scan_capability: PortableInventoryScanCapability::Supported,
            mutation_capability: PortableInventoryMutationCapability::PreviewOnly,
            reason_code: None,
            evidence_ids: vec![],
        };

        assert_ne!(
            target.mutation_capability,
            PortableInventoryMutationCapability::Supported
        );
        let capabilities = item_capabilities(
            target.target,
            PortableAssetKind::Skill,
            Some(true),
            false,
            false,
            true,
            mutation_capability_reason(&target),
        );
        assert!(!capabilities.can_enable);
        assert!(!capabilities.can_disable);
        assert!(!capabilities.can_uninstall);
        assert_eq!(
            capabilities.reason_code.as_deref(),
            Some("portable_mutation_preview_only")
        );
    }

    #[test]
    fn partial_manifest_plugin_deactivation_has_zero_remove_affordances() {
        let evaluated = EvaluatedTargetSupport {
            target: AgentTarget::Claude,
            mode: crate::agent_hub::support::EvaluatedSupportMode::Certified,
            capabilities: BTreeMap::from([
                (
                    TargetCapability::RenderPortableAssets,
                    CapabilitySupport::Supported,
                ),
                (
                    TargetCapability::ActivatePackage,
                    CapabilitySupport::Supported,
                ),
                (
                    TargetCapability::DeactivatePackage,
                    CapabilitySupport::Blocked,
                ),
            ]),
            write_allowed: true,
            reasons: vec![],
        };
        let target = PortableInventoryTargetDto {
            target: AgentTarget::Claude,
            installed: true,
            version: Some("1.0.0".into()),
            executable: Some("/bin/claude".into()),
            config_root: "/cfg/claude".into(),
            scan_capability: PortableInventoryScanCapability::Supported,
            mutation_capability: PortableInventoryMutationCapability::Supported,
            reason_code: None,
            evidence_ids: vec![],
        };
        let capabilities = item_capabilities(
            target.target,
            PortableAssetKind::Plugin,
            Some(false),
            action_capability_supported(
                &evaluated,
                target.target,
                PortableAssetKind::Plugin,
                PortableAssetActionKind::Enable,
            ),
            action_capability_supported(
                &evaluated,
                target.target,
                PortableAssetKind::Plugin,
                PortableAssetActionKind::Disable,
            ),
            true,
            action_capability_reason(
                &target,
                &evaluated,
                target.target,
                PortableAssetKind::Plugin,
            ),
        );

        assert!(capabilities.can_enable);
        assert!(!capabilities.can_disable);
        assert!(!capabilities.can_uninstall);
        assert_eq!(
            capabilities.reason_code.as_deref(),
            Some("deactivate_package_not_supported")
        );
    }

    #[test]
    fn partial_manifest_render_only_keeps_non_plugin_actions_available() {
        let evaluated = EvaluatedTargetSupport {
            target: AgentTarget::Claude,
            mode: crate::agent_hub::support::EvaluatedSupportMode::Certified,
            capabilities: BTreeMap::from([
                (
                    TargetCapability::RenderPortableAssets,
                    CapabilitySupport::Supported,
                ),
                (
                    TargetCapability::ActivatePackage,
                    CapabilitySupport::Blocked,
                ),
                (
                    TargetCapability::DeactivatePackage,
                    CapabilitySupport::Blocked,
                ),
            ]),
            write_allowed: true,
            reasons: vec![],
        };
        let target = PortableInventoryTargetDto {
            target: AgentTarget::Claude,
            installed: true,
            version: Some("1.0.0".into()),
            executable: Some("/bin/claude".into()),
            config_root: "/cfg/claude".into(),
            scan_capability: PortableInventoryScanCapability::Supported,
            mutation_capability: PortableInventoryMutationCapability::Supported,
            reason_code: None,
            evidence_ids: vec![],
        };
        let can_render = action_capability_supported(
            &evaluated,
            target.target,
            PortableAssetKind::Skill,
            PortableAssetActionKind::Disable,
        );
        let capabilities = item_capabilities(
            target.target,
            PortableAssetKind::Skill,
            Some(true),
            can_render,
            can_render,
            true,
            action_capability_reason(&target, &evaluated, target.target, PortableAssetKind::Skill),
        );

        assert!(capabilities.can_disable);
        assert!(capabilities.can_uninstall);
        assert!(capabilities.reason_code.is_none());
    }

    #[test]
    fn uncertified_opencode_executor_remains_blocked() {
        let (_tmp, env) = seed_all_targets_fixture();
        // OpenCode 仍无 min/current pin → evaluate 写能力 fail-closed。
        let probe = TargetProbe {
            target: AgentTarget::OpenCode,
            executable: Some(env.home.join("bin/opencode")),
            version: Some("1.0.0".into()),
            config_root: env.home.join(".opencode"),
            support: AdapterSupportLevel::Supported,
            fingerprint: "fixture-fingerprint".into(),
        };

        let target_dto = target_dto_from_probe(AgentTarget::OpenCode, &probe, &env).unwrap();
        assert_eq!(
            target_dto.mutation_capability,
            PortableInventoryMutationCapability::Blocked
        );
    }

    #[test]
    fn codex_known_version_unlocks_portable_mutation_after_phase1_certification() {
        let (_tmp, env) = seed_all_targets_fixture();
        let probe = TargetProbe {
            target: AgentTarget::Codex,
            executable: Some(env.home.join("bin/codex")),
            // phase-1 认证后 manifest 已 pin codex 0.145.0-alpha.4；匹配版本应解锁 mutation。
            version: Some("codex-cli 0.145.0-alpha.4".into()),
            config_root: env.home.join(".codex"),
            support: AdapterSupportLevel::Supported,
            fingerprint: "fixture-fingerprint".into(),
        };

        let target_dto = target_dto_from_probe(AgentTarget::Codex, &probe, &env).unwrap();
        assert_eq!(
            target_dto.mutation_capability,
            PortableInventoryMutationCapability::Supported,
            "phase-1 certified codex runtime must unlock portable write mutation"
        );
    }

    #[test]
    fn scan_finds_four_kinds_per_target_with_enabled_and_plugin_parent() {
        let (_tmp, env) = seed_all_targets_fixture();
        let scopes = user_and_projects(&env.home);
        let (targets, items) = scan_portable_inventory_facts(&env, &scopes).expect("scan");
        assert_eq!(targets.len(), 3);

        for target in [
            AgentTarget::Claude,
            AgentTarget::Codex,
            AgentTarget::OpenCode,
        ] {
            let t_items: Vec<_> = items.iter().filter(|i| i.target == target).collect();
            assert!(
                t_items.iter().any(|i| i.kind == PortableAssetKind::Skill),
                "{target:?} missing skill: {t_items:?}"
            );
            assert!(
                t_items.iter().any(|i| i.kind == PortableAssetKind::Plugin),
                "{target:?} missing plugin package"
            );
            assert!(
                t_items.iter().any(|i| i.kind == PortableAssetKind::Mcp),
                "{target:?} missing mcp"
            );
        }

        // Claude command present
        assert!(items
            .iter()
            .any(|i| { i.target == AgentTarget::Claude && i.kind == PortableAssetKind::Command }));

        // disabled skill actualEnabled=false
        let disabled = items
            .iter()
            .find(|i| {
                i.target == AgentTarget::Claude
                    && i.native_id == "old-review"
                    && i.kind == PortableAssetKind::Skill
            })
            .expect("disabled skill");
        assert_eq!(disabled.actual_enabled, Some(false));

        // active skill actualEnabled=true
        let active = items
            .iter()
            .find(|i| {
                i.target == AgentTarget::Claude
                    && i.native_id == "review"
                    && i.source_origin == PortableInventorySourceOrigin::Standalone
            })
            .expect("active skill");
        assert_eq!(active.actual_enabled, Some(true));
        assert!(active.content_hash.is_some());
        assert!(active.tree_hash.is_some());

        // MCP credential present/hash only + disabled MCP
        let mcp = items
            .iter()
            .find(|i| {
                i.target == AgentTarget::Claude
                    && i.kind == PortableAssetKind::Mcp
                    && i.native_id == "good-api"
            })
            .expect("mcp");
        let cred = mcp.mcp_credential.as_ref().expect("cred");
        assert!(cred.present);
        assert!(cred.hash.is_some());
        let wire = serde_json::to_value(cred).unwrap();
        assert!(!wire.to_string().contains("plain-fixture"));

        let off = items
            .iter()
            .find(|i| {
                i.target == AgentTarget::Claude
                    && i.kind == PortableAssetKind::Mcp
                    && i.native_id == "off-api"
            })
            .expect("disabled mcp");
        assert_eq!(off.actual_enabled, Some(false));

        // plugin component has parent; standalone same name remains separate
        let standalone = items
            .iter()
            .find(|i| {
                i.target == AgentTarget::Claude
                    && i.kind == PortableAssetKind::Skill
                    && i.native_id == "shared-name"
                    && i.source_origin == PortableInventorySourceOrigin::Standalone
            })
            .expect("standalone shared-name");
        let component = items
            .iter()
            .find(|i| {
                i.target == AgentTarget::Claude
                    && i.kind == PortableAssetKind::Skill
                    && i.native_id == "shared-name"
                    && i.source_origin == PortableInventorySourceOrigin::PluginComponent
            })
            .expect("plugin component shared-name");
        assert_ne!(standalone.inventory_item_id, component.inventory_item_id);
        assert!(component.parent_plugin_inventory_item_id.is_some());
        let plugin = items
            .iter()
            .find(|i| {
                i.target == AgentTarget::Claude
                    && i.kind == PortableAssetKind::Plugin
                    && i.native_id == "demo-plugin"
            })
            .expect("plugin package");
        assert_eq!(
            component.parent_plugin_inventory_item_id.as_deref(),
            Some(plugin.inventory_item_id.as_str())
        );
    }

    #[test]
    fn filtered_scan_limits_target_kind_and_scope_before_inventory_result() {
        let (_tmp, env) = seed_all_targets_fixture();
        let scopes = user_and_projects(&env.home);
        let query = PortableInventoryQuery {
            target: Some(AgentTarget::Claude),
            kind: Some(PortableAssetKind::Skill),
            scope_kind: Some(ScopeKind::User),
            local_project_id: None,
        };
        let (targets, items) = scan_portable_inventory_facts_query(&env, &scopes, query).unwrap();

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].target, AgentTarget::Claude);
        assert!(!items.is_empty());
        assert!(items.iter().all(|item| {
            item.target == AgentTarget::Claude
                && item.kind == PortableAssetKind::Skill
                && item.scope_kind == ScopeKind::User
                && item.content_hash.is_some()
                && item.tree_hash.is_none()
        }));
        assert!(items.iter().any(|item| {
            item.native_id == "shared-name"
                && item.source_origin == PortableInventorySourceOrigin::PluginComponent
        }));
        assert!(!items
            .iter()
            .any(|item| item.kind == PortableAssetKind::Plugin));
    }

    #[test]
    fn filtered_plugin_list_defers_recursive_tree_hash() {
        let (_tmp, env) = seed_all_targets_fixture();
        let scopes = user_and_projects(&env.home);
        let query = PortableInventoryQuery {
            target: Some(AgentTarget::Claude),
            kind: Some(PortableAssetKind::Plugin),
            scope_kind: Some(ScopeKind::User),
            local_project_id: None,
        };
        let (_targets, items) = scan_portable_inventory_facts_query(&env, &scopes, query).unwrap();
        assert!(!items.is_empty());
        assert!(items.iter().all(|item| {
            item.kind == PortableAssetKind::Plugin
                && item.content_hash.is_some()
                && item.tree_hash.is_none()
        }));
    }

    #[test]
    fn unresolved_local_project_query_fails_closed_before_scan() {
        let (_tmp, env) = seed_all_targets_fixture();
        let scopes = user_and_projects(&env.home);
        let query = PortableInventoryQuery {
            scope_kind: Some(ScopeKind::Project),
            local_project_id: Some("workbench-project".into()),
            ..PortableInventoryQuery::default()
        };
        let error = scan_portable_inventory_facts_query(&env, &scopes, query)
            .expect_err("pure scanner must not accept an unresolved local project id");
        assert!(error
            .to_string()
            .contains("PORTABLE_INVENTORY_LOCAL_PROJECT_ID_UNRESOLVED"));
    }

    #[test]
    fn unopted_project_is_read_only_and_opted_project_scanned() {
        let (_tmp, env) = seed_all_targets_fixture();
        let scopes = user_and_projects(&env.home);
        let (_targets, items) = scan_portable_inventory_facts(&env, &scopes).expect("scan");

        let opted = items
            .iter()
            .find(|i| i.project_id.as_deref() == Some("opted"))
            .expect("opted project item");
        assert!(opted.project_opted_in);
        // 无真实 CLI 时 mutation 可仍 blocked；但不得因 unopted 规则关闭
        assert_ne!(
            opted.capabilities.reason_code.as_deref(),
            Some("project_not_opted_in")
        );

        let unopted = items
            .iter()
            .find(|i| i.project_id.as_deref() == Some("unopted"))
            .expect("unopted project item");
        assert!(!unopted.project_opted_in);
        assert!(!unopted.capabilities.can_enable);
        assert!(!unopted.capabilities.can_disable);
        assert!(!unopted.capabilities.can_uninstall);
        assert!(!unopted.capabilities.can_adopt);
        assert_eq!(
            unopted.capabilities.reason_code.as_deref(),
            Some("project_not_opted_in")
        );

        // P1-1: even writable unmanaged user assets must not advertise canAdopt until ownership write exists
        for item in &items {
            assert!(
                !item.capabilities.can_adopt,
                "canAdopt must stay false until adopt is wired: {}",
                item.inventory_item_id
            );
        }
    }

    #[test]
    fn can_adopt_is_always_false_even_when_mutable() {
        let caps = item_capabilities(
            AgentTarget::Claude,
            PortableAssetKind::Skill,
            Some(true),
            true,
            true,
            true,
            None,
        );
        assert!(caps.can_disable);
        assert!(caps.can_uninstall);
        assert!(!caps.can_adopt);
    }

    #[test]
    fn recursive_plugin_tree_hash_detects_nested_content_and_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plugin");
        fs::create_dir_all(root.join("nested/empty")).unwrap();
        fs::write(root.join("plugin.json"), "{\"name\":\"demo\"}").unwrap();
        fs::write(root.join("nested/body.txt"), "v1").unwrap();
        let first = hash_plugin_root(&root).unwrap().1;

        fs::write(root.join("nested/body.txt"), "v2").unwrap();
        let changed = hash_plugin_root(&root).unwrap().1;
        assert_ne!(first, changed, "nested file content must be tree-bound");

        fs::remove_dir(root.join("nested/empty")).unwrap();
        let without_empty_dir = hash_plugin_root(&root).unwrap().1;
        assert_ne!(
            changed, without_empty_dir,
            "empty directory type/path is tree-bound"
        );
    }

    #[test]
    fn plugin_root_discovery_uses_installs_and_rejects_infrastructure_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let claude_root = dir.path().join(".claude");
        let installed = claude_root.join("plugins/cache/market/demo/1.0.0");
        fs::create_dir_all(&installed).unwrap();
        fs::create_dir_all(claude_root.join("plugins/cache/not-a-plugin/nested")).unwrap();
        fs::create_dir_all(claude_root.join("plugins/marketplaces/huge-tree")).unwrap();
        fs::create_dir_all(claude_root.join("plugins/data/session-state")).unwrap();
        write(
            &claude_root.join("plugins/installed_plugins.json"),
            &serde_json::json!({
                "version": 2,
                "plugins": {
                    "demo@market": [{
                        "scope": "user",
                        "installPath": installed.to_string_lossy()
                    }]
                }
            })
            .to_string(),
        );

        let roots = claude_user_plugin_roots(&claude_root);
        assert_eq!(roots.len(), 1, "{roots:?}");
        assert_eq!(roots[0].path, installed);
        assert_eq!(roots[0].registry_plugin_id.as_deref(), Some("demo"));
        assert!(!roots.iter().any(|c| {
            ["cache", "data", "marketplaces"]
                .iter()
                .any(|name| c.path.ends_with(name))
        }));

        let codex_root = dir.path().join(".codex");
        let codex_plugin = codex_root.join("plugins/cache/market/demo/2.0.0");
        write(
            &codex_plugin.join(".codex-plugin/plugin.json"),
            r#"{"name":"demo"}"#,
        );
        fs::create_dir_all(codex_root.join("plugins/.plugin-appserver")).unwrap();
        fs::create_dir_all(codex_root.join("plugins/data")).unwrap();

        let roots = codex_user_plugin_roots(&codex_root);
        assert_eq!(roots.len(), 1, "{roots:?}");
        assert_eq!(roots[0].path, codex_plugin);
    }

    /// Codex config.toml `[plugins."id@market"] enabled` 是启用权威。
    #[test]
    fn parse_codex_plugin_enablement_reads_enabled_flags() {
        let text = r#"
[plugins."browser@openai-bundled"]
enabled = true

[plugins."legacy@openai-bundled"]
enabled = false

[plugins."no-flag@openai-curated"]
"#;
        let map = parse_codex_plugin_enablement_from_toml(text);
        assert_eq!(map.get("browser@openai-bundled"), Some(&true));
        assert_eq!(map.get("legacy@openai-bundled"), Some(&false));
        // 缺 enabled 字段时默认 true（与 Codex 表存在即安装一致）
        assert_eq!(map.get("no-flag@openai-curated"), Some(&true));
        assert!(!map.contains_key("missing@x"));
    }

    #[test]
    fn codex_plugin_package_actual_enabled_follows_config_not_directory() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let codex = home.join(".codex");
        let browser = codex.join("plugins/cache/openai-bundled/browser/26.803.61601");
        write(
            &browser.join(".codex-plugin/plugin.json"),
            r#"{"name":"browser","version":"1"}"#,
        );
        let latex = codex.join("plugins/cache/openai-bundled/latex/0.2.2");
        write(
            &latex.join(".codex-plugin/plugin.json"),
            r#"{"name":"latex","version":"1"}"#,
        );
        write(
            &codex.join("config.toml"),
            r#"
[plugins."browser@openai-bundled"]
enabled = true

[plugins."computer-use@openai-bundled"]
enabled = false
"#,
        );
        let mut vars = BTreeMap::new();
        vars.insert("CODEX_HOME".into(), codex.to_string_lossy().into_owned());
        let env = TargetEnvironment {
            home: home.clone(),
            vars,
            path_entries: vec![],
        };
        let scopes = [PortableScanScope {
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            absolute_path: home.clone(),
        }];
        let query = PortableInventoryQuery {
            target: Some(AgentTarget::Codex),
            kind: Some(PortableAssetKind::Plugin),
            scope_kind: Some(ScopeKind::User),
            local_project_id: None,
        };
        let (_targets, items) =
            scan_portable_inventory_facts_query(&env, &scopes, query).expect("scan");
        let browser_item = items
            .iter()
            .find(|i| i.native_id == "browser" || i.native_id.starts_with("browser@"))
            .expect("browser package");
        assert_eq!(
            browser_item.actual_enabled,
            Some(true),
            "config enabled=true"
        );
        let latex_item = items
            .iter()
            .find(|i| i.native_id == "latex" || i.native_id.starts_with("latex@"))
            .expect("latex residual package");
        assert_eq!(
            latex_item.actual_enabled,
            Some(false),
            "cache-only package not listed enabled in config must not report always-true"
        );
        assert!(
            latex_item
                .warnings
                .iter()
                .any(|w| w.contains("codex_plugin_not_in_config")),
            "residual should warn: {:?}",
            latex_item.warnings
        );
    }

    #[test]
    fn claude_registry_identity_used_when_install_lacks_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let claude_root = dir.path().join(".claude");
        let installed = claude_root.join("plugins/cache/claude-plugins-official/pyright-lsp/1.0.0");
        fs::create_dir_all(&installed).unwrap();
        // no .claude-plugin/plugin.json — only LICENSE-like content
        write(&installed.join("README.md"), "x");
        write(
            &claude_root.join("plugins/installed_plugins.json"),
            &serde_json::json!({
                "version": 2,
                "plugins": {
                    "pyright-lsp@claude-plugins-official": [{
                        "scope": "user",
                        "installPath": installed.to_string_lossy()
                    }]
                }
            })
            .to_string(),
        );
        let roots = claude_user_plugin_roots(&claude_root);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].registry_plugin_id.as_deref(), Some("pyright-lsp"));
    }

    #[test]
    fn scan_is_read_only_no_file_mutations() {
        let (_tmp, env) = seed_all_targets_fixture();
        let scopes = user_and_projects(&env.home);
        let before = walk_snapshot(&env.home);
        let _ = scan_portable_inventory_facts(&env, &scopes).unwrap();
        // also exercise adapters directly
        let scope = LocalScopeMapping {
            scope_kind: ScopeKind::User,
            absolute_path: env.home.clone(),
            project_root: None,
            relative_root: None,
            codex_fallback_filenames: vec![],
        };
        let _ = ClaudeInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        let _ = CodexInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        let _ = OpenCodeInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        let _ = GrokInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        let _ = GeminiInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        let _ = CursorInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        let after = walk_snapshot(&env.home);
        assert_eq!(before, after, "scan must not write target files");
    }

    #[test]
    fn reconcile_snapshot_from_scan_keeps_standalone_and_component_separate() {
        let (_tmp, env) = seed_all_targets_fixture();
        let scopes = user_and_projects(&env.home);
        let (targets, items) = scan_portable_inventory_facts(&env, &scopes).unwrap();
        let snap = reconcile_portable_inventory_with_facts(targets, items, &[]).unwrap();
        let shared: Vec<_> = snap
            .items
            .iter()
            .filter(|i| {
                i.target == AgentTarget::Claude
                    && i.kind == PortableAssetKind::Skill
                    && i.native_id == "shared-name"
            })
            .collect();
        assert_eq!(shared.len(), 2);
        assert!(shared.iter().any(|i| {
            i.source_origin == PortableInventorySourceOrigin::Standalone
                && i.parent_plugin_inventory_item_id.is_none()
        }));
        assert!(shared.iter().any(|i| {
            i.source_origin == PortableInventorySourceOrigin::PluginComponent
                && i.parent_plugin_inventory_item_id.is_some()
        }));
        assert!(!snap.inventory_snapshot_hash.is_empty());
        assert!(!snap.refreshed_at.is_empty());
    }

    /// Business Logic: inventory_item_id 路径无关后，同一逻辑资产在 active 与 disabled 路径下
    /// 产出相同 id；claude.rs adapter 先扫 active 后扫 disabled，"先到先得"会让 disabled 版本
    /// 被丢弃、UI 永远显示 enabled。scanner 必须用"disabled 赢"合并策略：active+disabled 共存时
    /// （这是异常态，正常 disable 流程会清空 active），保留 disabled 反映用户最近的禁用意图。
    /// Code Logic: 构造同名 skill 同时存在于 active 路径（.claude/skills/dup-name）与 disabled
    /// 路径（.claude/disabled/skills/dup-name），跑 scan，断言只剩一条记录且 actual_enabled==Some(false)。
    #[test]
    fn scan_merges_active_and_disabled_with_same_logical_identity_keeps_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        // 同名 skill 同时在 active 与 disabled 目录
        write(
            &home.join(".claude/skills/dup-name/SKILL.md"),
            "---\nname: dup-name\ndescription: Active copy\n---\n# Active\n",
        );
        write(
            &home.join(".claude/disabled/skills/dup-name/SKILL.md"),
            "---\nname: dup-name\ndescription: Disabled copy\n---\n# Disabled\n",
        );
        let mut vars = Map::new();
        vars.insert(
            "CLAUDE_CONFIG_DIR".into(),
            home.join(".claude").to_string_lossy().into_owned(),
        );
        let env = TargetEnvironment {
            home: home.clone(),
            vars,
            path_entries: vec![],
        };
        let scopes = vec![PortableScanScope {
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            absolute_path: home.clone(),
        }];
        let (_targets, items) = scan_portable_inventory_facts(&env, &scopes).expect("scan");

        let dup: Vec<_> = items
            .iter()
            .filter(|i| {
                i.target == AgentTarget::Claude
                    && i.kind == PortableAssetKind::Skill
                    && i.native_id == "dup-name"
                    && i.source_origin == PortableInventorySourceOrigin::Standalone
            })
            .collect();
        // 必须合并成一条（同逻辑身份），不是两条
        assert_eq!(
            dup.len(),
            1,
            "active+disabled same logical identity must merge to one item, got: {dup:?}"
        );
        // disabled 赢：actual_enabled == Some(false)
        assert_eq!(
            dup[0].actual_enabled,
            Some(false),
            "merged item must reflect disabled (disabled wins)"
        );
        // source_path 应指向 disabled 路径（替换生效）
        assert!(
            dup[0]
                .source_path
                .as_deref()
                .unwrap_or_default()
                .contains("disabled/skills/dup-name"),
            "merged item source_path must point to disabled copy: {:?}",
            dup[0].source_path
        );
    }

    fn walk_snapshot(root: &Path) -> Vec<(String, u64)> {
        let mut v = Vec::new();
        for e in walkdir::WalkDir::new(root).follow_links(false) {
            let e = e.unwrap();
            if e.file_type().is_file() {
                let rel = e
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let len = e.metadata().unwrap().len();
                v.push((rel, len));
            }
        }
        v.sort();
        v
    }
}
