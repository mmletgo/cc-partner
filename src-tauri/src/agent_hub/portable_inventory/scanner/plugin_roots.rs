//! portable_inventory/scanner/plugin_roots — Plugin 包扫描与根候选
//!
//! Business Logic（为什么需要这个模块）:
//!     Plugin package 是顶层 Standalone 资产，必须在 inventory 中单列一行并关联包内组件；
//!     各 CLI Agent 的安装权威（installed_plugins.json / cache manifest / 直装 manifest）不同，
//!     必须按 target 收敛同一套根候选，禁止一级 read_dir(plugins) 把 cache/data 当成插件包。
//!
//! Code Logic（这个模块做什么）:
//!     `scan_plugin_packages` 遍历 plugin roots 生成 package inventory 行，并把包内组件经
//!     discovered_to_item 写入同一 items、回填 parent_plugin_inventory_item_id；
//!     `PluginRootCandidate` 携带 registry 身份与 origin 戳；其余函数负责各 target
//!     user/project scope 的根发现与跨 Agent 借用重分类。

use super::hashing::{hash_plugin_manifest, hash_plugin_root_cached};
use super::items::{
    action_capability_reason, apply_unopted_readonly_store_caps, discovered_to_item,
    item_capabilities, mutation_gates_for_origin, should_replace_with,
};
use super::PortableScanScope;
use crate::{
    agent_hub::{
        models::{AgentTarget, ScopeKind},
        plugins::decompose::discover_plugin_source_for_target,
        portable_inventory::{
            models::{
                inventory_item_id, PortableAssetKind, PortableInventoryItemDto,
                PortableInventoryManagementState, PortableInventorySourceOrigin,
                PortableInventoryTargetDto, PortableStoreFactDto,
            },
            plugin_enablement::{plugin_actual_enabled, ViewingPluginEnablement},
        },
        support::EvaluatedTargetSupport,
        targets::{
            portable::{is_borrowed_runtime_origin, PortableAssetOwner, PortableOriginKind},
            TargetEnvironment,
        },
    },
    error::AppError,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

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
pub(super) fn scan_plugin_packages(
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
    let codex_config_root = if target == AgentTarget::Codex && scope.scope_kind != ScopeKind::User {
        scope.absolute_path.join(".codex")
    } else {
        homes.codex.config_root.clone()
    };
    let enablement = ViewingPluginEnablement::load(
        target,
        &homes.claude.config_root,
        &codex_config_root,
        &homes.grok.config_root,
    );
    for candidate in roots {
        let root = candidate.path.clone();
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
        // 稳定身份：完整 registry key（id@marketplace）> 短 registry id > 路径布局 id > manifest/目录名。
        // 短 id 会把官方源与第三方 marketplace 安装塌成一行；uninstall 短名只卸掉其中一个，
        // rescan 仍看到另一份 → PORTABLE_ASSET_ACTION_RESCAN_MISMATCH。
        let dir_name = root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let qualified = candidate
            .registry_key
            .as_ref()
            .filter(|k| k.contains('@') && !k.trim().is_empty())
            .cloned()
            .or_else(|| {
                crate::agent_hub::portable_inventory::plugin_paths::plugin_registry_key_from_path(
                    Some(&root.display().to_string()),
                )
                .filter(|k| k.contains('@'))
            });
        let short = candidate
            .registry_plugin_id
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .or_else(|| {
                crate::agent_hub::portable_inventory::plugin_paths::plugin_id_from_path(Some(
                    &root.display().to_string(),
                ))
            })
            .unwrap_or_else(|| source.plugin_id.clone());
        if source.name == dir_name || source.name.is_empty() {
            source.name = short.clone();
        }
        if let Some(key) = qualified {
            source.plugin_id = key;
        } else if !short.is_empty() && (source.plugin_id == dir_name || source.plugin_id.is_empty())
        {
            source.plugin_id = short;
        }
        let identity_leaf = source
            .plugin_id
            .split('@')
            .next()
            .unwrap_or(source.plugin_id.as_str());
        if crate::agent_hub::portable_inventory::plugin_paths::is_plugin_infrastructure_name(
            identity_leaf,
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
            let can_mutate_scope =
                scope.project_opted_in && scope.scope_kind != ScopeKind::Directory;
            let (mut origin_kind, mut owned_by, mut native_output_candidate) =
                classify_plugin_package_origin(target, &root, homes);
            if candidate.origin_kind != PortableOriginKind::Native {
                origin_kind = candidate.origin_kind;
                owned_by = candidate.owned_by;
                native_output_candidate = origin_kind.is_native_output_candidate();
            }
            let native = owned_by.as_hub_target() == Some(target)
                && origin_kind == PortableOriginKind::Native;
            let (enabled, warn) = plugin_actual_enabled(
                &enablement,
                &source.plugin_id,
                candidate.registry_key.as_deref(),
                native,
            );
            if let Some(w) = warn {
                warnings.push(w);
            }
            let actual_enabled = Some(enabled);
            let (can_enable, can_disable, can_uninstall, enablement_target, uninstall_target) =
                mutation_gates_for_origin(
                    target,
                    owned_by,
                    native_output_candidate,
                    origin_kind,
                    PortableAssetKind::Plugin,
                    evaluated,
                    can_mutate_scope,
                );
            let borrowed =
                is_borrowed_runtime_origin(target, owned_by, native_output_candidate, origin_kind);
            let reason = if !scope.project_opted_in && scope.scope_kind != ScopeKind::User {
                Some("project_not_opted_in".into())
            } else if borrowed {
                None
            } else {
                action_capability_reason(target_dto, evaluated, target, PortableAssetKind::Plugin)
            };
            let mut item = PortableInventoryItemDto {
                inventory_item_id: inv_id.clone(),
                target,
                loaded_by: target,
                owned_by,
                origin_kind,
                native_output_candidate,
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
                    enablement_target,
                    uninstall_target,
                    PortableAssetKind::Plugin,
                    actual_enabled,
                    can_enable,
                    can_disable,
                    can_uninstall,
                    true,
                    reason,
                    borrowed,
                    origin_kind,
                    native_output_candidate,
                    &PortableStoreFactDto::default(),
                    PortableAssetKind::Plugin,
                ),
                warnings,
                mcp_credential: None,
                store: PortableStoreFactDto::default(),
            };
            apply_unopted_readonly_store_caps(&mut item, can_mutate_scope);
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

/// 可扫描的 plugin package 根（含可选 registry 身份与 origin 戳）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PluginRootCandidate {
    pub(super) path: PathBuf,
    /// installed_plugins.json 的 key 前缀（`pyright-lsp@market` → `pyright-lsp`）
    pub(super) registry_plugin_id: Option<String>,
    /// 完整 registry key（`pyright-lsp@claude-plugins-official`），用于 enabledPlugins 精确查找
    pub(super) registry_key: Option<String>,
    pub(super) origin_kind: PortableOriginKind,
    pub(super) owned_by: PortableAssetOwner,
}

impl PluginRootCandidate {
    /// 默认 native 根；调用方可再按路径重分类。
    fn path_only(path: PathBuf) -> Self {
        Self::native(path, PortableAssetOwner::Unknown)
    }

    /// Native 写出候选，所有者已知。
    fn native(path: PathBuf, owned_by: PortableAssetOwner) -> Self {
        Self {
            path,
            registry_plugin_id: None,
            registry_key: None,
            origin_kind: PortableOriginKind::Native,
            owned_by,
        }
    }

    /// Native 写出候选，所有者由扫描 target 推导。
    #[allow(dead_code)]
    fn native_for(target: AgentTarget, path: PathBuf) -> Self {
        Self::native(path, PortableAssetOwner::from_target(target))
    }
}

/// 按路径判断 plugin package 的 origin / 所有者 / 是否可作 native 写出。
///
/// Business Logic（为什么需要这个函数）:
///     非本 target 的 Claude / `.agents` 根只是运行时借用，不得变成可卸载写出目标。
///
/// Code Logic（这个函数做什么）:
///     Claude 配置根且 target≠Claude → Compatibility/Claude；`.agents` →
///     Codex Skill 表为 LegacyStandalone + ownedBy Codex，plugin 包仍 SharedAgents；
///     其余 Agent 的 `.agents` Skill 为 Compatibility / SharedAgents；
///     否则 Native + from_target。
fn classify_plugin_package_origin(
    target: AgentTarget,
    path: &Path,
    homes: &crate::agent_hub::targets::paths::TargetHomes,
) -> (PortableOriginKind, PortableAssetOwner, bool) {
    let under_claude = path.starts_with(&homes.claude.config_root)
        || path.components().any(|c| c.as_os_str() == ".claude");
    if under_claude && target != AgentTarget::Claude {
        return (
            PortableOriginKind::Compatibility,
            PortableAssetOwner::Claude,
            false,
        );
    }
    let under_agents = path.components().any(|c| c.as_os_str() == ".agents");
    if under_agents {
        let origin_kind = if target == AgentTarget::Codex {
            PortableOriginKind::LegacyStandalone
        } else {
            PortableOriginKind::Compatibility
        };
        return (origin_kind, PortableAssetOwner::SharedAgents, false);
    }
    (
        PortableOriginKind::Native,
        PortableAssetOwner::from_target(target),
        true,
    )
}

/// 当前 scope 下应扫描的 plugin package 根（权威入口）。
///
/// Business Logic: adapters 与 inventory 必须共享同一根集合，禁止一级 read_dir(plugins)。
/// Code Logic: user 走 registry/cache manifest；project 只认直装 manifest。
pub(super) fn plugin_roots_for(
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
            grok_user_plugin_roots(&homes.grok.config_root, &homes.claude.config_root)
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
        (AgentTarget::Pi, ScopeKind::User) => {
            direct_manifest_plugin_roots(&homes.pi.config_root.join("plugins"), target)
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
        (AgentTarget::Pi, _) => {
            direct_manifest_plugin_roots(&scope.absolute_path.join(".pi").join("plugins"), target)
                .into_iter()
                .map(PluginRootCandidate::path_only)
                .collect()
        }
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
        AgentTarget::Grok => {
            let claude_root = config_root
                .parent()
                .map(|home| home.join(".claude"))
                .unwrap_or_else(|| config_root.join(".claude"));
            grok_user_plugin_roots(config_root, &claude_root)
        }
        AgentTarget::Gemini | AgentTarget::Cursor | AgentTarget::Pi => {
            direct_manifest_plugin_roots(&config_root.join("plugins"), target)
                .into_iter()
                .map(PluginRootCandidate::path_only)
                .collect()
        }
    };
    candidates.into_iter().map(|c| c.path).collect()
}

/// 收集 Grok user-scope plugin 根：native `.grok` + Claude 兼容 registry/marketplace。
///
/// Business Logic（为什么需要这个函数）:
///     Grok 运行时同时加载 `plugins`、`installed-plugins` 与 Claude 已安装插件；
///     兼容根必须标成 borrowed，不得当成 Grok native 写出/卸载目标。
///
/// Code Logic（这个函数做什么）:
///     直装 manifest + installed-plugins + `claude_user_plugin_roots` 重分类 +
///     `known_marketplaces.json` installLocation 下的 Claude manifest 包。
fn grok_user_plugin_roots(
    grok_config_root: &Path,
    claude_config_root: &Path,
) -> Vec<PluginRootCandidate> {
    let mut roots = Vec::new();
    roots.extend(
        direct_manifest_plugin_roots(&grok_config_root.join("plugins"), AgentTarget::Grok)
            .into_iter()
            .map(|path| PluginRootCandidate::native(path, PortableAssetOwner::Grok)),
    );
    roots.extend(
        direct_manifest_plugin_roots(
            &grok_config_root.join("installed-plugins"),
            AgentTarget::Grok,
        )
        .into_iter()
        .map(|path| PluginRootCandidate::native(path, PortableAssetOwner::Grok)),
    );
    roots.extend(reclassify_claude_plugin_roots_for_grok(
        claude_user_plugin_roots(claude_config_root),
    ));
    roots.extend(claude_marketplace_plugin_roots_for_grok(claude_config_root));
    roots
}

/// 把 Claude registry 候选改成 Grok 借用的 compatibility 根。
///
/// Business Logic（为什么需要这个函数）:
///     `claude_user_plugin_roots` 对 Claude 自己标 Native；Grok 只能借用，不能写成 native。
///
/// Code Logic（这个函数做什么）:
///     保留 path / registry_plugin_id，覆盖 origin_kind=Compatibility、owned_by=Claude。
fn reclassify_claude_plugin_roots_for_grok(
    candidates: Vec<PluginRootCandidate>,
) -> Vec<PluginRootCandidate> {
    candidates
        .into_iter()
        .map(|mut candidate| {
            candidate.origin_kind = PortableOriginKind::Compatibility;
            candidate.owned_by = PortableAssetOwner::Claude;
            candidate
        })
        .collect()
}

/// 解析 Claude `known_marketplaces.json` 的 installLocation，标成 Grok compatibility。
///
/// Business Logic（为什么需要这个函数）:
///     marketplace 克隆目录里的 Claude 插件对 Grok 也是运行时可见的借用资产。
///
/// Code Logic（这个函数做什么）:
///     复用 `parse_marketplace_install_locations`，再只收含 Claude manifest 的子目录。
fn claude_marketplace_plugin_roots_for_grok(claude_config_root: &Path) -> Vec<PluginRootCandidate> {
    let marketplace = claude_config_root
        .join("plugins")
        .join("known_marketplaces.json");
    crate::agent_hub::support::parse_marketplace_install_locations(&marketplace)
        .into_iter()
        .flat_map(|location| direct_manifest_plugin_roots(&location, AgentTarget::Claude))
        .map(|path| PluginRootCandidate {
            path,
            registry_plugin_id: None,
            registry_key: None,
            origin_kind: PortableOriginKind::Compatibility,
            owned_by: PortableAssetOwner::Claude,
        })
        .collect()
}

/// Claude 的 installed_plugins.json 是安装状态权威；cache/marketplaces/data 本身不是插件。
pub(super) fn claude_user_plugin_roots(config_root: &Path) -> Vec<PluginRootCandidate> {
    let plugins_root = config_root.join("plugins");
    let cache_root = plugins_root.join("cache");
    let mut roots = Vec::new();
    if let Ok(raw) = fs::read_to_string(plugins_root.join("installed_plugins.json")) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(plugins) = value.get("plugins").and_then(|v| v.as_object()) {
                for (full_key, installs) in plugins {
                    let registry_plugin_id = full_key
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
                            registry_key: Some(full_key.clone()),
                            origin_kind: PortableOriginKind::Native,
                            owned_by: PortableAssetOwner::Claude,
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
pub(super) fn codex_user_plugin_roots(config_root: &Path) -> Vec<PluginRootCandidate> {
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
                    registry_key: None,
                    origin_kind: PortableOriginKind::Native,
                    owned_by: PortableAssetOwner::Codex,
                });
            }
        }
    }
    roots
}

/// 只返回含目标 manifest 的一级插件目录，拒绝 cache/data/staging 等基础设施目录。
fn direct_manifest_plugin_roots(base: &Path, target: AgentTarget) -> Vec<PathBuf> {
    let manifest = match target {
        AgentTarget::Claude => ".claude-plugin/plugin.json",
        AgentTarget::Codex => ".codex-plugin/plugin.json",
        AgentTarget::OpenCode => "package.json",
        AgentTarget::Grok | AgentTarget::Gemini | AgentTarget::Cursor | AgentTarget::Pi => {
            "plugin.json"
        }
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
