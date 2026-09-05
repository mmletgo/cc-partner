//! agent_hub/targets/portable — Skill/Command/Agent/MCP 可移植扫描与渲染共享层（目录模块）
//!
//! Business Logic（为什么需要这个模块）:
//!     Gate B 把 Skill/Command/Agent/MCP 纳入 Canonical Hub：各 CLI 适配器需在隔离 home 上
//!     只读扫描原生与兼容路径，产出带 origin path/hash/status 的发现记录；未知 frontmatter
//!     字段进入 target_extensions，未知文件记诊断；扫描不得写盘。
//!
//! Code Logic（这个模块做什么）:
//!     本目录由原单文件 portable.rs 按职责拆分：mod.rs 保留核心 DTO/枚举与分类、stamp
//!     杂项；frontmatter.rs 负责 frontmatter 解析；skill_scan.rs 负责 Skill 目录扫描与树
//!     hash；markdown_scan.rs 负责 Command/Agent Markdown 扫描；mcp_parse.rs 负责 MCP
//!     JSON/TOML 解析；render.rs 负责投影渲染；tests.rs 为内嵌单测。对外模块路径
//!     `crate::agent_hub::targets::portable::*` 经本文件的 `pub use` 保持不变。

mod frontmatter;
mod markdown_scan;
mod mcp_parse;
mod render;
mod skill_scan;

#[cfg(test)]
mod tests;

pub use frontmatter::{parse_simple_frontmatter, unknown_fields_extension};
pub use markdown_scan::{
    scan_agent_markdown_dir, scan_command_markdown_dir, scan_disabled_command_markdown_dir,
};
pub use mcp_parse::{
    parse_codex_agents_toml, parse_codex_mcp_toml, parse_json_or_jsonc, parse_mcp_servers_json_map,
};
pub use render::{
    claude_user_mcp_config_path, render_agent_projection, render_command_projection,
    render_mcp_projection, render_portable_payload, render_skill_projection,
};
pub use skill_scan::{
    hash_skill_directory, hash_skill_directory_dereferenced, scan_disabled_skill_dirs,
    scan_disabled_skill_dirs_manifest_only, scan_skill_dirs, scan_skill_dirs_manifest_only,
};

use crate::{
    agent_hub::{
        assets::{PortabilityDiagnostic, PortableAssetPayload},
        models::{AgentTarget, AssetKind, ScopeKind},
        portable_store::{classify_store_link_with_ancestors, StoreLinkClass},
    },
    error::AppError,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 单测串行化 `CC_PARTNER_DATA_DIR` 覆盖，避免跨 adapter 并行污染。
///
/// Business Logic（为什么需要这个类型）:
///     本模块族（portable_store / portable_actions targets）的测试裸写
///     `CC_PARTNER_DATA_DIR`；历史上这里是一把独立 static 锁，与 config.rs
///     `install_data_dir_env` 的锁互不互斥，两族并行时会在 user_mirror 等
///     测试 `build_app_state` 的 await 中途覆写变量造成挂死。现统一委托
///     config 的全 crate 唯一 data_dir 测试锁。
///
/// Code Logic（这个类型做什么）:
///     零尺寸标记类型，`lock()` 转发到 `crate::config::data_dir_env_lock()`
///     并原样返回 `LockResult`；调用点保持 `DATA_DIR_ENV_LOCK.lock()…` 语法。
#[cfg(test)]
pub(crate) struct DataDirEnvLock;

#[cfg(test)]
impl DataDirEnvLock {
    /// 取全局 data_dir 测试锁；返回 `Result` 以保持既有调用点
    /// `.lock().unwrap()` / `.lock().unwrap_or_else(...)` 语法不变。
    #[cfg(test)]
    pub(crate) fn lock(
        &self,
    ) -> Result<
        std::sync::MutexGuard<'static, ()>,
        std::sync::PoisonError<std::sync::MutexGuard<'static, ()>>,
    > {
        crate::config::data_dir_env_lock().lock()
    }
}

#[cfg(test)]
pub(crate) use DataDirEnvLock as DATA_DIR_ENV_LOCK;

/// 发现来源分类。
///
/// Business Logic（为什么需要这个枚举）:
///     原生路径可作为后续物化候选；兼容/legacy 路径只作 adoption 输入，不得当 native 写出。
///
/// Code Logic（这个枚举做什么）:
///     camelCase：`native` / `compatibility` / `legacyStandalone` / `plugin`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PortableOriginKind {
    /// 该 target 原生路径（可成为受管投影目标）
    #[default]
    Native,
    /// 兼容扫描路径（如 OpenCode 对 `.claude/skills` / `.agents/skills`）
    Compatibility,
    /// 遗留独立 skill 根（如 Codex 对 `.agents/skills`）
    LegacyStandalone,
    /// Plugin 包内提供的组件
    Plugin,
}

/// 可移植资产的所有权身份。
///
/// Business Logic（为什么需要这个枚举）:
///     库存必须区分「谁拥有这份文件」与「谁加载了它」；兼容/共享根不得被当成当前
///     target 的 native 写出目标。
///
/// Code Logic（这个枚举做什么）:
///     camelCase wire；OpenCode 与 `AgentTarget` 一样 rename 为 `opencode`；
///     `from_target` 映射 Hub target；缺省 `unknown`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PortableAssetOwner {
    /// Claude Code
    Claude,
    /// Codex CLI
    Codex,
    /// OpenCode CLI
    #[serde(rename = "opencode")]
    OpenCode,
    /// Grok Build
    Grok,
    /// Gemini CLI
    Gemini,
    /// Cursor CLI
    Cursor,
    /// Pi Coding Agent
    Pi,
    /// 共享 `~/.agents` 根
    SharedAgents,
    /// Hub portable-store 真树（各 Agent 仅挂软链/投影）
    PortableStore,
    /// 无法判定所有者
    #[default]
    Unknown,
}

impl PortableAssetOwner {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Grok => "grok",
            Self::Gemini => "gemini",
            Self::Cursor => "cursor",
            Self::Pi => "pi",
            Self::SharedAgents => "sharedAgents",
            Self::PortableStore => "portableStore",
            Self::Unknown => "unknown",
        }
    }

    /// 从扫描 target 推导 native 所有者。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     原生构造路径上 ownedBy 必须等于加载该资产的 target，避免默认 unknown。
    ///
    /// Code Logic（这个函数做什么）:
    ///     一对一映射 `AgentTarget` → 同名 owner。
    pub fn from_target(target: AgentTarget) -> Self {
        match target {
            AgentTarget::Claude => Self::Claude,
            AgentTarget::Codex => Self::Codex,
            AgentTarget::OpenCode => Self::OpenCode,
            AgentTarget::Grok => Self::Grok,
            AgentTarget::Gemini => Self::Gemini,
            AgentTarget::Cursor => Self::Cursor,
            AgentTarget::Pi => Self::Pi,
        }
    }

    /// 把 Hub 所有者映射回 AgentTarget；共享 `~/.agents` / unknown 没有一对一 target。
    pub fn as_hub_target(self) -> Option<AgentTarget> {
        match self {
            Self::Claude => Some(AgentTarget::Claude),
            Self::Codex => Some(AgentTarget::Codex),
            Self::OpenCode => Some(AgentTarget::OpenCode),
            Self::Grok => Some(AgentTarget::Grok),
            Self::Gemini => Some(AgentTarget::Gemini),
            Self::Cursor => Some(AgentTarget::Cursor),
            Self::Pi => Some(AgentTarget::Pi),
            Self::SharedAgents | Self::PortableStore | Self::Unknown => None,
        }
    }
}

/// 运行时从其他 Agent / 共享目录加载的项：启停卸载必须改写所有者磁盘。
///
/// Business Logic（为什么需要这个函数）:
///     「借用」只描述所有权/扫描根，不描述 Hub 一致性。漂移（hash 偏离）仍是
///     本 Agent 已安装资产，不得因为 nativeOutputCandidate=false 或
///     legacyStandalone（如 Codex `~/.agents/skills`）被当成外借。
///
/// Code Logic（这个函数做什么）:
///     compatibility 根、sharedAgents、其他 Hub owner、或 store 挂在兼容路径
///     → true。同 Agent 的 native/legacy/plugin 即使不能 native 写出也不是借用。
///     `native_output_candidate` 只保留给调用方签名对齐，不再单独判借用。
pub fn is_borrowed_runtime_origin(
    viewing: AgentTarget,
    owned_by: PortableAssetOwner,
    _native_output_candidate: bool,
    origin_kind: PortableOriginKind,
) -> bool {
    if origin_kind == PortableOriginKind::Compatibility {
        return true;
    }
    if owned_by == PortableAssetOwner::SharedAgents {
        return true;
    }
    if owned_by == PortableAssetOwner::PortableStore {
        return origin_kind == PortableOriginKind::Compatibility;
    }
    owned_by
        .as_hub_target()
        .is_some_and(|owner| owner != viewing)
}

/// 借用项的真实写盘 target：Hub 所有者，或共享 `~/.agents` 走 Codex adapter。
///
/// Business Logic（为什么需要这个函数）:
///     Grok 列表里的 Claude skill 必须用 Claude 目录语义 disable；
///     `~/.agents/skills` 由 Codex executor 认路径（active↔hub disabled）。
///
/// Code Logic（这个函数做什么）:
///     SharedAgents → Codex；其他 Hub owner 且非本 target native → owner；否则 viewing。
pub fn mutation_target_for_origin(
    viewing: AgentTarget,
    owned_by: PortableAssetOwner,
    native_output_candidate: bool,
) -> AgentTarget {
    if owned_by == PortableAssetOwner::SharedAgents {
        return AgentTarget::Codex;
    }
    match owned_by.as_hub_target() {
        Some(owner) if !native_output_candidate || owner != viewing => owner,
        Some(_) | None => viewing,
    }
}

/// 按动作选择写盘 target：plugin 启停跟当前 Agent，卸载/技能移动仍跟所有者。
///
/// Business Logic（为什么需要这个函数）:
///     每个 Agent 有自己的 plugin 开关；在 Codex/Grok/OpenCode 里禁用不得去改 Claude 标记。
///     卸载仍改所有者磁盘。
///
/// Code Logic（这个函数做什么）:
///     Plugin 且 enablement_action → viewing；否则 `mutation_target_for_origin`。
pub fn mutation_target_for_action(
    viewing: AgentTarget,
    owned_by: PortableAssetOwner,
    native_output_candidate: bool,
    kind: AssetKind,
    enablement_action: bool,
) -> AgentTarget {
    if kind == AssetKind::Plugin && enablement_action {
        return viewing;
    }
    mutation_target_for_origin(viewing, owned_by, native_output_candidate)
}

impl PortableOriginKind {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Compatibility => "compatibility",
            Self::LegacyStandalone => "legacyStandalone",
            Self::Plugin => "plugin",
        }
    }

    /// 是否可作为本 target 的 native 输出候选。
    pub fn is_native_output_candidate(self) -> bool {
        matches!(self, Self::Native | Self::Plugin)
    }
}

/// 发现状态。
///
/// Business Logic（为什么需要这个枚举）:
///     active 表示当前 CLI 可直接消费；discovered 仅登记；blocked 表示不可安全采用。
///
/// Code Logic（这个枚举做什么）:
///     camelCase：`active` / `discovered` / `disabled` / `blocked`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableDiscoveryStatus {
    /// 当前启用且可被 CLI 发现
    Active,
    /// 已发现但未判定启用
    Discovered,
    /// 用户或系统禁用
    Disabled,
    /// 解析失败或被安全门挡住
    Blocked,
}

impl PortableDiscoveryStatus {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Discovered => "discovered",
            Self::Disabled => "disabled",
            Self::Blocked => "blocked",
        }
    }
}

/// 单个 portable 资产的 origin 记录。
///
/// Business Logic（为什么需要这个结构体）:
///     adoption 与 UI 需要精确 source path、target、native ID、content/tree hash 与状态。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；携带 path/hash/status/origin_kind/native_id。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableAssetOrigin {
    /// 发现所属 target
    pub target: AgentTarget,
    /// 源文件或目录绝对路径
    pub path: PathBuf,
    /// origin 分类
    pub origin_kind: PortableOriginKind,
    /// 目标侧原生 ID（通常为目录名或 server key）
    pub native_id: String,
    /// 主内容 hash（Skill 为 SKILL.md；单文件为正文；MCP 为配置 canonical 字节）
    pub content_hash: String,
    /// Skill 树 manifest hash；非目录资产为 None
    pub tree_hash: Option<String>,
    /// 发现状态
    pub status: PortableDiscoveryStatus,
    /// 是否可作为该 target 的 native 输出候选
    pub native_output_candidate: bool,
    /// 资产所有者（兼容/共享根可与 `target` 不同）
    #[serde(default)]
    pub owned_by: PortableAssetOwner,
    /// 父 Plugin 原生 ID（component 专用；standalone 为 None）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_plugin_id: Option<String>,
}

/// 为已扫描发现打上所有者。
///
/// Business Logic（为什么需要这个函数）:
///     共享扫描 helper 默认按 target 填 ownedBy；表驱动/兼容根需要覆盖为真实所有者。
///
/// Code Logic（这个函数做什么）:
///     就地写入 `origin.owned_by`。
pub fn stamp_owned_by(assets: &mut [DiscoveredPortableAsset], owner: PortableAssetOwner) {
    for asset in assets {
        if asset.origin.owned_by != PortableAssetOwner::PortableStore {
            asset.origin.owned_by = owner;
        }
    }
}

/// 按发现表覆盖 origin 分类、所有者与 native 写出资格。
///
/// Business Logic（为什么需要这个函数）:
///     兼容/legacy 根绝不能成为 native 写出目标；表是权威，扫描 helper 的默认值必须被覆盖。
///
/// Code Logic（这个函数做什么）:
///     写入 owned_by / origin_kind，并用 `origin_kind.is_native_output_candidate()` 重算写出资格。
pub fn stamp_table_origin(
    assets: &mut [DiscoveredPortableAsset],
    owner: PortableAssetOwner,
    origin_kind: PortableOriginKind,
) {
    let native_output_candidate = origin_kind.is_native_output_candidate();
    for asset in assets {
        if asset.origin.owned_by != PortableAssetOwner::PortableStore {
            asset.origin.owned_by = owner;
        }
        asset.origin.origin_kind = origin_kind;
        asset.origin.native_output_candidate = native_output_candidate;
    }
}

/// 扫描到的可移植资产（含 payload 与 origin）。
///
/// Business Logic（为什么需要这个结构体）:
///     同名多 origin 在 adoption 前必须作为独立发现保留，不能静默合并。
///
/// Code Logic（这个结构体做什么）:
///     聚合 kind/semantic_name/payload/origin/diagnostics。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredPortableAsset {
    /// 资产类别
    pub kind: AssetKind,
    /// 语义名（frontmatter name 或文件 stem）
    pub semantic_name: String,
    /// scope
    pub scope_kind: ScopeKind,
    /// typed payload
    pub payload: PortableAssetPayload,
    /// origin 记录
    pub origin: PortableAssetOrigin,
    /// 未知文件/字段等诊断（无凭据原文）
    pub diagnostics: Vec<PortabilityDiagnostic>,
}

/// 渲染上下文（Gate B Task 3 最小集）。
///
/// Business Logic（为什么需要这个结构体）:
///     render 需要知道投影目标根与是否受管 plugin 路径。
///
/// Code Logic（这个结构体做什么）:
///     可选 package_root / relative_key / desired_enabled。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssetRenderContext {
    /// 投影根目录（plugin 或 config root 子树）
    pub package_root: Option<PathBuf>,
    /// 逻辑相对键
    pub relative_key: Option<String>,
    /// 期望启用
    #[serde(default = "default_true")]
    pub desired_enabled: bool,
}

fn default_true() -> bool {
    true
}

/// 单 target 上的文件/配置投影计划（不写盘）。
///
/// Business Logic（为什么需要这个结构体）:
///     projection 后续 task 消费 relative path + bytes；本任务只生成计划。
///
/// Code Logic（这个结构体做什么）:
///     target + files + config patches + diagnostics。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetAssetProjection {
    /// 目标
    pub target: AgentTarget,
    /// 文件投影（相对 package_root 或 config_root）
    pub files: Vec<ProjectedAssetFile>,
    /// 诊断
    #[serde(default)]
    pub diagnostics: Vec<PortabilityDiagnostic>,
}

/// 单个投影文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedAssetFile {
    /// 相对路径（正斜杠）
    pub relative_path: String,
    /// UTF-8 或原文字节
    pub bytes: Vec<u8>,
}

/// store 软链归 portableStore；其余按扫描 target。
///
/// Business Logic: 真树属于 Hub store，不能再标成 Claude/Grok 私有副本。
/// Code Logic: `classify_store_link` 命中 StoreLink → PortableStore。
fn store_or_target_owner(target: AgentTarget, path: &Path) -> PortableAssetOwner {
    if matches!(
        classify_store_link_with_ancestors(path),
        StoreLinkClass::StoreLink { .. }
    ) {
        PortableAssetOwner::PortableStore
    } else {
        PortableAssetOwner::from_target(target)
    }
}

/// 合并多个发现列表。
pub fn merge_discoveries(
    parts: impl IntoIterator<Item = Vec<DiscoveredPortableAsset>>,
) -> Vec<DiscoveredPortableAsset> {
    let mut out = Vec::new();
    for part in parts {
        out.extend(part);
    }
    out
}

/// 为 Plugin 目录下组件打 parent_plugin_id。
///
/// Business Logic: component 保持独立身份并记录父 package。
/// Code Logic: 写 origin.parent_plugin_id + origin_kind=Plugin。
pub fn stamp_parent_plugin(discoveries: &mut [DiscoveredPortableAsset], plugin_id: &str) {
    for d in discoveries {
        d.origin.parent_plugin_id = Some(plugin_id.to_string());
        d.origin.origin_kind = PortableOriginKind::Plugin;
    }
}

/// 只读扫描 plugin 根下 skills/commands（不写 CAS）。
///
/// Business Logic: inventory 需要 package component 事实，扫描不得 materialize CAS。
/// Code Logic: scan skills/commands 后 stamp parent。
pub fn scan_plugin_components_readonly(
    target: AgentTarget,
    scope_kind: ScopeKind,
    plugin_root: &Path,
    plugin_id: &str,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    scan_plugin_components_readonly_filtered(target, scope_kind, plugin_root, plugin_id, None)
}

/// 按组件 kind 扫描 plugin，避免 Command 页读取全部 Skill 树。
pub fn scan_plugin_components_readonly_filtered(
    target: AgentTarget,
    scope_kind: ScopeKind,
    plugin_root: &Path,
    plugin_id: &str,
    kind: Option<AssetKind>,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let mut parts = Vec::new();
    if kind.is_none() || kind == Some(AssetKind::Skill) {
        let mut skills = if kind == Some(AssetKind::Skill) {
            scan_skill_dirs_manifest_only(
                target,
                scope_kind,
                &plugin_root.join("skills"),
                PortableOriginKind::Plugin,
            )?
        } else {
            scan_skill_dirs(
                target,
                scope_kind,
                &plugin_root.join("skills"),
                PortableOriginKind::Plugin,
            )?
        };
        stamp_parent_plugin(&mut skills, plugin_id);
        parts.push(skills);
    }
    if kind.is_none() || kind == Some(AssetKind::Command) {
        let mut commands = scan_command_markdown_dir(
            target,
            scope_kind,
            &plugin_root.join("commands"),
            PortableOriginKind::Plugin,
        )?;
        stamp_parent_plugin(&mut commands, plugin_id);
        parts.push(commands);
    }
    Ok(merge_discoveries(parts))
}
