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
    PortableInventoryMutationCapability, PortableInventoryScanCapability,
    PortableInventorySnapshotDto, PortableInventorySourceOrigin, PortableInventoryTargetDto,
    PortableMcpCredentialFactDto,
};
use crate::agent_hub::portable_inventory::reconcile::reconcile_portable_inventory;
use crate::agent_hub::support::{
    builtin_support_manifest, evaluate_target_support, find_target_record, CapabilitySupport,
    RuntimeProbeSnapshot, TargetCapability,
};
use crate::agent_hub::targets::portable::{
    DiscoveredPortableAsset, PortableDiscoveryStatus, PortableOriginKind,
};
use crate::agent_hub::targets::{
    AssetAdapter, ClaudeInstructionAdapter, CodexInstructionAdapter, LocalScopeMapping,
    OpenCodeInstructionAdapter, TargetEnvironment, TargetPathResolver, TargetProbe,
};
use crate::error::AppError;
use crate::state::AppState;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

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
    if let Some(hit) = crate::agent_hub::portable_inventory::cache::get_cached_portable_inventory()
    {
        return Ok(hit);
    }
    let env = current_target_environment();
    let snapshot = inspect_portable_inventory_with_env(state, &env).await?;
    crate::agent_hub::portable_inventory::cache::store_cached_portable_inventory(snapshot.clone());
    Ok(snapshot)
}

/// 使用注入环境刷新 portable inventory（可测）。
///
/// Business Logic: 隔离 HOME 与生产扫描共用同一路径/origin 规则；发现即管理。
/// Code Logic: scopes → scan → `ensure_discovered_portable_items_managed` → reconcile。
pub async fn inspect_portable_inventory_with_env(
    state: &AppState,
    env: &TargetEnvironment,
) -> Result<PortableInventorySnapshotDto, AppError> {
    let scopes = collect_scan_scopes(state, env).await?;
    let (targets, mut discovered) = scan_portable_inventory_facts(env, &scopes)?;
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
    let adapters: Vec<Box<dyn AssetAdapter>> = vec![
        Box::new(ClaudeInstructionAdapter),
        Box::new(CodexInstructionAdapter),
        Box::new(OpenCodeInstructionAdapter),
    ];
    let homes = TargetPathResolver::resolve_all(env);
    let mut target_dtos = Vec::with_capacity(adapters.len());
    let mut items: Vec<PortableInventoryItemDto> = Vec::new();
    let mut seen_ids = BTreeSet::new();

    for adapter in &adapters {
        let target = adapter.target();
        let probe = adapter.probe(env)?;
        let target_dto = target_dto_from_probe(target, &probe, env)?;
        target_dtos.push(target_dto.clone());

        for scope in scopes {
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
            let discoveries = match adapter.scan_portable_assets(&mapping, env) {
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
            };
            // Plugin package roots（package 本体 + 组件 parent 关联）
            let plugin_packages =
                scan_plugin_packages(target, scope, env, &homes, &target_dto, &mut seen_ids)?;
            items.extend(plugin_packages);

            for disc in discoveries {
                // Instruction/Agent/Hook 不进 portable 四类库存
                let Ok(kind) = PortableAssetKind::try_from_asset_kind(disc.kind) else {
                    continue;
                };
                if let Some(item) =
                    discovered_to_item(kind, &disc, scope, &target_dto, &mut seen_ids)
                {
                    items.push(item);
                }
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
) -> Result<Vec<PortableScanScope>, AppError> {
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
    };
    let installed = probe.executable.is_some();
    let manifest = builtin_support_manifest()?;
    let snapshot = RuntimeProbeSnapshot {
        target,
        executable: probe.executable.clone(),
        version: probe.version.clone(),
        config_root: probe.config_root.clone(),
        fingerprint: probe.fingerprint.clone(),
        help_fingerprint: None,
    };
    let evaluated = evaluate_target_support(&manifest, &snapshot);
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
    let mutation_cap = if direct_local_management
        || (evaluated.allows_write_capability(TargetCapability::RenderPortableAssets)
            && evaluated.allows_write_capability(TargetCapability::ActivatePackage))
    {
        PortableInventoryMutationCapability::Supported
    } else if matches!(
        evaluated.capability(TargetCapability::RenderPortableAssets),
        CapabilitySupport::ReadOnly | CapabilitySupport::Supported
    ) {
        PortableInventoryMutationCapability::PreviewOnly
    } else {
        PortableInventoryMutationCapability::Blocked
    };
    let reason_code = if probe.version.is_none() {
        Some("cli_version_unknown".into())
    } else if !installed {
        Some("cli_not_installed".into())
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
fn scan_plugin_packages(
    target: AgentTarget,
    scope: &PortableScanScope,
    env: &TargetEnvironment,
    homes: &crate::agent_hub::targets::paths::TargetHomes,
    target_dto: &PortableInventoryTargetDto,
    seen: &mut BTreeSet<String>,
) -> Result<Vec<PortableInventoryItemDto>, AppError> {
    let roots = plugin_roots_for(target, scope, env, homes);
    let mut out = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let source = match discover_plugin_source_for_target(
            target,
            &root,
            scope.scope_id.clone(),
            scope.scope_kind,
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let source_identity = root.display().to_string();
        let inv_id =
            inventory_item_id(target, &scope.scope_id, &source_identity, &source.plugin_id);
        if !seen.insert(inv_id.clone()) {
            continue;
        }
        let (content_hash, tree_hash) = hash_plugin_root(&root)?;
        let mut warnings = Vec::new();
        // 组件同名 skill 路径存在时不合并——仅作 package 行
        let component_skill = root.join("skills");
        if component_skill.is_dir() {
            warnings.push("plugin_has_components".into());
        }
        let can_mutate = scope.project_opted_in
            && scope.scope_kind != ScopeKind::Directory
            && target_dto.mutation_capability != PortableInventoryMutationCapability::Blocked;
        let reason = if !scope.project_opted_in && scope.scope_kind != ScopeKind::User {
            Some("project_not_opted_in".into())
        } else if target_dto.mutation_capability == PortableInventoryMutationCapability::Blocked {
            target_dto.reason_code.clone()
        } else {
            None
        };
        out.push(PortableInventoryItemDto {
            inventory_item_id: inv_id,
            target,
            kind: PortableAssetKind::Plugin,
            native_id: source.plugin_id.clone(),
            display_name: source.name,
            description: source.description,
            version: source.version,
            scope_id: scope.scope_id.clone(),
            scope_kind: scope.scope_kind,
            project_id: scope.project_id.clone(),
            project_opted_in: scope.project_opted_in,
            source_path: Some(source_identity),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            content_hash: Some(content_hash),
            tree_hash: Some(tree_hash),
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::Unmanaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: item_capabilities(
                target,
                PortableAssetKind::Plugin,
                Some(true),
                can_mutate,
                true,
                reason,
            ),
            warnings,
            mcp_credential: None,
        });
    }
    Ok(out)
}

fn plugin_roots_for(
    target: AgentTarget,
    scope: &PortableScanScope,
    env: &TargetEnvironment,
    homes: &crate::agent_hub::targets::paths::TargetHomes,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let candidates = match (target, scope.scope_kind) {
        (AgentTarget::Claude, ScopeKind::User) => {
            vec![homes.claude.config_root.join("plugins")]
        }
        (AgentTarget::Claude, _) => {
            vec![scope.absolute_path.join(".claude").join("plugins")]
        }
        (AgentTarget::Codex, ScopeKind::User) => {
            vec![homes.codex.config_root.join("plugins")]
        }
        (AgentTarget::Codex, _) => {
            vec![scope.absolute_path.join(".codex").join("plugins")]
        }
        (AgentTarget::OpenCode, ScopeKind::User) => {
            vec![
                homes.opencode.config_root.join("plugins"),
                env.home.join(".opencode").join("plugins"),
            ]
        }
        (AgentTarget::OpenCode, _) => {
            vec![
                scope.absolute_path.join(".opencode").join("plugins"),
                scope.absolute_path.join("plugins"),
            ]
        }
    };
    for base in candidates {
        if !base.is_dir() {
            continue;
        }
        if let Ok(read) = fs::read_dir(&base) {
            for entry in read.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    roots.push(p);
                }
            }
        }
    }
    roots
}

/// Plugin 根目录 content_hash + tree_hash（与 inventory 行同源）。
///
/// Business Logic: planner `expected_source_hash` 与 apply recheck 必须共享同一 material 域，
/// 禁止路径字符串 sha 与 manifest 字节 hash 混用导致生产 plugin 永远 SOURCE_HASH_CHANGED。
/// Code Logic: 优先 manifest 文件字节；无 manifest 才回落 path display（与历史 inventory 一致）；
/// tree_hash 为根下一层名排序拼接。
pub fn hash_plugin_root(root: &Path) -> Result<(String, String), AppError> {
    // 优先 manifest 字节 + 目录 tree（不写 CAS）
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
    let content_hash = if hasher_material.is_empty() {
        sha256_hex(root.display().to_string().as_bytes())
    } else {
        sha256_hex(&hasher_material)
    };
    // 轻量 tree：子路径名排序拼接
    let mut names = Vec::new();
    if let Ok(read) = fs::read_dir(root) {
        for e in read.flatten() {
            names.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    let tree_hash = sha256_hex(names.join("\n").as_bytes());
    Ok((content_hash, tree_hash))
}

fn discovered_to_item(
    kind: PortableAssetKind,
    disc: &DiscoveredPortableAsset,
    scope: &PortableScanScope,
    target_dto: &PortableInventoryTargetDto,
    seen: &mut BTreeSet<String>,
) -> Option<PortableInventoryItemDto> {
    let source_path = disc.origin.path.display().to_string();
    let source_identity = source_path.clone();
    let inv_id = inventory_item_id(
        disc.origin.target,
        &scope.scope_id,
        &source_identity,
        &disc.origin.native_id,
    );
    if !seen.insert(inv_id.clone()) {
        return None;
    }

    let source_origin = origin_to_inventory_origin(disc.origin.origin_kind, kind);
    let parent_plugin_inventory_item_id = disc
        .origin
        .parent_plugin_id
        .as_ref()
        .map(|plugin_id| {
            // parent package identity uses package root (parent of skills/commands/...)
            let parent_root =
                infer_plugin_root(&disc.origin.path).unwrap_or_else(|| disc.origin.path.clone());
            inventory_item_id(
                disc.origin.target,
                &scope.scope_id,
                &parent_root.display().to_string(),
                plugin_id,
            )
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
    let mutation_target_ok =
        target_dto.mutation_capability != PortableInventoryMutationCapability::Blocked;
    let enable_semantics = enable_semantics_supported(kind, disc.origin.target);
    let reason = if !scope.project_opted_in && scope.scope_kind != ScopeKind::User {
        Some("project_not_opted_in".into())
    } else if !mutation_target_ok {
        target_dto.reason_code.clone()
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

    Some(PortableInventoryItemDto {
        inventory_item_id: inv_id,
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
            can_mutate_scope && mutation_target_ok,
            enable_semantics,
            reason,
        ),
        warnings,
        mcp_credential,
    })
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

fn item_capabilities(
    target: AgentTarget,
    kind: PortableAssetKind,
    actual_enabled: Option<bool>,
    can_mutate: bool,
    enable_semantics: bool,
    reason: Option<String>,
) -> PortableInventoryItemCapabilitiesDto {
    let can_toggle = can_mutate && enable_semantics && actual_enabled.is_some();
    let can_enable = can_toggle
        && actual_enabled == Some(false)
        && supports_direct_local_action(target, kind, PortableAssetActionKind::Enable);
    let can_disable = can_toggle
        && actual_enabled == Some(true)
        && supports_direct_local_action(target, kind, PortableAssetActionKind::Disable);
    PortableInventoryItemCapabilitiesDto {
        can_enable,
        can_disable,
        can_uninstall: can_mutate
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
    let mut cur = path.to_path_buf();
    // climb until parent name is "plugins" or hit root
    for _ in 0..8 {
        let parent = cur.parent()?.to_path_buf();
        let name = parent.file_name()?.to_string_lossy();
        if name == "plugins" {
            return Some(cur);
        }
        cur = parent;
    }
    None
}

fn infer_parent_plugin_from_path(
    target: AgentTarget,
    scope_id: &str,
    path: &Path,
) -> Option<String> {
    let root = infer_plugin_root(path)?;
    let plugin_id = root.file_name()?.to_string_lossy().into_owned();
    Some(inventory_item_id(
        target,
        scope_id,
        &root.display().to_string(),
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
    fn claude_direct_management_is_not_blocked_by_projection_manifest() {
        let (_tmp, env) = seed_all_targets_fixture();
        let probe = TargetProbe {
            target: AgentTarget::Claude,
            executable: Some(env.home.join("bin/claude")),
            version: Some("2.1.207 (Claude Code)".into()),
            config_root: env.home.join(".claude"),
            support: AdapterSupportLevel::Supported,
            fingerprint: "fixture-fingerprint".into(),
        };

        let target = target_dto_from_probe(AgentTarget::Claude, &probe, &env).unwrap();

        assert_eq!(
            target.mutation_capability,
            PortableInventoryMutationCapability::Supported
        );
        assert_eq!(target.reason_code, None);
        assert!(target
            .evidence_ids
            .iter()
            .any(|id| id == "L2-AGENT-HUB-PORTABLE-PARITY-001"));
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
    fn codex_mutation_unlocked_when_runtime_meets_pin() {
        let (_tmp, env) = seed_all_targets_fixture();
        let probe = TargetProbe {
            target: AgentTarget::Codex,
            executable: Some(env.home.join("bin/codex")),
            // 与 support-manifest currentTestedVersion 同 core
            version: Some("codex-cli 0.145.0-alpha.4".into()),
            config_root: env.home.join(".codex"),
            support: AdapterSupportLevel::Supported,
            fingerprint: "fixture-fingerprint".into(),
        };

        let target_dto = target_dto_from_probe(AgentTarget::Codex, &probe, &env).unwrap();
        assert_eq!(
            target_dto.mutation_capability,
            PortableInventoryMutationCapability::Supported,
            "codex pin should unlock portable mutation capability at inventory layer"
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
            None,
        );
        assert!(caps.can_disable);
        assert!(caps.can_uninstall);
        assert!(!caps.can_adopt);
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
