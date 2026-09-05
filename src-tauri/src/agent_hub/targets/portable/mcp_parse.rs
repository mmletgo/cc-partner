//! agent_hub/targets/portable/mcp_parse — MCP 配置解析
//!
//! Business Logic（为什么需要这个模块）:
//!     各 CLI Agent 的 MCP server 配置分散在 JSON/JSONC（Claude/OpenCode `mcpServers`）
//!     与 TOML（Codex `mcp_servers` / `agents` 表）中；Hub 需要只读解析为统一的
//!     MCP/Agent 发现记录，且 content_hash 必须与 portable action CAS 同域，
//!     避免键序/规范差异造成假漂移。
//!
//! Code Logic（这个模块做什么）:
//!     从原 portable.rs 拆出：`parse_mcp_servers_json_map` JSON map 解析、
//!     `parse_codex_mcp_toml` / `parse_codex_agents_toml` TOML 表解析、
//!     `parse_json_or_jsonc` JSONC 兼容解析；未知字段保留进 target_extensions。

use crate::{
    agent_hub::{
        assets::{
            McpTransport, PortabilityDiagnostic, PortableAgent, PortableAssetPayload,
            PortableMcpServer, CODE_UNKNOWN_SOURCE_FIELD,
        },
        config_patch::value_content_hash,
        models::{AgentTarget, AssetKind, ScopeKind},
        object_store::sha256_hex,
    },
    error::AppError,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use super::{
    DiscoveredPortableAsset, PortableAssetOrigin, PortableAssetOwner, PortableDiscoveryStatus,
    PortableOriginKind,
};

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
