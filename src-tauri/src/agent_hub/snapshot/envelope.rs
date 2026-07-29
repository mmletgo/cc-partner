//! agent_hub/snapshot/envelope — SnapshotEnvelope v1 类型、hash 与校验
//!
//! Business Logic（为什么需要这个模块）:
//!     LAN/Git 共用固定 format/version/canonicalization 的可验证 manifest；
//!     必须在导入/传输前强制 referential integrity 与硬上限。
//!
//! Code Logic（这个模块做什么）:
//!     定义 camelCase envelope DTO、limits、canonicalize_without_hash、
//!     compute_snapshot_hash、validate_snapshot（含固定 32 字节 digest 的 CT hash 比较）。

use crate::agent_hub::models::{
    AgentTarget, AssetKind, AssetPolicy, RevisionOperation, RevisionOriginKind,
};
use crate::agent_hub::snapshot::canonical_json::{
    canonicalize_value, parse_json_value_strict, CanonicalJsonError,
};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use uuid::Uuid;

/// Wire format 名称。
pub const FORMAT_NAME: &str = "cc-partner-agent-hub";
/// Wire format 版本。
pub const FORMAT_VERSION: u32 = 1;
/// Canonicalization 算法 token。
pub const CANONICALIZATION_NAME: &str = "RFC8785-JSON";

/// selection 最多 entries。
pub const DEFAULT_MAX_ENTRIES: u64 = 100_000;
/// 未压缩总量上限（2 GiB）。
pub const DEFAULT_MAX_UNCOMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// 单 blob 上限（512 MiB）。
pub const DEFAULT_MAX_BLOB_BYTES: u64 = 512 * 1024 * 1024;
/// manifest 上限（32 MiB）。
pub const DEFAULT_MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;
/// LAN chunk 上限（8 MiB）。
pub const DEFAULT_MAX_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// Snapshot 资源上限。
///
/// Business Logic（为什么需要这个结构体）:
///     导出/导入必须共享同一硬上限，超过时 blocked 而非半截成功。
///
/// Code Logic（这个结构体做什么）:
///     保存 entries / uncompressed / blob / manifest / chunk 字节上限。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotLimits {
    /// selection 与集合条目硬上限
    pub max_entries: u64,
    /// 未压缩 object 总字节
    pub max_uncompressed_bytes: u64,
    /// 单 blob 字节
    pub max_blob_bytes: u64,
    /// manifest canonical 字节
    pub max_manifest_bytes: u64,
    /// LAN chunk 字节
    pub max_chunk_bytes: u64,
}

/// 返回 v1 默认硬上限。
///
/// Business Logic: 产品固定 100k / 2GiB / 512MiB / 32MiB / 8MiB。
/// Code Logic: 常量构造。
pub fn default_snapshot_limits() -> SnapshotLimits {
    SnapshotLimits {
        max_entries: DEFAULT_MAX_ENTRIES,
        max_uncompressed_bytes: DEFAULT_MAX_UNCOMPRESSED_BYTES,
        max_blob_bytes: DEFAULT_MAX_BLOB_BYTES,
        max_manifest_bytes: DEFAULT_MAX_MANIFEST_BYTES,
        max_chunk_bytes: DEFAULT_MAX_CHUNK_BYTES,
    }
}

/// Snapshot 选择范围。
///
/// Business Logic: 用户/Git 导出 scope 必须显式列出，不能靠目录遍历猜测。
/// Code Logic: camelCase scopeIds/assetIds/includeHistory。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSelection {
    /// 选中的 scope id
    pub scope_ids: Vec<String>,
    /// 选中的 logical asset id
    pub asset_ids: Vec<String>,
    /// 是否包含完整 revision ancestry
    pub include_history: bool,
}

/// Snapshot 中的逻辑资产身份。
///
/// Business Logic: 跨设备恢复需要稳定 logical identity，不含本机绝对路径。
/// Code Logic: camelCase 身份字段 + policy + 可选 deletedAt。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotAsset {
    /// logical asset id
    pub id: String,
    /// scope id
    pub scope_id: String,
    /// 资产种类
    pub kind: AssetKind,
    /// origin namespace
    pub origin_namespace: String,
    /// logical key
    pub logical_key: String,
    /// 展示名
    pub display_name: String,
    /// 共享策略
    pub policy: AssetPolicy,
    /// tombstone 时间；None 表示未删除
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

/// lineage 身份（可与 asset id 不同，用于跨设备合并）。
///
/// Business Logic: 导入时按 lineage 拼接 DAG。
/// Code Logic: id + root_asset_id。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotLineage {
    /// lineage id（通常为原始 asset id）
    pub id: String,
    /// 创建该 lineage 的 asset id
    pub root_asset_id: String,
}

/// Snapshot revision 节点。
///
/// Business Logic: 必须保留 parents/tombstone/operation 才能做 merge-base。
/// Code Logic: camelCase；generation 用十进制字符串避免超 safe int。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRevision {
    /// UUIDv7 revision id
    pub id: String,
    /// lineage id
    pub asset_lineage_id: String,
    /// 父 revision id 列表
    pub parents: Vec<String>,
    /// generation 十进制字符串（可超 2^53-1）
    pub generation: String,
    /// upsert / delete
    pub operation: RevisionOperation,
    /// 来源种类
    pub origin_kind: RevisionOriginKind,
    /// 可选来源 target
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_target: Option<AgentTarget>,
    /// 来源 replica
    pub origin_replica_id: String,
    /// payload blob hash
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<String>,
    /// tree manifest hash
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_manifest_hash: Option<String>,
    /// 创建时间 RFC3339
    pub created_at: String,
}

/// target variant 投影元数据。
///
/// Business Logic: targetOnly/adapted 变体必须随 snapshot 迁移。
/// Code Logic: assetId + target + revisionId。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotVariant {
    /// 所属 asset
    pub asset_id: String,
    /// CLI target
    pub target: AgentTarget,
    /// 变体 head revision
    pub revision_id: String,
}

/// 未解决 conflict 快照。
///
/// Business Logic: 跨设备必须保留 freeze 状态，不能静默解冲突。
/// Code Logic: camelCase conflict 字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotConflict {
    /// conflict id
    pub id: String,
    /// asset id
    pub asset_id: String,
    /// 受影响 target（None=common）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<AgentTarget>,
    /// base revision
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision_id: Option<String>,
    /// hub revision
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub_revision_id: Option<String>,
    /// external revision
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_revision_id: Option<String>,
    /// 详情 JSON（可含敏感？产品要求诊断脱敏；此处保留结构，validate 不打印）
    pub detail_json: String,
    /// 创建时间
    pub created_at: String,
}

/// 外部别名（如远端 hubProjectId → 本地）。
///
/// Business Logic: 项目身份映射进入 snapshot，但不含本机绝对路径。
/// Code Logic: kind + external_id + local_id。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotAlias {
    /// 别名种类（如 hubProjectId）
    pub kind: String,
    /// 外部 id
    pub external_id: String,
    /// 本地 canonical id
    pub local_id: String,
}

/// CAS object 描述符（hash + size，无 payload 正文）。
///
/// Business Logic: objects 按 hash 排序记录 size，供协商缺失内容。
/// Code Logic: hash + size 十进制字符串。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotObjectDescriptor {
    /// SHA-256 hex
    pub hash: String,
    /// 字节大小十进制字符串
    pub size: String,
}

/// SnapshotEnvelope v1。
///
/// Business Logic（为什么需要这个结构体）:
///     Git lane 与 LAN push 的唯一可移植 manifest 形状。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 全字段；asset_heads 用 BTreeMap 保证确定性键序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotEnvelopeV1 {
    /// 固定 `cc-partner-agent-hub`
    pub format: String,
    /// 固定 1
    pub format_version: u32,
    /// 固定 `RFC8785-JSON`
    pub canonicalization: String,
    /// UUIDv7 snapshot id
    pub snapshot_id: String,
    /// SHA-256 hex of canonical JSON without this field
    pub snapshot_hash: String,
    /// 源 replica id
    pub source_replica_id: String,
    /// 创建时间 RFC3339
    pub created_at: String,
    /// 选择范围
    pub selection: SnapshotSelection,
    /// assetId → head revision ids
    pub asset_heads: BTreeMap<String, Vec<String>>,
    /// 逻辑资产身份
    pub assets: Vec<SnapshotAsset>,
    /// lineage 列表
    pub lineages: Vec<SnapshotLineage>,
    /// revision DAG 节点
    pub revisions: Vec<SnapshotRevision>,
    /// target variants
    pub variants: Vec<SnapshotVariant>,
    /// 未解决 conflicts
    pub conflicts: Vec<SnapshotConflict>,
    /// 外部别名
    pub aliases: Vec<SnapshotAlias>,
    /// object 描述符
    pub objects: Vec<SnapshotObjectDescriptor>,
}

/// Snapshot 校验/hash 错误（诊断仅含 counts/sizes，无 secret 正文）。
///
/// Business Logic: limit/schema 失败必须稳定可测且不回显凭据。
/// Code Logic: Display/Debug 只输出 code + 数值/长度；手写 Debug 避免嵌套泄漏。
#[derive(Clone, PartialEq, Eq)]
pub enum SnapshotError {
    /// canonical JSON 子集错误
    Canonical(CanonicalJsonError),
    /// schema / 字段错误（detail 仅类型/长度元数据）
    Schema { code: String, detail: String },
    /// 资源上限
    Limit {
        code: String,
        actual: u64,
        limit: u64,
    },
    /// hash 不匹配
    HashMismatch,
    /// 引用完整性
    Referential { code: String, detail: String },
}

impl fmt::Debug for SnapshotError {
    /// 诊断 Debug：code/counts/sizes only，不打印可能嵌值的原文。
    ///
    /// Business Logic: 日志 `{:?}` 也不得泄露 secret。
    /// Code Logic: 手写 Debug，Canonical 委托其自身安全 Debug。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Canonical(e) => write!(f, "Canonical({e:?})"),
            Self::Schema { code, detail } => {
                write!(
                    f,
                    "Schema {{ code: {code:?}, detail_len: {} }}",
                    detail.len()
                )
            }
            Self::Limit {
                code,
                actual,
                limit,
            } => write!(
                f,
                "Limit {{ code: {code:?}, actual: {actual}, limit: {limit} }}"
            ),
            Self::HashMismatch => write!(f, "HashMismatch"),
            Self::Referential { code, detail } => {
                write!(
                    f,
                    "Referential {{ code: {code:?}, detail_len: {} }}",
                    detail.len()
                )
            }
        }
    }
}

impl fmt::Display for SnapshotError {
    /// 稳定诊断文案（仅 code/counts/sizes）。
    ///
    /// Business Logic: 错误可进 log/UI；禁止 secret 原文。
    /// Code Logic: 格式化枚举字段。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Canonical(e) => write!(f, "snapshot_canonical: {e}"),
            Self::Schema { code, detail } => {
                write!(f, "snapshot_schema:{code}:{detail}")
            }
            Self::Limit {
                code,
                actual,
                limit,
            } => write!(f, "snapshot_limit:{code}:actual={actual}:limit={limit}"),
            Self::HashMismatch => write!(f, "snapshot_hash_mismatch"),
            Self::Referential { code, detail } => {
                write!(f, "snapshot_referential:{code}:{detail}")
            }
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<CanonicalJsonError> for SnapshotError {
    fn from(value: CanonicalJsonError) -> Self {
        Self::Canonical(value)
    }
}

/// 将 envelope 转为可 canonical 的 JSON Value（含 snapshotHash）。
///
/// Business Logic: typed → JSON 的中间层，保证字段名 camelCase。
/// Code Logic: serde_json::to_value。
fn envelope_to_value(envelope: &SnapshotEnvelopeV1) -> Result<Value, SnapshotError> {
    // 稳定诊断 only：不转发 serde 原文（可能嵌字段值）。
    serde_json::to_value(envelope).map_err(|_| SnapshotError::Schema {
        code: "serialize_failed".into(),
        detail: "type=serde_json".into(),
    })
}

/// 从 Value 中移除 snapshotHash 字段（若存在）。
///
/// Business Logic: hash 输入必须 omit 自身字段。
/// Code Logic: Object remove `snapshotHash`。
fn omit_snapshot_hash(mut value: Value) -> Value {
    if let Value::Object(map) = &mut value {
        map.remove("snapshotHash");
    }
    value
}

/// 计算移除 snapshotHash 后的 canonical JSON 字节。
///
/// Business Logic（为什么需要这个函数）:
///     snapshotHash 是对「不含自身字段」的 manifest 的摘要；更换仅 hash 字段不得改变输入。
///
/// Code Logic（这个函数做什么）:
///     to_value → remove snapshotHash → canonicalize_value。
pub fn canonicalize_snapshot_without_hash(
    envelope: &SnapshotEnvelopeV1,
) -> Result<Vec<u8>, SnapshotError> {
    let value = envelope_to_value(envelope)?;
    let without = omit_snapshot_hash(value);
    Ok(canonicalize_value(&without)?)
}

/// 计算 snapshotHash（小写 hex SHA-256）。
///
/// Business Logic（为什么需要这个函数）:
///     导出与导入双方必须对同一 manifest 得到同一 hash。
///
/// Code Logic（这个函数做什么）:
///     canonicalize_snapshot_without_hash → Sha256 → lowercase hex。
pub fn compute_snapshot_hash(envelope: &SnapshotEnvelopeV1) -> Result<String, SnapshotError> {
    let bytes = canonicalize_snapshot_without_hash(envelope)?;
    Ok(hex_encode_lower(&Sha256::digest(bytes)))
}

/// 校验 snapshot JSON 文本并返回 typed envelope。
///
/// Business Logic（为什么需要这个函数）:
///     导入/LAN prepare 必须 fail-closed：format/hash/limits/引用完整性。
///
/// Code Logic（这个函数做什么）:
///     严格解析（重复 key）→ typed 反序列化 → schema/limits/referential →
///     规范化 hex 后固定 32 字节 digest 的 CT 比较。
pub fn validate_snapshot(
    json_text: &str,
    limits: &SnapshotLimits,
) -> Result<SnapshotEnvelopeV1, SnapshotError> {
    if json_text.len() as u64 > limits.max_manifest_bytes {
        return Err(SnapshotError::Limit {
            code: "manifest_bytes".into(),
            actual: json_text.len() as u64,
            limit: limits.max_manifest_bytes,
        });
    }

    let value = parse_json_value_strict(json_text)?;
    // 预检 manifest canonical 大小（对 omit-hash 前的 value 粗测；最终以 typed 为准）
    let rough = canonicalize_value(&value)?;
    if rough.len() as u64 > limits.max_manifest_bytes {
        return Err(SnapshotError::Limit {
            code: "manifest_bytes".into(),
            actual: rough.len() as u64,
            limit: limits.max_manifest_bytes,
        });
    }

    // 稳定诊断 only：类型名 + 错误分类，永不转发 serde 可能嵌入字段值的原文。
    let envelope: SnapshotEnvelopeV1 =
        serde_json::from_value(value).map_err(|e| SnapshotError::Schema {
            code: "deserialize_failed".into(),
            detail: format!("type=serde_json,category={}", classify_serde_error(&e)),
        })?;

    validate_envelope_schema(&envelope)?;
    validate_limits(&envelope, limits)?;
    validate_referential_integrity(&envelope)?;

    let expected = compute_snapshot_hash(&envelope)?;
    if !constant_time_hex_eq(&expected, &envelope.snapshot_hash) {
        return Err(SnapshotError::HashMismatch);
    }

    Ok(envelope)
}

/// 将 serde 错误归类为稳定短 token（不含 message 原文）。
///
/// Business Logic: deserialize_failed 诊断不得回显字段值。
/// Code Logic: 只看 error 分类/是否 EOF 等，不使用 `e.to_string()` 全文。
fn classify_serde_error(err: &serde_json::Error) -> &'static str {
    if err.is_data() {
        "data"
    } else if err.is_syntax() {
        "syntax"
    } else if err.is_eof() {
        "eof"
    } else if err.is_io() {
        "io"
    } else {
        "other"
    }
}

/// 校验 format/version/UUID/RFC3339/排序唯一 ID 等 schema。
///
/// Business Logic: 错误 format 不得进入 importer。
/// Code Logic: 逐字段检查。
fn validate_envelope_schema(envelope: &SnapshotEnvelopeV1) -> Result<(), SnapshotError> {
    if envelope.format != FORMAT_NAME {
        return Err(SnapshotError::Schema {
            code: "format".into(),
            detail: format!("expected {FORMAT_NAME}"),
        });
    }
    if envelope.format_version != FORMAT_VERSION {
        return Err(SnapshotError::Schema {
            code: "format_version".into(),
            detail: format!("expected {FORMAT_VERSION}"),
        });
    }
    if envelope.canonicalization != CANONICALIZATION_NAME {
        return Err(SnapshotError::Schema {
            code: "canonicalization".into(),
            detail: format!("expected {CANONICALIZATION_NAME}"),
        });
    }
    require_uuid("snapshot_id", &envelope.snapshot_id)?;
    require_uuid("source_replica_id", &envelope.source_replica_id)?;
    require_rfc3339("created_at", &envelope.created_at)?;
    require_sha256_hex("snapshot_hash", &envelope.snapshot_hash)?;

    // revisions: sorted unique ids
    ensure_sorted_unique_ids(
        "revisions",
        envelope.revisions.iter().map(|r| r.id.as_str()),
    )?;
    for rev in &envelope.revisions {
        require_uuid("revision_id", &rev.id)?;
        require_rfc3339("revision_created_at", &rev.created_at)?;
        parse_decimal_u64("generation", &rev.generation)?;
        if let Some(h) = &rev.payload_hash {
            require_sha256_hex("payload_hash", h)?;
        }
        if let Some(h) = &rev.tree_manifest_hash {
            require_sha256_hex("tree_manifest_hash", h)?;
        }
        require_uuid("origin_replica_id", &rev.origin_replica_id)?;
    }

    // objects: sorted unique hashes
    ensure_sorted_unique_ids("objects", envelope.objects.iter().map(|o| o.hash.as_str()))?;
    for obj in &envelope.objects {
        require_sha256_hex("object_hash", &obj.hash)?;
        parse_decimal_u64("object_size", &obj.size)?;
    }

    // assets unique ids
    ensure_unique_ids("assets", envelope.assets.iter().map(|a| a.id.as_str()))?;
    for asset in &envelope.assets {
        if let Some(ts) = &asset.deleted_at {
            require_rfc3339("asset_deleted_at", ts)?;
        }
    }

    ensure_unique_ids("lineages", envelope.lineages.iter().map(|l| l.id.as_str()))?;
    ensure_unique_ids(
        "conflicts",
        envelope.conflicts.iter().map(|c| c.id.as_str()),
    )?;
    for c in &envelope.conflicts {
        require_rfc3339("conflict_created_at", &c.created_at)?;
    }

    Ok(())
}

/// 校验硬上限（entries / blob / uncompressed / chunk 配置 ceiling）。
///
/// Business Logic: 超限必须 stable diagnostic（counts/sizes only）。
/// Code Logic: 计 selection+集合条目与 object sizes；chunk 校验产品配置上限。
///
/// 说明：`max_chunk_bytes` 在此 fail-closed 的是 **limits 配置** 的产品天花板
/// （0 或 > 8 MiB）。实际 LAN payload 分块大小强制仍由 transport Task 3+ 负责。
fn validate_limits(
    envelope: &SnapshotEnvelopeV1,
    limits: &SnapshotLimits,
) -> Result<(), SnapshotError> {
    let entry_count = count_entries(envelope);
    if entry_count > limits.max_entries {
        return Err(SnapshotError::Limit {
            code: "entries".into(),
            actual: entry_count,
            limit: limits.max_entries,
        });
    }

    let mut total_uncompressed: u64 = 0;
    for obj in &envelope.objects {
        let size = parse_decimal_u64("object_size", &obj.size)?;
        if size > limits.max_blob_bytes {
            return Err(SnapshotError::Limit {
                code: "blob_bytes".into(),
                actual: size,
                limit: limits.max_blob_bytes,
            });
        }
        total_uncompressed = total_uncompressed.saturating_add(size);
    }
    if total_uncompressed > limits.max_uncompressed_bytes {
        return Err(SnapshotError::Limit {
            code: "uncompressed_bytes".into(),
            actual: total_uncompressed,
            limit: limits.max_uncompressed_bytes,
        });
    }

    // manifest bytes（canonical without hash）
    let manifest = canonicalize_snapshot_without_hash(envelope)?;
    if manifest.len() as u64 > limits.max_manifest_bytes {
        return Err(SnapshotError::Limit {
            code: "manifest_bytes".into(),
            actual: manifest.len() as u64,
            limit: limits.max_manifest_bytes,
        });
    }

    // Product chunk ceiling on limits config (tests may lower; never raise above 8 MiB).
    // Payload chunk size enforcement remains transport Task 3+.
    if limits.max_chunk_bytes == 0 || limits.max_chunk_bytes > DEFAULT_MAX_CHUNK_BYTES {
        return Err(SnapshotError::Limit {
            code: "chunk_bytes".into(),
            actual: limits.max_chunk_bytes,
            limit: DEFAULT_MAX_CHUNK_BYTES,
        });
    }

    Ok(())
}

/// 统计 snapshot 条目数（selection + 各集合）。
///
/// Business Logic: entries 上限覆盖 selection 与导出集合。
/// Code Logic: 累加各 Vec/Map 长度。
fn count_entries(envelope: &SnapshotEnvelopeV1) -> u64 {
    let mut n = 0u64;
    n = n.saturating_add(envelope.selection.scope_ids.len() as u64);
    n = n.saturating_add(envelope.selection.asset_ids.len() as u64);
    n = n.saturating_add(envelope.asset_heads.len() as u64);
    for heads in envelope.asset_heads.values() {
        n = n.saturating_add(heads.len() as u64);
    }
    n = n.saturating_add(envelope.assets.len() as u64);
    n = n.saturating_add(envelope.lineages.len() as u64);
    n = n.saturating_add(envelope.revisions.len() as u64);
    n = n.saturating_add(envelope.variants.len() as u64);
    n = n.saturating_add(envelope.conflicts.len() as u64);
    n = n.saturating_add(envelope.aliases.len() as u64);
    n = n.saturating_add(envelope.objects.len() as u64);
    n
}

/// 校验 heads/parents/payload/lineage/root 引用存在。
///
/// Business Logic: 半截 ancestry 不得通过 validate；空 lineages + 非空 revisions 也 fail。
/// Code Logic: 建 revision/object/asset/lineage 集合后查引用（长度-only detail）。
fn validate_referential_integrity(envelope: &SnapshotEnvelopeV1) -> Result<(), SnapshotError> {
    let asset_ids: HashSet<&str> = envelope.assets.iter().map(|a| a.id.as_str()).collect();
    let lineage_ids: HashSet<&str> = envelope.lineages.iter().map(|l| l.id.as_str()).collect();
    let revision_ids: HashSet<&str> = envelope.revisions.iter().map(|r| r.id.as_str()).collect();
    let object_hashes: HashSet<&str> = envelope.objects.iter().map(|o| o.hash.as_str()).collect();

    // lineage.root_asset_id 必须指向 assets（lineages 非空时逐条检查）。
    for lineage in &envelope.lineages {
        if !asset_ids.contains(lineage.root_asset_id.as_str()) {
            return Err(SnapshotError::Referential {
                code: "lineage_unknown_root_asset".into(),
                detail: format!("root_asset_id_len={}", lineage.root_asset_id.len()),
            });
        }
    }

    for (asset_id, heads) in &envelope.asset_heads {
        if !asset_ids.contains(asset_id.as_str()) {
            return Err(SnapshotError::Referential {
                code: "asset_head_unknown_asset".into(),
                detail: format!("asset_id_len={}", asset_id.len()),
            });
        }
        for head in heads {
            if !revision_ids.contains(head.as_str()) {
                return Err(SnapshotError::Referential {
                    code: "asset_head_unknown_revision".into(),
                    detail: format!("revision_id_len={}", head.len()),
                });
            }
        }
    }

    // 只要存在 revision，就必须能在 lineages 中解析 asset_lineage_id
    // （空 lineages + 非空 revisions 一律失败；不再跳过）。
    for rev in &envelope.revisions {
        if !lineage_ids.contains(rev.asset_lineage_id.as_str()) {
            return Err(SnapshotError::Referential {
                code: "revision_unknown_lineage".into(),
                detail: format!("lineage_id_len={}", rev.asset_lineage_id.len()),
            });
        }
        for p in &rev.parents {
            if !revision_ids.contains(p.as_str()) {
                return Err(SnapshotError::Referential {
                    code: "revision_unknown_parent".into(),
                    detail: format!("parent_id_len={}", p.len()),
                });
            }
        }
        if let Some(h) = &rev.payload_hash {
            if !object_hashes.contains(h.as_str()) {
                return Err(SnapshotError::Referential {
                    code: "revision_unknown_payload".into(),
                    detail: format!("hash_len={}", h.len()),
                });
            }
        }
        if let Some(h) = &rev.tree_manifest_hash {
            if !object_hashes.contains(h.as_str()) {
                return Err(SnapshotError::Referential {
                    code: "revision_unknown_tree".into(),
                    detail: format!("hash_len={}", h.len()),
                });
            }
        }
    }

    for v in &envelope.variants {
        if !asset_ids.contains(v.asset_id.as_str()) {
            return Err(SnapshotError::Referential {
                code: "variant_unknown_asset".into(),
                detail: format!("asset_id_len={}", v.asset_id.len()),
            });
        }
        if !revision_ids.contains(v.revision_id.as_str()) {
            return Err(SnapshotError::Referential {
                code: "variant_unknown_revision".into(),
                detail: format!("revision_id_len={}", v.revision_id.len()),
            });
        }
    }

    for c in &envelope.conflicts {
        if !asset_ids.contains(c.asset_id.as_str()) {
            return Err(SnapshotError::Referential {
                code: "conflict_unknown_asset".into(),
                detail: format!("asset_id_len={}", c.asset_id.len()),
            });
        }
        for id in [
            &c.base_revision_id,
            &c.hub_revision_id,
            &c.external_revision_id,
        ]
        .into_iter()
        .flatten()
        {
            if !revision_ids.contains(id.as_str()) {
                return Err(SnapshotError::Referential {
                    code: "conflict_unknown_revision".into(),
                    detail: format!("revision_id_len={}", id.len()),
                });
            }
        }
    }

    Ok(())
}

/// 要求字段是合法 UUID。
fn require_uuid(field: &str, value: &str) -> Result<(), SnapshotError> {
    Uuid::parse_str(value).map_err(|_| SnapshotError::Schema {
        code: format!("invalid_uuid_{field}"),
        detail: format!("len={}", value.len()),
    })?;
    Ok(())
}

/// 要求字段是 RFC3339 时间戳。
fn require_rfc3339(field: &str, value: &str) -> Result<(), SnapshotError> {
    DateTime::parse_from_rfc3339(value)
        .map(|_: DateTime<FixedOffset>| ())
        .map_err(|_| SnapshotError::Schema {
            code: format!("invalid_rfc3339_{field}"),
            detail: format!("len={}", value.len()),
        })
}

/// 要求小写/大小写均可的 64 hex SHA-256。
fn require_sha256_hex(field: &str, value: &str) -> Result<(), SnapshotError> {
    if value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(SnapshotError::Schema {
            code: format!("invalid_sha256_{field}"),
            detail: format!("len={}", value.len()),
        });
    }
    Ok(())
}

/// 解析十进制 u64 字符串。
fn parse_decimal_u64(field: &str, value: &str) -> Result<u64, SnapshotError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SnapshotError::Schema {
            code: format!("invalid_decimal_{field}"),
            detail: format!("len={}", value.len()),
        });
    }
    value.parse::<u64>().map_err(|_| SnapshotError::Schema {
        code: format!("invalid_decimal_{field}"),
        detail: format!("len={}", value.len()),
    })
}

/// 要求 id 列表严格升序且唯一。
fn ensure_sorted_unique_ids<'a, I>(label: &str, ids: I) -> Result<(), SnapshotError>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut prev: Option<&str> = None;
    let mut count = 0u64;
    for id in ids {
        count += 1;
        if let Some(p) = prev {
            if id <= p {
                return Err(SnapshotError::Schema {
                    code: format!("{label}_not_sorted_unique"),
                    detail: format!("count={count}"),
                });
            }
        }
        prev = Some(id);
    }
    Ok(())
}

/// 要求 id 唯一（不强制排序）。
fn ensure_unique_ids<'a, I>(label: &str, ids: I) -> Result<(), SnapshotError>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(SnapshotError::Schema {
                code: format!("{label}_duplicate_id"),
                detail: format!("len={}", id.len()),
            });
        }
    }
    Ok(())
}

/// 小写 hex 编码。
fn hex_encode_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// 将 64 hex 字符解码为固定 32 字节 digest（非 CT 校验步骤）。
///
/// Business Logic: 非法 hex/长度错误在比较前 fail-closed，不进入 CT 循环。
/// Code Logic: 仅接受恰好 64 个 ASCII hex 位；大小写不敏感。
fn decode_sha256_hex_digest(hex: &str) -> Option<[u8; 32]> {
    let bytes = hex.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        let hi = from_hex_nibble(bytes[i * 2])?;
        let lo = from_hex_nibble(bytes[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

/// 单个 hex nibble → 0..=15；非法返回 None。
fn from_hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 规范化 hex 后对固定 32 字节 digest 做 constant-time 比较。
///
/// Business Logic: 避免 snapshot hash 比较的内容相关 early-exit。
/// Code Logic:
///     1) 非 CT 预检：两侧分别 decode 为 32 字节（长度/非法 hex → false）；
///     2) CT 循环：XOR 全部 32 字节无内容 early-exit，再检查 diff==0。
fn constant_time_hex_eq(a: &str, b: &str) -> bool {
    let Some(da) = decode_sha256_hex_digest(a) else {
        return false;
    };
    let Some(db) = decode_sha256_hex_digest(b) else {
        return false;
    };
    let mut diff: u8 = 0;
    for i in 0..32 {
        diff |= da[i] ^ db[i];
    }
    // 无分支归约：diff==0 为真。
    diff == 0
}

/// 测试与 builder 辅助：把 envelope 序列化为紧凑 JSON 文本（非 canonical；仅 fixture）。
#[cfg(test)]
fn envelope_json_for_validate(envelope: &SnapshotEnvelopeV1) -> String {
    // 用我们的 canonical 输出作为 validate 输入（含 snapshotHash）
    let mut value = envelope_to_value(envelope).expect("to_value");
    if let Value::Object(map) = &mut value {
        // ensure snapshotHash present
        let _ = map;
    }
    let bytes = canonicalize_value(&value).expect("canon");
    String::from_utf8(bytes).expect("utf8")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "plain-fixture-secret";

    /// 构造最小合法 envelope fixture。
    fn sample_envelope() -> SnapshotEnvelopeV1 {
        let object_hash = hex_encode_lower(&Sha256::digest(SECRET.as_bytes()));
        let rev_id = "01900000-0000-7000-8000-000000000001";
        let asset_id = "01900000-0000-7000-8000-0000000000a1";
        let lineage_id = asset_id;
        let replica = "01900000-0000-7000-8000-0000000000b1";
        let snap_id = "01900000-0000-7000-8000-0000000000c1";
        let created = "2026-07-29T12:00:00Z";

        let mut envelope = SnapshotEnvelopeV1 {
            format: FORMAT_NAME.into(),
            format_version: FORMAT_VERSION,
            canonicalization: CANONICALIZATION_NAME.into(),
            snapshot_id: snap_id.into(),
            snapshot_hash: "0".repeat(64),
            source_replica_id: replica.into(),
            created_at: created.into(),
            selection: SnapshotSelection {
                scope_ids: vec!["scope-user".into()],
                asset_ids: vec![asset_id.into()],
                include_history: true,
            },
            asset_heads: BTreeMap::from([(asset_id.into(), vec![rev_id.into()])]),
            assets: vec![SnapshotAsset {
                id: asset_id.into(),
                scope_id: "scope-user".into(),
                kind: AssetKind::Mcp,
                origin_namespace: "standalone".into(),
                logical_key: "mcp-secret".into(),
                display_name: "Secret MCP".into(),
                policy: AssetPolicy::Shared,
                deleted_at: None,
            }],
            lineages: vec![SnapshotLineage {
                id: lineage_id.into(),
                root_asset_id: asset_id.into(),
            }],
            revisions: vec![SnapshotRevision {
                id: rev_id.into(),
                asset_lineage_id: lineage_id.into(),
                parents: vec![],
                generation: "0".into(),
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: replica.into(),
                payload_hash: Some(object_hash.clone()),
                tree_manifest_hash: None,
                created_at: created.into(),
            }],
            variants: vec![],
            conflicts: vec![],
            aliases: vec![SnapshotAlias {
                kind: "hubProjectId".into(),
                external_id: "ext-1".into(),
                local_id: "local-1".into(),
            }],
            objects: vec![SnapshotObjectDescriptor {
                hash: object_hash,
                size: (SECRET.len() as u64).to_string(),
            }],
        };
        envelope.snapshot_hash = compute_snapshot_hash(&envelope).expect("hash");
        envelope
    }

    /// Business Logic: 仅改 snapshotHash 不改变 recompute 输入。
    #[test]
    fn changing_only_snapshot_hash_does_not_change_hash_input() {
        let mut a = sample_envelope();
        let input_a = canonicalize_snapshot_without_hash(&a).unwrap();
        a.snapshot_hash = "f".repeat(64);
        let input_b = canonicalize_snapshot_without_hash(&a).unwrap();
        assert_eq!(input_a, input_b);
        assert_eq!(
            compute_snapshot_hash(&sample_envelope()).unwrap(),
            compute_snapshot_hash(&a).unwrap()
        );
    }

    /// Business Logic: parent/tombstone/alias/object size 变更改变 hash。
    #[test]
    fn changing_parent_tombstone_alias_or_size_changes_hash() {
        let base = sample_envelope();
        let h0 = compute_snapshot_hash(&base).unwrap();

        // parent change
        let mut with_parent = base.clone();
        let parent_id = "01900000-0000-7000-8000-000000000002";
        with_parent.revisions.push(SnapshotRevision {
            id: parent_id.into(),
            asset_lineage_id: base.lineages[0].id.clone(),
            parents: vec![],
            generation: "0".into(),
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: base.source_replica_id.clone(),
            payload_hash: base.revisions[0].payload_hash.clone(),
            tree_manifest_hash: None,
            created_at: base.created_at.clone(),
        });
        // keep revisions sorted by id
        with_parent.revisions.sort_by(|a, b| a.id.cmp(&b.id));
        with_parent.revisions[1].parents = vec![parent_id.into()];
        // after sort index may change — set parent on the original head
        for rev in &mut with_parent.revisions {
            if rev.id == base.revisions[0].id {
                rev.parents = vec![parent_id.into()];
                rev.generation = "1".into();
            }
        }
        with_parent.snapshot_hash = compute_snapshot_hash(&with_parent).unwrap();
        assert_ne!(h0, compute_snapshot_hash(&with_parent).unwrap());

        // tombstone
        let mut tomb = base.clone();
        tomb.assets[0].deleted_at = Some("2026-07-29T13:00:00Z".into());
        assert_ne!(h0, compute_snapshot_hash(&tomb).unwrap());

        // alias
        let mut alias = base.clone();
        alias.aliases[0].external_id = "ext-2".into();
        assert_ne!(h0, compute_snapshot_hash(&alias).unwrap());

        // object size
        let mut sized = base.clone();
        sized.objects[0].size = "999".into();
        assert_ne!(h0, compute_snapshot_hash(&sized).unwrap());
    }

    /// Business Logic: 合法 fixture 通过 validate。
    #[test]
    fn valid_envelope_passes_validation() {
        let env = sample_envelope();
        let json = envelope_json_for_validate(&env);
        let limits = default_snapshot_limits();
        let parsed = validate_snapshot(&json, &limits).expect("valid");
        assert_eq!(parsed.snapshot_hash, env.snapshot_hash);
        // secret 在 object payload 中不出现于 manifest；确保诊断也不含
        let err_display_probe = SnapshotError::Limit {
            code: "entries".into(),
            actual: 1,
            limit: 0,
        }
        .to_string();
        assert!(!err_display_probe.contains(SECRET));
        assert!(!json.contains(SECRET));
    }

    /// Business Logic: hash 篡改被检出。
    #[test]
    fn hash_mismatch_rejected() {
        let mut env = sample_envelope();
        env.snapshot_hash = "a".repeat(64);
        let json = envelope_json_for_validate(&env);
        let err = validate_snapshot(&json, &default_snapshot_limits()).unwrap_err();
        assert!(matches!(err, SnapshotError::HashMismatch));
        assert!(!err.to_string().contains(SECRET));
    }

    /// Business Logic: 错误 format 拒绝。
    #[test]
    fn wrong_format_rejected() {
        let mut env = sample_envelope();
        env.format = "other".into();
        env.snapshot_hash = compute_snapshot_hash(&env).unwrap();
        let json = envelope_json_for_validate(&env);
        let err = validate_snapshot(&json, &default_snapshot_limits()).unwrap_err();
        assert!(matches!(err, SnapshotError::Schema { code, .. } if code == "format"));
    }

    /// Business Logic: entries 在 limit 与 limit+1 边界。
    #[test]
    fn entries_limit_boundary() {
        let env = sample_envelope();
        let entry_count = count_entries(&env);
        let mut limits = default_snapshot_limits();
        limits.max_entries = entry_count;
        let json = envelope_json_for_validate(&env);
        assert!(validate_snapshot(&json, &limits).is_ok());

        limits.max_entries = entry_count - 1;
        let err = validate_snapshot(&json, &limits).unwrap_err();
        match &err {
            SnapshotError::Limit {
                code,
                actual,
                limit,
            } => {
                assert_eq!(code, "entries");
                assert_eq!(*actual, entry_count);
                assert_eq!(*limit, entry_count - 1);
                assert!(!format!("{code}{actual}{limit}").contains(SECRET));
            }
            other => panic!("expected limit, got {other}"),
        }
        assert!(!err.to_string().contains(SECRET));
    }

    /// Business Logic: blob size 在 limit 与 limit+1 边界。
    #[test]
    fn blob_limit_boundary() {
        let env = sample_envelope();
        let size: u64 = env.objects[0].size.parse().unwrap();
        let mut limits = default_snapshot_limits();
        limits.max_blob_bytes = size;
        let json = envelope_json_for_validate(&env);
        assert!(validate_snapshot(&json, &limits).is_ok());

        limits.max_blob_bytes = size - 1;
        let err = validate_snapshot(&json, &limits).unwrap_err();
        assert!(
            matches!(&err, SnapshotError::Limit { code, actual, limit } if code == "blob_bytes" && *actual == size && *limit == size - 1)
        );
        assert!(!err.to_string().contains(SECRET));
    }

    /// Business Logic: uncompressed 总量 limit 边界。
    #[test]
    fn uncompressed_limit_boundary() {
        let env = sample_envelope();
        let size: u64 = env.objects[0].size.parse().unwrap();
        let mut limits = default_snapshot_limits();
        limits.max_uncompressed_bytes = size;
        let json = envelope_json_for_validate(&env);
        assert!(validate_snapshot(&json, &limits).is_ok());

        limits.max_uncompressed_bytes = size - 1;
        let err = validate_snapshot(&json, &limits).unwrap_err();
        assert!(matches!(
            &err,
            SnapshotError::Limit {
                code,
                ..
            } if code == "uncompressed_bytes"
        ));
        assert!(!err.to_string().contains(SECRET));
    }

    /// Business Logic: manifest 超限拒绝且诊断无 secret。
    #[test]
    fn manifest_limit_boundary() {
        let env = sample_envelope();
        let json = envelope_json_for_validate(&env);
        let mut limits = default_snapshot_limits();
        limits.max_manifest_bytes = json.len() as u64;
        // validate 用 json_text.len 与 canonical 双重检查；等长应通过
        assert!(validate_snapshot(&json, &limits).is_ok());

        limits.max_manifest_bytes = (json.len() as u64).saturating_sub(1);
        let err = validate_snapshot(&json, &limits).unwrap_err();
        assert!(matches!(
            &err,
            SnapshotError::Limit { code, .. } if code == "manifest_bytes"
        ));
        assert!(!err.to_string().contains(SECRET));
    }

    /// Business Logic: chunk 配置上限 8MiB 接受；0 与 8MiB+1 fail-closed。
    ///
    /// 说明：本测试校验 limits 配置天花板；LAN payload 分块强制仍属 transport Task 3+。
    #[test]
    fn chunk_limit_boundary_on_limits_config() {
        let env = sample_envelope();
        let json = envelope_json_for_validate(&env);

        // limit: default 8 MiB accepted
        let mut limits = default_snapshot_limits();
        assert_eq!(limits.max_chunk_bytes, DEFAULT_MAX_CHUNK_BYTES);
        assert_eq!(DEFAULT_MAX_CHUNK_BYTES, 8 * 1024 * 1024);
        assert!(validate_snapshot(&json, &limits).is_ok());

        // limit+1: product ceiling rejected with stable Limit diagnostic
        limits.max_chunk_bytes = DEFAULT_MAX_CHUNK_BYTES + 1;
        let err = validate_snapshot(&json, &limits).unwrap_err();
        match &err {
            SnapshotError::Limit {
                code,
                actual,
                limit,
            } => {
                assert_eq!(code, "chunk_bytes");
                assert_eq!(*actual, DEFAULT_MAX_CHUNK_BYTES + 1);
                assert_eq!(*limit, DEFAULT_MAX_CHUNK_BYTES);
            }
            other => panic!("expected Limit chunk_bytes, got {other:?}"),
        }
        assert!(!err.to_string().contains(SECRET));
        assert!(!format!("{err:?}").contains(SECRET));

        // 0 rejected
        limits.max_chunk_bytes = 0;
        let err0 = validate_snapshot(&json, &limits).unwrap_err();
        assert!(matches!(
            &err0,
            SnapshotError::Limit {
                code,
                actual: 0,
                limit,
            } if code == "chunk_bytes" && *limit == DEFAULT_MAX_CHUNK_BYTES
        ));
    }

    /// Business Logic: 未知 parent 引用拒绝。
    #[test]
    fn unknown_parent_rejected() {
        let mut env = sample_envelope();
        env.revisions[0].parents = vec!["01900000-0000-7000-8000-000000000099".into()];
        env.snapshot_hash = compute_snapshot_hash(&env).unwrap();
        let json = envelope_json_for_validate(&env);
        let err = validate_snapshot(&json, &default_snapshot_limits()).unwrap_err();
        assert!(
            matches!(&err, SnapshotError::Referential { code, .. } if code == "revision_unknown_parent")
        );
        assert!(!err.to_string().contains(SECRET));
    }

    /// Business Logic: 未排序 object hash 拒绝。
    #[test]
    fn unsorted_object_hashes_rejected() {
        let mut env = sample_envelope();
        let h1 = hex_encode_lower(&Sha256::digest(b"a"));
        let h2 = hex_encode_lower(&Sha256::digest(b"b"));
        // ensure reverse order
        let (hi, lo) = if h1 > h2 { (h1, h2) } else { (h2, h1) };
        env.objects = vec![
            SnapshotObjectDescriptor {
                hash: hi,
                size: "1".into(),
            },
            SnapshotObjectDescriptor {
                hash: lo,
                size: "1".into(),
            },
        ];
        // payload still points to original secret object — referential may also fail;
        // focus on sort check: clear payload refs
        env.revisions[0].payload_hash = None;
        env.snapshot_hash = compute_snapshot_hash(&env).unwrap();
        let json = envelope_json_for_validate(&env);
        let err = validate_snapshot(&json, &default_snapshot_limits()).unwrap_err();
        assert!(matches!(
            err,
            SnapshotError::Schema { code, .. } if code == "objects_not_sorted_unique"
        ));
    }

    /// Business Logic: 重复 key 的 raw JSON 拒绝。
    #[test]
    fn duplicate_key_raw_json_rejected() {
        let raw = r#"{"format":"cc-partner-agent-hub","format":"cc-partner-agent-hub"}"#;
        let err = validate_snapshot(raw, &default_snapshot_limits()).unwrap_err();
        assert!(matches!(
            err,
            SnapshotError::Canonical(CanonicalJsonError::DuplicateKey { .. })
        ));
    }

    /// Business Logic: secret 不得出现在任何 SnapshotError Display **与** Debug 中。
    #[test]
    fn diagnostics_never_include_plain_fixture_secret() {
        let env = sample_envelope();
        // force multiple failure modes
        let cases = vec![
            SnapshotError::HashMismatch,
            SnapshotError::Limit {
                code: "entries".into(),
                actual: 100_001,
                limit: 100_000,
            },
            SnapshotError::Schema {
                code: "format".into(),
                detail: "expected cc-partner-agent-hub".into(),
            },
            SnapshotError::Referential {
                code: "revision_unknown_parent".into(),
                detail: "parent_id_len=36".into(),
            },
        ];
        for err in cases {
            assert!(!err.to_string().contains(SECRET), "Display leaked");
            assert!(!format!("{err:?}").contains(SECRET), "Debug leaked");
        }

        // hash mismatch path (Display + Debug, no escape hatch)
        let mut bad = env.clone();
        bad.snapshot_hash = "b".repeat(64);
        let json = envelope_json_for_validate(&bad);
        let err = validate_snapshot(&json, &default_snapshot_limits()).unwrap_err();
        assert!(!err.to_string().contains(SECRET));
        assert!(!format!("{err:?}").contains(SECRET));

        // secret-shaped wrong-type field → deserialize_failed 不得回显值
        let mut secret_field = serde_json::to_value(&env).expect("to_value");
        if let Value::Object(map) = &mut secret_field {
            map.insert("formatVersion".into(), Value::String(SECRET.to_string()));
        }
        let raw = canonicalize_value(&secret_field).expect("canon");
        let raw_text = String::from_utf8(raw).expect("utf8");
        assert!(raw_text.contains(SECRET), "fixture must embed secret");
        let err = validate_snapshot(&raw_text, &default_snapshot_limits()).unwrap_err();
        assert!(matches!(&err, SnapshotError::Schema { code, .. } if code == "deserialize_failed"));
        assert!(
            !err.to_string().contains(SECRET),
            "Display leaked secret: {}",
            err
        );
        assert!(
            !format!("{err:?}").contains(SECRET),
            "Debug leaked secret: {:?}",
            err
        );

        // secret-shaped duplicate key path
        let dup = format!(r#"{{"{SECRET}":1,"{SECRET}":2,"format":"cc-partner-agent-hub"}}"#);
        let err = validate_snapshot(&dup, &default_snapshot_limits()).unwrap_err();
        assert!(!err.to_string().contains(SECRET));
        assert!(!format!("{err:?}").contains(SECRET));
    }

    /// Business Logic: 未知 lineage id 拒绝（含空 lineages + 非空 revisions）。
    #[test]
    fn unknown_lineage_id_rejected() {
        let mut env = sample_envelope();
        env.revisions[0].asset_lineage_id = "01900000-0000-7000-8000-0000000000ff".into();
        env.snapshot_hash = compute_snapshot_hash(&env).unwrap();
        let json = envelope_json_for_validate(&env);
        let err = validate_snapshot(&json, &default_snapshot_limits()).unwrap_err();
        assert!(
            matches!(&err, SnapshotError::Referential { code, .. } if code == "revision_unknown_lineage")
        );
        assert!(!err.to_string().contains(SECRET));
        assert!(!format!("{err:?}").contains(SECRET));

        // empty lineages + non-empty revisions must fail
        let mut empty_lin = sample_envelope();
        empty_lin.lineages.clear();
        empty_lin.snapshot_hash = compute_snapshot_hash(&empty_lin).unwrap();
        let json = envelope_json_for_validate(&empty_lin);
        let err = validate_snapshot(&json, &default_snapshot_limits()).unwrap_err();
        assert!(
            matches!(&err, SnapshotError::Referential { code, .. } if code == "revision_unknown_lineage")
        );
    }

    /// Business Logic: lineage.root_asset_id 悬空拒绝。
    #[test]
    fn dangling_root_asset_id_rejected() {
        let mut env = sample_envelope();
        env.lineages[0].root_asset_id = "01900000-0000-7000-8000-0000000000ee".into();
        env.snapshot_hash = compute_snapshot_hash(&env).unwrap();
        let json = envelope_json_for_validate(&env);
        let err = validate_snapshot(&json, &default_snapshot_limits()).unwrap_err();
        assert!(
            matches!(&err, SnapshotError::Referential { code, .. } if code == "lineage_unknown_root_asset")
        );
        assert!(!err.to_string().contains(SECRET));
        assert!(!format!("{err:?}").contains(SECRET));
    }

    /// Business Logic: CT hex 比较在合法 digest 上等价，非法 hex 不走 CT 即 false。
    #[test]
    fn constant_time_hex_eq_accepts_equal_digests_only() {
        let a = "a".repeat(64);
        let b = "A".repeat(64);
        assert!(constant_time_hex_eq(&a, &b));
        let c = "b".repeat(64);
        assert!(!constant_time_hex_eq(&a, &c));
        assert!(!constant_time_hex_eq(&a, "zz"));
        assert!(!constant_time_hex_eq("not-hex", &a));
    }

    /// Business Logic: asset_heads key 排序进入 hash。
    #[test]
    fn asset_heads_key_order_is_deterministic() {
        let mut a = sample_envelope();
        let mut b = sample_envelope();
        a.asset_heads.insert(
            "01900000-0000-7000-8000-0000000000a2".into(),
            vec![a.revisions[0].id.clone()],
        );
        // insert reverse into b
        b.asset_heads = BTreeMap::new();
        b.asset_heads.insert(
            "01900000-0000-7000-8000-0000000000a2".into(),
            vec![b.revisions[0].id.clone()],
        );
        b.asset_heads
            .insert(a.assets[0].id.clone(), vec![b.revisions[0].id.clone()]);
        // also need matching assets for referential later; only hash compare
        a.assets.push(SnapshotAsset {
            id: "01900000-0000-7000-8000-0000000000a2".into(),
            scope_id: "scope-user".into(),
            kind: AssetKind::Skill,
            origin_namespace: "standalone".into(),
            logical_key: "k2".into(),
            display_name: "k2".into(),
            policy: AssetPolicy::Shared,
            deleted_at: None,
        });
        b.assets = a.assets.clone();
        assert_eq!(
            canonicalize_snapshot_without_hash(&a).unwrap(),
            canonicalize_snapshot_without_hash(&b).unwrap()
        );
    }
}
