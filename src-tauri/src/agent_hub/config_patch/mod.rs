//! agent_hub/config_patch — ownership-aware TOML / JSONC 语义 patch
//!
//! Business Logic（为什么需要这个模块）:
//!     CLI 配置文件（Codex TOML、Claude/OpenCode JSON/JSONC）含用户私有字段、注释与顺序；
//!     Hub 只能改自己拥有的语义路径，不得整文件重序列化毁掉无关 span。CAS 冲突时
//!     `expected_base_hash` 不匹配必须可见 Conflict，禁止盲覆盖。
//!
//! Code Logic（这个模块做什么）:
//!     导出 `ManagedConfigPatch` / `SemanticConfigPatcher` / outcome 类型；
//!     提供 `TomlConfigPatcher`、`JsoncConfigPatcher` 与 projection 集成 helper。

pub mod jsonc;
pub mod toml;

pub use jsonc::JsoncConfigPatcher;
pub use toml::TomlConfigPatcher;

use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::projection::atomic_writer::FileWriteRequest;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Hub 拥有路径上的一次语义 patch。
///
/// Business Logic（为什么需要这个结构体）:
///     投影只提交 Hub 拥有的 leaf/table key；`expected_base_hash` 提供 per-path CAS。
///
/// Code Logic（这个结构体做什么）:
///     path 为从根起的语义 key 序列；value=None 表示删除该 key。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedConfigPatch {
    /// 拥有者资产/绑定 id
    pub owner_id: String,
    /// 语义路径（如 `mcp_servers` / `cc_partner_x`）
    pub path: Vec<String>,
    /// 新值；None=删除
    pub value: Option<serde_json::Value>,
    /// 期望的路径值 hash（None=首次写入/无 CAS）
    pub expected_base_hash: Option<String>,
}

/// inspect 结果：路径上当前值与 hash。
///
/// Business Logic（为什么需要这个结构体）:
///     调度/预览需要知道 owned path 当前是否存在与内容指纹。
///
/// Code Logic（这个结构体做什么）:
///     value 用 JSON 统一表示；缺失时 present=false。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnedConfigValue {
    /// 路径是否存在
    pub present: bool,
    /// 当前 JSON 形态值（缺失为 Null）
    pub value: serde_json::Value,
    /// 规范序列化后的 SHA-256（缺失为 None）
    pub value_hash: Option<String>,
}

/// 单条 owned path 的前后 diff 预览。
///
/// Business Logic（为什么需要这个结构体）:
///     UI/Attention 需要精确 preview，而非整文件 diff。
///
/// Code Logic（这个结构体做什么）:
///     保存 path、owner、前后 hash 与值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPathDiff {
    /// 拥有者
    pub owner_id: String,
    /// 语义路径
    pub path: Vec<String>,
    /// 变更前 hash
    pub before_hash: Option<String>,
    /// 变更后 hash
    pub after_hash: Option<String>,
    /// 变更前值
    pub before_value: Option<serde_json::Value>,
    /// 变更后值
    pub after_value: Option<serde_json::Value>,
}

/// 应用后的 owned path hash 快照（写入 materialization 元数据）。
///
/// Business Logic（为什么需要这个结构体）:
///     配置文件不是整文件 Hub 资产；只记录 owned path 的 baseValueHash。
///
/// Code Logic（这个结构体做什么）:
///     稳定 camelCase JSON 可嵌入 managed_paths_json 旁的元数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigOwnedPathMeta {
    /// 拥有者
    pub owner_id: String,
    /// 语义路径
    pub path: Vec<String>,
    /// 成功应用后该路径值 hash
    pub base_value_hash: Option<String>,
}

/// patch 结果形态。
///
/// Business Logic（为什么需要这个枚举）:
///     Conflict/Blocked 必须可区分且保留原字节，禁止覆盖。
///
/// Code Logic（这个枚举做什么）:
///     Applied / Conflict / Blocked 三态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ConfigPatchOutcome {
    /// 全部 patch 成功
    Applied,
    /// 某路径 CAS 失败
    Conflict {
        /// 冲突路径
        path: Vec<String>,
        /// 当前 hash
        current_hash: Option<String>,
        /// 期望 hash
        expected_base_hash: Option<String>,
        /// owner
        owner_id: String,
    },
    /// 文档非法 / 父节点类型错 / 无法安全改写
    Blocked {
        /// 稳定原因摘要（不含 secret 正文）
        reason: String,
    },
}

/// apply 返回体。
///
/// Business Logic（为什么需要这个结构体）:
///     调用方拿新字节、owned hash 与 exact preview；Conflict/Blocked 时 bytes=原件。
///
/// Code Logic（这个结构体做什么）:
///     聚合 outcome + bytes + 元数据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchedConfig {
    /// 结果形态
    pub outcome: ConfigPatchOutcome,
    /// 新文档字节；非 Applied 时等于输入
    pub bytes: Vec<u8>,
    /// 成功路径的 post-hash 快照
    pub owned_path_hashes: Vec<ConfigOwnedPathMeta>,
    /// 精确路径 preview
    pub preview_diff: Vec<ConfigPathDiff>,
    /// 整文件 hash（Applied 后）
    pub document_hash: String,
}

/// 语义配置 patcher 合同。
///
/// Business Logic（为什么需要这个 trait）:
///     TOML/JSONC 实现同一 CAS 与 ownership 语义，供 projection 统一调用。
///
/// Code Logic（这个 trait 做什么）:
///     inspect 单路径；apply 批量 patch（短路 Conflict/Blocked）。
pub trait SemanticConfigPatcher {
    /// 读取路径当前值。
    ///
    /// Business Logic: 投影/预览在写前观察 owned path。
    /// Code Logic: 解析文档并导航 path。
    fn inspect(&self, bytes: &[u8], path: &[String]) -> Result<OwnedConfigValue, AppError>;

    /// 应用一组 owned path patch。
    ///
    /// Business Logic: 只改 Hub 路径；CAS 失败或非法文档不得改写。
    /// Code Logic: 顺序应用；首个 Conflict/Blocked 立即返回原字节。
    fn apply(
        &self,
        bytes: &[u8],
        patches: &[ManagedConfigPatch],
    ) -> Result<PatchedConfig, AppError>;
}

/// 对 serde_json::Value 做确定性规范序列化后哈希。
///
/// Business Logic（为什么需要这个函数）:
///     owned path CAS 必须与键序无关地稳定。
///
/// Code Logic（这个函数做什么）:
///     递归排序 object 键后 `to_string` + sha256。
pub fn value_content_hash(value: &serde_json::Value) -> String {
    let canonical = canonicalize_value(value);
    sha256_hex(canonical.to_string().as_bytes())
}

/// 递归排序 object 键，保证哈希稳定。
///
/// Business Logic: Map 键序差异不得制造伪冲突。
/// Code Logic: object → BTreeMap 顺序重建。
pub fn canonicalize_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                if let Some(v) = map.get(k) {
                    out.insert(k.clone(), canonicalize_value(v));
                }
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalize_value).collect())
        }
        other => other.clone(),
    }
}

/// 缺失路径的 inspect 结果。
///
/// Business Logic: 统一缺失语义。
/// Code Logic: present=false。
pub fn missing_owned_value() -> OwnedConfigValue {
    OwnedConfigValue {
        present: false,
        value: serde_json::Value::Null,
        value_hash: None,
    }
}

/// 从 JSON 值构造 present OwnedConfigValue。
///
/// Business Logic: inspect 成功路径共用。
/// Code Logic: 计算 value_content_hash。
pub fn owned_value_from_json(value: serde_json::Value) -> OwnedConfigValue {
    let value_hash = Some(value_content_hash(&value));
    OwnedConfigValue {
        present: true,
        value,
        value_hash,
    }
}

/// 构造 CAS Conflict 结果（保留原字节）。
///
/// Business Logic: 冲突必须不改盘。
/// Code Logic: outcome=Conflict，bytes=input。
pub fn conflict_result(
    bytes: &[u8],
    patch: &ManagedConfigPatch,
    current_hash: Option<String>,
) -> PatchedConfig {
    PatchedConfig {
        outcome: ConfigPatchOutcome::Conflict {
            path: patch.path.clone(),
            current_hash,
            expected_base_hash: patch.expected_base_hash.clone(),
            owner_id: patch.owner_id.clone(),
        },
        bytes: bytes.to_vec(),
        owned_path_hashes: vec![],
        preview_diff: vec![],
        document_hash: sha256_hex(bytes),
    }
}

/// 构造 Blocked 结果（保留原字节）。
///
/// Business Logic: 非法文档/父类型错不得改写。
/// Code Logic: outcome=Blocked。
pub fn blocked_result(bytes: &[u8], reason: impl Into<String>) -> PatchedConfig {
    PatchedConfig {
        outcome: ConfigPatchOutcome::Blocked {
            reason: reason.into(),
        },
        bytes: bytes.to_vec(),
        owned_path_hashes: vec![],
        preview_diff: vec![],
        document_hash: sha256_hex(bytes),
    }
}

/// 校验单条 patch 的 CAS 期望。
///
/// Business Logic: expected 与 current 不等 → Conflict。
/// Code Logic: expected=None 始终通过；缺失路径 current=None。
pub fn check_cas(expected: Option<&str>, current_hash: Option<&str>) -> bool {
    match expected {
        None => true,
        Some(exp) => current_hash == Some(exp),
    }
}

/// 配置 patch 投影准备结果。
///
/// Business Logic（为什么需要这个结构体）:
///     投影执行时从外部文件读现状、语义 patch，再交给 Gate A 原子写。
///
/// Code Logic（这个结构体做什么）:
///     打包 patched 字节、整文件 hash、owned path meta、preview。
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedConfigProjection {
    /// patch 结果
    pub patched: PatchedConfig,
    /// 写前期望的外部整文件 hash（None=文件不存在）
    pub expected_external_hash: Option<String>,
}

/// 从当前外部配置字节准备投影写请求输入。
///
/// Business Logic（为什么需要这个函数）:
///     配置文件永不整文件 Hub 拥有；执行时基于现状 patch，再用文件级 precondition。
///
/// Code Logic（这个函数做什么）:
///     apply patches → 记录 expected_external_hash=写前整文件 hash；
///     成功时调用方用 FileWriteRequest 原子替换。
pub fn prepare_config_projection(
    patcher: &dyn SemanticConfigPatcher,
    current_bytes: Option<&[u8]>,
    patches: &[ManagedConfigPatch],
) -> Result<PreparedConfigProjection, AppError> {
    let existing = current_bytes.unwrap_or(b"");
    let expected_external_hash = if current_bytes.is_some() {
        Some(sha256_hex(existing))
    } else {
        None
    };
    let seed = if current_bytes.is_some() {
        existing.to_vec()
    } else {
        // 空文档起点：JSONC 用 {}，TOML 用空；调用方应传正确空模板
        existing.to_vec()
    };
    let patched = patcher.apply(&seed, patches)?;
    Ok(PreparedConfigProjection {
        patched,
        expected_external_hash,
    })
}

/// 将成功的 prepare 结果转为 Gate A FileWriteRequest 字段。
///
/// Business Logic: Applied 后才能写盘；Conflict/Blocked 不得构造写请求。
/// Code Logic: 返回 (bytes, rendered_hash, expected_external_hash)。
pub fn file_write_parts_if_applied(
    prepared: &PreparedConfigProjection,
) -> Option<(Vec<u8>, String, Option<String>)> {
    match prepared.patched.outcome {
        ConfigPatchOutcome::Applied => Some((
            prepared.patched.bytes.clone(),
            prepared.patched.document_hash.clone(),
            prepared.expected_external_hash.clone(),
        )),
        _ => None,
    }
}

/// 辅助：在临时路径上演示 config patch + atomic write（测试/集成用）。
///
/// Business Logic: 证明 projection 链路不把配置当整文件 Hub artifact。
/// Code Logic: prepare → FileWriteRequest → AtomicProjectionWriter。
pub fn apply_config_patch_atomically(
    patcher: &dyn SemanticConfigPatcher,
    target: &Path,
    patches: &[ManagedConfigPatch],
) -> Result<PreparedConfigProjection, AppError> {
    use crate::agent_hub::projection::atomic_writer::{AtomicProjectionWriter, AtomicWriteOutcome};

    let current = if target.exists() {
        Some(std::fs::read(target).map_err(AppError::from)?)
    } else {
        None
    };
    let prepared = prepare_config_projection(patcher, current.as_deref(), patches)?;
    let Some((bytes, rendered_hash, expected)) = file_write_parts_if_applied(&prepared) else {
        return Ok(prepared);
    };
    let writer = AtomicProjectionWriter::new();
    let outcome = writer.write_file(FileWriteRequest {
        target,
        rendered_bytes: &bytes,
        rendered_hash: &rendered_hash,
        expected_external_hash: expected.as_deref(),
    })?;
    match outcome {
        AtomicWriteOutcome::Replaced { .. } | AtomicWriteOutcome::AlreadyRendered { .. } => {
            Ok(prepared)
        }
        AtomicWriteOutcome::Drift { .. } => {
            let mut blocked = prepared;
            blocked.patched.outcome = ConfigPatchOutcome::Blocked {
                reason: "external_file_drift".into(),
            };
            Ok(blocked)
        }
        AtomicWriteOutcome::DirectoryUnknownFiles { .. } => {
            Err(AppError::generic("config patch 不走 directory 写路径"))
        }
    }
}

/// 将 ConfigOwnedPathMeta 列表序列化为 JSON（供 materialization 旁路元数据）。
///
/// Business Logic: 持久化 `{ownerId,path,baseValueHash}`。
/// Code Logic: serde_json 紧凑数组。
pub fn serialize_owned_path_meta(meta: &[ConfigOwnedPathMeta]) -> Result<String, AppError> {
    serde_json::to_string(meta).map_err(|e| AppError::generic(format!("owned path meta: {e}")))
}

/// 解析 owned path meta JSON。
///
/// Business Logic: 读回 CAS 基线。
/// Code Logic: serde_json。
pub fn parse_owned_path_meta(json: &str) -> Result<Vec<ConfigOwnedPathMeta>, AppError> {
    serde_json::from_str(json).map_err(|e| AppError::validation(format!("owned path meta: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::config_patch::jsonc::JsoncConfigPatcher;
    use crate::agent_hub::config_patch::toml::TomlConfigPatcher;

    fn patch(
        owner: &str,
        path: &[&str],
        value: Option<serde_json::Value>,
        expected: Option<&str>,
    ) -> ManagedConfigPatch {
        ManagedConfigPatch {
            owner_id: owner.into(),
            path: path.iter().map(|s| (*s).to_string()).collect(),
            value,
            expected_base_hash: expected.map(|s| s.to_string()),
        }
    }

    #[test]
    fn value_content_hash_is_key_order_independent() {
        let a: serde_json::Value = serde_json::json!({"b":1,"a":2});
        let b: serde_json::Value = serde_json::json!({"a":2,"b":1});
        assert_eq!(value_content_hash(&a), value_content_hash(&b));
    }

    #[test]
    fn owned_path_meta_round_trip() {
        let meta = vec![ConfigOwnedPathMeta {
            owner_id: "asset-1".into(),
            path: vec!["mcp_servers".into(), "cc_partner_x".into()],
            base_value_hash: Some("abc".into()),
        }];
        let s = serialize_owned_path_meta(&meta).expect("ser");
        let back = parse_owned_path_meta(&s).expect("de");
        assert_eq!(back, meta);
        assert!(s.contains("ownerId"));
        assert!(s.contains("baseValueHash"));
    }

    #[test]
    fn prepare_config_projection_records_external_hash() {
        let bytes = br#"{"mcpServers":{}}"#;
        let patcher = JsoncConfigPatcher;
        let prepared = prepare_config_projection(
            &patcher,
            Some(bytes),
            &[patch(
                "o1",
                &["mcpServers", "cc_partner_x"],
                Some(serde_json::json!({"command":"x"})),
                None,
            )],
        )
        .expect("prepare");
        assert!(matches!(
            prepared.patched.outcome,
            ConfigPatchOutcome::Applied
        ));
        assert_eq!(
            prepared.expected_external_hash.as_deref(),
            Some(sha256_hex(bytes).as_str())
        );
        let parts = file_write_parts_if_applied(&prepared).expect("applied parts");
        assert_eq!(parts.1, prepared.patched.document_hash);
    }

    #[test]
    fn apply_config_patch_atomically_preserves_unrelated_toml() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("config.toml");
        let seed = r#"# keep header
model = "gpt-user"

[mcp_servers.user-owned]
command = "uvx"
args = ["a", "b"]

[mcp_servers.cc_partner_x]
command = "old"
"#;
        std::fs::write(&path, seed).expect("write");
        let patcher = TomlConfigPatcher;
        let prepared = apply_config_patch_atomically(
            &patcher,
            &path,
            &[patch(
                "hub",
                &["mcp_servers", "cc_partner_x"],
                Some(serde_json::json!({"command":"new","args":["1"]})),
                None,
            )],
        )
        .expect("atomic");
        assert!(matches!(
            prepared.patched.outcome,
            ConfigPatchOutcome::Applied
        ));
        let after = std::fs::read_to_string(&path).expect("read");
        assert!(after.contains("# keep header"));
        assert!(after.contains("model = \"gpt-user\""));
        assert!(after.contains("[mcp_servers.user-owned]"));
        assert!(after.contains("args = [\"a\", \"b\"]"));
        assert!(after.contains("command = \"new\"") || after.contains("command=\"new\""));
    }
}
