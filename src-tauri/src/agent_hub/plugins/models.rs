//! agent_hub/plugins/models — Immutable PluginPackage / Hook / Residual 类型
//!
//! Business Logic（为什么需要这个模块）:
//!     package revision 只保存 metadata、固定 component revision refs 与 residual CAS tree refs；
//!     后续 component 更新不得改写旧 package revision，Snapshot 必须能闭合历史 refs。
//!
//! Code Logic（这个模块做什么）:
//!     定义 camelCase serde 载荷与 ownership/residual/hook intent 枚举；
//!     排序后做确定性 JSON 序列化；校验重复 ref、空 id、Hook 合同体积上限。

use crate::agent_hub::models::{AgentTarget, AssetKind, RevisionId};
use crate::agent_hub::snapshot::envelope::{default_snapshot_limits, SnapshotLimits};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Component 相对 package 的所有权语义。
///
/// Business Logic（为什么需要这个枚举）:
///     删除 package 时必须区分独占 / 共享 / standalone 引用，禁止误删共享 component。
///
/// Code Logic（这个枚举做什么）:
///     camelCase wire；as_str / parse 供 SQL TEXT round-trip。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComponentOwnership {
    /// package 独占（无 standalone / 他 package 引用时才可 tombstone）
    PackageOwned,
    /// 被多个 package 引用
    Shared,
    /// 已绑定独立 standalone 逻辑资产
    Standalone,
}

impl ComponentOwnership {
    /// 稳定 wire/DB 字符串。
    ///
    /// Business Logic: 边表与删除决策依赖稳定 token。
    /// Code Logic: `packageOwned` / `shared` / `standalone`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PackageOwned => "packageOwned",
            Self::Shared => "shared",
            Self::Standalone => "standalone",
        }
    }

    /// 解析 wire/DB 字符串。
    ///
    /// Business Logic: 未知 ownership 必须 fail-closed。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "packageOwned" => Some(Self::PackageOwned),
            "shared" => Some(Self::Shared),
            "standalone" => Some(Self::Standalone),
            _ => None,
        }
    }
}

/// Residual runtime 载荷类别。
///
/// Business Logic（为什么需要这个枚举）:
///     residual 默认 source-only；分类用于诊断与 target 投影策略，不改写原字节。
///
/// Code Logic（这个枚举做什么）:
///     camelCase；as_str / parse。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResidualKind {
    /// 通用 runtime 文件树
    Runtime,
    /// Hook 脚本/配置残差（未归一化为 PortableHook 的部分）
    Hooks,
    /// 其它 package 资产树
    Assets,
    /// npm 包残差（OpenCode）
    Npm,
    /// custom tool 残差
    CustomTool,
}

impl ResidualKind {
    /// 稳定 wire/DB 字符串。
    ///
    /// Business Logic: residual 边表主键使用稳定 kind token。
    /// Code Logic: camelCase 词。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Hooks => "hooks",
            Self::Assets => "assets",
            Self::Npm => "npm",
            Self::CustomTool => "customTool",
        }
    }

    /// 解析 wire/DB 字符串。
    ///
    /// Business Logic: 未知 residual kind 不得 silent fallback。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "runtime" => Some(Self::Runtime),
            "hooks" => Some(Self::Hooks),
            "assets" => Some(Self::Assets),
            "npm" => Some(Self::Npm),
            "customTool" => Some(Self::CustomTool),
            _ => None,
        }
    }
}

/// Hook 事件意图（跨 CLI 归一化标签）。
///
/// Business Logic（为什么需要这个枚举）:
///     Hook 默认 targetOnly；只有有双端 schema + 信任模型 evidence 的 mapping 才能跨 target。
///
/// Code Logic（这个枚举做什么）:
///     camelCase intent token；未知 CLI 事件可落 `Custom` 并进 target_extensions。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HookEventIntent {
    /// 会话开始
    SessionStart,
    /// 会话结束
    SessionEnd,
    /// 用户提交 prompt
    UserPromptSubmit,
    /// 工具调用前
    PreToolUse,
    /// 工具调用后
    PostToolUse,
    /// 通知
    Notification,
    /// 停止
    Stop,
    /// 子 agent 停止
    SubagentStop,
    /// 压缩前
    PreCompact,
    /// 权限请求
    PermissionRequest,
    /// 未归一化 / 自定义意图
    Custom,
}

impl HookEventIntent {
    /// 稳定 wire/DB 字符串。
    ///
    /// Business Logic: Hook payload 与 support mapping 查表依赖稳定 token。
    /// Code Logic: camelCase。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "sessionStart",
            Self::SessionEnd => "sessionEnd",
            Self::UserPromptSubmit => "userPromptSubmit",
            Self::PreToolUse => "preToolUse",
            Self::PostToolUse => "postToolUse",
            Self::Notification => "notification",
            Self::Stop => "stop",
            Self::SubagentStop => "subagentStop",
            Self::PreCompact => "preCompact",
            Self::PermissionRequest => "permissionRequest",
            Self::Custom => "custom",
        }
    }

    /// 解析 wire/DB 字符串。
    ///
    /// Business Logic: 未知 intent fail-closed。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "sessionStart" => Some(Self::SessionStart),
            "sessionEnd" => Some(Self::SessionEnd),
            "userPromptSubmit" => Some(Self::UserPromptSubmit),
            "preToolUse" => Some(Self::PreToolUse),
            "postToolUse" => Some(Self::PostToolUse),
            "notification" => Some(Self::Notification),
            "stop" => Some(Self::Stop),
            "subagentStop" => Some(Self::SubagentStop),
            "preCompact" => Some(Self::PreCompact),
            "permissionRequest" => Some(Self::PermissionRequest),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// Plugin package 对单个 component 的固定 revision 引用。
///
/// Business Logic（为什么需要这个结构体）:
///     package 不嵌入 component 正文，只钉住 revision_id；component 更新生成新 package revision。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；排序键为 {kind, assetId, revisionId}。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginComponentRef {
    /// component AssetKind（Skill/Command/Agent/Mcp/Hook）
    pub kind: AssetKind,
    /// component 逻辑资产 id
    pub asset_id: String,
    /// 固定 revision id
    pub revision_id: RevisionId,
    /// 所有权语义
    pub ownership: ComponentOwnership,
}

impl PluginComponentRef {
    /// 稳定排序键。
    ///
    /// Business Logic: canonical 序列化前必须确定性排序，保证跨端 hash 一致。
    /// Code Logic: (kind.as_str, asset_id, revision_id)。
    pub fn sort_key(&self) -> (&str, &str, &str) {
        (
            self.kind.as_str(),
            self.asset_id.as_str(),
            self.revision_id.as_str(),
        )
    }
}

/// Plugin residual CAS 树引用（source-only runtime 等）。
///
/// Business Logic（为什么需要这个结构体）:
///     未知 runtime 文件保持原字节树进入 CAS；跨 target 默认不投影。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；排序键 {target, residualKind, treeHash}。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginResidualRef {
    /// residual 所属 target
    pub target: AgentTarget,
    /// residual 类别
    pub residual_kind: ResidualKind,
    /// TreeManifest SHA-256 hex
    pub tree_manifest_hash: String,
}

impl PluginResidualRef {
    /// 稳定排序键。
    ///
    /// Business Logic: residual 顺序不得影响 snapshotHash。
    /// Code Logic: (target, residual_kind, tree_hash)。
    pub fn sort_key(&self) -> (&str, &str, &str) {
        (
            self.target.as_str(),
            self.residual_kind.as_str(),
            self.tree_manifest_hash.as_str(),
        )
    }
}

/// Canonical PluginPackage revision payload。
///
/// Business Logic（为什么需要这个结构体）:
///     package 元数据 + 固定 component/residual refs 是跨设备恢复 Plugin 的最小可验证单位。
///
/// Code Logic（这个结构体做什么）:
///     camelCase JSON；target_extensions 用 BTreeMap 键序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginPackagePayload {
    /// 稳定 plugin 身份（非 display name）
    pub plugin_id: String,
    /// 展示名
    pub name: String,
    /// 可选版本
    pub version: Option<String>,
    /// 可选描述
    pub description: Option<String>,
    /// 来源 CLI target
    pub source_target: AgentTarget,
    /// 固定 component revision 引用
    pub component_refs: Vec<PluginComponentRef>,
    /// residual 树引用
    pub residual_refs: Vec<PluginResidualRef>,
    /// 各 target 未归一化扩展
    pub target_extensions: BTreeMap<AgentTarget, serde_json::Value>,
}

/// Canonical PortableHook 载荷。
///
/// Business Logic（为什么需要这个结构体）:
///     Hook 保存 event intent 与 I/O 合同；命令/脚本树可选；默认 targetOnly。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；contracts 为 JSON Value；command_tree_hash 指向 CAS 树。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableHook {
    /// 事件意图
    pub event_intent: HookEventIntent,
    /// 输入合同 JSON
    pub input_contract: serde_json::Value,
    /// 输出合同 JSON
    pub output_contract: serde_json::Value,
    /// 可选命令/脚本 tree hash
    pub command_tree_hash: Option<String>,
    /// 来源 target
    pub source_target: AgentTarget,
    /// target 扩展
    pub target_extensions: BTreeMap<AgentTarget, serde_json::Value>,
}

/// 允许作为 plugin component 的 AssetKind。
///
/// Business Logic: Plugin/Instruction 不能嵌套为 component。
/// Code Logic: Skill/Command/Agent/Mcp/Hook。
pub fn ensure_component_kind_allowed(kind: AssetKind) -> Result<(), AppError> {
    match kind {
        AssetKind::Skill
        | AssetKind::Command
        | AssetKind::Agent
        | AssetKind::Mcp
        | AssetKind::Hook => Ok(()),
        other => Err(AppError::validation(format!(
            "agent_hub_plugin_component_kind_not_allowed:{}",
            other.as_str()
        ))),
    }
}

/// 校验 SHA-256 hex。
///
/// Business Logic: 非法 hash 不得写入 residual/command tree 引用。
/// Code Logic: 长度 64 且仅 `[0-9a-f]`。
fn validate_sha256_hex(field: &str, hash: &str) -> Result<(), AppError> {
    if hash.len() != 64 || !hash.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
        return Err(AppError::validation(format!(
            "agent_hub_plugin_invalid_hash:{field}"
        )));
    }
    Ok(())
}

/// 对 package payload 的 refs 原地排序。
///
/// Business Logic（为什么需要这个函数）:
///     调用方输入顺序不定；canonical hash 要求固定序。
///
/// Code Logic（这个函数做什么）:
///     component_refs 按 {kind,assetId,revisionId}；residual_refs 按 {target,residualKind,treeHash}。
pub fn sort_plugin_package_payload(payload: &mut PluginPackagePayload) {
    payload
        .component_refs
        .sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    payload
        .residual_refs
        .sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
}

/// 校验 PluginPackagePayload 结构（不访问 DB/CAS）。
///
/// Business Logic（为什么需要这个函数）:
///     空 plugin_id、重复 component、非法 kind/hash 不得开启 SQL 事务。
///
/// Code Logic（这个函数做什么）:
///     trim plugin_id/name；component 唯一键 (kind,assetId,revisionId)；
///     residual hash 形态；调用 ensure_component_kind_allowed。
pub fn validate_plugin_package_payload(payload: &PluginPackagePayload) -> Result<(), AppError> {
    if payload.plugin_id.trim().is_empty() {
        return Err(AppError::validation(
            "agent_hub_plugin_empty_plugin_id".to_string(),
        ));
    }
    if payload.name.trim().is_empty() {
        return Err(AppError::validation(
            "agent_hub_plugin_empty_name".to_string(),
        ));
    }
    let mut seen_components: BTreeSet<(String, String, String)> = BTreeSet::new();
    for r in &payload.component_refs {
        ensure_component_kind_allowed(r.kind)?;
        if r.asset_id.trim().is_empty() {
            return Err(AppError::validation(
                "agent_hub_plugin_component_empty_asset_id".to_string(),
            ));
        }
        if r.revision_id.as_str().trim().is_empty() {
            return Err(AppError::validation(
                "agent_hub_plugin_component_empty_revision_id".to_string(),
            ));
        }
        let key = (
            r.kind.as_str().to_string(),
            r.asset_id.clone(),
            r.revision_id.as_str().to_string(),
        );
        if !seen_components.insert(key) {
            return Err(AppError::validation(format!(
                "agent_hub_plugin_duplicate_component_ref:{}:{}:{}",
                r.kind.as_str(),
                r.asset_id,
                r.revision_id.as_str()
            )));
        }
    }
    let mut seen_residuals: BTreeSet<(String, String, String)> = BTreeSet::new();
    for r in &payload.residual_refs {
        validate_sha256_hex("tree_manifest_hash", &r.tree_manifest_hash)?;
        let key = (
            r.target.as_str().to_string(),
            r.residual_kind.as_str().to_string(),
            r.tree_manifest_hash.clone(),
        );
        if !seen_residuals.insert(key) {
            return Err(AppError::validation(format!(
                "agent_hub_plugin_duplicate_residual_ref:{}:{}:{}",
                r.target.as_str(),
                r.residual_kind.as_str(),
                &r.tree_manifest_hash[..8.min(r.tree_manifest_hash.len())]
            )));
        }
    }
    Ok(())
}

/// 校验 PortableHook，并限制合同体积不超过 Snapshot limits。
///
/// Business Logic（为什么需要这个函数）:
///     过大 contract 会撑破 Snapshot hard limits；须在写入前 fail-closed。
///
/// Code Logic（这个函数做什么）:
///     序列化 input/output contract 字节和与可选 tree hash；与 SnapshotLimits 比较。
pub fn validate_portable_hook(
    hook: &PortableHook,
    limits: &SnapshotLimits,
) -> Result<(), AppError> {
    let input_bytes = serde_json::to_vec(&hook.input_contract)
        .map_err(|e| AppError::generic(format!("agent_hub_hook_input_serialize:{e}")))?;
    let output_bytes = serde_json::to_vec(&hook.output_contract)
        .map_err(|e| AppError::generic(format!("agent_hub_hook_output_serialize:{e}")))?;
    let contract_total = (input_bytes.len() as u64).saturating_add(output_bytes.len() as u64);
    if contract_total > limits.max_blob_bytes {
        return Err(AppError::validation(format!(
            "agent_hub_hook_contract_exceeds_snapshot_limit:actual={contract_total}:limit={}",
            limits.max_blob_bytes
        )));
    }
    if contract_total > limits.max_manifest_bytes {
        return Err(AppError::validation(format!(
            "agent_hub_hook_contract_exceeds_snapshot_manifest_limit:actual={contract_total}:limit={}",
            limits.max_manifest_bytes
        )));
    }
    if let Some(hash) = &hook.command_tree_hash {
        validate_sha256_hex("command_tree_hash", hash)?;
    }
    Ok(())
}

/// 将 PluginPackagePayload 序列化为确定性 canonical JSON 字节。
///
/// Business Logic（为什么需要这个函数）:
///     同一 package 语义跨设备必须得到相同 payload_hash。
///
/// Code Logic（这个函数做什么）:
///     clone → sort → validate → serde_json::to_vec。
pub fn canonical_plugin_package_bytes(payload: &PluginPackagePayload) -> Result<Vec<u8>, AppError> {
    let mut sorted = payload.clone();
    sort_plugin_package_payload(&mut sorted);
    validate_plugin_package_payload(&sorted)?;
    serde_json::to_vec(&sorted)
        .map_err(|e| AppError::generic(format!("agent_hub_plugin_package_serialize:{e}")))
}

/// 从 canonical JSON 反序列化 PluginPackagePayload。
///
/// Business Logic: load package revision 时还原 typed 载荷。
/// Code Logic: from_slice + sort + validate。
pub fn from_plugin_package_bytes(bytes: &[u8]) -> Result<PluginPackagePayload, AppError> {
    let mut payload: PluginPackagePayload = serde_json::from_slice(bytes)
        .map_err(|e| AppError::generic(format!("agent_hub_plugin_package_deserialize:{e}")))?;
    sort_plugin_package_payload(&mut payload);
    validate_plugin_package_payload(&payload)?;
    Ok(payload)
}

/// 将 PortableHook 序列化为确定性 canonical JSON 字节。
///
/// Business Logic: Hook revision payload_hash 跨设备一致。
/// Code Logic: validate with default snapshot limits → to_vec。
pub fn canonical_portable_hook_bytes(hook: &PortableHook) -> Result<Vec<u8>, AppError> {
    validate_portable_hook(hook, &default_snapshot_limits())?;
    serde_json::to_vec(hook)
        .map_err(|e| AppError::generic(format!("agent_hub_portable_hook_serialize:{e}")))
}

/// 从 canonical JSON 反序列化 PortableHook。
///
/// Business Logic: load Hook revision。
/// Code Logic: from_slice + validate。
pub fn from_portable_hook_bytes(bytes: &[u8]) -> Result<PortableHook, AppError> {
    let hook: PortableHook = serde_json::from_slice(bytes)
        .map_err(|e| AppError::generic(format!("agent_hub_portable_hook_deserialize:{e}")))?;
    validate_portable_hook(&hook, &default_snapshot_limits())?;
    Ok(hook)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::object_store::{ObjectStore, TreeEntry, TreeEntryType, TreeManifest};
    use crate::agent_hub::snapshot::envelope::SnapshotLimits;
    use tempfile::TempDir;

    fn sample_component(kind: AssetKind, asset: &str, rev: &str) -> PluginComponentRef {
        PluginComponentRef {
            kind,
            asset_id: asset.into(),
            revision_id: RevisionId::from(rev),
            ownership: ComponentOwnership::PackageOwned,
        }
    }

    fn sample_package() -> PluginPackagePayload {
        PluginPackagePayload {
            plugin_id: "demo.plugin".into(),
            name: "Demo".into(),
            version: Some("1.0.0".into()),
            description: Some("d".into()),
            source_target: AgentTarget::Claude,
            component_refs: vec![
                sample_component(AssetKind::Skill, "asset-skill", "rev-s1"),
                sample_component(AssetKind::Command, "asset-cmd", "rev-c1"),
            ],
            residual_refs: vec![PluginResidualRef {
                target: AgentTarget::Claude,
                residual_kind: ResidualKind::Runtime,
                tree_manifest_hash: "a".repeat(64),
            }],
            target_extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn rejects_empty_plugin_id() {
        let mut p = sample_package();
        p.plugin_id = "  ".into();
        let err = validate_plugin_package_payload(&p).unwrap_err();
        assert!(err.to_string().contains("empty_plugin_id"));
    }

    #[test]
    fn rejects_duplicate_component_refs() {
        let mut p = sample_package();
        p.component_refs
            .push(sample_component(AssetKind::Skill, "asset-skill", "rev-s1"));
        let err = validate_plugin_package_payload(&p).unwrap_err();
        assert!(err.to_string().contains("duplicate_component_ref"));
    }

    #[test]
    fn rejects_disallowed_component_kind() {
        let mut p = sample_package();
        p.component_refs
            .push(sample_component(AssetKind::Plugin, "nested", "rev-p"));
        let err = validate_plugin_package_payload(&p).unwrap_err();
        assert!(err.to_string().contains("component_kind_not_allowed"));
    }

    #[test]
    fn sort_is_deterministic_for_canonical_bytes() {
        let mut a = sample_package();
        a.component_refs = vec![
            sample_component(AssetKind::Command, "b", "r2"),
            sample_component(AssetKind::Skill, "a", "r1"),
        ];
        let mut b = a.clone();
        b.component_refs.reverse();
        let ba = canonical_plugin_package_bytes(&a).unwrap();
        let bb = canonical_plugin_package_bytes(&b).unwrap();
        assert_eq!(ba, bb);
        let round = from_plugin_package_bytes(&ba).unwrap();
        assert_eq!(round.component_refs[0].kind, AssetKind::Command);
        assert_eq!(round.component_refs[1].kind, AssetKind::Skill);
    }

    #[test]
    fn hook_contract_exceeding_snapshot_limits_fails() {
        let huge = serde_json::json!({"blob": "x".repeat(128)});
        let hook = PortableHook {
            event_intent: HookEventIntent::PreToolUse,
            input_contract: huge.clone(),
            output_contract: huge,
            command_tree_hash: None,
            source_target: AgentTarget::Claude,
            target_extensions: BTreeMap::new(),
        };
        let limits = SnapshotLimits {
            max_entries: 10,
            max_uncompressed_bytes: 1024,
            max_blob_bytes: 32,
            max_manifest_bytes: 16,
            max_chunk_bytes: 8,
        };
        let err = validate_portable_hook(&hook, &limits).unwrap_err();
        assert!(err.to_string().contains("exceeds_snapshot"));
    }

    #[tokio::test]
    async fn residual_runtime_tree_round_trips_exact_bytes_through_cas() {
        let dir = TempDir::new().unwrap();
        let store = ObjectStore::open(dir.path()).unwrap();
        let body = b"#!/usr/bin/env node\nconsole.log('plugin-runtime')\n";
        let blob = store.put_blob(body).await.unwrap();
        let manifest = TreeManifest {
            entries: vec![TreeEntry {
                path: "runtime/index.js".into(),
                blob_hash: blob.hash.clone(),
                entry_type: TreeEntryType::File,
                executable: false,
            }],
        };
        let tree = store.put_tree(&manifest).await.unwrap();
        let restored_tree = store.get_tree(&tree.hash).await.unwrap();
        assert_eq!(restored_tree.entries[0].blob_hash, blob.hash);
        let restored = store.get_blob(&blob.hash).await.unwrap();
        assert_eq!(restored.as_slice(), body);
        // residual ref 形态
        let residual = PluginResidualRef {
            target: AgentTarget::OpenCode,
            residual_kind: ResidualKind::Runtime,
            tree_manifest_hash: tree.hash,
        };
        validate_sha256_hex("tree_manifest_hash", &residual.tree_manifest_hash).unwrap();
    }
}
