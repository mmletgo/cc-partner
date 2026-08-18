//! agent_hub/targets/portable — Skill/Command/Agent/MCP 可移植扫描与渲染 DTO
//!
//! Business Logic（为什么需要这个模块）:
//!     Gate B 把 Skill/Command/Agent/MCP 纳入 Canonical Hub：各 CLI 适配器需在隔离 home 上
//!     只读扫描原生与兼容路径，产出带 origin path/hash/status 的发现记录；未知 frontmatter
//!     字段进入 target_extensions，未知文件记诊断；扫描不得写盘。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `DiscoveredPortableAsset` / origin / 渲染上下文与投影 DTO；提供 frontmatter 解析、
//!     Skill 树 hash、Markdown Command/Agent 解析、MCP JSON/TOML 解析与共享目录扫描 helper。

use crate::agent_hub::assets::{
    CommandArgument, McpTransport, PortabilityDiagnostic, PortableAgent, PortableAssetPayload,
    PortableCommand, PortableMcpServer, PortableSkill, CODE_UNKNOWN_SOURCE_FIELD,
};
use crate::agent_hub::config_patch::value_content_hash;
use crate::agent_hub::models::{AgentTarget, AssetKind, ScopeKind};
use crate::agent_hub::object_store::{sha256_hex, TreeEntry, TreeEntryType, TreeManifest};
use crate::agent_hub::portable_store::{classify_store_link, StoreLinkClass};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// 单测串行化 `CC_PARTNER_DATA_DIR` 覆盖，避免跨 adapter 并行污染。
#[cfg(test)]
pub(crate) static DATA_DIR_ENV_LOCK: Mutex<()> = Mutex::new(());

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

/// 解析 YAML frontmatter（`---` ... `---`）为 key→string map + 未知键列表。
///
/// Business Logic: Skill/Command/Agent 元数据在 frontmatter；未知键不得丢弃。
/// Code Logic: 仅处理简单 `key: value` 行，不引入完整 YAML 依赖。
pub fn parse_simple_frontmatter(text: &str) -> (BTreeMap<String, String>, Vec<String>, &str) {
    let trimmed = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !trimmed.starts_with("---") {
        return (BTreeMap::new(), Vec::new(), text);
    }
    let rest = &trimmed[3..];
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
        .unwrap_or(rest);
    let Some((front, body)) = rest
        .split_once("\n---")
        .or_else(|| rest.split_once("\r\n---"))
    else {
        return (BTreeMap::new(), Vec::new(), text);
    };
    let body = body
        .strip_prefix('\n')
        .or_else(|| body.strip_prefix("\r\n"))
        .unwrap_or(body);
    let mut map = BTreeMap::new();
    let mut unknown_order = Vec::new();
    for line in front.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim().to_string();
        let mut val = v.trim().to_string();
        if (val.starts_with('"') && val.ends_with('"'))
            || (val.starts_with('\'') && val.ends_with('\''))
        {
            val = val[1..val.len().saturating_sub(1)].to_string();
        }
        if !map.contains_key(&key) {
            unknown_order.push(key.clone());
        }
        map.insert(key, val);
    }
    (map, unknown_order, body)
}

/// 已知 frontmatter 键（不进入 unknown）。
const KNOWN_SKILL_KEYS: &[&str] = &["name", "description"];
const KNOWN_COMMAND_KEYS: &[&str] = &[
    "name",
    "description",
    "argument-hint",
    "argument_hint",
    "arguments",
    "allowed-tools",
    "model",
];
const KNOWN_AGENT_KEYS: &[&str] = &[
    "name",
    "description",
    "tools",
    "model",
    "mode",
    "permission",
    "permissions",
    "provider",
];

/// 从 map 收集未知键诊断与 target_extensions 对象。
///
/// Business Logic: 未知字段保留在 source target extension，并记 unknownSourceField。
/// Code Logic: 过滤 known keys；剩余进 JSON object + 诊断。
pub fn unknown_fields_extension(
    target: AgentTarget,
    fields: &BTreeMap<String, String>,
    known: &[&str],
    pointer_prefix: &str,
) -> (BTreeMap<AgentTarget, Value>, Vec<PortabilityDiagnostic>) {
    let mut ext_obj = serde_json::Map::new();
    let mut diags = Vec::new();
    for (k, v) in fields {
        if known.contains(&k.as_str()) {
            continue;
        }
        ext_obj.insert(k.clone(), Value::String(v.clone()));
        diags.push(
            PortabilityDiagnostic::new(
                CODE_UNKNOWN_SOURCE_FIELD,
                format!("{pointer_prefix}/{k}"),
                "unknown source frontmatter field retained in target_extensions",
            )
            .with_value_metadata(v),
        );
    }
    let mut extensions = BTreeMap::new();
    if !ext_obj.is_empty() {
        extensions.insert(target, Value::Object(ext_obj));
    }
    (extensions, diags)
}

/// 计算目录 TreeManifest（不写 CAS）并返回 manifest hash + skill_md hash。
///
/// Business Logic: discovery 需要稳定 content/tree hash，但 scan 不得写 objects 目录。
/// Code Logic: walk 文件；构建 sorted TreeManifest；hash(JSON) 与 SKILL.md 字节 hash。
pub fn hash_skill_directory(
    dir: &Path,
) -> Result<(String, String, TreeManifest, Vec<PortabilityDiagnostic>), AppError> {
    let mut entries = Vec::new();
    let mut diagnostics = Vec::new();
    let mut skill_md_hash: Option<String> = None;
    walk_files(dir, dir, &mut entries, &mut diagnostics, &mut skill_md_hash)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let Some(skill_hash) = skill_md_hash else {
        return Err(AppError::validation(
            "agent_hub_portable_skill_tree_missing_skill_md".to_string(),
        ));
    };
    let manifest = TreeManifest { entries };
    let bytes = serde_json::to_vec(&manifest)
        .map_err(|e| AppError::generic(format!("tree manifest serialize: {e}")))?;
    let tree_hash = sha256_hex(&bytes);
    Ok((skill_hash, tree_hash, manifest, diagnostics))
}

type SkillHashResult = (String, String, TreeManifest, Vec<PortabilityDiagnostic>);

#[derive(Clone)]
struct CachedSkillHash {
    metadata_fingerprint: String,
    result: SkillHashResult,
}

/// 只读 discovery 专用增量 hash；adoption/action 仍调用未缓存函数重新验证内容。
fn hash_skill_directory_cached(dir: &Path) -> Result<SkillHashResult, AppError> {
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, CachedSkillHash>>> = OnceLock::new();
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let metadata_fingerprint = super::tree_metadata::tree_metadata_fingerprint(dir)?;
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
        .filter(|entry| entry.metadata_fingerprint == metadata_fingerprint)
        .cloned()
    {
        return Ok(hit.result);
    }
    let result = hash_skill_directory(dir)?;
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.len() >= 2_048 && !guard.contains_key(&key) {
        guard.clear();
    }
    guard.insert(
        key,
        CachedSkillHash {
            metadata_fingerprint,
            result: result.clone(),
        },
    );
    Ok(result)
}

fn walk_files(
    root: &Path,
    current: &Path,
    entries: &mut Vec<TreeEntry>,
    diagnostics: &mut Vec<PortabilityDiagnostic>,
    skill_md_hash: &mut Option<String>,
) -> Result<(), AppError> {
    let read = match fs::read_dir(current) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let mut children: Vec<_> = read.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|e| e.file_name());
    for entry in children {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            match classify_store_link(&path) {
                StoreLinkClass::StoreLink { .. } => {
                    let followed = match fs::metadata(&path) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    if followed.is_dir() {
                        walk_files(root, &path, entries, diagnostics, skill_md_hash)?;
                        continue;
                    }
                    if followed.is_file() {
                        let bytes = fs::read(&path)?;
                        let hash = sha256_hex(&bytes);
                        let rel = relative_posix(root, &path);
                        if rel == "SKILL.md" || rel.ends_with("/SKILL.md") {
                            *skill_md_hash = Some(hash.clone());
                        }
                        entries.push(TreeEntry {
                            path: rel,
                            blob_hash: hash,
                            entry_type: TreeEntryType::File,
                            executable: is_executable(&followed),
                        });
                        continue;
                    }
                }
                StoreLinkClass::EscapeLink | StoreLinkClass::Regular => {
                    diagnostics.push(PortabilityDiagnostic::new(
                        "store_symlink_escape",
                        relative_posix(root, &path),
                        "symlink outside portable-store rejected",
                    ));
                    continue;
                }
            }
            continue;
        }
        if meta.is_dir() {
            walk_files(root, &path, entries, diagnostics, skill_md_hash)?;
            continue;
        }
        if !meta.is_file() {
            diagnostics.push(PortabilityDiagnostic::new(
                CODE_UNKNOWN_SOURCE_FIELD,
                relative_posix(root, &path),
                "unknown non-file entry in skill tree",
            ));
            continue;
        }
        let bytes = fs::read(&path)?;
        let hash = sha256_hex(&bytes);
        let rel = relative_posix(root, &path);
        if rel == "SKILL.md" || rel.ends_with("/SKILL.md") {
            *skill_md_hash = Some(hash.clone());
        }
        let executable = is_executable(&meta);
        if executable {
            diagnostics.push(PortabilityDiagnostic::target_executable(format!(
                "tree/{rel}"
            )));
        }
        entries.push(TreeEntry {
            path: rel,
            blob_hash: hash,
            entry_type: TreeEntryType::File,
            executable,
        });
    }
    Ok(())
}

fn is_executable(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

fn relative_posix(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

/// 扫描含 SKILL.md 的子目录为 Skill 发现。
///
/// Business Logic: 每个子目录是独立 origin；同名目录在不同根上保持分离。
/// Code Logic: read_dir → 有 SKILL.md 则 parse + hash。
pub fn scan_skill_dirs(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    scan_skill_dirs_with_mode(target, scope_kind, root, origin_kind, false)
}

/// Inventory 列表专用 Skill 扫描：读取 SKILL.md 身份，目录树延迟到动作 preview。
pub fn scan_skill_dirs_manifest_only(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    scan_skill_dirs_with_mode(target, scope_kind, root, origin_kind, true)
}

fn scan_skill_dirs_with_mode(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
    defer_tree_hash: bool,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    if !root.is_dir() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            match classify_store_link(&path) {
                StoreLinkClass::StoreLink { .. } => {
                    // 包根 store 软链：跟随并按目录扫描。
                }
                StoreLinkClass::EscapeLink | StoreLinkClass::Regular => {
                    out.push(blocked_escape_skill(target, scope_kind, origin_kind, &path));
                    continue;
                }
            }
        } else if !meta.is_dir() {
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let skill_bytes = match fs::read(&skill_md) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::debug!(
                    target = "agent_hub.portable",
                    %error,
                    path = %skill_md.display(),
                    "skip unreadable skill manifest"
                );
                continue;
            }
        };
        let text = String::from_utf8(skill_bytes.clone()).unwrap_or_default();
        let (fields, _order, _body) = parse_simple_frontmatter(&text);
        let dir_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("skill")
            .to_string();
        let name = fields
            .get("name")
            .cloned()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| dir_name.clone());
        let description = fields.get("description").cloned().unwrap_or_default();
        let (skill_hash, tree_hash, payload_tree_hash, mut diags) = if defer_tree_hash {
            let skill_hash = sha256_hex(&skill_bytes);
            (
                skill_hash.clone(),
                None,
                format!("deferred:{skill_hash}"),
                Vec::new(),
            )
        } else {
            let (skill_hash, tree_hash, _manifest, diagnostics) =
                match hash_skill_directory_cached(&path) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!(
                            target = "agent_hub.portable",
                            error = %e,
                            "skip skill dir without valid tree"
                        );
                        continue;
                    }
                };
            (skill_hash, Some(tree_hash.clone()), tree_hash, diagnostics)
        };
        let (extensions, field_diags) =
            unknown_fields_extension(target, &fields, KNOWN_SKILL_KEYS, "/frontmatter");
        diags.extend(field_diags);
        let payload = PortableAssetPayload::Skill(PortableSkill {
            name: name.clone(),
            description,
            skill_markdown_hash: skill_hash.clone(),
            tree_manifest_hash: payload_tree_hash,
            target_extensions: extensions,
        });
        out.push(DiscoveredPortableAsset {
            kind: AssetKind::Skill,
            semantic_name: name.clone(),
            scope_kind,
            payload,
            origin: PortableAssetOrigin {
                target,
                owned_by: store_or_target_owner(target, &path),
                path,
                origin_kind,
                native_id: dir_name,
                content_hash: skill_hash,
                tree_hash,
                status: PortableDiscoveryStatus::Active,
                native_output_candidate: origin_kind.is_native_output_candidate(),
                parent_plugin_id: None,
            },
            diagnostics: diags,
        });
    }
    Ok(out)
}

/// 扫描 `*.md` 目录为 Command。
///
/// Business Logic: 文件 stem 为 native id；frontmatter name 可覆盖语义名。
/// Code Logic: 读 md → frontmatter + body → PortableCommand。
pub fn scan_command_markdown_dir(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    if !root.is_dir() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink()
                && !matches!(classify_store_link(&path), StoreLinkClass::StoreLink { .. })
            {
                out.push(blocked_escape_command(
                    target,
                    scope_kind,
                    origin_kind,
                    &path,
                ));
                continue;
            }
        }
        let bytes = fs::read(&path)?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let content_hash = sha256_hex(&bytes);
        let (fields, _, body) = parse_simple_frontmatter(&text);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("command")
            .to_string();
        let name = fields
            .get("name")
            .cloned()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| stem.clone());
        let description = fields.get("description").cloned();
        let (extensions, diags) =
            unknown_fields_extension(target, &fields, KNOWN_COMMAND_KEYS, "/frontmatter");
        let arguments = parse_argument_hint(
            fields
                .get("argument-hint")
                .or_else(|| fields.get("argument_hint")),
        );
        let payload_cmd = PortableCommand {
            name: name.clone(),
            description,
            prompt_template: body.to_string(),
            arguments,
            target_extensions: extensions,
        };
        let mut all_diags = diags;
        all_diags.extend(payload_cmd.collect_diagnostics());
        if payload_cmd.validate().is_err() {
            continue;
        }
        out.push(DiscoveredPortableAsset {
            kind: AssetKind::Command,
            semantic_name: name,
            scope_kind,
            payload: PortableAssetPayload::Command(payload_cmd),
            origin: PortableAssetOrigin {
                target,
                owned_by: store_or_target_owner(target, &path),
                path,
                origin_kind,
                native_id: stem,
                content_hash,
                tree_hash: None,
                status: PortableDiscoveryStatus::Active,
                native_output_candidate: origin_kind.is_native_output_candidate(),
                parent_plugin_id: None,
            },
            diagnostics: all_diags,
        });
    }
    Ok(out)
}

/// store 软链归 portableStore；其余按扫描 target。
///
/// Business Logic: 真树属于 Hub store，不能再标成 Claude/Grok 私有副本。
/// Code Logic: `classify_store_link` 命中 StoreLink → PortableStore。
fn store_or_target_owner(target: AgentTarget, path: &Path) -> PortableAssetOwner {
    if matches!(classify_store_link(path), StoreLinkClass::StoreLink { .. }) {
        PortableAssetOwner::PortableStore
    } else {
        PortableAssetOwner::from_target(target)
    }
}

/// 逃逸 skill 包根：记 blocked，不跟随哈希。
fn blocked_escape_skill(
    target: AgentTarget,
    scope_kind: ScopeKind,
    origin_kind: PortableOriginKind,
    path: &Path,
) -> DiscoveredPortableAsset {
    let dir_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("skill")
        .to_string();
    DiscoveredPortableAsset {
        kind: AssetKind::Skill,
        semantic_name: dir_name.clone(),
        scope_kind,
        payload: PortableAssetPayload::Skill(PortableSkill {
            name: dir_name.clone(),
            description: String::new(),
            skill_markdown_hash: String::new(),
            tree_manifest_hash: String::new(),
            target_extensions: BTreeMap::new(),
        }),
        origin: PortableAssetOrigin {
            target,
            path: path.to_path_buf(),
            origin_kind,
            native_id: dir_name,
            content_hash: String::new(),
            tree_hash: None,
            status: PortableDiscoveryStatus::Blocked,
            native_output_candidate: false,
            owned_by: PortableAssetOwner::Unknown,
            parent_plugin_id: None,
        },
        diagnostics: vec![PortabilityDiagnostic::new(
            "store_symlink_escape",
            relative_posix(path.parent().unwrap_or(path), path),
            "skill root symlink escapes portable-store",
        )],
    }
}

/// 逃逸 command 文件软链：记 blocked。
fn blocked_escape_command(
    target: AgentTarget,
    scope_kind: ScopeKind,
    origin_kind: PortableOriginKind,
    path: &Path,
) -> DiscoveredPortableAsset {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("command")
        .to_string();
    DiscoveredPortableAsset {
        kind: AssetKind::Command,
        semantic_name: stem.clone(),
        scope_kind,
        payload: PortableAssetPayload::Command(PortableCommand {
            name: stem.clone(),
            description: None,
            prompt_template: String::new(),
            arguments: vec![],
            target_extensions: BTreeMap::new(),
        }),
        origin: PortableAssetOrigin {
            target,
            path: path.to_path_buf(),
            origin_kind,
            native_id: stem,
            content_hash: String::new(),
            tree_hash: None,
            status: PortableDiscoveryStatus::Blocked,
            native_output_candidate: false,
            owned_by: PortableAssetOwner::Unknown,
            parent_plugin_id: None,
        },
        diagnostics: vec![PortabilityDiagnostic::new(
            "store_symlink_escape",
            relative_posix(path.parent().unwrap_or(path), path),
            "command symlink escapes portable-store",
        )],
    }
}

fn parse_argument_hint(hint: Option<&String>) -> Vec<CommandArgument> {
    let Some(h) = hint else {
        return vec![];
    };
    // 形如 "[version] [tag?]" 或 "version tag"
    let mut args = Vec::new();
    for token in h.split_whitespace() {
        let t = token.trim_matches(|c| c == '[' || c == ']' || c == '<' || c == '>');
        if t.is_empty() {
            continue;
        }
        let required = !token.contains('?') && !t.ends_with('?');
        let name = t.trim_end_matches('?').to_string();
        if name.is_empty() {
            continue;
        }
        args.push(CommandArgument {
            name,
            description: None,
            required,
        });
    }
    args
}

/// 扫描 `*.md` 目录为 Agent。
///
/// Business Logic: agents 目录 Markdown 与 command 类似，但 body 为 instructions。
/// Code Logic: frontmatter tools/mode/model → tool_intents / mode_intent / extensions。
pub fn scan_agent_markdown_dir(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    if !root.is_dir() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let bytes = fs::read(&path)?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let content_hash = sha256_hex(&bytes);
        let (fields, _, body) = parse_simple_frontmatter(&text);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("agent")
            .to_string();
        let name = fields
            .get("name")
            .cloned()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| stem.clone());
        let description = fields.get("description").cloned();
        let mode_intent = fields.get("mode").cloned();
        let tool_intents = fields
            .get("tools")
            .map(|s| {
                s.split([',', ' '])
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let (extensions, diags) =
            unknown_fields_extension(target, &fields, KNOWN_AGENT_KEYS, "/frontmatter");
        // model/permission 也进 extensions 以便 collect_diagnostics
        let mut extensions = extensions;
        let mut ext_obj = extensions
            .remove(&target)
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        if let Some(model) = fields.get("model") {
            ext_obj.insert("model".into(), Value::String(model.clone()));
        }
        if let Some(provider) = fields.get("provider") {
            ext_obj.insert("provider".into(), Value::String(provider.clone()));
        }
        if let Some(p) = fields
            .get("permission")
            .or_else(|| fields.get("permissions"))
        {
            ext_obj.insert("permissions".into(), Value::String(p.clone()));
        }
        if !ext_obj.is_empty() {
            extensions.insert(target, Value::Object(ext_obj));
        }
        let agent = PortableAgent {
            name: name.clone(),
            description,
            instructions: body.to_string(),
            mode_intent,
            tool_intents,
            target_extensions: extensions,
        };
        let mut all_diags = diags;
        all_diags.extend(agent.collect_diagnostics());
        if agent.validate().is_err() {
            continue;
        }
        out.push(DiscoveredPortableAsset {
            kind: AssetKind::Agent,
            semantic_name: name,
            scope_kind,
            payload: PortableAssetPayload::Agent(agent),
            origin: PortableAssetOrigin {
                target,
                path,
                origin_kind,
                native_id: stem,
                content_hash,
                tree_hash: None,
                status: PortableDiscoveryStatus::Active,
                native_output_candidate: origin_kind.is_native_output_candidate(),
                owned_by: PortableAssetOwner::from_target(target),
                parent_plugin_id: None,
            },
            diagnostics: all_diags,
        });
    }
    Ok(out)
}

/// 从 JSON 对象 map 解析 MCP servers（Claude / OpenCode `mcpServers`）。
///
/// Business Logic: 每个 server key 独立 origin；env/headers/url 原文进入 payload。
/// Code Logic: 支持 stdio/http 形态字段。
pub fn parse_mcp_servers_json_map(
    target: AgentTarget,
    scope_kind: ScopeKind,
    map: &serde_json::Map<String, Value>,
    config_path: &Path,
    origin_kind: PortableOriginKind,
    enabled_default: bool,
) -> Vec<DiscoveredPortableAsset> {
    let mut out = Vec::new();
    for (key, value) in map {
        match mcp_from_json_value(target, key, value, enabled_default) {
            Ok((server, diags)) => {
                // 与 portable action CAS（value_content_hash）同域，避免键序/规范差异假漂移
                let content_hash = value_content_hash(value);
                let enabled = server.enabled;
                out.push(DiscoveredPortableAsset {
                    kind: AssetKind::Mcp,
                    semantic_name: key.clone(),
                    scope_kind,
                    payload: PortableAssetPayload::Mcp(server),
                    origin: PortableAssetOrigin {
                        target,
                        path: config_path.to_path_buf(),
                        origin_kind,
                        native_id: key.clone(),
                        content_hash,
                        tree_hash: None,
                        status: if enabled {
                            PortableDiscoveryStatus::Active
                        } else {
                            PortableDiscoveryStatus::Disabled
                        },
                        native_output_candidate: origin_kind.is_native_output_candidate(),
                        owned_by: PortableAssetOwner::from_target(target),
                        parent_plugin_id: None,
                    },
                    diagnostics: diags,
                });
            }
            Err(e) => {
                tracing::debug!(
                    target = "agent_hub.portable",
                    key = %key,
                    error = %e,
                    "skip mcp server parse"
                );
            }
        }
    }
    out
}

fn mcp_from_json_value(
    target: AgentTarget,
    key: &str,
    value: &Value,
    enabled_default: bool,
) -> Result<(PortableMcpServer, Vec<PortabilityDiagnostic>), AppError> {
    let obj = value
        .as_object()
        .ok_or_else(|| AppError::validation("mcp_server_not_object"))?;
    let enabled = obj
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(enabled_default);
    let mut env = BTreeMap::new();
    if let Some(env_obj) = obj.get("env").and_then(|v| v.as_object()) {
        for (k, v) in env_obj {
            if let Some(s) = v.as_str() {
                env.insert(k.clone(), s.to_string());
            } else {
                env.insert(k.clone(), v.to_string());
            }
        }
    }
    let mut headers = BTreeMap::new();
    if let Some(h) = obj.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in h {
            if let Some(s) = v.as_str() {
                headers.insert(k.clone(), s.to_string());
            }
        }
    }
    let transport = if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
        McpTransport::Http {
            url: url.to_string(),
            headers,
        }
    } else {
        let command = obj
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let args = obj
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let cwd = obj
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        McpTransport::Stdio { command, args, cwd }
    };
    let tool_allow = string_list_field(obj, "toolAllow")
        .or_else(|| string_list_field(obj, "tools"))
        .unwrap_or_default();
    let tool_deny = string_list_field(obj, "toolDeny").unwrap_or_default();

    let known = [
        "type",
        "command",
        "args",
        "cwd",
        "url",
        "headers",
        "env",
        "enabled",
        "toolAllow",
        "toolDeny",
        "tools",
    ];
    let mut ext_obj = serde_json::Map::new();
    let mut diags = Vec::new();
    for (k, v) in obj {
        if known.contains(&k.as_str()) {
            continue;
        }
        ext_obj.insert(k.clone(), v.clone());
        diags.push(PortabilityDiagnostic::new(
            CODE_UNKNOWN_SOURCE_FIELD,
            format!("/mcpServers/{key}/{k}"),
            "unknown mcp source field retained in target_extensions",
        ));
    }
    let mut target_extensions = BTreeMap::new();
    if !ext_obj.is_empty() {
        target_extensions.insert(target, Value::Object(ext_obj));
    }
    let server = PortableMcpServer {
        key: key.to_string(),
        transport,
        env,
        enabled,
        tool_allow,
        tool_deny,
        target_extensions,
    };
    server.validate()?;
    diags.extend(server.collect_diagnostics());
    Ok((server, diags))
}

fn string_list_field(obj: &serde_json::Map<String, Value>, key: &str) -> Option<Vec<String>> {
    obj.get(key).and_then(|v| {
        v.as_array().map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
    })
}

/// 从 TOML 文本解析 Codex `mcp_servers` 表。
///
/// Business Logic: Codex 使用 `mcp_servers.<key>` TOML；只读扫描。
/// Code Logic: 枚举 server key 后经 `TomlConfigPatcher::inspect` 取完整 leaf JSON
/// （含 int/float/array/table），content_hash 与 apply CAS 同域；再映射 PortableMcpServer。
pub fn parse_codex_mcp_toml(
    target: AgentTarget,
    scope_kind: ScopeKind,
    text: &str,
    config_path: &Path,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    use crate::agent_hub::config_patch::{SemanticConfigPatcher, TomlConfigPatcher};

    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| AppError::validation(format!("codex_config_toml_invalid:{e}")))?;
    let Some(servers) = doc.get("mcp_servers").and_then(|i| i.as_table()) else {
        return Ok(vec![]);
    };
    let keys: Vec<String> = servers
        .iter()
        .filter(|(_, item)| item.as_table().is_some())
        .map(|(key, _)| key.to_string())
        .collect();
    let patcher = TomlConfigPatcher;
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for key in keys {
        let owned = match patcher.inspect(bytes, &["mcp_servers".into(), key.clone()]) {
            Ok(v) if v.present => v,
            Ok(_) => continue,
            Err(e) => {
                tracing::debug!(
                    target = "agent_hub.portable",
                    key = %key,
                    error = %e,
                    "skip codex mcp leaf inspect"
                );
                continue;
            }
        };
        let value = owned.value;
        let content_hash = owned
            .value_hash
            .unwrap_or_else(|| value_content_hash(&value));
        if let Ok((server, diags)) = mcp_from_json_value(target, &key, &value, true) {
            let enabled = server.enabled;
            out.push(DiscoveredPortableAsset {
                kind: AssetKind::Mcp,
                semantic_name: key.clone(),
                scope_kind,
                payload: PortableAssetPayload::Mcp(server),
                origin: PortableAssetOrigin {
                    target,
                    path: config_path.to_path_buf(),
                    origin_kind: PortableOriginKind::Native,
                    native_id: key,
                    content_hash,
                    tree_hash: None,
                    status: if enabled {
                        PortableDiscoveryStatus::Active
                    } else {
                        PortableDiscoveryStatus::Disabled
                    },
                    native_output_candidate: true,
                    owned_by: PortableAssetOwner::from_target(target),
                    parent_plugin_id: None,
                },
                diagnostics: diags,
            });
        }
    }
    Ok(out)
}

/// 从 Codex TOML 解析 `agents.<name>` 引用为 Agent 发现（config_file 指针）。
///
/// Business Logic: Codex agent 常为 config 引用 + 外部文件；扫描记录 origin 到 config。
/// Code Logic: 读 agents 表；若有 config_file 则读 instructions。
pub fn parse_codex_agents_toml(
    target: AgentTarget,
    scope_kind: ScopeKind,
    text: &str,
    config_path: &Path,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| AppError::validation(format!("codex_config_toml_invalid:{e}")))?;
    let Some(agents) = doc.get("agents").and_then(|i| i.as_table()) else {
        return Ok(vec![]);
    };
    let base = config_path.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();
    for (name, item) in agents.iter() {
        let Some(table) = item.as_table() else {
            continue;
        };
        let description = table
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let config_file = table
            .get("config_file")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        let (instructions, content_path, content_hash) = if let Some(rel) = &config_file {
            let p = if rel.is_absolute() {
                rel.clone()
            } else {
                base.join(rel)
            };
            if p.is_file() {
                let bytes = fs::read(&p)?;
                let hash = sha256_hex(&bytes);
                (String::from_utf8_lossy(&bytes).into_owned(), p, hash)
            } else {
                (String::new(), config_path.to_path_buf(), sha256_hex(b""))
            }
        } else {
            let bytes = text.as_bytes();
            (String::new(), config_path.to_path_buf(), sha256_hex(bytes))
        };
        let mut ext = serde_json::Map::new();
        for (k, v) in table.iter() {
            if k == "description" || k == "config_file" {
                continue;
            }
            if let Some(s) = v.as_str() {
                ext.insert(k.to_string(), Value::String(s.to_string()));
            }
        }
        let mut target_extensions = BTreeMap::new();
        if !ext.is_empty() {
            target_extensions.insert(target, Value::Object(ext.clone()));
        }
        if let Some(cf) = &config_file {
            let entry = target_extensions
                .entry(target)
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Some(obj) = entry.as_object_mut() {
                obj.insert(
                    "config_file".into(),
                    Value::String(cf.to_string_lossy().into_owned()),
                );
            }
        }
        let agent = PortableAgent {
            name: name.to_string(),
            description,
            instructions,
            mode_intent: None,
            tool_intents: vec![],
            target_extensions,
        };
        if agent.validate().is_err() {
            continue;
        }
        let mut diags = agent.collect_diagnostics();
        for k in ext.keys() {
            diags.push(PortabilityDiagnostic::new(
                CODE_UNKNOWN_SOURCE_FIELD,
                format!("/agents/{name}/{k}"),
                "unknown codex agent field retained in target_extensions",
            ));
        }
        out.push(DiscoveredPortableAsset {
            kind: AssetKind::Agent,
            semantic_name: name.to_string(),
            scope_kind,
            payload: PortableAssetPayload::Agent(agent),
            origin: PortableAssetOrigin {
                target,
                path: content_path,
                origin_kind: PortableOriginKind::Native,
                native_id: name.to_string(),
                content_hash,
                tree_hash: None,
                status: PortableDiscoveryStatus::Active,
                native_output_candidate: true,
                owned_by: PortableAssetOwner::from_target(target),
                parent_plugin_id: None,
            },
            diagnostics: diags,
        });
    }
    Ok(out)
}

/// 读取 JSON 或 JSONC 文本为 `Value`（JSONC 先剥注释）。
///
/// Business Logic: OpenCode 配置可能是 jsonc。
/// Code Logic: 优先 serde_json；失败则 strip comments 再解析。
pub fn parse_json_or_jsonc(text: &str) -> Result<Value, AppError> {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Ok(v);
    }
    let stripped = strip_jsonc_comments(text);
    serde_json::from_str(&stripped)
        .map_err(|e| AppError::validation(format!("jsonc_parse_failed:{e}")))
}

/// 极简 JSONC 注释剥离（字符串感知）。
fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < bytes.len() {
            let n = bytes[i + 1] as char;
            if n == '/' {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if n == '*' {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// 渲染 Skill 为 `skills/<name>/SKILL.md` 树投影（仅 SKILL.md 正文；supporting 在 CAS）。
///
/// Business Logic: Task 3 只生成主 Markdown 投影计划；完整 tree 物化在后续 package task。
/// Code Logic: 写出 frontmatter + 占位 body（body 由调用方注入时用 context）。
pub fn render_skill_projection(
    target: AgentTarget,
    skill: &PortableSkill,
    skill_markdown: &str,
) -> TargetAssetProjection {
    let body = if skill_markdown.trim().is_empty() {
        format!(
            "---\nname: {}\ndescription: {}\n---\n",
            skill.name, skill.description
        )
    } else {
        skill_markdown.to_string()
    };
    TargetAssetProjection {
        target,
        files: vec![ProjectedAssetFile {
            relative_path: format!("skills/{}/SKILL.md", skill.name),
            bytes: body.into_bytes(),
        }],
        diagnostics: vec![],
    }
}

/// 渲染 Command Markdown。
pub fn render_command_projection(
    target: AgentTarget,
    command: &PortableCommand,
) -> TargetAssetProjection {
    let mut fm = format!("---\nname: {}\n", command.name);
    if let Some(d) = &command.description {
        fm.push_str(&format!("description: {d}\n"));
    }
    fm.push_str("---\n");
    fm.push_str(&command.prompt_template);
    if !command.prompt_template.ends_with('\n') {
        fm.push('\n');
    }
    TargetAssetProjection {
        target,
        files: vec![ProjectedAssetFile {
            relative_path: format!("commands/{}.md", command.name),
            bytes: fm.into_bytes(),
        }],
        diagnostics: command.collect_diagnostics(),
    }
}

/// 渲染 Agent Markdown。
pub fn render_agent_projection(
    target: AgentTarget,
    agent: &PortableAgent,
) -> TargetAssetProjection {
    let mut fm = format!("---\nname: {}\n", agent.name);
    if let Some(d) = &agent.description {
        fm.push_str(&format!("description: {d}\n"));
    }
    if let Some(m) = &agent.mode_intent {
        fm.push_str(&format!("mode: {m}\n"));
    }
    if !agent.tool_intents.is_empty() {
        fm.push_str(&format!("tools: {}\n", agent.tool_intents.join(", ")));
    }
    fm.push_str("---\n");
    fm.push_str(&agent.instructions);
    if !agent.instructions.ends_with('\n') {
        fm.push('\n');
    }
    TargetAssetProjection {
        target,
        files: vec![ProjectedAssetFile {
            relative_path: format!("agents/{}.md", agent.name),
            bytes: fm.into_bytes(),
        }],
        diagnostics: agent.collect_diagnostics(),
    }
}

/// 渲染 MCP 为 JSON 片段（server 对象），供后续 config patch 使用。
pub fn render_mcp_projection(
    target: AgentTarget,
    server: &PortableMcpServer,
) -> Result<TargetAssetProjection, AppError> {
    let mut obj = serde_json::Map::new();
    match &server.transport {
        McpTransport::Stdio { command, args, cwd } => {
            obj.insert("type".into(), Value::String("stdio".into()));
            obj.insert("command".into(), Value::String(command.clone()));
            obj.insert(
                "args".into(),
                Value::Array(args.iter().cloned().map(Value::String).collect()),
            );
            if let Some(c) = cwd {
                obj.insert("cwd".into(), Value::String(c.clone()));
            }
        }
        McpTransport::Http { url, headers } => {
            obj.insert("type".into(), Value::String("http".into()));
            obj.insert("url".into(), Value::String(url.clone()));
            let mut h = serde_json::Map::new();
            for (k, v) in headers {
                h.insert(k.clone(), Value::String(v.clone()));
            }
            obj.insert("headers".into(), Value::Object(h));
        }
    }
    if !server.env.is_empty() {
        let mut e = serde_json::Map::new();
        for (k, v) in &server.env {
            e.insert(k.clone(), Value::String(v.clone()));
        }
        obj.insert("env".into(), Value::Object(e));
    }
    obj.insert("enabled".into(), Value::Bool(server.enabled));
    let bytes = serde_json::to_vec_pretty(&Value::Object(obj))
        .map_err(|e| AppError::generic(format!("mcp render: {e}")))?;
    Ok(TargetAssetProjection {
        target,
        files: vec![ProjectedAssetFile {
            relative_path: format!("mcp/{}.json", server.key),
            bytes,
        }],
        diagnostics: server.collect_diagnostics(),
    })
}

/// 分派 render。
pub fn render_portable_payload(
    target: AgentTarget,
    asset: &PortableAssetPayload,
) -> Result<TargetAssetProjection, AppError> {
    match asset {
        PortableAssetPayload::Skill(s) => Ok(render_skill_projection(target, s, "")),
        PortableAssetPayload::Command(c) => Ok(render_command_projection(target, c)),
        PortableAssetPayload::Agent(a) => Ok(render_agent_projection(target, a)),
        PortableAssetPayload::Mcp(m) => render_mcp_projection(target, m),
    }
}

/// Claude user MCP 配置路径（与 legacy 模块一致）。
///
/// Business Logic: CLAUDE_CONFIG_DIR 设置时读 `<dir>/.claude.json`，否则 `~/.claude.json`。
/// Code Logic: 读注入 env。
pub fn claude_user_mcp_config_path(env: &super::TargetEnvironment) -> PathBuf {
    if let Some(dir) = env.var("CLAUDE_CONFIG_DIR") {
        PathBuf::from(dir).join(".claude.json")
    } else {
        env.home.join(".claude.json")
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

/// 扫描 disabled 目录下的 skills（actualEnabled=false）。
///
/// Business Logic: active/disabled 路径必须映射为真实启用状态。
/// Code Logic: 复用 scan_skill_dirs 后强制 status=Disabled。
pub fn scan_disabled_skill_dirs(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let mut found = scan_skill_dirs(target, scope_kind, root, origin_kind)?;
    for d in &mut found {
        d.origin.status = PortableDiscoveryStatus::Disabled;
    }
    Ok(found)
}

/// Inventory 列表专用 disabled Skill 扫描；延迟目录 tree hash。
pub fn scan_disabled_skill_dirs_manifest_only(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let mut found = scan_skill_dirs_manifest_only(target, scope_kind, root, origin_kind)?;
    for discovery in &mut found {
        discovery.origin.status = PortableDiscoveryStatus::Disabled;
    }
    Ok(found)
}

/// 扫描 disabled 目录下的 commands。
///
/// Business Logic: disabled command 路径映射 actualEnabled=false。
/// Code Logic: 复用 scan_command_markdown_dir 后强制 Disabled。
pub fn scan_disabled_command_markdown_dir(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let mut found = scan_command_markdown_dir(target, scope_kind, root, origin_kind)?;
    for d in &mut found {
        d.origin.status = PortableDiscoveryStatus::Disabled;
    }
    Ok(found)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::targets::{
        AssetAdapter, ClaudeInstructionAdapter, CodexInstructionAdapter, CursorInstructionAdapter,
        GeminiInstructionAdapter, GrokInstructionAdapter, LocalScopeMapping,
        OpenCodeInstructionAdapter, PiInstructionAdapter, TargetEnvironment,
    };
    use std::collections::BTreeMap as Map;

    fn write(path: &Path, text: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    #[test]
    fn plugin_enablement_writes_viewing_agent_not_owner() {
        assert_eq!(
            mutation_target_for_action(
                AgentTarget::Grok,
                PortableAssetOwner::Claude,
                false,
                AssetKind::Plugin,
                true,
            ),
            AgentTarget::Grok
        );
        assert_eq!(
            mutation_target_for_action(
                AgentTarget::Grok,
                PortableAssetOwner::Claude,
                false,
                AssetKind::Plugin,
                false,
            ),
            AgentTarget::Claude
        );
        assert_eq!(
            mutation_target_for_action(
                AgentTarget::Grok,
                PortableAssetOwner::Claude,
                false,
                AssetKind::Skill,
                true,
            ),
            AgentTarget::Claude
        );
        assert_eq!(
            mutation_target_for_action(
                AgentTarget::Codex,
                PortableAssetOwner::Claude,
                false,
                AssetKind::Plugin,
                true,
            ),
            AgentTarget::Codex
        );
        assert_eq!(
            mutation_target_for_action(
                AgentTarget::OpenCode,
                PortableAssetOwner::Claude,
                false,
                AssetKind::Plugin,
                true,
            ),
            AgentTarget::OpenCode
        );
        assert_eq!(
            mutation_target_for_action(
                AgentTarget::Cursor,
                PortableAssetOwner::Claude,
                false,
                AssetKind::Plugin,
                true,
            ),
            AgentTarget::Cursor
        );
    }

    #[test]
    fn borrowed_runtime_origin_is_owner_based_not_drift_or_legacy() {
        assert!(
            !is_borrowed_runtime_origin(
                AgentTarget::Claude,
                PortableAssetOwner::Claude,
                true,
                PortableOriginKind::Native,
            ),
            "same-agent native is installed"
        );
        assert!(
            !is_borrowed_runtime_origin(
                AgentTarget::Claude,
                PortableAssetOwner::Claude,
                false,
                PortableOriginKind::Native,
            ),
            "same-agent native stays installed even when not a native output candidate"
        );
        assert!(
            !is_borrowed_runtime_origin(
                AgentTarget::Codex,
                PortableAssetOwner::Codex,
                false,
                PortableOriginKind::LegacyStandalone,
            ),
            "Codex ~/.agents/skills is this Agent's install, not borrowed"
        );
        assert!(
            !is_borrowed_runtime_origin(
                AgentTarget::Claude,
                PortableAssetOwner::PortableStore,
                true,
                PortableOriginKind::Native,
            ),
            "store attached on native path is installed"
        );
        assert!(
            !is_borrowed_runtime_origin(
                AgentTarget::Codex,
                PortableAssetOwner::PortableStore,
                false,
                PortableOriginKind::LegacyStandalone,
            ),
            "store attached on Codex legacy root is installed"
        );
        assert!(is_borrowed_runtime_origin(
            AgentTarget::Grok,
            PortableAssetOwner::Claude,
            false,
            PortableOriginKind::Compatibility,
        ));
        assert!(is_borrowed_runtime_origin(
            AgentTarget::Grok,
            PortableAssetOwner::SharedAgents,
            true,
            PortableOriginKind::Native,
        ));
        assert!(is_borrowed_runtime_origin(
            AgentTarget::Grok,
            PortableAssetOwner::PortableStore,
            false,
            PortableOriginKind::Compatibility,
        ));
    }

    fn isolated_fixture() -> (tempfile::TempDir, TargetEnvironment) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        // Claude
        write(
            &home.join(".claude/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review changes\ncustomFlag: keep-me\n---\n# Review\n",
        );
        write(
            &home.join(".claude/commands/release.md"),
            "---\nname: release\ndescription: Cut release\n---\nShip $ARGUMENTS\n",
        );
        write(
            &home.join(".claude/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews PRs\nmodel: sonnet\n---\nBe thorough.\n",
        );
        // CLAUDE_CONFIG_DIR 指向 ~/.claude 时，MCP 配置为 <CLAUDE_CONFIG_DIR>/.claude.json
        write(
            &home.join(".claude/.claude.json"),
            r#"{
  "mcpServers": {
    "private-api": {
      "type": "http",
      "url": "https://example.invalid/mcp?token=plain-fixture",
      "headers": { "Authorization": "Bearer plain-fixture" },
      "env": { "API_TOKEN": "plain-fixture" }
    }
  }
}"#,
        );
        // agents compat
        write(
            &home.join(".agents/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Agents copy\n---\n# Agents review\n",
        );
        // Codex
        write(
            &home.join(".codex/config.toml"),
            r#"
model = "o3"

[mcp_servers.private-api]
command = "uvx"
args = ["srv"]
env = { API_TOKEN = "plain-fixture" }

[agents.reviewer]
description = "Reviews"
config_file = "agents/reviewer.md"
"#,
        );
        write(
            &home.join(".codex/agents/reviewer.md"),
            "Codex reviewer instructions\n",
        );
        // OpenCode native under XDG-style default ~/.config/opencode — use OPENCODE_CONFIG_DIR
        write(
            &home.join(".opencode/skills/review/SKILL.md"),
            "---\nname: review\ndescription: OC skill\n---\n# OC\n",
        );
        write(
            &home.join(".opencode/commands/release.md"),
            "---\nname: release\n---\nOC release\n",
        );
        write(
            &home.join(".opencode/agents/reviewer.md"),
            "---\nname: reviewer\n---\nOC agent\n",
        );
        write(
            &home.join("opencode.jsonc"),
            r#"{
  // keep comment
  "mcpServers": {
    "private-api": {
      "command": "uvx",
      "args": ["oc-srv"],
      "env": { "API_TOKEN": "plain-fixture" }
    }
  }
}
"#,
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
            home,
            vars,
            path_entries: vec![],
        };
        (dir, env)
    }

    fn user_scope(home: &Path) -> LocalScopeMapping {
        LocalScopeMapping {
            scope_kind: ScopeKind::User,
            absolute_path: home.to_path_buf(),
            project_root: None,
            relative_root: None,
            codex_fallback_filenames: vec![],
        }
    }

    #[test]
    fn incremental_skill_hash_cache_invalidates_when_tree_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("skills");
        let skill = root.join("review");
        write(&skill.join("SKILL.md"), "---\nname: review\n---\nfirst\n");
        write(&skill.join("notes.txt"), "one\n");

        let first = scan_skill_dirs(
            AgentTarget::Claude,
            ScopeKind::User,
            &root,
            PortableOriginKind::Native,
        )
        .expect("first scan");
        let second = scan_skill_dirs(
            AgentTarget::Claude,
            ScopeKind::User,
            &root,
            PortableOriginKind::Native,
        )
        .expect("cached scan");
        assert_eq!(first[0].origin.tree_hash, second[0].origin.tree_hash);

        write(&skill.join("notes.txt"), "two changed\n");
        let changed = scan_skill_dirs(
            AgentTarget::Claude,
            ScopeKind::User,
            &root,
            PortableOriginKind::Native,
        )
        .expect("changed scan");
        assert_ne!(first[0].origin.tree_hash, changed[0].origin.tree_hash);
    }

    #[test]
    fn claude_scan_finds_skill_command_agent_mcp() {
        let (_tmp, env) = isolated_fixture();
        let scope = user_scope(&env.home);
        let found = ClaudeInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        assert!(
            found.iter().any(|d| d.kind == AssetKind::Skill
                && d.semantic_name == "review"
                && d.origin.origin_kind == PortableOriginKind::Native),
            "skills={found:?}"
        );
        assert!(found
            .iter()
            .any(|d| d.kind == AssetKind::Command && d.semantic_name == "release"));
        assert!(found
            .iter()
            .any(|d| d.kind == AssetKind::Agent && d.semantic_name == "reviewer"));
        let mcp = found
            .iter()
            .find(|d| d.kind == AssetKind::Mcp && d.semantic_name == "private-api")
            .expect("mcp");
        match &mcp.payload {
            PortableAssetPayload::Mcp(s) => {
                assert_eq!(
                    s.env.get("API_TOKEN").map(String::as_str),
                    Some("plain-fixture")
                );
            }
            _ => panic!("expected mcp"),
        }
        // unknown frontmatter retained
        let skill = found.iter().find(|d| d.kind == AssetKind::Skill).unwrap();
        match &skill.payload {
            PortableAssetPayload::Skill(s) => {
                let ext = s.target_extensions.get(&AgentTarget::Claude).unwrap();
                assert_eq!(ext["customFlag"], "keep-me");
            }
            _ => panic!("skill"),
        }
        assert!(skill
            .diagnostics
            .iter()
            .any(|d| d.code == CODE_UNKNOWN_SOURCE_FIELD));
    }

    /// Codex MCP content_hash 必须等于 TomlConfigPatcher leaf inspect（含 int 字段）。
    #[test]
    fn parse_codex_mcp_content_hash_matches_toml_leaf_with_integers() {
        use crate::agent_hub::config_patch::{SemanticConfigPatcher, TomlConfigPatcher};

        let text = r#"
[mcp_servers.node_repl]
command = "node"
startup_timeout_sec = 120
args = ["mcp"]
enabled = true
"#;
        let path = PathBuf::from("/tmp/codex-config.toml");
        let found =
            parse_codex_mcp_toml(AgentTarget::Codex, ScopeKind::User, text, &path).expect("parse");
        assert_eq!(found.len(), 1);
        let owned = TomlConfigPatcher
            .inspect(text.as_bytes(), &["mcp_servers".into(), "node_repl".into()])
            .expect("inspect");
        assert!(owned.present);
        assert_eq!(
            found[0].origin.content_hash,
            owned.value_hash.expect("hash"),
            "scan content_hash must share CAS domain with apply Toml leaf"
        );
        // 不完整 string-only 重建不得冒充 content_hash
        let incomplete = serde_json::json!({
            "command": "node",
            "args": ["mcp"],
            "enabled": true,
        });
        assert_ne!(
            found[0].origin.content_hash,
            crate::agent_hub::config_patch::value_content_hash(&incomplete)
        );
    }

    #[test]
    fn mcp_content_hash_is_key_order_independent_like_value_content_hash() {
        // inventory content_hash 必须与 action CAS 的 value_content_hash 同域，
        // 否则 ensure/reconcile 会因键序把健康 MCP 标成 drift。
        use crate::agent_hub::config_patch::value_content_hash;
        use serde_json::json;

        let a = json!({
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "ctx"],
        });
        let b = json!({
            "args": ["-y", "ctx"],
            "command": "npx",
            "type": "stdio",
        });
        let mut map_a = serde_json::Map::new();
        map_a.insert("ctx".into(), a);
        let mut map_b = serde_json::Map::new();
        map_b.insert("ctx".into(), b);
        let path = PathBuf::from("/tmp/mcp-hash-order.json");
        let disc_a = parse_mcp_servers_json_map(
            AgentTarget::Claude,
            ScopeKind::User,
            &map_a,
            &path,
            PortableOriginKind::Native,
            true,
        );
        let disc_b = parse_mcp_servers_json_map(
            AgentTarget::Claude,
            ScopeKind::User,
            &map_b,
            &path,
            PortableOriginKind::Native,
            true,
        );
        assert_eq!(disc_a.len(), 1);
        assert_eq!(disc_b.len(), 1);
        assert_eq!(
            disc_a[0].origin.content_hash, disc_b[0].origin.content_hash,
            "reordered mcp leaf keys must share content_hash"
        );
        let expected = value_content_hash(map_a.get("ctx").unwrap());
        assert_eq!(
            disc_a[0].origin.content_hash, expected,
            "content_hash must equal value_content_hash(leaf)"
        );
    }

    #[test]
    fn codex_scan_mcp_and_legacy_agents_skills() {
        let (_tmp, env) = isolated_fixture();
        let scope = user_scope(&env.home);
        let found = CodexInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        assert!(found.iter().any(|d| d.kind == AssetKind::Mcp));
        let legacy = found
            .iter()
            .find(|d| {
                d.kind == AssetKind::Skill
                    && d.origin.origin_kind == PortableOriginKind::LegacyStandalone
            })
            .expect("legacy .agents/skills");
        assert!(legacy.origin.path.to_string_lossy().contains(".agents"));
        assert!(!legacy.origin.native_output_candidate);
        assert!(found.iter().any(|d| d.kind == AssetKind::Agent));
    }

    #[test]
    fn opencode_marks_compat_origins_not_native_output() {
        let (_tmp, env) = isolated_fixture();
        let scope = user_scope(&env.home);
        let found = OpenCodeInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        let native_skills: Vec<_> = found
            .iter()
            .filter(|d| {
                d.kind == AssetKind::Skill && d.origin.origin_kind == PortableOriginKind::Native
            })
            .collect();
        assert_eq!(native_skills.len(), 1);
        assert!(native_skills[0]
            .origin
            .path
            .to_string_lossy()
            .contains(".opencode"));
        let compat: Vec<_> = found
            .iter()
            .filter(|d| {
                d.kind == AssetKind::Skill
                    && d.origin.origin_kind == PortableOriginKind::Compatibility
            })
            .collect();
        assert!(
            compat.len() >= 2,
            "expected .claude and .agents compat, got {compat:?}"
        );
        assert!(compat.iter().all(|d| !d.origin.native_output_candidate));
        // same semantic name, separate discoveries
        let reviews: Vec<_> = found
            .iter()
            .filter(|d| d.kind == AssetKind::Skill && d.semantic_name == "review")
            .collect();
        assert!(reviews.len() >= 3, "reviews={reviews:?}");
        let paths: std::collections::BTreeSet<_> =
            reviews.iter().map(|d| d.origin.path.clone()).collect();
        assert_eq!(paths.len(), reviews.len());
    }

    #[test]
    fn scan_does_not_write_files() {
        let (_tmp, env) = isolated_fixture();
        let scope = user_scope(&env.home);
        let before = walk_snapshot(&env.home);
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
        let _ = PiInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        let after = walk_snapshot(&env.home);
        assert_eq!(before, after);
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

    #[test]
    fn render_round_trip_command() {
        let cmd = PortableCommand {
            name: "release".into(),
            description: Some("d".into()),
            prompt_template: "go".into(),
            arguments: vec![],
            target_extensions: BTreeMap::new(),
        };
        let proj = render_command_projection(AgentTarget::Claude, &cmd);
        assert_eq!(proj.files[0].relative_path, "commands/release.md");
        let text = String::from_utf8(proj.files[0].bytes.clone()).unwrap();
        assert!(text.contains("name: release"));
        assert!(text.contains("go"));
    }

    #[test]
    fn skill_scan_follows_store_symlink_and_rejects_escape() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let store = crate::agent_hub::portable_store::ensure_portable_store_layout(&data).unwrap();
        let skill = crate::agent_hub::portable_store::store_skill_dir(&store, "foo");
        write(
            &skill.join("SKILL.md"),
            "---\nname: foo\ndescription: Store skill\n---\n# Foo\n",
        );
        let native_root = dir.path().join("skills");
        fs::create_dir_all(&native_root).unwrap();
        crate::agent_hub::portable_store::create_store_link(&skill, &native_root.join("foo"))
            .unwrap();
        let found = scan_skill_dirs(
            AgentTarget::Claude,
            ScopeKind::User,
            &native_root,
            PortableOriginKind::Native,
        )
        .unwrap();
        let store_hit = found
            .iter()
            .find(|a| a.origin.native_id == "foo")
            .expect("store skill");
        assert_eq!(store_hit.origin.owned_by, PortableAssetOwner::PortableStore);
        assert_eq!(store_hit.origin.status, PortableDiscoveryStatus::Active);
        assert!(!store_hit.origin.content_hash.is_empty());

        let escape = dir.path().join("escape");
        fs::create_dir_all(&escape).unwrap();
        write(&escape.join("SKILL.md"), "---\nname: evil\n---\n# x\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&escape, native_root.join("evil")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&escape, native_root.join("evil")).unwrap();
        let again = scan_skill_dirs(
            AgentTarget::Claude,
            ScopeKind::User,
            &native_root,
            PortableOriginKind::Native,
        )
        .unwrap();
        let blocked = again
            .iter()
            .find(|a| a.origin.native_id == "evil")
            .expect("escape");
        assert_eq!(blocked.origin.status, PortableDiscoveryStatus::Blocked);
        assert!(blocked
            .diagnostics
            .iter()
            .any(|d| d.code == "store_symlink_escape"));
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }
}
