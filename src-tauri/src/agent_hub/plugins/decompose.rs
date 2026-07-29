//! agent_hub/plugins/decompose — 目标 Plugin 清单检视与 import
//!
//! Business Logic（为什么需要这个模块）:
//!     Plugin 不是最低同步单位：须拆成 Skill/MCP/Command/Agent/Hook 与 residual runtime，
//!     在 import 前给出精确 child 逻辑键、hash、ownership 与可移植性预览。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `PluginDecomposer`、`DiscoveredPluginSource` 与 preview/import 类型；
//!     复用 Gate B portable 扫描；未知 manifest 字段进 target_extensions；
//!     未知 runtime 文件进 residual CAS 树；Hook 默认 TargetOnly 无 mapping。

use crate::agent_hub::assets::{PortableAssetPayload, PortableSkill};
use crate::agent_hub::models::{
    AgentTarget, AssetKind, AssetPolicy, LogicalAsset, NewLogicalAsset, Revision,
    RevisionOriginKind, ScopeKind,
};
use crate::agent_hub::object_store::{ObjectStore, TreeEntry, TreeEntryType, TreeManifest};
use crate::agent_hub::plugins::models::{
    ensure_component_kind_allowed, sort_plugin_package_payload, validate_plugin_package_payload,
    validate_portable_hook, ComponentOwnership, HookEventIntent, PluginComponentRef,
    PluginPackagePayload, PluginResidualRef, PortableHook, ResidualKind,
};
use crate::agent_hub::snapshot::envelope::default_snapshot_limits;
use crate::agent_hub::targets::portable::{
    parse_json_or_jsonc, parse_mcp_servers_json_map, parse_simple_frontmatter,
    scan_agent_markdown_dir, scan_command_markdown_dir, scan_skill_dirs, DiscoveredPortableAsset,
    PortableOriginKind,
};
use crate::error::AppError;
use crate::storage::agent_hub_repo::AgentHubRepo;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 磁盘上发现的 Plugin 源（尚未 import）。
///
/// Business Logic（为什么需要这个结构体）:
///     decomposer 需要 root 路径、source target 与 scope 才能扫描 component / residual。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；携带 plugin 身份字段与绝对 root。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredPluginSource {
    /// 稳定 plugin id（manifest name 或目录名）
    pub plugin_id: String,
    /// 展示名
    pub name: String,
    /// 可选版本
    pub version: Option<String>,
    /// 可选描述
    pub description: Option<String>,
    /// 来源 CLI
    pub source_target: AgentTarget,
    /// Plugin 根目录绝对路径
    pub root_path: PathBuf,
    /// Hub scope id
    pub scope_id: String,
    /// scope 种类（扫描 portable 用）
    pub scope_kind: ScopeKind,
}

/// child component 可移植性状态。
///
/// Business Logic（为什么需要这个枚举）:
///     preview 必须区分 portable / Hook targetOnly / residual 源文件。
///
/// Code Logic（这个枚举做什么）:
///     camelCase。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComponentPortability {
    /// 可跨 target 投影的 portable component
    Portable,
    /// 默认仅回来源 target（Hook 无 mapping）
    TargetOnly,
    /// 仅 residual 保留原字节（不作为 typed component）
    SourceResidual,
}

impl ComponentPortability {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Portable => "portable",
            Self::TargetOnly => "targetOnly",
            Self::SourceResidual => "sourceResidual",
        }
    }
}

/// 预览中的 child 载荷形态。
///
/// Business Logic: preview 须展示精确 payload 形态，import 前可确认。
/// Code Logic: Portable 资产或 Hook。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ComponentPayloadPreview {
    /// Gate B portable payload
    Portable {
        /// typed payload
        payload: PortableAssetPayload,
    },
    /// Hook（默认 targetOnly）
    Hook {
        /// PortableHook
        hook: PortableHook,
    },
}

/// 单个 component 的分解预览。
///
/// Business Logic（为什么需要这个结构体）:
///     import 前 UI/测试需看到 logical key、hash、ownership 与可移植性。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 聚合字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentPreview {
    /// component AssetKind
    pub kind: AssetKind,
    /// 作用域内逻辑键（semantic name）
    pub logical_key: String,
    /// 展示名
    pub display_name: String,
    /// 预估 ownership（import 时若链接 standalone 则 Standalone）
    pub ownership: ComponentOwnership,
    /// 可移植性
    pub portability: ComponentPortability,
    /// 载荷预览
    pub payload: ComponentPayloadPreview,
    /// 主内容 hash（可选）
    pub content_hash: Option<String>,
    /// tree hash（Skill / hook command tree）
    pub tree_hash: Option<String>,
    /// 源相对路径（诊断）
    pub source_relative_path: String,
}

/// residual 预览。
///
/// Business Logic: runtime 文件必须原字节进入 CAS 并钉住 tree hash。
/// Code Logic: residual kind + target + tree hash + 相对路径列表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResidualPreview {
    /// residual 类别
    pub residual_kind: ResidualKind,
    /// 所属 target
    pub target: AgentTarget,
    /// TreeManifest hash（已写入 ObjectStore）
    pub tree_manifest_hash: String,
    /// 相对路径清单（排序）
    pub relative_paths: Vec<String>,
}

/// Plugin 分解预览（inspect 产物）。
///
/// Business Logic（为什么需要这个结构体）:
///     用户确认前必须看到完整 component/residual 矩阵；import 不得静默改写语义。
///
/// Code Logic（这个结构体做什么）:
///     聚合 metadata + components + residuals + target_extensions。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDecompositionPreview {
    /// plugin id
    pub plugin_id: String,
    /// 展示名
    pub name: String,
    /// 版本
    pub version: Option<String>,
    /// 描述
    pub description: Option<String>,
    /// 来源 target
    pub source_target: AgentTarget,
    /// scope
    pub scope_id: String,
    /// scope 种类
    pub scope_kind: ScopeKind,
    /// 源 root
    pub root_path: PathBuf,
    /// child previews
    pub components: Vec<ComponentPreview>,
    /// residual previews
    pub residuals: Vec<ResidualPreview>,
    /// 未知 manifest 字段等
    pub target_extensions: BTreeMap<AgentTarget, Value>,
}

/// 用户确认后的 import 请求。
///
/// Business Logic（为什么需要这个结构体）:
///     可将 child 链接到已确认 standalone；否则 `originNamespace=plugin:<pluginId>`。
///
/// Code Logic（这个结构体做什么）:
///     preview + standalone 链接 map + origin 元数据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmedPluginDecomposition {
    /// inspect 预览
    pub preview: PluginDecompositionPreview,
    /// logical_key → 已存在 standalone asset_id（同 kind）
    #[serde(default)]
    pub link_standalone: BTreeMap<String, String>,
    /// origin replica id
    pub origin_replica_id: String,
}

/// import 结果：package 资产与 revision。
///
/// Business Logic: 调用方需要 package head 与固定 component refs。
/// Code Logic: package asset + revision + payload。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginPackageRevision {
    /// Plugin 逻辑资产
    pub package_asset: LogicalAsset,
    /// package revision
    pub revision: Revision,
    /// 固定 refs payload
    pub payload: PluginPackagePayload,
    /// 创建/链接的 component 资产 id（kind:logical_key → asset_id）
    pub component_asset_ids: BTreeMap<String, String>,
}

/// Plugin 分解器合同。
///
/// Business Logic（为什么需要这个 trait）:
///     inspect 只读 CAS/磁盘；import 写 Hub revision + 边表。
///
/// Code Logic（这个 trait 做什么）:
///     inspect / import 两个阶段。
pub trait PluginDecomposer {
    /// 检视源 Plugin：portable → typed child；Hook → targetOnly；runtime → residual。
    ///
    /// Business Logic: preview 必须在 import 前给出精确 hash 与 ownership 状态。
    /// Code Logic: 扫描 root，将 residual/skill 树写入 ObjectStore。
    fn inspect(
        &self,
        source: &DiscoveredPluginSource,
        objects: &ObjectStore,
    ) -> impl std::future::Future<Output = Result<PluginDecompositionPreview, AppError>> + Send;

    /// 确认后 import：创建 child assets（namespace plugin:<id> 或链接 standalone）与 package revision。
    ///
    /// Business Logic: component 固定 revision ref；不改写旧 package。
    /// Code Logic: 事务经 repo API 写入。
    fn import(
        &self,
        preview: ConfirmedPluginDecomposition,
    ) -> impl std::future::Future<Output = Result<PluginPackageRevision, AppError>> + Send;
}

/// 默认分解器：持有 repo 句柄。
///
/// Business Logic: production import 需要 AgentHubRepo + ObjectStore。
/// Code Logic: Arc 共享。
#[derive(Clone)]
pub struct DefaultPluginDecomposer {
    /// 仓储
    pub repo: Arc<AgentHubRepo>,
    /// CAS
    pub store: Arc<ObjectStore>,
}

impl DefaultPluginDecomposer {
    /// 构造。
    ///
    /// Business Logic: service 层注入 repo/store。
    /// Code Logic: Arc wrap。
    pub fn new(repo: Arc<AgentHubRepo>, store: Arc<ObjectStore>) -> Self {
        Self { repo, store }
    }
}

impl PluginDecomposer for DefaultPluginDecomposer {
    async fn inspect(
        &self,
        source: &DiscoveredPluginSource,
        objects: &ObjectStore,
    ) -> Result<PluginDecompositionPreview, AppError> {
        inspect_plugin_source(source, objects).await
    }

    async fn import(
        &self,
        confirmed: ConfirmedPluginDecomposition,
    ) -> Result<PluginPackageRevision, AppError> {
        import_confirmed(
            self.repo.as_ref(),
            self.store.as_ref(),
            confirmed,
            RevisionOriginKind::Filesystem,
        )
        .await
    }
}

/// 按 target 从磁盘 root 构造 `DiscoveredPluginSource`（不扫描 component）。
///
/// Business Logic（为什么需要这个函数）:
///     target adapter 只负责定位 Plugin 根与元数据；分解在 `inspect_plugin_source`。
///
/// Code Logic（这个函数做什么）:
///     Claude/Codex 读 `.*-plugin/plugin.json`；OpenCode 读 `package.json`；缺省用目录名。
pub fn discover_plugin_source_for_target(
    target: AgentTarget,
    root: &Path,
    scope_id: impl Into<String>,
    scope_kind: ScopeKind,
) -> Result<DiscoveredPluginSource, AppError> {
    if !root.is_dir() {
        return Err(AppError::validation(format!(
            "agent_hub_plugin_root_not_dir:{}",
            root.display()
        )));
    }
    let dir_name = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("plugin")
        .to_string();
    let mut plugin_id = dir_name.clone();
    let mut name = dir_name;
    let mut version = None;
    let mut description = None;
    let manifest_rel = match target {
        AgentTarget::Claude => Some(".claude-plugin/plugin.json"),
        AgentTarget::Codex => Some(".codex-plugin/plugin.json"),
        AgentTarget::OpenCode => Some("package.json"),
    };
    if let Some(rel) = manifest_rel {
        if let Ok(text) = fs::read_to_string(root.join(rel)) {
            if let Ok(val) = parse_json_or_jsonc(&text) {
                if let Some(n) = val.get("name").and_then(|v| v.as_str()) {
                    if !n.trim().is_empty() {
                        plugin_id = n.to_string();
                        name = n.to_string();
                    }
                }
                version = val
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                description = val
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
        }
    }
    Ok(DiscoveredPluginSource {
        plugin_id,
        name,
        version,
        description,
        source_target: target,
        root_path: root.to_path_buf(),
        scope_id: scope_id.into(),
        scope_kind,
    })
}

/// 同步 inspect 实现（也可在测试中直接调用）。
///
/// Business Logic: 三 target 共用扫描骨架，按 manifest 形态分支。
/// Code Logic: match source_target → 扫描 helpers。
pub async fn inspect_plugin_source(
    source: &DiscoveredPluginSource,
    objects: &ObjectStore,
) -> Result<PluginDecompositionPreview, AppError> {
    if !source.root_path.is_dir() {
        return Err(AppError::validation(format!(
            "agent_hub_plugin_root_not_dir:{}",
            source.root_path.display()
        )));
    }
    if source.plugin_id.trim().is_empty() {
        return Err(AppError::validation(
            "agent_hub_plugin_empty_plugin_id".to_string(),
        ));
    }

    let mut claimed: BTreeSet<String> = BTreeSet::new();
    let mut components: Vec<ComponentPreview> = Vec::new();
    let mut residuals: Vec<ResidualPreview> = Vec::new();
    let mut target_extensions: BTreeMap<AgentTarget, Value> = BTreeMap::new();
    let mut name = source.name.clone();
    let mut version = source.version.clone();
    let mut description = source.description.clone();

    match source.source_target {
        AgentTarget::Claude => {
            let manifest_rel = ".claude-plugin/plugin.json";
            if let Some((meta, ext, paths)) = read_plugin_manifest(&source.root_path, manifest_rel)?
            {
                apply_manifest_meta(
                    &meta,
                    &mut name,
                    &mut version,
                    &mut description,
                    &mut target_extensions,
                    source.source_target,
                    ext,
                );
                for p in paths {
                    claimed.insert(p);
                }
            }
            collect_portable_children(source, &mut components, &mut claimed, objects).await?;
            collect_mcp_json(
                source,
                &source.root_path.join(".mcp.json"),
                ".mcp.json",
                &mut components,
                &mut claimed,
            )?;
            // also mcpServers inside plugin.json already handled as extension; scan nested
            let manifest_path = source.root_path.join(manifest_rel);
            if let Ok(text) = fs::read_to_string(&manifest_path) {
                if let Ok(val) = parse_json_or_jsonc(&text) {
                    if let Some(map) = val.get("mcpServers").and_then(|v| v.as_object()).cloned() {
                        let found = parse_mcp_servers_json_map(
                            source.source_target,
                            source.scope_kind,
                            &map,
                            &manifest_path,
                            PortableOriginKind::Plugin,
                            true,
                        );
                        for d in found {
                            push_discovered_component(
                                d,
                                manifest_rel,
                                ComponentOwnership::PackageOwned,
                                &mut components,
                            );
                        }
                    }
                }
            }
            collect_hooks(source, &mut components, &mut claimed, objects).await?;
            collect_residual_groups(
                source,
                &claimed,
                objects,
                ResidualKind::Runtime,
                &mut residuals,
            )
            .await?;
        }
        AgentTarget::Codex => {
            let manifest_rel = ".codex-plugin/plugin.json";
            if let Some((meta, ext, paths)) = read_plugin_manifest(&source.root_path, manifest_rel)?
            {
                apply_manifest_meta(
                    &meta,
                    &mut name,
                    &mut version,
                    &mut description,
                    &mut target_extensions,
                    source.source_target,
                    ext,
                );
                for p in paths {
                    claimed.insert(p);
                }
            }
            collect_portable_children(source, &mut components, &mut claimed, objects).await?;
            // agents config residual if present as TOML outside portable agents/
            let agents_cfg = source.root_path.join("config.toml");
            if agents_cfg.is_file() {
                claimed.insert("config.toml".into());
                let tree = put_single_file_tree(objects, &agents_cfg, "config.toml").await?;
                residuals.push(ResidualPreview {
                    residual_kind: ResidualKind::Assets,
                    target: AgentTarget::Codex,
                    tree_manifest_hash: tree,
                    relative_paths: vec!["config.toml".into()],
                });
            }
            collect_residual_groups(
                source,
                &claimed,
                objects,
                ResidualKind::Runtime,
                &mut residuals,
            )
            .await?;
        }
        AgentTarget::OpenCode => {
            // package.json → npm residual
            let pkg_json = source.root_path.join("package.json");
            if pkg_json.is_file() {
                claimed.insert("package.json".into());
                if let Ok(text) = fs::read_to_string(&pkg_json) {
                    if let Ok(val) = parse_json_or_jsonc(&text) {
                        if let Some(n) = val.get("name").and_then(|v| v.as_str()) {
                            if name.trim().is_empty() || name == source.plugin_id {
                                name = n.to_string();
                            }
                        }
                        if let Some(v) = val.get("version").and_then(|v| v.as_str()) {
                            version = Some(v.to_string());
                        }
                        if let Some(d) = val.get("description").and_then(|v| v.as_str()) {
                            description = Some(d.to_string());
                        }
                        // unknown package fields → extensions
                        let mut ext = serde_json::Map::new();
                        if let Some(obj) = val.as_object() {
                            for (k, v) in obj {
                                if matches!(
                                    k.as_str(),
                                    "name" | "version" | "description" | "main" | "type"
                                ) {
                                    continue;
                                }
                                ext.insert(k.clone(), v.clone());
                            }
                        }
                        if !ext.is_empty() {
                            target_extensions.insert(AgentTarget::OpenCode, Value::Object(ext));
                        }
                    }
                }
                let tree = put_single_file_tree(objects, &pkg_json, "package.json").await?;
                residuals.push(ResidualPreview {
                    residual_kind: ResidualKind::Npm,
                    target: AgentTarget::OpenCode,
                    tree_manifest_hash: tree,
                    relative_paths: vec!["package.json".into()],
                });
            }
            // portable adjacent
            collect_portable_children(source, &mut components, &mut claimed, objects).await?;
            // custom tools
            let tools_dir = source.root_path.join("tools");
            if tools_dir.is_dir() {
                claim_tree_paths(&tools_dir, "tools", &mut claimed);
                let tree = put_dir_tree(objects, &tools_dir).await?;
                residuals.push(ResidualPreview {
                    residual_kind: ResidualKind::CustomTool,
                    target: AgentTarget::OpenCode,
                    tree_manifest_hash: tree.hash,
                    relative_paths: tree.paths,
                });
            }
            // JS/TS runtime at root and src/
            collect_js_ts_runtime(source, objects, &mut residuals, &mut claimed).await?;
            collect_residual_groups(
                source,
                &claimed,
                objects,
                ResidualKind::Runtime,
                &mut residuals,
            )
            .await?;
        }
    }

    components.sort_by(|a, b| {
        a.kind
            .as_str()
            .cmp(b.kind.as_str())
            .then(a.logical_key.cmp(&b.logical_key))
    });
    residuals.sort_by(|a, b| {
        a.residual_kind
            .as_str()
            .cmp(b.residual_kind.as_str())
            .then(a.tree_manifest_hash.cmp(&b.tree_manifest_hash))
    });

    Ok(PluginDecompositionPreview {
        plugin_id: source.plugin_id.clone(),
        name: if name.trim().is_empty() {
            source.plugin_id.clone()
        } else {
            name
        },
        version,
        description,
        source_target: source.source_target,
        scope_id: source.scope_id.clone(),
        scope_kind: source.scope_kind,
        root_path: source.root_path.clone(),
        components,
        residuals,
        target_extensions,
    })
}

/// import 确认后的 package revision。
///
/// Business Logic: child 默认 `plugin:<pluginId>`；链接 standalone 时 ownership=Standalone。
/// Code Logic: 创建/复用 child assets → append revisions → append package。
pub async fn import_confirmed(
    repo: &AgentHubRepo,
    store: &ObjectStore,
    confirmed: ConfirmedPluginDecomposition,
    origin_kind: RevisionOriginKind,
) -> Result<PluginPackageRevision, AppError> {
    let preview = &confirmed.preview;
    let ns = format!("plugin:{}", preview.plugin_id);
    let mut component_refs: Vec<PluginComponentRef> = Vec::new();
    let mut component_asset_ids: BTreeMap<String, String> = BTreeMap::new();

    for comp in &preview.components {
        ensure_component_kind_allowed(comp.kind)?;
        let map_key = format!("{}:{}", comp.kind.as_str(), comp.logical_key);
        let (asset_id, ownership) =
            if let Some(standalone_id) = confirmed.link_standalone.get(&comp.logical_key) {
                // 链接到已确认 standalone：component 使用 standalone 资产自身
                let asset = repo.get_asset(standalone_id).await?.ok_or_else(|| {
                    AppError::validation(format!(
                        "agent_hub_plugin_standalone_missing:{standalone_id}"
                    ))
                })?;
                if asset.kind != comp.kind {
                    return Err(AppError::validation(format!(
                        "agent_hub_plugin_standalone_kind_mismatch:{}",
                        comp.logical_key
                    )));
                }
                // 登记 standalone 边（component_asset_id 指向实际 child 资产时也可用；
                // 这里 child 即 standalone 本身）
                repo.upsert_component_standalone_ref(&asset.id, &asset.id)
                    .await?;
                (asset.id, ComponentOwnership::Standalone)
            } else {
                // 查找或创建 plugin-namespaced child
                let existing = repo
                    .get_asset_by_unique_key(&preview.scope_id, comp.kind, &ns, &comp.logical_key)
                    .await?;
                let asset = if let Some(a) = existing {
                    a
                } else {
                    let policy = match comp.portability {
                        ComponentPortability::Portable => AssetPolicy::Shared,
                        ComponentPortability::TargetOnly => AssetPolicy::TargetOnly,
                        ComponentPortability::SourceResidual => AssetPolicy::TargetOnly,
                    };
                    repo.insert_asset(NewLogicalAsset {
                        scope_id: preview.scope_id.clone(),
                        kind: comp.kind,
                        origin_namespace: ns.clone(),
                        logical_key: comp.logical_key.clone(),
                        display_name: comp.display_name.clone(),
                        policy,
                    })
                    .await?
                };
                (asset.id, ComponentOwnership::PackageOwned)
            };

        let revision = match &comp.payload {
            ComponentPayloadPreview::Portable { payload } => {
                // Skill tree must exist in CAS
                if let PortableAssetPayload::Skill(skill) = payload {
                    let _ = store
                        .get_tree(&skill.tree_manifest_hash)
                        .await
                        .map_err(|_| {
                            AppError::validation(
                                "agent_hub_plugin_skill_tree_missing_in_cas".to_string(),
                            )
                        })?;
                }
                repo.append_portable_asset_revision(
                    &asset_id,
                    payload,
                    store,
                    origin_kind,
                    Some(preview.source_target),
                    confirmed.origin_replica_id.clone(),
                    None,
                )
                .await?
            }
            ComponentPayloadPreview::Hook { hook } => {
                validate_portable_hook(hook, &default_snapshot_limits())?;
                if let Some(h) = &hook.command_tree_hash {
                    let _ = store.get_tree(h).await.map_err(|_| {
                        AppError::validation("agent_hub_hook_command_tree_missing".to_string())
                    })?;
                }
                // ensure Hook asset kind
                let asset = repo
                    .get_asset(&asset_id)
                    .await?
                    .ok_or_else(|| AppError::not_found("agent_hub_asset_not_found".to_string()))?;
                if asset.kind != AssetKind::Hook {
                    return Err(AppError::validation(format!(
                        "agent_hub_hook_asset_kind_mismatch:{}",
                        asset.kind.as_str()
                    )));
                }
                repo.append_portable_hook_revision(
                    &asset_id,
                    hook,
                    store,
                    origin_kind,
                    Some(preview.source_target),
                    confirmed.origin_replica_id.clone(),
                    None,
                )
                .await?
            }
        };

        component_asset_ids.insert(map_key, asset_id.clone());
        component_refs.push(PluginComponentRef {
            kind: comp.kind,
            asset_id,
            revision_id: revision.id,
            ownership,
        });
    }

    // residual refs from preview（已在 CAS）
    let residual_refs: Vec<PluginResidualRef> = preview
        .residuals
        .iter()
        .map(|r| PluginResidualRef {
            target: r.target,
            residual_kind: r.residual_kind,
            tree_manifest_hash: r.tree_manifest_hash.clone(),
        })
        .collect();

    let mut payload = PluginPackagePayload {
        plugin_id: preview.plugin_id.clone(),
        name: preview.name.clone(),
        version: preview.version.clone(),
        description: preview.description.clone(),
        source_target: preview.source_target,
        component_refs,
        residual_refs,
        target_extensions: preview.target_extensions.clone(),
    };
    sort_plugin_package_payload(&mut payload);
    validate_plugin_package_payload(&payload)?;

    // package asset：namespace standalone，logical_key = plugin_id
    let package_asset = {
        let existing = repo
            .get_asset_by_unique_key(
                &preview.scope_id,
                AssetKind::Plugin,
                "standalone",
                &preview.plugin_id,
            )
            .await?;
        if let Some(a) = existing {
            a
        } else {
            repo.insert_asset(NewLogicalAsset {
                scope_id: preview.scope_id.clone(),
                kind: AssetKind::Plugin,
                origin_namespace: "standalone".into(),
                logical_key: preview.plugin_id.clone(),
                display_name: preview.name.clone(),
                policy: AssetPolicy::TargetOnly,
            })
            .await?
        }
    };

    let revision = repo
        .append_plugin_package_revision(
            &package_asset.id,
            &payload,
            store,
            origin_kind,
            Some(preview.source_target),
            confirmed.origin_replica_id.clone(),
            package_asset.current_revision_id.clone(),
        )
        .await?;

    let package_asset = repo
        .get_asset(&package_asset.id)
        .await?
        .ok_or_else(|| AppError::not_found("agent_hub_asset_not_found".to_string()))?;

    Ok(PluginPackageRevision {
        package_asset,
        revision,
        payload,
        component_asset_ids,
    })
}

// ─── helpers ───────────────────────────────────────────────────────────────

struct ManifestMeta {
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
}

#[allow(clippy::type_complexity)]
fn read_plugin_manifest(
    root: &Path,
    relative: &str,
) -> Result<Option<(ManifestMeta, serde_json::Map<String, Value>, Vec<String>)>, AppError> {
    let path = root.join(relative);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .map_err(|e| AppError::generic(format!("read plugin manifest: {e}")))?;
    let val = parse_json_or_jsonc(&text)?;
    let obj = val
        .as_object()
        .cloned()
        .ok_or_else(|| AppError::validation("agent_hub_plugin_manifest_not_object".to_string()))?;
    let meta = ManifestMeta {
        name: obj
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        version: obj
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        description: obj
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    };
    let known = [
        "name",
        "version",
        "description",
        "skills",
        "commands",
        "agents",
        "mcpServers",
        "hooks",
    ];
    let mut unknown = serde_json::Map::new();
    for (k, v) in &obj {
        if !known.contains(&k.as_str()) {
            unknown.insert(k.clone(), v.clone());
        }
    }
    Ok(Some((meta, unknown, vec![relative.replace('\\', "/")])))
}

fn apply_manifest_meta(
    meta: &ManifestMeta,
    name: &mut String,
    version: &mut Option<String>,
    description: &mut Option<String>,
    target_extensions: &mut BTreeMap<AgentTarget, Value>,
    target: AgentTarget,
    unknown: serde_json::Map<String, Value>,
) {
    if let Some(n) = &meta.name {
        if !n.trim().is_empty() {
            *name = n.clone();
        }
    }
    if meta.version.is_some() {
        *version = meta.version.clone();
    }
    if meta.description.is_some() {
        *description = meta.description.clone();
    }
    if !unknown.is_empty() {
        target_extensions.insert(target, Value::Object(unknown));
    }
}

async fn collect_portable_children(
    source: &DiscoveredPluginSource,
    components: &mut Vec<ComponentPreview>,
    claimed: &mut BTreeSet<String>,
    objects: &ObjectStore,
) -> Result<(), AppError> {
    let root = &source.root_path;
    let skills = scan_skill_dirs(
        source.source_target,
        source.scope_kind,
        &root.join("skills"),
        PortableOriginKind::Plugin,
    )?;
    for mut d in skills {
        claim_path_under(root, &d.origin.path, claimed);
        // Skill tree 必须进入 CAS，preview hash 与 import 共用
        if let PortableAssetPayload::Skill(skill) = &d.payload {
            let materialized =
                materialize_skill_tree_to_cas(objects, &d.origin.path, skill).await?;
            d.origin.content_hash = materialized.skill_markdown_hash.clone();
            d.origin.tree_hash = Some(materialized.tree_manifest_hash.clone());
            d.payload = PortableAssetPayload::Skill(materialized);
        }
        push_discovered_component(d, "skills", ComponentOwnership::PackageOwned, components);
    }
    let commands = scan_command_markdown_dir(
        source.source_target,
        source.scope_kind,
        &root.join("commands"),
        PortableOriginKind::Plugin,
    )?;
    for d in commands {
        claim_path_under(root, &d.origin.path, claimed);
        push_discovered_component(d, "commands", ComponentOwnership::PackageOwned, components);
    }
    let agents = scan_agent_markdown_dir(
        source.source_target,
        source.scope_kind,
        &root.join("agents"),
        PortableOriginKind::Plugin,
    )?;
    for d in agents {
        claim_path_under(root, &d.origin.path, claimed);
        push_discovered_component(d, "agents", ComponentOwnership::PackageOwned, components);
    }
    Ok(())
}

fn push_discovered_component(
    d: DiscoveredPortableAsset,
    default_rel: &str,
    ownership: ComponentOwnership,
    components: &mut Vec<ComponentPreview>,
) {
    let content_hash = Some(d.origin.content_hash.clone());
    let tree_hash = d.origin.tree_hash.clone();
    let source_relative_path = d
        .origin
        .path
        .file_name()
        .map(|s| format!("{default_rel}/{}", s.to_string_lossy()))
        .unwrap_or_else(|| default_rel.to_string());
    components.push(ComponentPreview {
        kind: d.kind,
        logical_key: d.semantic_name.clone(),
        display_name: d.semantic_name,
        ownership,
        portability: ComponentPortability::Portable,
        payload: ComponentPayloadPreview::Portable { payload: d.payload },
        content_hash,
        tree_hash,
        source_relative_path,
    });
}

fn collect_mcp_json(
    source: &DiscoveredPluginSource,
    path: &Path,
    rel: &str,
    components: &mut Vec<ComponentPreview>,
    claimed: &mut BTreeSet<String>,
) -> Result<(), AppError> {
    if !path.is_file() {
        return Ok(());
    }
    claimed.insert(rel.replace('\\', "/"));
    let text =
        fs::read_to_string(path).map_err(|e| AppError::generic(format!("read mcp json: {e}")))?;
    let val = parse_json_or_jsonc(&text)?;
    let Some(map) = val.get("mcpServers").and_then(|v| v.as_object()).cloned() else {
        return Ok(());
    };
    let found = parse_mcp_servers_json_map(
        source.source_target,
        source.scope_kind,
        &map,
        path,
        PortableOriginKind::Plugin,
        true,
    );
    for d in found {
        push_discovered_component(d, rel, ComponentOwnership::PackageOwned, components);
    }
    Ok(())
}

async fn collect_hooks(
    source: &DiscoveredPluginSource,
    components: &mut Vec<ComponentPreview>,
    claimed: &mut BTreeSet<String>,
    objects: &ObjectStore,
) -> Result<(), AppError> {
    let candidates = [
        source.root_path.join("hooks.json"),
        source.root_path.join("hooks").join("hooks.json"),
    ];
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(&source.root_path)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| "hooks.json".into());
        claimed.insert(rel.clone());
        let text =
            fs::read_to_string(&path).map_err(|e| AppError::generic(format!("read hooks: {e}")))?;
        let val = parse_json_or_jsonc(&text)?;
        let hooks = extract_hook_entries(&val);
        for (idx, entry) in hooks.into_iter().enumerate() {
            let event = entry
                .get("event")
                .or_else(|| entry.get("eventIntent"))
                .and_then(|v| v.as_str())
                .unwrap_or("custom");
            let intent = map_hook_event(event);
            let input = entry
                .get("input")
                .or_else(|| entry.get("inputContract"))
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default()));
            let output = entry
                .get("output")
                .or_else(|| entry.get("outputContract"))
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default()));
            let mut command_tree_hash = None;
            if let Some(cmd_rel) = entry.get("command").and_then(|v| v.as_str()) {
                let cmd_path = source.root_path.join(cmd_rel);
                if cmd_path.is_file() {
                    claimed.insert(cmd_rel.replace('\\', "/"));
                    command_tree_hash =
                        Some(put_single_file_tree(objects, &cmd_path, cmd_rel).await?);
                }
            }
            let mut ext = BTreeMap::new();
            // retain raw entry as target extension for re-projection
            ext.insert(source.source_target, Value::Object(entry.clone()));
            let hook = PortableHook {
                event_intent: intent,
                input_contract: input,
                output_contract: output,
                command_tree_hash: command_tree_hash.clone(),
                source_target: source.source_target,
                target_extensions: ext,
            };
            validate_portable_hook(&hook, &default_snapshot_limits())?;
            let logical_key = format!("hook-{event}-{idx}");
            components.push(ComponentPreview {
                kind: AssetKind::Hook,
                logical_key: logical_key.clone(),
                display_name: logical_key,
                ownership: ComponentOwnership::PackageOwned,
                // Hook 默认 targetOnly，无 mapping
                portability: ComponentPortability::TargetOnly,
                payload: ComponentPayloadPreview::Hook { hook },
                content_hash: None,
                tree_hash: command_tree_hash,
                source_relative_path: rel.clone(),
            });
        }
        // claim hooks/ directory residual scripts if any remain
        let hooks_dir = source.root_path.join("hooks");
        if hooks_dir.is_dir() {
            claim_tree_paths(&hooks_dir, "hooks", claimed);
        }
    }
    Ok(())
}

fn extract_hook_entries(val: &Value) -> Vec<serde_json::Map<String, Value>> {
    if let Some(arr) = val.get("hooks").and_then(|v| v.as_array()) {
        return arr.iter().filter_map(|v| v.as_object().cloned()).collect();
    }
    if let Some(obj) = val.get("hooks").and_then(|v| v.as_object()) {
        // Claude-style map of event → array
        let mut out = Vec::new();
        for (event, items) in obj {
            if let Some(arr) = items.as_array() {
                for item in arr {
                    let mut m = item.as_object().cloned().unwrap_or_default();
                    m.entry("event".to_string())
                        .or_insert_with(|| Value::String(event.clone()));
                    out.push(m);
                }
            } else if let Some(m) = items.as_object() {
                let mut m = m.clone();
                m.entry("event".to_string())
                    .or_insert_with(|| Value::String(event.clone()));
                out.push(m);
            }
        }
        return out;
    }
    if let Some(m) = val.as_object() {
        if m.contains_key("event") || m.contains_key("eventIntent") {
            return vec![m.clone()];
        }
    }
    Vec::new()
}

fn map_hook_event(raw: &str) -> HookEventIntent {
    let s = raw.trim();
    // accept both camelCase wire and PascalCase CLI names
    match s {
        "sessionStart" | "SessionStart" => HookEventIntent::SessionStart,
        "sessionEnd" | "SessionEnd" => HookEventIntent::SessionEnd,
        "userPromptSubmit" | "UserPromptSubmit" => HookEventIntent::UserPromptSubmit,
        "preToolUse" | "PreToolUse" => HookEventIntent::PreToolUse,
        "postToolUse" | "PostToolUse" => HookEventIntent::PostToolUse,
        "notification" | "Notification" => HookEventIntent::Notification,
        "stop" | "Stop" => HookEventIntent::Stop,
        "subagentStop" | "SubagentStop" => HookEventIntent::SubagentStop,
        "preCompact" | "PreCompact" => HookEventIntent::PreCompact,
        "permissionRequest" | "PermissionRequest" => HookEventIntent::PermissionRequest,
        _ => HookEventIntent::Custom,
    }
}

struct DirTreePut {
    hash: String,
    paths: Vec<String>,
}

async fn put_dir_tree(objects: &ObjectStore, dir: &Path) -> Result<DirTreePut, AppError> {
    let result = objects.put_tree_from_directory(dir).await?;
    let manifest = objects.get_tree(&result.object.hash).await?;
    let mut paths: Vec<String> = manifest.entries.iter().map(|e| e.path.clone()).collect();
    paths.sort();
    Ok(DirTreePut {
        hash: result.object.hash,
        paths,
    })
}

async fn put_single_file_tree(
    objects: &ObjectStore,
    file: &Path,
    rel: &str,
) -> Result<String, AppError> {
    let bytes = fs::read(file).map_err(|e| AppError::generic(format!("read residual: {e}")))?;
    let blob = objects.put_blob(&bytes).await?;
    let manifest = TreeManifest {
        entries: vec![TreeEntry {
            path: rel.replace('\\', "/"),
            blob_hash: blob.hash,
            entry_type: TreeEntryType::File,
            executable: false,
        }],
    };
    let tree = objects.put_tree(&manifest).await?;
    Ok(tree.hash)
}

async fn collect_js_ts_runtime(
    source: &DiscoveredPluginSource,
    objects: &ObjectStore,
    residuals: &mut Vec<ResidualPreview>,
    claimed_mut: &mut BTreeSet<String>,
) -> Result<(), AppError> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_matching_files(
        &source.root_path,
        &source.root_path,
        claimed_mut,
        &mut files,
        &|rel, path| {
            if rel.starts_with("skills/")
                || rel.starts_with("commands/")
                || rel.starts_with("agents/")
                || rel.starts_with("tools/")
            {
                return false;
            }
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            matches!(ext.as_str(), "js" | "ts" | "mjs" | "cjs" | "jsx" | "tsx")
        },
    )?;
    if files.is_empty() {
        return Ok(());
    }
    let mut entries = Vec::new();
    let mut rels = Vec::new();
    for (rel, path) in files {
        claimed_mut.insert(rel.clone());
        let bytes = fs::read(&path).map_err(|e| AppError::generic(format!("read runtime: {e}")))?;
        let blob = objects.put_blob(&bytes).await?;
        entries.push(TreeEntry {
            path: rel.clone(),
            blob_hash: blob.hash,
            entry_type: TreeEntryType::File,
            executable: false,
        });
        rels.push(rel);
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    rels.sort();
    let tree = objects.put_tree(&TreeManifest { entries }).await?;
    residuals.push(ResidualPreview {
        residual_kind: ResidualKind::Runtime,
        target: AgentTarget::OpenCode,
        tree_manifest_hash: tree.hash,
        relative_paths: rels,
    });
    Ok(())
}

async fn collect_residual_groups(
    source: &DiscoveredPluginSource,
    claimed: &BTreeSet<String>,
    objects: &ObjectStore,
    kind: ResidualKind,
    residuals: &mut Vec<ResidualPreview>,
) -> Result<(), AppError> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_matching_files(
        &source.root_path,
        &source.root_path,
        claimed,
        &mut files,
        &|rel, _path| {
            // skip already claimed and well-known portable roots if fully claimed
            !claimed.contains(rel)
                && !rel.starts_with("skills/")
                && !rel.starts_with("commands/")
                && !rel.starts_with("agents/")
                && rel != "skills"
                && rel != "commands"
                && rel != "agents"
        },
    )?;
    if files.is_empty() {
        return Ok(());
    }
    let mut entries = Vec::new();
    let mut rels = Vec::new();
    for (rel, path) in files {
        let bytes =
            fs::read(&path).map_err(|e| AppError::generic(format!("read residual: {e}")))?;
        let blob = objects.put_blob(&bytes).await?;
        entries.push(TreeEntry {
            path: rel.clone(),
            blob_hash: blob.hash,
            entry_type: TreeEntryType::File,
            executable: false,
        });
        rels.push(rel);
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    rels.sort();
    let tree = objects.put_tree(&TreeManifest { entries }).await?;
    residuals.push(ResidualPreview {
        residual_kind: kind,
        target: source.source_target,
        tree_manifest_hash: tree.hash,
        relative_paths: rels,
    });
    Ok(())
}

fn collect_matching_files(
    root: &Path,
    current: &Path,
    claimed: &BTreeSet<String>,
    out: &mut Vec<(String, PathBuf)>,
    pred: &dyn Fn(&str, &Path) -> bool,
) -> Result<(), AppError> {
    let read = match fs::read_dir(current) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let mut children: Vec<_> = read
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::generic(format!("readdir: {e}")))?;
    children.sort_by_key(|e| e.file_name());
    for entry in children {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect_matching_files(root, &path, claimed, out, pred)?;
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"));
        if claimed.contains(&rel) {
            continue;
        }
        if pred(&rel, &path) {
            out.push((rel, path));
        }
    }
    Ok(())
}

fn claim_path_under(root: &Path, path: &Path, claimed: &mut BTreeSet<String>) {
    if path.is_dir() {
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        claim_tree_paths(path, &rel, claimed);
        return;
    }
    if let Ok(rel) = path.strip_prefix(root) {
        claimed.insert(rel.to_string_lossy().replace('\\', "/"));
    }
}

fn claim_tree_paths(dir: &Path, prefix: &str, claimed: &mut BTreeSet<String>) {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in read.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if path.is_dir() {
            claim_tree_paths(&path, &rel, claimed);
        } else {
            claimed.insert(rel);
        }
    }
}

/// 从磁盘 skill 目录把 tree 写入 CAS，并返回更新后的 PortableSkill。
///
/// Business Logic: discovery 的 tree hash 与 CAS 一致，但 blob 必须实际入库。
/// Code Logic: put_tree_from_directory + 校验 SKILL.md。
pub async fn materialize_skill_tree_to_cas(
    objects: &ObjectStore,
    skill_dir: &Path,
    skill: &PortableSkill,
) -> Result<PortableSkill, AppError> {
    let put = objects.put_tree_from_directory(skill_dir).await?;
    let mut out = skill.clone();
    out.tree_manifest_hash = put.object.hash;
    // skill_markdown_hash should match CAS blob for SKILL.md
    let manifest = objects.get_tree(&out.tree_manifest_hash).await?;
    if let Some(entry) = manifest
        .entries
        .iter()
        .find(|e| e.path == "SKILL.md" || e.path.ends_with("/SKILL.md"))
    {
        out.skill_markdown_hash = entry.blob_hash.clone();
    }
    out.validate()?;
    out.validate_tree_manifest(&manifest)?;
    Ok(out)
}

/// 确保 preview 内 Skill payload 的 tree 在 CAS。
///
/// Business Logic: import 前 skill tree 必须可 get_tree。
/// Code Logic: 若 get_tree 失败且 source path 存在则 materialize。
pub async fn ensure_preview_skills_in_cas(
    preview: &mut PluginDecompositionPreview,
    objects: &ObjectStore,
) -> Result<(), AppError> {
    for comp in &mut preview.components {
        let ComponentPayloadPreview::Portable { payload } = &mut comp.payload else {
            continue;
        };
        let PortableAssetPayload::Skill(skill) = payload else {
            continue;
        };
        if objects.get_tree(&skill.tree_manifest_hash).await.is_ok() {
            continue;
        }
        // 尝试从 root/skills/<native> 物化
        let skill_dir = preview.root_path.join("skills").join(&skill.name);
        let alt = preview.root_path.join("skills");
        let dir = if skill_dir.is_dir() {
            skill_dir
        } else {
            // scan children for matching name
            let mut found = None;
            if alt.is_dir() {
                for entry in fs::read_dir(&alt).into_iter().flatten().flatten() {
                    let p = entry.path();
                    if p.is_dir() && p.join("SKILL.md").is_file() {
                        let text = fs::read_to_string(p.join("SKILL.md")).unwrap_or_default();
                        let (fields, _, _) = parse_simple_frontmatter(&text);
                        let name = fields.get("name").cloned().unwrap_or_else(|| {
                            p.file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("skill")
                                .to_string()
                        });
                        if name == skill.name {
                            found = Some(p);
                            break;
                        }
                    }
                }
            }
            found.ok_or_else(|| {
                AppError::validation(format!("agent_hub_plugin_skill_dir_missing:{}", skill.name))
            })?
        };
        *skill = materialize_skill_tree_to_cas(objects, &dir, skill).await?;
        comp.tree_hash = Some(skill.tree_manifest_hash.clone());
        comp.content_hash = Some(skill.skill_markdown_hash.clone());
    }
    Ok(())
}

// ─── tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{NewScopeNode, ScopeKind};
    use crate::storage::agent_hub_repo::AgentHubRepo;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::HashSet;
    use std::io::Write;
    use std::str::FromStr;
    use tempfile::TempDir;

    async fn test_repo() -> AgentHubRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        AgentHubRepo::new(pool)
    }

    async fn test_store() -> (TempDir, ObjectStore) {
        let dir = TempDir::new().unwrap();
        let store = ObjectStore::open(dir.path()).unwrap();
        (dir, store)
    }

    fn write_file(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    fn claude_fixture(root: &Path) {
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{
  "name": "claude-demo",
  "version": "0.1.0",
  "description": "mixed claude plugin",
  "skills": "./skills",
  "unknownMarketplaceField": "keep-me"
}"#,
        );
        write_file(
            &root.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review carefully\n---\nDo review.\n",
        );
        write_file(
            &root.join("commands/ship.md"),
            "---\nname: ship\ndescription: Ship it\n---\nShip prompt\n",
        );
        write_file(
            &root.join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Agent\n---\nAgent body\n",
        );
        write_file(
            &root.join(".mcp.json"),
            r#"{
  "mcpServers": {
    "docs": {
      "command": "uvx",
      "args": ["mcp-server-fetch"],
      "env": { "TOKEN": "secret-fixture" }
    }
  }
}"#,
        );
        write_file(
            &root.join("hooks.json"),
            r#"{
  "hooks": [
    {
      "event": "PreToolUse",
      "input": { "tool": "string" },
      "output": { "permission": "allow|deny" },
      "command": "hooks/pre.sh"
    }
  ]
}"#,
        );
        write_file(&root.join("hooks/pre.sh"), "#!/bin/sh\necho pre\n");
        write_file(
            &root.join("runtime/index.js"),
            "console.log('claude-runtime')\n",
        );
    }

    fn codex_fixture(root: &Path) {
        write_file(
            &root.join(".codex-plugin/plugin.json"),
            r#"{
  "name": "codex-demo",
  "version": "1.0.0",
  "description": "codex plugin",
  "skills": "./skills",
  "extraCodexFlag": true
}"#,
        );
        write_file(
            &root.join("skills/analyze/SKILL.md"),
            "---\nname: analyze\ndescription: Analyze\n---\nAnalyze body\n",
        );
        write_file(
            &root.join("agents/worker.md"),
            "---\nname: worker\n---\nWorker instructions\n",
        );
        write_file(
            &root.join("config.toml"),
            "[agents.worker]\nconfig_file = \"agents/worker.md\"\n",
        );
        write_file(&root.join("assets/icon.png"), "PNG-BYTES");
    }

    fn opencode_fixture(root: &Path) {
        write_file(
            &root.join("package.json"),
            r#"{
  "name": "opencode-demo",
  "version": "2.0.0",
  "description": "local opencode plugin",
  "main": "index.ts",
  "dependencies": { "zod": "3.0.0" }
}"#,
        );
        write_file(
            &root.join("index.ts"),
            "export default function plugin() { return {}; }\n",
        );
        write_file(
            &root.join("tools/custom-tool.ts"),
            "export const tool = { name: 'custom' };\n",
        );
        write_file(
            &root.join("skills/nearby/SKILL.md"),
            "---\nname: nearby\ndescription: Portable skill next to runtime\n---\nSkill\n",
        );
        write_file(
            &root.join("commands/run.md"),
            "---\nname: run\n---\nRun command\n",
        );
        write_file(
            &root.join("agents/helper.md"),
            "---\nname: helper\n---\nHelper agent\n",
        );
    }

    #[tokio::test]
    async fn claude_plugin_decomposes_portable_hook_target_only_and_runtime_residual() {
        let fixture = TempDir::new().unwrap();
        claude_fixture(fixture.path());
        let (_cas_dir, store) = test_store().await;
        let source = DiscoveredPluginSource {
            plugin_id: "claude-demo".into(),
            name: "claude-demo".into(),
            version: None,
            description: None,
            source_target: AgentTarget::Claude,
            root_path: fixture.path().to_path_buf(),
            scope_id: "scope-user".into(),
            scope_kind: ScopeKind::User,
        };
        let mut preview = inspect_plugin_source(&source, &store).await.unwrap();
        ensure_preview_skills_in_cas(&mut preview, &store)
            .await
            .unwrap();

        let kinds: HashSet<_> = preview.components.iter().map(|c| c.kind).collect();
        assert!(kinds.contains(&AssetKind::Skill));
        assert!(kinds.contains(&AssetKind::Command));
        assert!(kinds.contains(&AssetKind::Agent));
        assert!(kinds.contains(&AssetKind::Mcp));
        assert!(kinds.contains(&AssetKind::Hook));

        let hook = preview
            .components
            .iter()
            .find(|c| c.kind == AssetKind::Hook)
            .expect("hook");
        assert_eq!(hook.portability, ComponentPortability::TargetOnly);
        match &hook.payload {
            ComponentPayloadPreview::Hook { hook } => {
                assert_eq!(hook.event_intent, HookEventIntent::PreToolUse);
                assert_eq!(hook.source_target, AgentTarget::Claude);
            }
            _ => panic!("expected hook payload"),
        }

        assert!(
            preview
                .residuals
                .iter()
                .any(|r| r.residual_kind == ResidualKind::Runtime
                    && r.target == AgentTarget::Claude
                    && r.relative_paths.iter().any(|p| p.contains("runtime"))),
            "runtime residual missing: {:?}",
            preview.residuals
        );
        // unknown manifest field retained
        assert!(preview.target_extensions.contains_key(&AgentTarget::Claude));

        // residual exact bytes round-trip
        let residual = preview
            .residuals
            .iter()
            .find(|r| r.residual_kind == ResidualKind::Runtime)
            .unwrap();
        let tree = store.get_tree(&residual.tree_manifest_hash).await.unwrap();
        let runtime_entry = tree
            .entries
            .iter()
            .find(|e| e.path.contains("runtime"))
            .expect("runtime entry");
        let body = store.get_blob(&runtime_entry.blob_hash).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("claude-runtime"));
    }

    #[tokio::test]
    async fn codex_plugin_decomposes_skill_agent_and_residual_assets() {
        let fixture = TempDir::new().unwrap();
        codex_fixture(fixture.path());
        let (_cas_dir, store) = test_store().await;
        let source = DiscoveredPluginSource {
            plugin_id: "codex-demo".into(),
            name: "codex-demo".into(),
            version: None,
            description: None,
            source_target: AgentTarget::Codex,
            root_path: fixture.path().to_path_buf(),
            scope_id: "scope-user".into(),
            scope_kind: ScopeKind::User,
        };
        let mut preview = inspect_plugin_source(&source, &store).await.unwrap();
        ensure_preview_skills_in_cas(&mut preview, &store)
            .await
            .unwrap();
        assert!(preview
            .components
            .iter()
            .any(|c| c.kind == AssetKind::Skill));
        assert!(preview
            .components
            .iter()
            .any(|c| c.kind == AssetKind::Agent));
        assert!(
            preview
                .residuals
                .iter()
                .any(|r| r.residual_kind == ResidualKind::Assets
                    || r.relative_paths.iter().any(|p| p.contains("icon")
                        || p.contains("config.toml")
                        || p.contains("assets"))),
            "codex residuals: {:?}",
            preview.residuals
        );
    }

    #[tokio::test]
    async fn opencode_plugin_decomposes_js_npm_custom_tool_and_portable_adjacent() {
        let fixture = TempDir::new().unwrap();
        opencode_fixture(fixture.path());
        let (_cas_dir, store) = test_store().await;
        let source = DiscoveredPluginSource {
            plugin_id: "opencode-demo".into(),
            name: "opencode-demo".into(),
            version: None,
            description: None,
            source_target: AgentTarget::OpenCode,
            root_path: fixture.path().to_path_buf(),
            scope_id: "scope-user".into(),
            scope_kind: ScopeKind::User,
        };
        let mut preview = inspect_plugin_source(&source, &store).await.unwrap();
        ensure_preview_skills_in_cas(&mut preview, &store)
            .await
            .unwrap();

        let portable_kinds: HashSet<_> = preview
            .components
            .iter()
            .filter(|c| c.portability == ComponentPortability::Portable)
            .map(|c| c.kind)
            .collect();
        assert!(portable_kinds.contains(&AssetKind::Skill));
        assert!(portable_kinds.contains(&AssetKind::Command));
        assert!(portable_kinds.contains(&AssetKind::Agent));

        assert!(preview
            .residuals
            .iter()
            .any(|r| r.residual_kind == ResidualKind::Npm));
        assert!(preview
            .residuals
            .iter()
            .any(|r| r.residual_kind == ResidualKind::CustomTool));
        assert!(preview
            .residuals
            .iter()
            .any(|r| r.residual_kind == ResidualKind::Runtime
                && r.relative_paths.iter().any(|p| p.ends_with(".ts"))));
    }

    #[tokio::test]
    async fn import_sets_plugin_namespace_and_fixed_component_refs() {
        let fixture = TempDir::new().unwrap();
        claude_fixture(fixture.path());
        let repo = Arc::new(test_repo().await);
        let (_cas_dir, store) = test_store().await;
        let store = Arc::new(store);
        let scope = repo
            .insert_scope(NewScopeNode {
                id: Some("scope-user".into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let source = DiscoveredPluginSource {
            plugin_id: "claude-demo".into(),
            name: "claude-demo".into(),
            version: None,
            description: None,
            source_target: AgentTarget::Claude,
            root_path: fixture.path().to_path_buf(),
            scope_id: scope.id.clone(),
            scope_kind: ScopeKind::User,
        };
        let decomposer = DefaultPluginDecomposer::new(repo.clone(), store.clone());
        let mut preview = decomposer.inspect(&source, store.as_ref()).await.unwrap();
        ensure_preview_skills_in_cas(&mut preview, store.as_ref())
            .await
            .unwrap();
        let result = decomposer
            .import(ConfirmedPluginDecomposition {
                preview,
                link_standalone: BTreeMap::new(),
                origin_replica_id: "01900000-0000-7000-8000-0000000000d1".into(),
            })
            .await
            .unwrap();
        assert_eq!(result.payload.plugin_id, "claude-demo");
        assert!(!result.payload.component_refs.is_empty());
        // child assets use plugin namespace
        for cref in &result.payload.component_refs {
            let asset = repo.get_asset(&cref.asset_id).await.unwrap().unwrap();
            if cref.kind == AssetKind::Hook {
                assert_eq!(asset.origin_namespace, "plugin:claude-demo");
                assert_eq!(asset.policy, AssetPolicy::TargetOnly);
            } else {
                assert_eq!(asset.origin_namespace, "plugin:claude-demo");
            }
        }
        // package revision lists fixed refs
        let comps = repo
            .list_plugin_components_for_revision(result.revision.id.as_str())
            .await
            .unwrap();
        assert_eq!(comps.len(), result.payload.component_refs.len());
    }
}
