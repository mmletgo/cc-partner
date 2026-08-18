//! portable_inventory/models — 库存 DTO、身份与确定性 hash
//!
//! Business Logic（为什么需要这个模块）:
//!     UI/动作计划依赖稳定 inventory 身份与 hash；任何影响动作许可的事实变化都必须使旧 preview 失效。
//!     Inventory 只描述观测事实与对账结果，不得冒充 desired 或自动纳管。
//!
//! Code Logic（这个模块做什么）:
//!     camelCase DTO；仅 skill/command/plugin/mcp；MCP 凭据仅 present/hash；
//!     `inventory_item_id` 由 target|scope|originNamespace（路径无关）|nativeId 派生；
//!     `inventory_snapshot_hash` 对排序后的 material 做 RFC8785 兼容 canonical JSON + sha256。

use crate::agent_hub::models::{AgentTarget, AssetKind, DesiredPresence, ScopeKind};
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::snapshot::canonical_json::canonicalize_value;
use crate::agent_hub::targets::portable::{PortableAssetOwner, PortableOriginKind};
use crate::error::AppError;
use serde::{Deserialize, Serialize};

/// Portable inventory 允许的四类资产。
///
/// Business Logic（为什么需要这个枚举）:
///     本合同只覆盖 Skill/Command/Plugin/MCP；Instruction/Agent/Hook 不得进入库存边界。
///
/// Code Logic（这个枚举做什么）:
///     camelCase wire；`try_from_asset_kind` 拒绝非四类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableAssetKind {
    /// Skill 目录资产
    Skill,
    /// 斜杠/原生命令
    Command,
    /// Plugin 包
    Plugin,
    /// MCP server 配置
    Mcp,
}

/// Portable inventory 扫描过滤条件。
///
/// Business Logic（为什么需要这个结构体）:
///     资产页只应等待当前 Agent/类型/scope 的事实，不能因查看 Skill 而阻塞于所有 Plugin。
///
/// Code Logic（这个结构体做什么）:
///     三个可选精确过滤字段；全空表示完整权威扫描，供 mutation/Pull 继续使用。
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct PortableInventoryQuery {
    /// 仅扫描一个 Agent target。
    pub target: Option<AgentTarget>,
    /// 仅扫描一种 portable 资产。
    pub kind: Option<PortableAssetKind>,
    /// 仅扫描一种 scope；None 为 user + 已映射 project。
    pub scope_kind: Option<ScopeKind>,
    /// 本机 Workbench project id；仅与 project scope 合用并在 scanner 边界解析。
    #[serde(rename = "localProjectId", skip_serializing_if = "Option::is_none")]
    pub local_project_id: Option<String>,
}

impl PortableAssetKind {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Command => "command",
            Self::Plugin => "plugin",
            Self::Mcp => "mcp",
        }
    }

    /// 从通用 `AssetKind` 转换；非四类返回 Validation。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     库存边界必须 fail-closed 拒绝 Instruction/Agent/Hook，避免静默混入。
    ///
    /// Code Logic（这个函数做什么）:
    ///     match skill/command/plugin/mcp；其它 → validation。
    pub fn try_from_asset_kind(kind: AssetKind) -> Result<Self, AppError> {
        match kind {
            AssetKind::Skill => Ok(Self::Skill),
            AssetKind::Command => Ok(Self::Command),
            AssetKind::Plugin => Ok(Self::Plugin),
            AssetKind::Mcp => Ok(Self::Mcp),
            other => Err(AppError::validation(format!(
                "portable_inventory_unsupported_kind:{}",
                other.as_str()
            ))),
        }
    }

    /// 转回通用 AssetKind（仅四类）。
    pub fn to_asset_kind(self) -> AssetKind {
        match self {
            Self::Skill => AssetKind::Skill,
            Self::Command => AssetKind::Command,
            Self::Plugin => AssetKind::Plugin,
            Self::Mcp => AssetKind::Mcp,
        }
    }

    /// 是否进入 portable-store 软链附加模型。
    ///
    /// Business Logic: 只有 Skill/Command 能本机一份 + native 软链。
    ///     MCP 是各家配置 leaf，跨 Agent 走 Pull；Plugin 是 viewing 开关。
    /// Code Logic: skill/command 为真。
    pub fn supports_portable_store(self) -> bool {
        matches!(self, Self::Skill | Self::Command)
    }
}

/// 库存对账后的管理状态。
///
/// Business Logic（为什么需要这个枚举）:
///     UI 与动作计划需要明确区分未纳管、Hub 管理、漂移、碰撞与不支持。
///
/// Code Logic（这个枚举做什么）:
///     camelCase：`unmanaged`/`hubManaged`/`drifted`/`externalCollision`/`unsupported`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableInventoryManagementState {
    /// 无 canonical match
    Unmanaged,
    /// Hub ownership 且 observed hash 与 applied 一致
    HubManaged,
    /// Hub ownership 存在但 observed hash 偏离
    Drifted,
    /// 同一 materialized identity 被不兼容外部 source 占用
    ExternalCollision,
    /// adapter/版本不支持所需语义
    Unsupported,
}

impl PortableInventoryManagementState {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unmanaged => "unmanaged",
            Self::HubManaged => "hubManaged",
            Self::Drifted => "drifted",
            Self::ExternalCollision => "externalCollision",
            Self::Unsupported => "unsupported",
        }
    }
}

/// 扫描到的资产来源命名空间。
///
/// Business Logic（为什么需要这个枚举）:
///     standalone 与 plugin component 不得按 displayName 静默合并；nativeConfig 用于 MCP 等。
///
/// Code Logic（这个枚举做什么）:
///     camelCase origin token。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableInventorySourceOrigin {
    /// 独立目录/文件
    Standalone,
    /// Plugin 包内组件
    PluginComponent,
    /// 原生配置文件（如 MCP）
    NativeConfig,
}

impl PortableInventorySourceOrigin {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::PluginComponent => "pluginComponent",
            Self::NativeConfig => "nativeConfig",
        }
    }

    /// 映射到 Hub `origin_namespace` 前缀语义（component 由调用方填 plugin:id）。
    pub fn default_origin_namespace(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::PluginComponent => "plugin",
            Self::NativeConfig => "standalone",
        }
    }
}

/// Target 扫描能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableInventoryScanCapability {
    /// 已支持完整扫描
    Supported,
    /// 仅只读
    ReadOnly,
    /// 被阻断
    Blocked,
}

impl PortableInventoryScanCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::ReadOnly => "readOnly",
            Self::Blocked => "blocked",
        }
    }
}

/// Target 写入/动作能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableInventoryMutationCapability {
    /// 已支持 mutation
    Supported,
    /// 仅 preview
    PreviewOnly,
    /// 被阻断
    Blocked,
}

impl PortableInventoryMutationCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::PreviewOnly => "previewOnly",
            Self::Blocked => "blocked",
        }
    }
}

/// 单项动作能力摘要。
///
/// Business Logic（为什么需要这个结构体）:
///     列表/详情需要按项决定是否暴露 enable/disable/uninstall/adopt，且可附证据。
///
/// Code Logic（这个结构体做什么）:
///     camelCase bool 门闩 + 可选 reason/evidence。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PortableInventoryItemCapabilitiesDto {
    /// 是否允许启用
    pub can_enable: bool,
    /// 是否允许禁用
    pub can_disable: bool,
    /// 是否允许卸载
    pub can_uninstall: bool,
    /// 是否允许显式 adopt
    pub can_adopt: bool,
    /// 是否允许安装到源同类 target
    pub can_install_to_source_target: bool,
    /// 是否允许把本机 native 迁入 portable-store
    #[serde(default)]
    pub can_migrate_to_store: bool,
    /// 是否允许把 store 附加到当前 Agent
    #[serde(default)]
    pub can_attach: bool,
    /// 是否允许从当前 Agent 卸下 store 软链
    #[serde(default)]
    pub can_detach: bool,
    /// 是否允许本机彻底删除 store 真树
    #[serde(default)]
    pub can_destroy_store: bool,
    /// 是否允许把当前磁盘记为一致基准（漂移项）
    #[serde(default)]
    pub can_confirm_current_version: bool,
    /// 是否允许把逃逸软链解引为 native 路径上的真实副本
    #[serde(default)]
    pub can_materialize_escape_link: bool,
    /// 能力阻断/限制原因码
    pub reason_code: Option<String>,
    /// 证据 ID
    pub evidence_ids: Vec<String>,
}

/// portable-store 观测事实。
///
/// Business Logic（为什么需要这个结构体）:
///     同一 storeId 在 Grok/Claude 路径上必须去重；卸下 Grok 后若 Claude 仍挂着，
///     只能提示「仍被其他路径加载」，不得暗改 Claude。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；缺省全 false/None，旧快照可反序列化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PortableStoreFactDto {
    /// `skill:foo` / `command:bar` / `mcp:id`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_id: Option<String>,
    /// 当前 viewing Agent 自己的 native 根是否已挂上
    #[serde(default)]
    pub store_attached: bool,
    /// 本 Agent 未挂，但仍被其他路径加载（如 Grok 扫到 Claude 链）
    #[serde(default)]
    pub loaded_via_other_path: bool,
    /// 仍在加载的另一路径所属 target
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loaded_via_target: Option<AgentTarget>,
}

/// MCP 凭据观测事实（仅 present/hash）。
///
/// Business Logic（为什么需要这个结构体）:
///     Inventory 必须报告凭据是否存在以便诊断，但绝不返回 secret 原文。
///
/// Code Logic（这个结构体做什么）:
///     仅 `present` + 可选 `hash`；无 value/token/secret 字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableMcpCredentialFactDto {
    /// 是否检测到凭据材料
    pub present: bool,
    /// 凭据材料 hash（无 secret 原文）
    pub hash: Option<String>,
}

/// 单 target 探测与能力事实。
///
/// Business Logic（为什么需要这个结构体）:
///     快照 hash 覆盖 executable/version/configRoot/capability；CLI 指纹变化使 preview 失效。
///
/// Code Logic（这个结构体做什么）:
///     camelCase target DTO。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableInventoryTargetDto {
    /// Agent 目标
    pub target: AgentTarget,
    /// CLI 是否已安装
    pub installed: bool,
    /// 探测版本
    pub version: Option<String>,
    /// 可执行路径
    pub executable: Option<String>,
    /// 配置根
    pub config_root: String,
    /// 扫描能力
    pub scan_capability: PortableInventoryScanCapability,
    /// 写入能力
    pub mutation_capability: PortableInventoryMutationCapability,
    /// 原因码
    pub reason_code: Option<String>,
    /// 证据 ID
    pub evidence_ids: Vec<String>,
}

/// 单条库存项（观测事实 + 对账摘要）。
///
/// Business Logic（为什么需要这个结构体）:
///     列表/详情/动作计划的权威 read model；`actualEnabled` 仅来自扫描，
///     `desired*` 仅来自 Hub binding，互不冒充。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；含 managementState、capabilities、可选 MCP credential 事实。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableInventoryItemDto {
    /// 稳定库存身份
    pub inventory_item_id: String,
    /// 所属 target
    pub target: AgentTarget,
    /// 当前扫描/加载该行的 target（与 `target` 相同）
    pub loaded_by: AgentTarget,
    /// 资产所有者（兼容/共享根可与 target 不同）
    pub owned_by: PortableAssetOwner,
    /// 发现分类
    pub origin_kind: PortableOriginKind,
    /// 是否可作为该 target 的 native 写出候选
    pub native_output_candidate: bool,
    /// 四类 kind
    pub kind: PortableAssetKind,
    /// 目标侧原生 ID
    pub native_id: String,
    /// 展示名
    pub display_name: String,
    /// 描述
    pub description: Option<String>,
    /// 版本
    pub version: Option<String>,
    /// scope id
    pub scope_id: String,
    /// scope kind
    pub scope_kind: ScopeKind,
    /// 项目 id（user 级为 None）
    pub project_id: Option<String>,
    /// 项目是否已 opt-in
    pub project_opted_in: bool,
    /// 源路径
    pub source_path: Option<String>,
    /// 源 origin 命名空间
    pub source_origin: PortableInventorySourceOrigin,
    /// 父 plugin 库存 id
    pub parent_plugin_inventory_item_id: Option<String>,
    /// 实际启用状态（仅扫描）
    pub actual_enabled: Option<bool>,
    /// 内容 hash
    pub content_hash: Option<String>,
    /// 目录树 hash
    pub tree_hash: Option<String>,
    /// 匹配到的 canonical asset id
    pub canonical_asset_id: Option<String>,
    /// 匹配到的 applied revision id
    pub canonical_revision_id: Option<String>,
    /// 对账管理状态
    pub management_state: PortableInventoryManagementState,
    /// Hub desired presence
    pub desired_presence: Option<DesiredPresence>,
    /// Hub desired enabled
    pub desired_enabled: Option<bool>,
    /// materialization 状态摘要
    pub materialization_status: Option<String>,
    /// 单项能力
    pub capabilities: PortableInventoryItemCapabilitiesDto,
    /// 警告码/文案（无 secret）
    pub warnings: Vec<String>,
    /// MCP 凭据事实（仅 present/hash；非 MCP 为 None）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_credential: Option<PortableMcpCredentialFactDto>,
    /// portable-store 观测（缺省表示与 store 无关）
    #[serde(default)]
    pub store: PortableStoreFactDto,
}

/// 完整库存快照。
///
/// Business Logic（为什么需要这个结构体）:
///     inspect 出口；hash 覆盖 target/item/ownership 摘要，任何相关事实变化使旧 preview 失效。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；含 `inventorySnapshotHash`/`refreshedAt`/`stale`/`targets`/`items`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableInventorySnapshotDto {
    /// 确定性快照 hash
    pub inventory_snapshot_hash: String,
    /// 刷新时间 RFC3339
    pub refreshed_at: String,
    /// 是否已陈旧（由上层世代/并发置位；本层默认 false）
    pub stale: bool,
    /// target 事实
    pub targets: Vec<PortableInventoryTargetDto>,
    /// 库存项
    pub items: Vec<PortableInventoryItemDto>,
}

/// 由 target、scope、origin namespace（路径无关的逻辑源）与 native ID 派生稳定库存身份。
///
/// Business Logic（为什么需要这个函数）:
///     库存身份不得使用展示名；target/scope/逻辑源/nativeId 任一变化都必须产生新 id。
///     **路径无关**：source_identity 用 origin_namespace（"standalone" / "plugin:{id}"）而非绝对路径，
///     这样同一逻辑资产在 active 路径和 disabled 路径下拥有同一个 id，enable/disable 移动文件
///     不会让 id 漂移。绝对路径仍保留在 PortableInventoryItemDto.source_path 字段供 UI 显示。
///
/// Code Logic（这个函数做什么）:
///     `sha256_hex(target|scope|sourceIdentity|nativeId)` 小写 hex。
pub fn inventory_item_id(
    target: AgentTarget,
    scope: &str,
    source_identity: &str,
    native_id: &str,
) -> String {
    let material = format!(
        "{}|{}|{}|{}",
        target.as_str(),
        scope,
        source_identity,
        native_id
    );
    sha256_hex(material.as_bytes())
}

/// 计算确定性 inventory snapshot hash。
///
/// Business Logic（为什么需要这个函数）:
///     Preview/apply 绑定 inventory hash；插入顺序无关，事实变化必须换 hash。
///     覆盖 CLI 指纹、scope/opt-in、原生 ID、路径、hash、启用、来源、对账/ownership 摘要。
///
/// Code Logic（这个函数做什么）:
///     构造排序后的 material → serde_json Value → canonicalize_value → sha256_hex。
///     不含 `refreshedAt`/`stale` 时间戳字段。
pub fn inventory_snapshot_hash(
    targets: &[PortableInventoryTargetDto],
    items: &[PortableInventoryItemDto],
) -> Result<String, AppError> {
    let mut target_materials: Vec<TargetHashMaterial> = targets
        .iter()
        .map(|t| TargetHashMaterial {
            target: t.target.as_str().to_string(),
            installed: t.installed,
            version: t.version.clone(),
            executable: t.executable.clone(),
            config_root: t.config_root.clone(),
            scan_capability: t.scan_capability.as_str().to_string(),
            mutation_capability: t.mutation_capability.as_str().to_string(),
            reason_code: t.reason_code.clone(),
            evidence_ids: {
                let mut e = t.evidence_ids.clone();
                e.sort();
                e
            },
        })
        .collect();
    target_materials.sort_by(|a, b| {
        a.target
            .cmp(&b.target)
            .then_with(|| a.config_root.cmp(&b.config_root))
            .then_with(|| a.executable.cmp(&b.executable))
    });

    let mut item_materials: Vec<ItemHashMaterial> = items
        .iter()
        .map(|i| ItemHashMaterial {
            inventory_item_id: i.inventory_item_id.clone(),
            target: i.target.as_str().to_string(),
            origin_kind: i.origin_kind.as_str().to_string(),
            owned_by: i.owned_by.as_str().to_string(),
            native_output_candidate: i.native_output_candidate,
            kind: i.kind.as_str().to_string(),
            native_id: i.native_id.clone(),
            scope_id: i.scope_id.clone(),
            scope_kind: i.scope_kind.as_str().to_string(),
            project_id: i.project_id.clone(),
            project_opted_in: i.project_opted_in,
            source_path: i.source_path.clone(),
            source_origin: i.source_origin.as_str().to_string(),
            parent_plugin_inventory_item_id: i.parent_plugin_inventory_item_id.clone(),
            actual_enabled: i.actual_enabled,
            content_hash: i.content_hash.clone(),
            tree_hash: i.tree_hash.clone(),
            canonical_asset_id: i.canonical_asset_id.clone(),
            canonical_revision_id: i.canonical_revision_id.clone(),
            management_state: i.management_state.as_str().to_string(),
            desired_presence: i.desired_presence.map(|p| p.as_str().to_string()),
            desired_enabled: i.desired_enabled,
            materialization_status: i.materialization_status.clone(),
            warnings: {
                let mut w = i.warnings.clone();
                w.sort();
                w
            },
            mcp_credential_present: i.mcp_credential.as_ref().map(|c| c.present),
            mcp_credential_hash: i.mcp_credential.as_ref().and_then(|c| c.hash.clone()),
            capabilities: CapabilityHashMaterial {
                can_enable: i.capabilities.can_enable,
                can_disable: i.capabilities.can_disable,
                can_uninstall: i.capabilities.can_uninstall,
                can_adopt: i.capabilities.can_adopt,
                can_install_to_source_target: i.capabilities.can_install_to_source_target,
                can_migrate_to_store: i.capabilities.can_migrate_to_store,
                can_attach: i.capabilities.can_attach,
                can_detach: i.capabilities.can_detach,
                can_destroy_store: i.capabilities.can_destroy_store,
                can_confirm_current_version: i.capabilities.can_confirm_current_version,
                can_materialize_escape_link: i.capabilities.can_materialize_escape_link,
                reason_code: i.capabilities.reason_code.clone(),
                evidence_ids: {
                    let mut e = i.capabilities.evidence_ids.clone();
                    e.sort();
                    e
                },
            },
            store_id: i.store.store_id.clone(),
            store_attached: i.store.store_attached,
            loaded_via_other_path: i.store.loaded_via_other_path,
            loaded_via_target: i.store.loaded_via_target.map(|t| t.as_str().to_string()),
        })
        .collect();
    item_materials.sort_by(|a, b| {
        a.inventory_item_id
            .cmp(&b.inventory_item_id)
            .then_with(|| a.target.cmp(&b.target))
            .then_with(|| a.native_id.cmp(&b.native_id))
    });

    let material = SnapshotHashMaterial {
        targets: target_materials,
        items: item_materials,
    };
    let value = serde_json::to_value(&material)
        .map_err(|e| AppError::generic(format!("portable_inventory_hash_to_value:{e}")))?;
    let bytes = canonicalize_value(&value)
        .map_err(|e| AppError::validation(format!("portable_inventory_hash_canon:{e}")))?;
    Ok(sha256_hex(&bytes))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotHashMaterial {
    targets: Vec<TargetHashMaterial>,
    items: Vec<ItemHashMaterial>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TargetHashMaterial {
    target: String,
    installed: bool,
    version: Option<String>,
    executable: Option<String>,
    config_root: String,
    scan_capability: String,
    mutation_capability: String,
    reason_code: Option<String>,
    evidence_ids: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ItemHashMaterial {
    inventory_item_id: String,
    target: String,
    origin_kind: String,
    owned_by: String,
    native_output_candidate: bool,
    kind: String,
    native_id: String,
    scope_id: String,
    scope_kind: String,
    project_id: Option<String>,
    project_opted_in: bool,
    source_path: Option<String>,
    source_origin: String,
    parent_plugin_inventory_item_id: Option<String>,
    actual_enabled: Option<bool>,
    content_hash: Option<String>,
    tree_hash: Option<String>,
    canonical_asset_id: Option<String>,
    canonical_revision_id: Option<String>,
    management_state: String,
    desired_presence: Option<String>,
    desired_enabled: Option<bool>,
    materialization_status: Option<String>,
    warnings: Vec<String>,
    mcp_credential_present: Option<bool>,
    mcp_credential_hash: Option<String>,
    capabilities: CapabilityHashMaterial,
    store_id: Option<String>,
    store_attached: bool,
    loaded_via_other_path: bool,
    loaded_via_target: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CapabilityHashMaterial {
    can_enable: bool,
    can_disable: bool,
    can_uninstall: bool,
    can_adopt: bool,
    can_install_to_source_target: bool,
    can_migrate_to_store: bool,
    can_attach: bool,
    can_detach: bool,
    can_destroy_store: bool,
    can_confirm_current_version: bool,
    can_materialize_escape_link: bool,
    reason_code: Option<String>,
    evidence_ids: Vec<String>,
}
