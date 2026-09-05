//! portable_inventory/scanner/items — 发现项→inventory item 组装与能力判定
//!
//! Business Logic（为什么需要这个模块）:
//!     扫描得到的 `DiscoveredPortableAsset` 需要统一转换为 UI/动作消费的
//!     `PortableInventoryItemDto`（actualEnabled/origin/store/capability），
//!     并按 support manifest 与 origin 戳判定每个动作能否露出，
//!     避免借用视图改写所有者配置、unopted 项目露出写动作。
//!
//! Code Logic（这个模块做什么）:
//!     `discovered_to_item` 组装 item 并按 disabled-wins 去重；store 事实推导与仓库目录
//!     补注（store_fact_for_discovery / inject_store_catalog_items / annotate_*）；
//!     逐动作能力判定（item_capabilities / mutation_gates_for_origin / action_*）；
//!     MCP credential hash、plugin parent 推断与 current_target_environment 构造。

use super::PortableScanScope;
use crate::agent_hub::{
    assets::{McpTransport, PortableAssetPayload},
    models::{AgentTarget, AssetKind, ScopeKind},
    object_store::sha256_hex,
    portable_actions::{
        models::PortableAssetActionKind,
        targets::{is_file_only_viewing_toggle, supports_direct_local_action},
    },
    portable_inventory::models::{
        inventory_item_id, PortableAssetKind, PortableInventoryItemCapabilitiesDto,
        PortableInventoryItemDto, PortableInventoryManagementState,
        PortableInventoryMutationCapability, PortableInventorySourceOrigin,
        PortableInventoryTargetDto, PortableMcpCredentialFactDto, PortableStoreFactDto,
    },
    portable_store::{
        classify_store_link_with_ancestors, is_under_portable_store, store_command_file,
        store_id_for, store_id_from_canonical, store_skill_dir, try_portable_store_root_for_scope,
        validate_store_native_id, PortableStoreKind, StoreLinkClass,
    },
    support::{CapabilitySupport, EvaluatedTargetSupport, TargetCapability},
    targets::{
        portable::{
            is_borrowed_runtime_origin, mutation_target_for_action, mutation_target_for_origin,
            DiscoveredPortableAsset, PortableAssetOwner, PortableDiscoveryStatus,
            PortableOriginKind,
        },
        TargetEnvironment,
    },
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[allow(clippy::too_many_arguments)] // 内部 helper：kind/disc/scope/target/evaluated/seen/items 7 段语义独立
pub(super) fn discovered_to_item(
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

    let (owned_by, store) = store_fact_for_discovery(disc, scope);
    let actual_enabled = store_catalog_enabled(&store, actual_enabled_for(kind, disc));
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
    if store.loaded_via_other_path {
        warnings.push("store_loaded_via_other_path".into());
    }

    let mcp_credential = if kind == PortableAssetKind::Mcp {
        Some(mcp_credential_fact(disc))
    } else {
        None
    };

    let can_mutate_scope = scope.project_opted_in;
    let (can_enable, can_disable, can_uninstall, enablement_target, uninstall_target) =
        mutation_gates_for_origin(
            disc.origin.target,
            owned_by,
            disc.origin.native_output_candidate,
            disc.origin.origin_kind,
            kind,
            evaluated,
            can_mutate_scope,
        );
    let borrowed = is_borrowed_runtime_origin(
        disc.origin.target,
        owned_by,
        disc.origin.native_output_candidate,
        disc.origin.origin_kind,
    );
    let enable_semantics = enable_semantics_supported(kind, enablement_target);
    let reason = if !scope.project_opted_in && scope.scope_kind != ScopeKind::User {
        Some("project_not_opted_in".into())
    } else if borrowed {
        None
    } else if !can_enable || !can_disable {
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

    let mut item = PortableInventoryItemDto {
        inventory_item_id: inv_id.clone(),
        target: disc.origin.target,
        loaded_by: disc.origin.target,
        owned_by,
        origin_kind: disc.origin.origin_kind,
        native_output_candidate: disc.origin.native_output_candidate,
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
            enablement_target,
            uninstall_target,
            kind,
            actual_enabled,
            can_enable,
            can_disable,
            can_uninstall,
            enable_semantics,
            reason,
            borrowed,
            disc.origin.origin_kind,
            disc.origin.native_output_candidate,
            &store,
            kind,
        ),
        warnings,
        mcp_credential,
        store,
    };
    apply_escape_link_repair_capability(&mut item);
    apply_unopted_readonly_store_caps(&mut item, can_mutate_scope);

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
pub(super) fn should_replace_with(
    new_item: &PortableInventoryItemDto,
    existing: &PortableInventoryItemDto,
) -> bool {
    if let (Some(new_id), Some(old_id)) = (&new_item.store.store_id, &existing.store.store_id) {
        if new_id == old_id {
            if new_item.store.store_attached && !existing.store.store_attached {
                return true;
            }
            // 本 Agent 真正挂上的 native 软链可以盖过兼容根；未附加的仓库目录注入不能盖。
            if new_item.origin_kind == PortableOriginKind::Native
                && existing.origin_kind != PortableOriginKind::Native
                && new_item.store.store_attached
            {
                return true;
            }
            return false;
        }
    }
    if new_item.store.store_id.is_some() && !new_item.store.store_attached {
        return false;
    }
    if new_item.actual_enabled == Some(false) && existing.actual_enabled != Some(false) {
        return true;
    }
    false
}

/// 本机真树若与 portable-store 同 native_id 且身份内容相同，视为已在仓库中的重复挂载。
///
/// Business Logic: 卸下 CODEX_HOME 软链后，~/.agents 同内容真树不应再显示「迁入便携仓库」。
///     列表扫描不得为判定 leftover 而递归哈希大型 Skill 树。
/// Code Logic: Skill 比对 SKILL.md 内容 hash；Command 比对文件 content hash。
fn leftover_duplicate_store_id(
    disc: &DiscoveredPortableAsset,
    store_root: &Path,
) -> Option<String> {
    let store_kind = match disc.kind {
        AssetKind::Skill => PortableStoreKind::Skill,
        AssetKind::Command => PortableStoreKind::Command,
        _ => return None,
    };
    let store_target = match store_kind {
        PortableStoreKind::Skill => store_skill_dir(store_root, &disc.origin.native_id),
        PortableStoreKind::Command => store_command_file(store_root, &disc.origin.native_id),
        PortableStoreKind::Mcp => return None,
    };
    let first_level_same = match store_kind {
        PortableStoreKind::Skill => {
            let store_md = store_target.join("SKILL.md");
            fs::read(&store_md)
                .ok()
                .is_some_and(|bytes| sha256_hex(&bytes) == disc.origin.content_hash)
        }
        PortableStoreKind::Command => fs::read(&store_target)
            .ok()
            .is_some_and(|bytes| sha256_hex(&bytes) == disc.origin.content_hash),
        PortableStoreKind::Mcp => false,
    };
    if first_level_same {
        return Some(store_id_for(store_kind, &disc.origin.native_id));
    }
    leftover_nested_package_id(disc, store_root)
        .map(|package_id| store_id_for(PortableStoreKind::Skill, &package_id))
}

fn leftover_nested_package_id(disc: &DiscoveredPortableAsset, store_root: &Path) -> Option<String> {
    if disc.kind != AssetKind::Skill {
        return None;
    }
    let parent = disc.origin.path.parent()?;
    let parent_name = parent.file_name()?.to_str()?;
    if parent_name.starts_with('.') {
        return None;
    }
    let (package_id, nested) = if parent_name == "skills" {
        let package_id = parent.parent()?.file_name()?.to_str()?;
        (
            package_id,
            store_skill_dir(store_root, package_id)
                .join("skills")
                .join(&disc.origin.native_id),
        )
    } else {
        (
            parent_name,
            store_skill_dir(store_root, parent_name).join(&disc.origin.native_id),
        )
    };
    if package_id.starts_with('.') {
        return None;
    }
    validate_store_native_id(package_id).ok()?;
    let store_hash = fs::read(nested.join("SKILL.md"))
        .ok()
        .map(|bytes| sha256_hex(&bytes))?;
    (store_hash == disc.origin.content_hash).then(|| package_id.to_string())
}

/// 从发现路径推导 store 事实与所有者。
///
/// Business Logic: store 软链归 portableStore；兼容根上的同一 storeId 只标「仍被其他路径加载」。
/// Code Logic: classify_store_link；兼容路径 → loaded_via_other_path；
///     native/legacyStandalone 软链算本 Agent 已附加。
fn store_fact_for_discovery(
    disc: &DiscoveredPortableAsset,
    scope: &PortableScanScope,
) -> (PortableAssetOwner, PortableStoreFactDto) {
    let path = &disc.origin.path;
    let scope_store = store_root_for_scan_scope(scope);
    match classify_store_link_with_ancestors(path) {
        StoreLinkClass::StoreLink {
            store_id,
            canonical,
            ..
        } => {
            let belongs = scope_store
                .as_ref()
                .is_some_and(|root| is_under_portable_store(&canonical, root));
            if !belongs {
                return (disc.origin.owned_by, PortableStoreFactDto::default());
            }
            let via = disc.origin.owned_by.as_hub_target();
            // Native 与 Codex 自有 legacy 根上的 store 软链都是本 Agent 附加；
            // 不得要求 native_output_candidate（legacyStandalone 恒为 false）。
            // Grok 等把 ~/.agents 当运行时根：包根软链也算已附加，才能在本 Agent 卸下/删除。
            let store_attached = matches!(
                disc.origin.origin_kind,
                PortableOriginKind::Native | PortableOriginKind::LegacyStandalone
            ) || is_shared_agents_runtime_path(&disc.origin.path);
            (
                PortableAssetOwner::PortableStore,
                PortableStoreFactDto {
                    store_id: Some(store_id),
                    store_attached,
                    loaded_via_other_path: !store_attached,
                    loaded_via_target: if store_attached { None } else { via },
                },
            )
        }
        StoreLinkClass::Regular => {
            let Some(store_root) = scope_store else {
                return (disc.origin.owned_by, PortableStoreFactDto::default());
            };
            if let Ok(canonical) = std::fs::canonicalize(path) {
                if let Some(store_id) = store_id_from_canonical(&canonical, &store_root) {
                    return (
                        PortableAssetOwner::PortableStore,
                        PortableStoreFactDto {
                            store_id: Some(store_id),
                            store_attached: false,
                            loaded_via_other_path: false,
                            loaded_via_target: None,
                        },
                    );
                }
                if let Some(store_id) = leftover_duplicate_store_id(disc, &store_root) {
                    // ~/.agents 等兼容根上的同内容真树仍会被 Codex 加载，应显示「卸下」而不是「再迁入」。
                    let attached = matches!(
                        disc.origin.origin_kind,
                        PortableOriginKind::Native | PortableOriginKind::LegacyStandalone
                    ) || is_shared_agents_runtime_path(&disc.origin.path);
                    return (
                        PortableAssetOwner::PortableStore,
                        PortableStoreFactDto {
                            store_id: Some(store_id),
                            store_attached: attached,
                            loaded_via_other_path: !attached,
                            loaded_via_target: if attached {
                                None
                            } else {
                                disc.origin.owned_by.as_hub_target()
                            },
                        },
                    );
                }
            }
            (disc.origin.owned_by, PortableStoreFactDto::default())
        }
        StoreLinkClass::EscapeLink => (disc.origin.owned_by, PortableStoreFactDto::default()),
    }
}

/// 当前扫描 scope 对应的仓库根；项目级不回退用户库。
fn store_root_for_scan_scope(scope: &PortableScanScope) -> Option<PathBuf> {
    let data = crate::config::data_dir().ok()?;
    try_portable_store_root_for_scope(&data, scope.scope_kind, scope.project_id.as_deref())
}

/// 把对应 scope 的 store 目录里尚未出现在该 target 盘点中的资产补进列表。
///
/// Business Logic: 卸下后真树仍在，UI 还要能附加/彻底删除。用户级扫用户库，项目级扫项目库。
/// Code Logic: 扫描该 scope 的 portable-store/skills|commands，再走 discovered_to_item 去重。
///     Skill 列表查询与 adapter 一样只读 SKILL.md，目录树延迟到动作 preview；
///     完整扫描仍算 tree hash，与 `scan_portable_assets` 对齐。
pub(super) fn inject_store_catalog_items(
    target: AgentTarget,
    scope: &PortableScanScope,
    target_dto: &PortableInventoryTargetDto,
    evaluated: &EvaluatedTargetSupport,
    kind_filter: Option<PortableAssetKind>,
    seen: &mut BTreeMap<String, usize>,
    items: &mut Vec<PortableInventoryItemDto>,
) {
    let Some(store_root) = store_root_for_scan_scope(scope) else {
        return;
    };
    if !store_root.is_dir() {
        return;
    }
    if kind_filter.is_none() || kind_filter == Some(PortableAssetKind::Skill) {
        let skills_root = store_root.join("skills");
        let discs = if kind_filter == Some(PortableAssetKind::Skill) {
            crate::agent_hub::targets::portable::scan_skill_dirs_manifest_only(
                target,
                scope.scope_kind,
                &skills_root,
                PortableOriginKind::Native,
            )
        } else {
            crate::agent_hub::targets::portable::scan_skill_dirs(
                target,
                scope.scope_kind,
                &skills_root,
                PortableOriginKind::Native,
            )
        };
        if let Ok(discs) = discs {
            for disc in discs {
                let inv_id = inventory_item_id(
                    target,
                    &scope.scope_id,
                    "standalone",
                    &disc.origin.native_id,
                );
                if seen.contains_key(&inv_id) {
                    continue;
                }
                discovered_to_item(
                    PortableAssetKind::Skill,
                    &disc,
                    scope,
                    target_dto,
                    evaluated,
                    seen,
                    items,
                );
            }
        }
    }
    if kind_filter.is_none() || kind_filter == Some(PortableAssetKind::Command) {
        if let Ok(discs) = crate::agent_hub::targets::portable::scan_command_markdown_dir(
            target,
            scope.scope_kind,
            &store_root.join("commands"),
            PortableOriginKind::Native,
        ) {
            for disc in discs {
                let inv_id = inventory_item_id(
                    target,
                    &scope.scope_id,
                    "standalone",
                    &disc.origin.native_id,
                );
                if seen.contains_key(&inv_id) {
                    continue;
                }
                discovered_to_item(
                    PortableAssetKind::Command,
                    &disc,
                    scope,
                    target_dto,
                    evaluated,
                    seen,
                    items,
                );
            }
        }
    }
}

/// 同一 storeId 若本 Agent 未附加、但兼容路径仍在，保留「仍被其他路径加载」。
///
/// Business Logic: 卸 Grok 不得为了列表干净去改 Claude。
/// Code Logic: 按 (target, storeId) 看是否已有 store_attached；否则标 hint。
pub(super) fn annotate_store_loaded_via_other_path(items: &mut [PortableInventoryItemDto]) {
    let mut attached: BTreeMap<(AgentTarget, String), bool> = BTreeMap::new();
    for item in items.iter() {
        if let Some(id) = item.store.store_id.as_deref() {
            let key = (item.target, id.to_string());
            let flag = attached.entry(key).or_insert(false);
            *flag |= item.store.store_attached;
        }
    }
    for item in items.iter_mut() {
        let Some(id) = item.store.store_id.clone() else {
            continue;
        };
        let attached_here = attached.get(&(item.target, id)).copied().unwrap_or(false);
        if !attached_here
            && (item.origin_kind == PortableOriginKind::Compatibility
                || item.store.loaded_via_other_path)
        {
            item.store.store_attached = false;
            item.store.loaded_via_other_path = true;
            let agents_shared = item
                .source_path
                .as_deref()
                .is_some_and(|p| p.replace('\\', "/").contains("/.agents/"));
            if agents_shared {
                item.store.loaded_via_target = None;
            } else if item.store.loaded_via_target.is_none() {
                item.store.loaded_via_target = infer_store_loaded_via_target(item);
            }
            if !item
                .warnings
                .iter()
                .any(|w| w == "store_loaded_via_other_path")
            {
                item.warnings.push("store_loaded_via_other_path".into());
            }
        }
    }
}

/// `~/.agents` 是 Codex 的技能安装根，也是 Grok/Cursor/Gemini 等官方会加载的共享根。
fn is_shared_agents_runtime_path(path: &std::path::Path) -> bool {
    path.to_string_lossy()
        .replace('\\', "/")
        .contains("/.agents/")
}

/// 从观测路径推断「仍被谁的目录加载」；禁止在不知道时默认 Claude。
///
/// Business Logic: Grok 会扫 `~/.agents` 与 `~/.claude/skills`。前者是共享根，
///     Claude 并不加载；默认 Claude 会让 superpowers 这类包谎称「来自 Claude Code」。
/// Code Logic: 有 Hub ownedBy 且不是当前 target 则用之；否则看 source_path。
///     `/.agents/` → None（共享根）；`/.claude/` → Claude；其它配置根同理。
fn infer_store_loaded_via_target(item: &PortableInventoryItemDto) -> Option<AgentTarget> {
    let path = item.source_path.as_deref().map(|p| p.replace('\\', "/"));
    if path.as_deref().is_some_and(|p| p.contains("/.agents/")) {
        return None;
    }
    if let Some(owner) = item.owned_by.as_hub_target() {
        if owner != item.target {
            return Some(owner);
        }
    }
    let path = path?;
    if path.contains("/.claude/") {
        return Some(AgentTarget::Claude);
    }
    if path.contains("/.codex/") {
        return Some(AgentTarget::Codex);
    }
    if path.contains("/.grok/") {
        return Some(AgentTarget::Grok);
    }
    if path.contains("/.gemini/") {
        return Some(AgentTarget::Gemini);
    }
    if path.contains("/.cursor/") {
        return Some(AgentTarget::Cursor);
    }
    if path.contains("/.pi/") || path.contains("/.pi-coding/") {
        return Some(AgentTarget::Pi);
    }
    if path.contains("/.config/opencode/") || path.contains("/.opencode/") {
        return Some(AgentTarget::OpenCode);
    }
    None
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

/// 未挂到当前 Agent 的仓库目录项不能标成已启用。
///
/// Business Logic: 卸下后真树仍在，inject 会把它当 Native Active 扫回来；
///     那不是当前 Agent 的加载路径，不得让 detach 回扫描到 enabled=true。
/// Code Logic: 有 storeId、未附加、也不是「仍被其他路径加载」→ Some(false)。
pub(super) fn store_catalog_enabled(
    store: &PortableStoreFactDto,
    discovered: Option<bool>,
) -> Option<bool> {
    if store.store_id.is_some() && !store.store_attached && !store.loaded_via_other_path {
        Some(false)
    } else {
        discovered
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
pub(super) fn mutation_capability_reason(target: &PortableInventoryTargetDto) -> Option<String> {
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
        PortableAssetActionKind::ConfirmCurrentVersion
        | PortableAssetActionKind::MaterializeEscapeLink => None,
        PortableAssetActionKind::Attach
        | PortableAssetActionKind::Detach
        | PortableAssetActionKind::DestroyStore
        | PortableAssetActionKind::MigrateToStore => {
            if kind.supports_portable_store() {
                Some(TargetCapability::RenderPortableAssets)
            } else {
                None
            }
        }
    }
}

/// 判断已求值的单项写能力能否立即执行。
pub(super) fn action_support_is_ready(
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
pub(super) fn action_capability_supported(
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
pub(super) fn action_capability_reason(
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

#[allow(clippy::too_many_arguments)] // origin 戳与 mutation 开关必须同函数强制 borrowed 能力
pub(super) fn item_capabilities(
    enablement_target: AgentTarget,
    uninstall_target: AgentTarget,
    kind: PortableAssetKind,
    actual_enabled: Option<bool>,
    can_enable_mutation: bool,
    can_disable_mutation: bool,
    can_uninstall_mutation: bool,
    enable_semantics: bool,
    reason: Option<String>,
    borrowed: bool,
    origin_kind: PortableOriginKind,
    _native_output_candidate: bool,
    store: &PortableStoreFactDto,
    _kind_for_store: PortableAssetKind,
) -> PortableInventoryItemCapabilitiesDto {
    let store_kind = kind.supports_portable_store();
    let store_write = store_kind
        && supports_direct_local_action(enablement_target, kind, PortableAssetActionKind::Attach);
    let borrowed_store_runtime = borrowed
        && store.store_id.is_some()
        && !store.store_attached
        && (store.loaded_via_other_path || origin_kind == PortableOriginKind::Compatibility);
    // Skill/Command 生命周期改走仓库：迁入 / 附加 / 卸下 / 彻底删除。
    // 启停与卸载只留给 Plugin（viewing 开关）和 MCP（各家配置 leaf）。
    let mut can_toggle_enable =
        !store_kind && can_enable_mutation && enable_semantics && actual_enabled.is_some();
    let mut can_toggle_disable =
        !store_kind && can_disable_mutation && enable_semantics && actual_enabled.is_some();
    // 借用 MCP 仍是所有者配置 leaf；不得在借用视图上启停/卸载，以免改写所有者 Claude 配置。
    if borrowed && kind == PortableAssetKind::Mcp {
        can_toggle_enable = false;
        can_toggle_disable = false;
    }
    let can_enable = can_toggle_enable
        && actual_enabled == Some(false)
        && supports_direct_local_action(enablement_target, kind, PortableAssetActionKind::Enable);
    let can_disable = can_toggle_disable
        && actual_enabled == Some(true)
        && supports_direct_local_action(enablement_target, kind, PortableAssetActionKind::Disable);
    let mut capabilities = PortableInventoryItemCapabilitiesDto {
        can_enable,
        can_disable,
        can_uninstall: !store_kind
            && can_uninstall_mutation
            && !(borrowed && kind == PortableAssetKind::Mcp)
            && supports_direct_local_action(
                uninstall_target,
                kind,
                PortableAssetActionKind::Uninstall,
            ),
        // Adopt ownership write is not wired (PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED).
        // Never advertise canAdopt=true — UI prioritizes Adopt as primary action otherwise.
        can_adopt: false,
        can_install_to_source_target: false,
        // 只迁本 Agent 自己的 native / Codex ~/.agents（legacyStandalone）。
        // Grok/Pi 等运行时从其他 Agent 加载的 compatibility 项不得迁入。
        // Plugin 组件真树仍在包内，不得单独迁入。
        can_migrate_to_store: store_write
            && store.store_id.is_none()
            && matches!(
                origin_kind,
                PortableOriginKind::Native | PortableOriginKind::LegacyStandalone
            )
            && !(borrowed && origin_kind != PortableOriginKind::LegacyStandalone),
        // 运行时从其他 Agent 加载的仓库项已经在用源软链，不必再给当前 Agent 附加一份；
        // 借用项也不得卸下，避免拆掉源 Agent 上的软链。
        can_attach: store_write
            && store.store_id.is_some()
            && !store.store_attached
            && !borrowed_store_runtime,
        can_detach: store_write
            && store.store_id.is_some()
            && store.store_attached
            && !borrowed_store_runtime,
        can_destroy_store: store_write && store.store_id.is_some() && !borrowed_store_runtime,
        can_confirm_current_version: false,
        can_materialize_escape_link: false,
        reason_code: reason,
        evidence_ids: vec![format!("L2-PORTABLE-{}-SCAN", kind.as_str().to_uppercase())],
    };
    if borrowed {
        // plugin 启停跟当前 Agent；卸载/技能移动仍跟所有者。reason 只作 UI 提示。
        capabilities.reason_code = Some("borrowed_runtime_origin".into());
    }
    capabilities
}

/// 逃逸软链只能解引，不能同时暴露启停/迁入。
///
/// Business Logic: `store_symlink_escape` 的 Skill/Command 必须给用户一条修复路；
///     Enable 会竞争主按钮且写盘语义不成立。
/// Code Logic: 独立/非 plugin 组件命中 warning 后打开 `can_materialize_escape_link` 并关掉其它 mutation。
fn apply_escape_link_repair_capability(item: &mut PortableInventoryItemDto) {
    let is_escape = matches!(
        item.kind,
        PortableAssetKind::Skill | PortableAssetKind::Command
    ) && item.source_origin != PortableInventorySourceOrigin::PluginComponent
        && item
            .warnings
            .iter()
            .any(|warning| warning == "store_symlink_escape");
    if !is_escape {
        item.capabilities.can_materialize_escape_link = false;
        return;
    }
    item.capabilities.can_materialize_escape_link = true;
    item.capabilities.can_enable = false;
    item.capabilities.can_disable = false;
    item.capabilities.can_uninstall = false;
    item.capabilities.can_migrate_to_store = false;
    item.capabilities.can_attach = false;
    item.capabilities.can_detach = false;
    item.capabilities.can_destroy_store = false;
    item.capabilities.can_install_to_source_target = false;
}

/// 未 opt-in 项目只读：仓库动作同样不得露出。
///
/// Business Logic: 项目未接入 Hub 时不得迁入/附加/卸下/销毁仓库真树。
/// Code Logic: `can_mutate_scope=false` 时关掉全部 store mutation 旗标。
pub(super) fn apply_unopted_readonly_store_caps(
    item: &mut PortableInventoryItemDto,
    can_mutate_scope: bool,
) {
    if can_mutate_scope {
        return;
    }
    item.capabilities.can_migrate_to_store = false;
    item.capabilities.can_attach = false;
    item.capabilities.can_detach = false;
    item.capabilities.can_destroy_store = false;
}

/// 按 origin 决定写盘门闩：plugin 启停看当前 Agent，卸载/技能移动看所有者。
///
/// Business Logic（为什么需要这个函数）:
///     每个 Agent 有自己的 plugin 开关；借用包不得拿所有者标记当当前 Agent 已关。
///     卸载仍改所有者磁盘。Skill/Command 迁入仓库仍走所有者目录。
///
/// Code Logic（这个函数做什么）:
///     Plugin 借用项：Enable/Disable → viewing allowlist；Uninstall → owner allowlist。
///     其余借用项仍走 owner direct-local-action。
pub(super) fn mutation_gates_for_origin(
    viewing: AgentTarget,
    owned_by: PortableAssetOwner,
    native_output_candidate: bool,
    origin_kind: PortableOriginKind,
    kind: PortableAssetKind,
    evaluated: &EvaluatedTargetSupport,
    can_mutate_scope: bool,
) -> (bool, bool, bool, AgentTarget, AgentTarget) {
    let owner_target = mutation_target_for_origin(viewing, owned_by, native_output_candidate);
    let enablement_target = mutation_target_for_action(
        viewing,
        owned_by,
        native_output_candidate,
        kind.to_asset_kind(),
        true,
    );
    let borrowed =
        is_borrowed_runtime_origin(viewing, owned_by, native_output_candidate, origin_kind);
    if borrowed {
        let plugin_enablement = kind == PortableAssetKind::Plugin;
        let enable_target = if plugin_enablement {
            viewing
        } else {
            owner_target
        };
        (
            can_mutate_scope
                && supports_direct_local_action(
                    enable_target,
                    kind,
                    PortableAssetActionKind::Enable,
                ),
            can_mutate_scope
                && supports_direct_local_action(
                    enable_target,
                    kind,
                    PortableAssetActionKind::Disable,
                ),
            can_mutate_scope
                && supports_direct_local_action(
                    owner_target,
                    kind,
                    PortableAssetActionKind::Uninstall,
                ),
            enablement_target,
            owner_target,
        )
    } else {
        (
            can_mutate_scope
                && file_or_certified_action(
                    evaluated,
                    viewing,
                    kind,
                    PortableAssetActionKind::Enable,
                ),
            can_mutate_scope
                && file_or_certified_action(
                    evaluated,
                    viewing,
                    kind,
                    PortableAssetActionKind::Disable,
                ),
            can_mutate_scope
                && file_or_certified_action(
                    evaluated,
                    viewing,
                    kind,
                    PortableAssetActionKind::Uninstall,
                ),
            enablement_target,
            owner_target,
        )
    }
}

/// 纯文件 viewing 开关不走 Activate/Deactivate 认证；其余仍要 support manifest。
fn file_or_certified_action(
    evaluated: &EvaluatedTargetSupport,
    target: AgentTarget,
    kind: PortableAssetKind,
    action: PortableAssetActionKind,
) -> bool {
    if is_file_only_viewing_toggle(target, kind, action) {
        return supports_direct_local_action(target, kind, action);
    }
    action_capability_supported(evaluated, target, kind, action)
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
pub(super) fn current_target_environment() -> TargetEnvironment {
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
    let path_entries = crate::agent_hub::targets::paths::gui_augmented_path_entries(
        &home,
        std::env::var_os("PATH").as_deref(),
    );
    TargetEnvironment {
        home,
        vars,
        path_entries,
    }
}
