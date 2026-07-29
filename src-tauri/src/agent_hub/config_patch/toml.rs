//! agent_hub/config_patch/toml — ownership-aware TOML patch（toml_edit）
//!
//! Business Logic（为什么需要这个模块）:
//!     Codex `config.toml` 等文件混有用户 model/mcp 与 Hub 受管 keys；必须只改 owned path，
//!     保留注释、顺序、空白与未纳管表项。
//!
//! Code Logic（这个模块做什么）:
//!     用 `toml_edit::DocumentMut` 导航语义 key；拒绝父节点类型变化；删除只移除 owned key。

use crate::agent_hub::config_patch::{
    blocked_result, check_cas, conflict_result, missing_owned_value, owned_value_from_json,
    value_content_hash, ConfigOwnedPathMeta, ConfigPatchOutcome, ConfigPathDiff,
    ManagedConfigPatch, OwnedConfigValue, PatchedConfig, SemanticConfigPatcher,
};
use crate::agent_hub::object_store::sha256_hex;
use crate::error::AppError;
use toml_edit::{DocumentMut, Item, Table, Value as TomlValue};

/// TOML 语义配置 patcher。
///
/// Business Logic（为什么需要这个结构体）:
///     Codex agents/mcp 配置落在 TOML 表路径上。
///
/// Code Logic（这个结构体做什么）:
///     无状态；实现 SemanticConfigPatcher。
#[derive(Debug, Default, Clone, Copy)]
pub struct TomlConfigPatcher;

impl TomlConfigPatcher {
    /// 构造 patcher。
    ///
    /// Business Logic: 无状态单例。
    /// Code Logic: default。
    pub fn new() -> Self {
        Self
    }

    /// 解析 TOML 文档。
    ///
    /// Business Logic: 非法 UTF-8/语法 → Blocked。
    /// Code Logic: from_utf8 + parse DocumentMut。
    fn parse(bytes: &[u8]) -> Result<DocumentMut, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "invalid_utf8".to_string())?;
        text.parse::<DocumentMut>()
            .map_err(|e| format!("invalid_toml:{e}"))
    }

    /// 导航到 path 的父表与 leaf key。
    ///
    /// Business Logic: 父必须是 table；类型错 → Blocked。
    /// Code Logic: 逐段 as_table_mut。
    fn navigate_parent_mut<'a>(
        doc: &'a mut DocumentMut,
        path: &[String],
    ) -> Result<(&'a mut Table, String), String> {
        if path.is_empty() {
            return Err("empty_path".into());
        }
        let (parents, leaf) = path.split_at(path.len() - 1);
        let mut table = doc.as_table_mut();
        for key in parents {
            let item = table
                .entry(key)
                .or_insert_with(|| Item::Table(Table::new()));
            if !item.is_table() {
                return Err(format!("parent_type_mismatch:{key}"));
            }
            table = item.as_table_mut().expect("checked is_table");
        }
        Ok((table, leaf[0].clone()))
    }

    /// 只读导航到路径值。
    ///
    /// Business Logic: inspect 用；缺失返回 None。
    /// Code Logic: 逐段 as_table。
    fn get_item<'a>(doc: &'a DocumentMut, path: &[String]) -> Result<Option<&'a Item>, String> {
        if path.is_empty() {
            return Err("empty_path".into());
        }
        let mut item: &Item = doc.as_item();
        for (i, key) in path.iter().enumerate() {
            let table = match item.as_table() {
                Some(t) => t,
                None => {
                    if i == 0 {
                        return Err("root_not_table".into());
                    }
                    return Err(format!("parent_type_mismatch:{}", path[i - 1]));
                }
            };
            match table.get(key.as_str()) {
                Some(next) => item = next,
                None => return Ok(None),
            }
        }
        Ok(Some(item))
    }

    /// Item → serde_json::Value（用于 CAS/inspect）。
    ///
    /// Business Logic: 值指纹统一走 JSON 规范序列化。
    /// Code Logic: 递归 table/array/value。
    fn item_to_json(item: &Item) -> Result<serde_json::Value, String> {
        match item {
            Item::None => Ok(serde_json::Value::Null),
            Item::Value(v) => Self::toml_value_to_json(v),
            Item::Table(t) => Self::table_to_json(t),
            Item::ArrayOfTables(a) => {
                let mut arr = Vec::new();
                for t in a.iter() {
                    arr.push(Self::table_to_json(t)?);
                }
                Ok(serde_json::Value::Array(arr))
            }
        }
    }

    fn table_to_json(table: &Table) -> Result<serde_json::Value, String> {
        let mut map = serde_json::Map::new();
        for (k, v) in table.iter() {
            map.insert(k.to_string(), Self::item_to_json(v)?);
        }
        Ok(serde_json::Value::Object(map))
    }

    fn toml_value_to_json(v: &TomlValue) -> Result<serde_json::Value, String> {
        match v {
            TomlValue::String(s) => Ok(serde_json::Value::String(s.value().to_string())),
            TomlValue::Integer(i) => Ok(serde_json::json!(*i.value())),
            TomlValue::Float(f) => {
                let n = *f.value();
                serde_json::Number::from_f64(n)
                    .map(serde_json::Value::Number)
                    .ok_or_else(|| "invalid_float".to_string())
            }
            TomlValue::Boolean(b) => Ok(serde_json::Value::Bool(*b.value())),
            TomlValue::Datetime(d) => Ok(serde_json::Value::String(d.to_string())),
            TomlValue::Array(arr) => {
                let mut out = Vec::new();
                for item in arr.iter() {
                    out.push(Self::toml_value_to_json(item)?);
                }
                Ok(serde_json::Value::Array(out))
            }
            TomlValue::InlineTable(t) => {
                let mut map = serde_json::Map::new();
                for (k, v) in t.iter() {
                    map.insert(k.to_string(), Self::toml_value_to_json(v)?);
                }
                Ok(serde_json::Value::Object(map))
            }
        }
    }

    /// serde_json → toml_edit Item。
    ///
    /// Business Logic: Hub 载荷以 JSON 进入，投影到 TOML 值。
    /// Code Logic: object→Table；array→Array；scalar→Value。
    fn json_to_item(value: &serde_json::Value) -> Result<Item, String> {
        match value {
            serde_json::Value::Null => Ok(Item::Value(TomlValue::from(""))),
            serde_json::Value::Bool(b) => Ok(Item::Value(TomlValue::from(*b))),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Item::Value(TomlValue::from(i)))
                } else if let Some(u) = n.as_u64() {
                    Ok(Item::Value(TomlValue::from(u as i64)))
                } else if let Some(f) = n.as_f64() {
                    Ok(Item::Value(TomlValue::from(f)))
                } else {
                    Err("unsupported_number".into())
                }
            }
            serde_json::Value::String(s) => Ok(Item::Value(TomlValue::from(s.as_str()))),
            serde_json::Value::Array(items) => {
                let mut arr = toml_edit::Array::new();
                for item in items {
                    let v = Self::json_to_value(item)?;
                    arr.push(v);
                }
                Ok(Item::Value(TomlValue::Array(arr)))
            }
            serde_json::Value::Object(map) => {
                // 嵌套 object：若看起来像“配置表”（含 command/args 等）用 Table，
                // 否则 inline table 也可；为可读性统一用 Table。
                let mut table = Table::new();
                for (k, v) in map {
                    table.insert(k, Self::json_to_item(v)?);
                }
                Ok(Item::Table(table))
            }
        }
    }

    fn json_to_value(value: &serde_json::Value) -> Result<TomlValue, String> {
        match value {
            serde_json::Value::Null => Ok(TomlValue::from("")),
            serde_json::Value::Bool(b) => Ok(TomlValue::from(*b)),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(TomlValue::from(i))
                } else if let Some(u) = n.as_u64() {
                    Ok(TomlValue::from(u as i64))
                } else if let Some(f) = n.as_f64() {
                    Ok(TomlValue::from(f))
                } else {
                    Err("unsupported_number".into())
                }
            }
            serde_json::Value::String(s) => Ok(TomlValue::from(s.as_str())),
            serde_json::Value::Array(items) => {
                let mut arr = toml_edit::Array::new();
                for item in items {
                    arr.push(Self::json_to_value(item)?);
                }
                Ok(TomlValue::Array(arr))
            }
            serde_json::Value::Object(map) => {
                let mut inline = toml_edit::InlineTable::new();
                for (k, v) in map {
                    inline.insert(k, Self::json_to_value(v)?);
                }
                Ok(TomlValue::InlineTable(inline))
            }
        }
    }

    /// 应用单条 patch 到 DocumentMut。
    ///
    /// Business Logic: set/remove owned leaf；父类型错 blocked。
    /// Code Logic: navigate_parent_mut + insert/remove。
    fn apply_one(
        doc: &mut DocumentMut,
        patch: &ManagedConfigPatch,
    ) -> Result<(Option<serde_json::Value>, Option<serde_json::Value>), String> {
        let before = match Self::get_item(doc, &patch.path)? {
            Some(item) => Some(Self::item_to_json(item)?),
            None => None,
        };
        let before_hash = before.as_ref().map(value_content_hash);
        if !check_cas(patch.expected_base_hash.as_deref(), before_hash.as_deref()) {
            return Err(format!("CAS_CONFLICT:{}", before_hash.unwrap_or_default()));
        }

        match &patch.value {
            Some(v) => {
                let (table, leaf) = Self::navigate_parent_mut(doc, &patch.path)?;
                // 若 leaf 已存在且为 table，而新值也是 object，直接替换整个 item
                let item = Self::json_to_item(v)?;
                table.insert(&leaf, item);
                Ok((before, Some(v.clone())))
            }
            None => {
                if before.is_none() {
                    // 删除不存在路径：CAS 已通过时视为 no-op 成功
                    return Ok((None, None));
                }
                let (table, leaf) = Self::navigate_parent_mut(doc, &patch.path)?;
                table.remove(leaf.as_str());
                // 不自动删除用户创建的空父表（Hub 未标记创建来源时保守保留）
                Ok((before, None))
            }
        }
    }
}

impl SemanticConfigPatcher for TomlConfigPatcher {
    fn inspect(&self, bytes: &[u8], path: &[String]) -> Result<OwnedConfigValue, AppError> {
        let doc = match Self::parse(bytes) {
            Ok(d) => d,
            Err(reason) => {
                return Err(AppError::validation(format!(
                    "config_patch_blocked:{reason}"
                )))
            }
        };
        match Self::get_item(&doc, path) {
            Ok(None) => Ok(missing_owned_value()),
            Ok(Some(item)) => match Self::item_to_json(item) {
                Ok(v) => Ok(owned_value_from_json(v)),
                Err(e) => Err(AppError::validation(format!("config_patch_blocked:{e}"))),
            },
            Err(e) => Err(AppError::validation(format!("config_patch_blocked:{e}"))),
        }
    }

    fn apply(
        &self,
        bytes: &[u8],
        patches: &[ManagedConfigPatch],
    ) -> Result<PatchedConfig, AppError> {
        let mut doc = match Self::parse(bytes) {
            Ok(d) => d,
            Err(reason) => return Ok(blocked_result(bytes, reason)),
        };

        let mut preview = Vec::new();
        let mut owned = Vec::new();

        for patch in patches {
            // 预取 CAS 当前值
            let before_item = match Self::get_item(&doc, &patch.path) {
                Ok(v) => v,
                Err(reason) => return Ok(blocked_result(bytes, reason)),
            };
            let before_json = match before_item {
                Some(item) => match Self::item_to_json(item) {
                    Ok(v) => Some(v),
                    Err(reason) => return Ok(blocked_result(bytes, reason)),
                },
                None => None,
            };
            let before_hash = before_json.as_ref().map(value_content_hash);
            if !check_cas(patch.expected_base_hash.as_deref(), before_hash.as_deref()) {
                return Ok(conflict_result(bytes, patch, before_hash));
            }

            match Self::apply_one(&mut doc, patch) {
                Ok((before, after)) => {
                    let after_hash = after.as_ref().map(value_content_hash);
                    preview.push(ConfigPathDiff {
                        owner_id: patch.owner_id.clone(),
                        path: patch.path.clone(),
                        before_hash: before.as_ref().map(value_content_hash),
                        after_hash: after_hash.clone(),
                        before_value: before,
                        after_value: after,
                    });
                    owned.push(ConfigOwnedPathMeta {
                        owner_id: patch.owner_id.clone(),
                        path: patch.path.clone(),
                        base_value_hash: after_hash,
                    });
                }
                Err(msg) if msg.starts_with("CAS_CONFLICT:") => {
                    let cur = msg.trim_start_matches("CAS_CONFLICT:");
                    let cur = if cur.is_empty() {
                        None
                    } else {
                        Some(cur.to_string())
                    };
                    return Ok(conflict_result(bytes, patch, cur));
                }
                Err(reason) => return Ok(blocked_result(bytes, reason)),
            }
        }

        let out = doc.to_string();
        let out_bytes = out.into_bytes();
        Ok(PatchedConfig {
            outcome: ConfigPatchOutcome::Applied,
            document_hash: sha256_hex(&out_bytes),
            bytes: out_bytes,
            owned_path_hashes: owned,
            preview_diff: preview,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::config_patch::value_content_hash;

    fn p(
        path: &[&str],
        value: Option<serde_json::Value>,
        expected: Option<&str>,
    ) -> ManagedConfigPatch {
        ManagedConfigPatch {
            owner_id: "hub-owner".into(),
            path: path.iter().map(|s| (*s).to_string()).collect(),
            value,
            expected_base_hash: expected.map(|s| s.to_string()),
        }
    }

    const SEED: &str = r#"# keep header comment
model = "gpt-user"

[mcp_servers.user-owned]
command = "uvx"
# user note
args = ["tool-a", "tool-b"]

[mcp_servers.cc_partner_x]
command = "old-cmd"
args = ["--flag"]

[agents.cc_partner_y]
description = "managed agent"
config_file = "agents/y.toml"
"#;

    #[test]
    fn toml_preserves_unrelated_spans_when_patching_owned_keys() {
        let patcher = TomlConfigPatcher;
        let before = SEED.as_bytes();
        let result = patcher
            .apply(
                before,
                &[
                    p(
                        &["mcp_servers", "cc_partner_x"],
                        Some(serde_json::json!({
                            "command": "new-cmd",
                            "args": ["--new"]
                        })),
                        None,
                    ),
                    p(
                        &["agents", "cc_partner_y"],
                        Some(serde_json::json!({
                            "description": "updated agent",
                            "config_file": "agents/y.toml"
                        })),
                        None,
                    ),
                ],
            )
            .expect("apply");
        assert!(matches!(result.outcome, ConfigPatchOutcome::Applied));
        let after = String::from_utf8(result.bytes.clone()).expect("utf8");
        assert!(after.contains("# keep header comment"));
        assert!(after.contains("model = \"gpt-user\""));
        assert!(after.contains("[mcp_servers.user-owned]"));
        assert!(after.contains("# user note") || after.contains("user note"));
        assert!(after.contains("tool-a"));
        assert!(after.contains("tool-b"));
        // owned updated
        assert!(after.contains("new-cmd") || after.contains("command = \"new-cmd\""));
        assert!(
            after.contains("updated agent") || after.contains("description = \"updated agent\"")
        );
        // order of user args preserved as array items
        let user_pos_a = after.find("tool-a").expect("a");
        let user_pos_b = after.find("tool-b").expect("b");
        assert!(user_pos_a < user_pos_b);
    }

    #[test]
    fn toml_conflict_when_expected_base_hash_mismatches() {
        let patcher = TomlConfigPatcher;
        let current = patcher
            .inspect(
                SEED.as_bytes(),
                &["mcp_servers".into(), "cc_partner_x".into()],
            )
            .expect("inspect");
        let wrong = "0".repeat(64);
        let result = patcher
            .apply(
                SEED.as_bytes(),
                &[p(
                    &["mcp_servers", "cc_partner_x"],
                    Some(serde_json::json!({"command":"hijack"})),
                    Some(&wrong),
                )],
            )
            .expect("apply");
        match result.outcome {
            ConfigPatchOutcome::Conflict {
                current_hash,
                expected_base_hash,
                ..
            } => {
                assert_eq!(current_hash, current.value_hash);
                assert_eq!(expected_base_hash.as_deref(), Some(wrong.as_str()));
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        // 原字节未改
        assert_eq!(result.bytes, SEED.as_bytes());
        assert!(!String::from_utf8_lossy(&result.bytes).contains("hijack"));
    }

    #[test]
    fn toml_cas_success_when_hash_matches() {
        let patcher = TomlConfigPatcher;
        let current = patcher
            .inspect(
                SEED.as_bytes(),
                &["mcp_servers".into(), "cc_partner_x".into()],
            )
            .expect("inspect");
        let hash = current.value_hash.expect("hash");
        let result = patcher
            .apply(
                SEED.as_bytes(),
                &[p(
                    &["mcp_servers", "cc_partner_x"],
                    Some(serde_json::json!({"command":"ok"})),
                    Some(&hash),
                )],
            )
            .expect("apply");
        assert!(matches!(result.outcome, ConfigPatchOutcome::Applied));
        assert!(String::from_utf8_lossy(&result.bytes).contains("ok"));
    }

    #[test]
    fn toml_removal_keeps_user_table() {
        let patcher = TomlConfigPatcher;
        let result = patcher
            .apply(
                SEED.as_bytes(),
                &[p(&["mcp_servers", "cc_partner_x"], None, None)],
            )
            .expect("apply");
        assert!(matches!(result.outcome, ConfigPatchOutcome::Applied));
        let after = String::from_utf8(result.bytes).expect("utf8");
        assert!(!after.contains("cc_partner_x") || !after.contains("old-cmd"));
        assert!(after.contains("[mcp_servers.user-owned]"));
        assert!(after.contains("model = \"gpt-user\""));
    }

    #[test]
    fn toml_parent_type_mismatch_is_blocked() {
        let seed = r#"
mcp_servers = "not-a-table"
"#;
        let patcher = TomlConfigPatcher;
        let result = patcher
            .apply(
                seed.as_bytes(),
                &[p(
                    &["mcp_servers", "cc_partner_x"],
                    Some(serde_json::json!({"command":"x"})),
                    None,
                )],
            )
            .expect("apply");
        assert!(matches!(result.outcome, ConfigPatchOutcome::Blocked { .. }));
        assert_eq!(result.bytes, seed.as_bytes());
    }

    #[test]
    fn toml_invalid_document_blocked() {
        let seed = b"[[[not valid";
        let patcher = TomlConfigPatcher;
        let result = patcher.apply(seed, &[]).expect("apply");
        // empty patches on invalid still blocked when parse fails
        assert!(matches!(result.outcome, ConfigPatchOutcome::Blocked { .. }));
    }

    #[test]
    fn inspect_owned_mcp_value_hash_stable() {
        let patcher = TomlConfigPatcher;
        let v1 = patcher
            .inspect(
                SEED.as_bytes(),
                &["mcp_servers".into(), "cc_partner_x".into()],
            )
            .expect("i1");
        let v2 = patcher
            .inspect(
                SEED.as_bytes(),
                &["mcp_servers".into(), "cc_partner_x".into()],
            )
            .expect("i2");
        assert_eq!(v1.value_hash, v2.value_hash);
        assert_eq!(v1.value_hash, Some(value_content_hash(&v1.value)));
    }
}
