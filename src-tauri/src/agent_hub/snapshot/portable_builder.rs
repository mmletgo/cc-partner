//! agent_hub/snapshot/portable_builder — 从 portable inventory 选择构建 SnapshotEnvelope
//!
//! Business Logic（为什么需要这个模块）:
//!     同类 Agent 远端 Pull 需要把用户勾选的 inventory 项冻结为可验证 SnapshotEnvelope/CAS，
//!     且**不得**在源端强制 adoption；选择只描述源事实，导入后由目标侧决定 install。
//!
//! Code Logic（这个模块做什么）:
//!     按 inventory item 读取源路径 → 写入 ObjectStore → 组装最小 v1 envelope；
//!     MCP 原文进 CAS；legacyLossy 占位标记 blocked。

use crate::agent_hub::assets::{
    canonical_bytes, from_canonical_bytes, CommandArgument, McpTransport, PortableAssetPayload,
    PortableCommand, PortableMcpServer, PortableSkill,
};
use crate::agent_hub::models::{
    AgentTarget, AssetKind, AssetPolicy, RevisionOperation, RevisionOriginKind, ScopeKind,
};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::portable_inventory::{PortableAssetKind, PortableInventoryItemDto};
use crate::agent_hub::snapshot::envelope::{
    compute_snapshot_hash, SnapshotAsset, SnapshotEnvelopeV1, SnapshotLineage,
    SnapshotObjectDescriptor, SnapshotRevision, SnapshotSelection, CANONICALIZATION_NAME,
    FORMAT_NAME, FORMAT_VERSION,
};
use crate::agent_hub::targets::portable::{
    hash_skill_directory_dereferenced, parse_simple_frontmatter, unknown_fields_extension,
};
use crate::error::AppError;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// 旧 peer MCP 脱敏占位串（不得当真实凭据导入）。
pub const LEGACY_LOSSY_PLACEHOLDER: &str = "__REDACTED_BY_CLAUDE_PARTNER__";

const KNOWN_SKILL_KEYS: &[&str] = &["name", "description"];
const KNOWN_COMMAND_KEYS: &[&str] = &["name", "description", "argument-hint", "argument_hint"];

/// 源端 selection 构建时累计 object 字节上限（与 pull staging 对齐，读 CAS 前 fail-closed）。
pub const PORTABLE_SELECTION_MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;

/// 单条选择项在 envelope 中的冻结描述。
///
/// Business Logic: preview/apply 需绑定 identity、hash、是否 credential-bearing / legacyLossy。
/// Code Logic: camelCase；携带 inventory_item_id 与 object hash。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableSelectionItem {
    /// 远端 inventory item id
    pub inventory_item_id: String,
    /// 逻辑 asset id（确定性派生）
    pub asset_id: String,
    /// 目标 Agent
    pub target: AgentTarget,
    /// 四类 kind
    pub kind: PortableAssetKind,
    /// 原生 ID
    pub native_id: String,
    /// 展示名
    pub display_name: String,
    /// scope id
    pub scope_id: String,
    /// 内容 object hash
    pub object_hash: String,
    /// 对象字节数
    pub object_size: u64,
    /// 构建时冻结的 inventory content_hash（供 destination 绑定 revalidated inventory）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// 构建时冻结的 inventory tree_hash
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
    /// 是否含 MCP 凭据材料
    pub credential_bearing: bool,
    /// 是否为 legacyLossy 占位（blocked，不得覆盖）
    pub legacy_lossy: bool,
    /// 警告（无 secret）
    pub warnings: Vec<String>,
}

/// portable selection 构建结果。
#[derive(Debug, Clone)]
pub struct BuiltPortableSelection {
    /// SnapshotEnvelope v1
    pub envelope: SnapshotEnvelopeV1,
    /// 选择项冻结列表
    pub items: Vec<PortableSelectionItem>,
    /// object_hash → 完整字节（供源端分块服务）
    pub object_bytes: BTreeMap<String, Vec<u8>>,
}

/// 从 inventory 项构建选择 envelope 并写入本机 CAS。
///
/// Business Logic（为什么需要这个函数）:
///     源端只冻结用户勾选事实；不创建 ownership/binding；跨 target 调用方必须先 fail。
///
/// Code Logic（这个函数做什么）:
///     过滤 selection → 读路径/payload → put_blob → 组装 envelope + 填 snapshot_hash。
pub async fn build_portable_selection_envelope(
    store: &ObjectStore,
    source_replica_id: &str,
    source_target: AgentTarget,
    selected: &[PortableInventoryItemDto],
) -> Result<BuiltPortableSelection, AppError> {
    let mut assets = Vec::new();
    let mut lineages = Vec::new();
    let mut revisions = Vec::new();
    let mut objects = Vec::new();
    let mut asset_heads = BTreeMap::new();
    let mut items_out = Vec::new();
    let mut object_bytes = BTreeMap::new();
    let mut seen_hashes = BTreeSet::new();
    let mut projected_object_bytes: u64 = 0;

    for item in selected {
        if item.target != source_target {
            return Err(AppError::validation(format!(
                "PORTABLE_PULL_TARGET_MISMATCH:item={} expected={} got={}",
                item.inventory_item_id,
                source_target.as_str(),
                item.target.as_str()
            )));
        }

        // Skill/Plugin 目录先写 CAS tree，plugin 主 payload 用权威 treeManifestHash 指针。
        let mut cas_tree_hash: Option<String> = None;
        if matches!(
            item.kind,
            PortableAssetKind::Skill | PortableAssetKind::Plugin
        ) {
            if let Some(path) = item.source_path.as_deref().map(PathBuf::from) {
                let is_file = path.is_file();
                let dir = if path.is_dir() {
                    path.clone()
                } else {
                    path.parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| path.clone())
                };
                if dir.is_dir() {
                    match store.put_tree_from_directory(&dir).await {
                        Ok(put) => {
                            let th = put.object.hash.clone();
                            cas_tree_hash = Some(th.clone());
                            if !seen_hashes.contains(&th) {
                                seen_hashes.insert(th.clone());
                                if let Ok(manifest_bytes) = store.get_blob(&th).await {
                                    ensure_selection_bytes_budget(
                                        projected_object_bytes,
                                        manifest_bytes.len() as u64,
                                    )?;
                                    projected_object_bytes = projected_object_bytes
                                        .saturating_add(manifest_bytes.len() as u64);
                                    object_bytes.insert(th.clone(), manifest_bytes.clone());
                                    objects.push(SnapshotObjectDescriptor {
                                        hash: th.clone(),
                                        size: manifest_bytes.len().to_string(),
                                    });
                                }
                            }
                            if let Ok(manifest) = store.get_tree(&put.object.hash).await {
                                for entry in manifest.entries {
                                    if !seen_hashes.contains(&entry.blob_hash) {
                                        // 先读 blob 会瞬时占用内存；在插入前检查 projected 上限。
                                        if let Ok(b) = store.get_blob(&entry.blob_hash).await {
                                            ensure_selection_bytes_budget(
                                                projected_object_bytes,
                                                b.len() as u64,
                                            )?;
                                            projected_object_bytes = projected_object_bytes
                                                .saturating_add(b.len() as u64);
                                            seen_hashes.insert(entry.blob_hash.clone());
                                            object_bytes.insert(entry.blob_hash.clone(), b.clone());
                                            objects.push(SnapshotObjectDescriptor {
                                                hash: entry.blob_hash,
                                                size: b.len().to_string(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) if item.kind == PortableAssetKind::Plugin => {
                            return Err(AppError::validation(format!(
                                "PORTABLE_PULL_PLUGIN_TREE_UNAVAILABLE:{}:{e}",
                                item.inventory_item_id
                            )));
                        }
                        Err(_) => {}
                    }
                } else if item.kind == PortableAssetKind::Plugin && !is_file {
                    return Err(AppError::validation(format!(
                        "PORTABLE_PULL_PLUGIN_TREE_UNAVAILABLE:{}",
                        item.inventory_item_id
                    )));
                }
            }
        }

        let packed = if item.kind == PortableAssetKind::Plugin {
            pack_plugin_item(item, cas_tree_hash.as_deref())?
        } else {
            pack_inventory_item(item)?
        };
        let hash = sha256_hex(&packed.bytes);
        let size = packed.bytes.len() as u64;
        if !seen_hashes.contains(&hash) {
            ensure_selection_bytes_budget(projected_object_bytes, size)?;
            if !packed.bytes.is_empty() {
                store.put_blob(&packed.bytes).await?;
            }
            projected_object_bytes = projected_object_bytes.saturating_add(size);
            seen_hashes.insert(hash.clone());
            object_bytes.insert(hash.clone(), packed.bytes.clone());
            objects.push(SnapshotObjectDescriptor {
                hash: hash.clone(),
                size: size.to_string(),
            });
        } else {
            object_bytes
                .entry(hash.clone())
                .or_insert_with(|| packed.bytes.clone());
        }

        let asset_id = stable_asset_id(item);
        let rev_id = format!("{asset_id}-rev1");
        let tree_manifest_hash = cas_tree_hash.or_else(|| item.tree_hash.clone());
        assets.push(SnapshotAsset {
            id: asset_id.clone(),
            scope_id: item.scope_id.clone(),
            kind: item.kind.to_asset_kind(),
            origin_namespace: item.source_origin.default_origin_namespace().to_string(),
            logical_key: item.native_id.clone(),
            display_name: item.display_name.clone(),
            policy: AssetPolicy::Shared,
            deleted_at: None,
        });
        lineages.push(SnapshotLineage {
            id: asset_id.clone(),
            root_asset_id: asset_id.clone(),
        });
        revisions.push(SnapshotRevision {
            id: rev_id.clone(),
            asset_lineage_id: asset_id.clone(),
            parents: vec![],
            generation: "0".into(),
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: Some(source_target),
            origin_replica_id: source_replica_id.to_string(),
            payload_hash: Some(hash.clone()),
            tree_manifest_hash,
            created_at: Utc::now().to_rfc3339(),
        });
        asset_heads.insert(asset_id.clone(), vec![rev_id]);
        items_out.push(PortableSelectionItem {
            inventory_item_id: item.inventory_item_id.clone(),
            asset_id,
            target: item.target,
            kind: item.kind,
            native_id: item.native_id.clone(),
            display_name: item.display_name.clone(),
            scope_id: item.scope_id.clone(),
            object_hash: hash,
            object_size: size,
            content_hash: item.content_hash.clone(),
            tree_hash: item.tree_hash.clone(),
            credential_bearing: packed.credential_bearing,
            legacy_lossy: packed.legacy_lossy,
            warnings: packed.warnings,
        });
    }

    let asset_ids: Vec<String> = assets.iter().map(|a| a.id.clone()).collect();
    let scope_ids: BTreeSet<String> = assets.iter().map(|a| a.scope_id.clone()).collect();
    let mut envelope = SnapshotEnvelopeV1 {
        format: FORMAT_NAME.into(),
        format_version: FORMAT_VERSION,
        canonicalization: CANONICALIZATION_NAME.into(),
        snapshot_id: Uuid::now_v7().to_string(),
        snapshot_hash: "0".repeat(64),
        source_replica_id: source_replica_id.to_string(),
        created_at: Utc::now().to_rfc3339(),
        selection: SnapshotSelection {
            scope_ids: scope_ids.into_iter().collect(),
            asset_ids,
            include_history: false,
        },
        asset_heads,
        assets,
        lineages,
        revisions,
        variants: vec![],
        conflicts: vec![],
        aliases: vec![],
        objects,
    };
    envelope.snapshot_hash = compute_snapshot_hash(&envelope)
        .map_err(|e| AppError::generic(format!("portable snapshot hash: {e}")))?;

    Ok(BuiltPortableSelection {
        envelope,
        items: items_out,
        object_bytes,
    })
}

/// 累计 object_bytes 前检查 selection 上限，超限立即拒绝（不得读完再拒）。
fn ensure_selection_bytes_budget(current: u64, next: u64) -> Result<(), AppError> {
    if current.saturating_add(next) > PORTABLE_SELECTION_MAX_OBJECT_BYTES {
        return Err(AppError::validation(
            "PORTABLE_PULL_STAGING_LIMIT".to_string(),
        ));
    }
    Ok(())
}

/// 打包后的 payload 字节与诊断。
#[derive(Debug)]
pub(crate) struct PackedItem {
    pub bytes: Vec<u8>,
    pub credential_bearing: bool,
    pub legacy_lossy: bool,
    pub warnings: Vec<String>,
}

/// 打包 Plugin：单文件原文；目录必须有 CAS treeManifestHash 指针（禁止 hash-list 假成功）。
pub(crate) fn pack_plugin_item(
    item: &PortableInventoryItemDto,
    cas_tree_hash: Option<&str>,
) -> Result<PackedItem, AppError> {
    let warnings = item.warnings.clone();
    let path = item
        .source_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    let Some(p) = path else {
        return Err(AppError::validation(format!(
            "PORTABLE_PULL_SOURCE_PATH_MISSING:{}",
            item.inventory_item_id
        )));
    };
    if p.is_file() {
        let bytes = std::fs::read(&p)
            .map_err(|e| AppError::generic(format!("read plugin {}: {e}", p.display())))?;
        return Ok(PackedItem {
            bytes,
            credential_bearing: false,
            legacy_lossy: false,
            warnings,
        });
    }
    if !p.is_dir() {
        return Err(AppError::not_found(format!(
            "PORTABLE_PULL_SOURCE_PATH_MISSING:{}",
            item.inventory_item_id
        )));
    }
    let tree_hash = cas_tree_hash
        .map(str::to_string)
        .or_else(|| item.tree_hash.clone())
        .filter(|h| !h.is_empty());
    let Some(tree_hash) = tree_hash else {
        return Err(AppError::validation(format!(
            "PORTABLE_PULL_PLUGIN_TREE_UNAVAILABLE:{}",
            item.inventory_item_id
        )));
    };
    let pointer = serde_json::json!({
        "kind": "portablePluginTreeRef",
        "rootName": p.file_name().and_then(|s| s.to_str()).unwrap_or("plugin"),
        "treeManifestHash": tree_hash,
        "nativeId": item.native_id,
    });
    let bytes = serde_json::to_vec(&pointer).map_err(AppError::from)?;
    Ok(PackedItem {
        bytes,
        credential_bearing: false,
        legacy_lossy: false,
        warnings,
    })
}

/// 将 inventory 项读为 canonical payload 字节。
pub(crate) fn pack_inventory_item(item: &PortableInventoryItemDto) -> Result<PackedItem, AppError> {
    let mut warnings = item.warnings.clone();
    let path = item
        .source_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());

    match item.kind {
        PortableAssetKind::Skill => {
            let Some(p) = path else {
                return Err(AppError::validation(format!(
                    "PORTABLE_PULL_SOURCE_PATH_MISSING:{}",
                    item.inventory_item_id
                )));
            };
            let payload = load_skill_payload(&p, item)?;
            Ok(PackedItem {
                bytes: canonical_bytes(&payload)?,
                credential_bearing: false,
                legacy_lossy: false,
                warnings,
            })
        }
        PortableAssetKind::Command => {
            let Some(p) = path else {
                return Err(AppError::validation(format!(
                    "PORTABLE_PULL_SOURCE_PATH_MISSING:{}",
                    item.inventory_item_id
                )));
            };
            let payload = load_command_payload(&p, item)?;
            Ok(PackedItem {
                bytes: canonical_bytes(&payload)?,
                credential_bearing: false,
                legacy_lossy: false,
                warnings,
            })
        }
        PortableAssetKind::Plugin => {
            // Prefer pack_plugin_item from envelope builder (has CAS tree hash).
            pack_plugin_item(item, None)
        }
        PortableAssetKind::Mcp => {
            let Some(p) = path else {
                warnings.push("mcp source path missing".into());
                return Ok(PackedItem {
                    bytes: Vec::new(),
                    credential_bearing: item
                        .mcp_credential
                        .as_ref()
                        .map(|c| c.present)
                        .unwrap_or(false),
                    legacy_lossy: true,
                    warnings,
                });
            };
            let raw = std::fs::read(&p)
                .map_err(|e| AppError::generic(format!("read mcp {}: {e}", p.display())))?;
            let text = String::from_utf8_lossy(&raw);
            if text.contains(LEGACY_LOSSY_PLACEHOLDER) {
                warnings.push("legacyLossy placeholder detected".into());
                return Ok(PackedItem {
                    bytes: raw,
                    credential_bearing: false,
                    legacy_lossy: true,
                    warnings,
                });
            }
            let credential_bearing = text_contains_credential_keys(&text);
            match load_mcp_payload(&raw, item) {
                Ok(payload) => Ok(PackedItem {
                    bytes: canonical_bytes(&payload)?,
                    credential_bearing,
                    legacy_lossy: false,
                    warnings,
                }),
                Err(_) => Ok(PackedItem {
                    bytes: raw,
                    credential_bearing,
                    legacy_lossy: false,
                    warnings,
                }),
            }
        }
    }
}

fn load_skill_payload(
    path: &Path,
    item: &PortableInventoryItemDto,
) -> Result<PortableAssetPayload, AppError> {
    let skill_md = if path.is_file() {
        path.to_path_buf()
    } else {
        path.join("SKILL.md")
    };
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf())
    };
    let text = std::fs::read_to_string(&skill_md)
        .map_err(|e| AppError::generic(format!("read skill {}: {e}", skill_md.display())))?;
    let (fields, _, _) = parse_simple_frontmatter(&text);
    let dir_name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&item.native_id)
        .to_string();
    let name = fields
        .get("name")
        .cloned()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(dir_name);
    let description = fields.get("description").cloned().unwrap_or_default();
    // 打包链路只读：仓库软链根（~/.agents 等）跟随到真树 hash；本机写路径不受影响。
    let (skill_hash, tree_hash, _, _) = hash_skill_directory_dereferenced(&dir).map_err(|e| {
        AppError::validation(format!(
            "PORTABLE_PULL_SKILL_HASH:{}:{e}",
            item.inventory_item_id
        ))
    })?;
    let (extensions, _) =
        unknown_fields_extension(item.target, &fields, KNOWN_SKILL_KEYS, "/frontmatter");
    Ok(PortableAssetPayload::Skill(PortableSkill {
        name,
        description,
        skill_markdown_hash: skill_hash,
        tree_manifest_hash: tree_hash,
        target_extensions: extensions,
    }))
}

fn load_command_payload(
    path: &Path,
    item: &PortableInventoryItemDto,
) -> Result<PortableAssetPayload, AppError> {
    let bytes = std::fs::read(path)
        .map_err(|e| AppError::generic(format!("read command {}: {e}", path.display())))?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let (fields, _, body) = parse_simple_frontmatter(&text);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&item.native_id)
        .to_string();
    let name = fields
        .get("name")
        .cloned()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(stem);
    let description = fields.get("description").cloned();
    let (extensions, _) =
        unknown_fields_extension(item.target, &fields, KNOWN_COMMAND_KEYS, "/frontmatter");
    let arguments = parse_argument_hint(
        fields
            .get("argument-hint")
            .or_else(|| fields.get("argument_hint")),
    );
    Ok(PortableAssetPayload::Command(PortableCommand {
        name,
        description,
        prompt_template: body.to_string(),
        arguments,
        target_extensions: extensions,
    }))
}

fn parse_argument_hint(raw: Option<&String>) -> Vec<CommandArgument> {
    let Some(hint) = raw.map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    // 粗解析：逗号/空格分隔的 name
    hint.split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|name| CommandArgument {
            name: name.to_string(),
            description: None,
            required: false,
        })
        .collect()
}

fn load_mcp_payload(
    raw: &[u8],
    item: &PortableInventoryItemDto,
) -> Result<PortableAssetPayload, AppError> {
    let value: serde_json::Value =
        serde_json::from_slice(raw).map_err(|e| AppError::validation(format!("mcp json: {e}")))?;
    // 支持两种形态：单 server object 或 {servers:{key:obj}} / 直接 key 对象
    let server_obj = if let Some(servers) = value.get("mcpServers").or_else(|| value.get("servers"))
    {
        servers
            .get(&item.native_id)
            .cloned()
            .or_else(|| servers.as_object().and_then(|m| m.values().next().cloned()))
            .unwrap_or(value.clone())
    } else {
        value
    };
    let obj = server_obj
        .as_object()
        .ok_or_else(|| AppError::validation("mcp_server_not_object"))?;
    let enabled = obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
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
            .unwrap_or("npx")
            .to_string();
        let args = obj
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let cwd = obj.get("cwd").and_then(|v| v.as_str()).map(str::to_string);
        McpTransport::Stdio { command, args, cwd }
    };
    Ok(PortableAssetPayload::Mcp(PortableMcpServer {
        key: item.native_id.clone(),
        transport,
        env,
        enabled,
        tool_allow: vec![],
        tool_deny: vec![],
        target_extensions: BTreeMap::new(),
    }))
}

fn text_contains_credential_keys(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("\"env\"")
        || lower.contains("authorization")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("token")
        || lower.contains("secret")
}

/// 稳定 asset id：sha256(target|scope|native|kind) 截断为 UUID 风格 hex。
fn stable_asset_id(item: &PortableInventoryItemDto) -> String {
    let material = format!(
        "portable|{}|{}|{}|{}",
        item.target.as_str(),
        item.scope_id,
        item.kind.as_str(),
        item.native_id
    );
    let h = sha256_hex(material.as_bytes());
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// 检测字节是否含 legacyLossy 占位。
pub fn bytes_are_legacy_lossy(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes).contains(LEGACY_LOSSY_PLACEHOLDER)
}

/// 从 CAS 字节恢复 payload（失败返回 None）。
pub fn try_payload_from_bytes(bytes: &[u8]) -> Option<PortableAssetPayload> {
    from_canonical_bytes(bytes).ok()
}

/// kind 从 AssetKind 映射（仅 portable 四类）。
pub fn portable_kind_from_asset(kind: AssetKind) -> Option<PortableAssetKind> {
    PortableAssetKind::try_from_asset_kind(kind).ok()
}

/// scope kind 透传辅助。
pub fn scope_kind_of(item: &PortableInventoryItemDto) -> ScopeKind {
    item.scope_kind
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::DesiredPresence;
    use crate::agent_hub::portable_inventory::{
        PortableAssetOwner, PortableInventoryItemCapabilitiesDto, PortableInventoryManagementState,
        PortableInventorySourceOrigin, PortableMcpCredentialFactDto, PortableOriginKind,
    };
    use tempfile::tempdir;

    fn sample_item(
        target: AgentTarget,
        kind: PortableAssetKind,
        native: &str,
        path: Option<&str>,
    ) -> PortableInventoryItemDto {
        PortableInventoryItemDto {
            inventory_item_id: format!("id-{native}"),
            target,
            loaded_by: target,
            owned_by: PortableAssetOwner::from_target(target),
            origin_kind: PortableOriginKind::Native,
            native_output_candidate: true,
            kind,
            native_id: native.into(),
            display_name: native.into(),
            description: None,
            version: None,
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            source_path: path.map(|s| s.to_string()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            content_hash: Some("abc".into()),
            tree_hash: None,
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::Unmanaged,
            desired_presence: Some(DesiredPresence::Present),
            desired_enabled: Some(true),
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto {
                can_enable: true,
                can_disable: true,
                can_uninstall: true,
                can_adopt: true,
                can_install_to_source_target: true,
                can_migrate_to_store: false,
                can_attach: false,
                can_detach: false,
                can_destroy_store: false,
                can_confirm_current_version: false,
                can_materialize_escape_link: false,

                reason_code: None,
                evidence_ids: vec![],
            },
            warnings: vec![],
            mcp_credential: None,
            store: Default::default(),
        }
    }

    #[test]
    fn plugin_directory_without_tree_hash_fails_closed() {
        let tmp = tempdir().unwrap();
        let plugin = tmp.path().join("my-plugin");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("plugin.json"), r#"{"name":"my-plugin"}"#).unwrap();
        let mut item = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Plugin,
            "my-plugin",
            Some(plugin.to_str().unwrap()),
        );
        item.tree_hash = None;
        let err = pack_plugin_item(&item, None).unwrap_err();
        assert!(err
            .to_string()
            .contains("PORTABLE_PULL_PLUGIN_TREE_UNAVAILABLE"));
        // with CAS tree hash pointer succeeds and is not the old hash-list shape
        let packed = pack_plugin_item(&item, Some("deadbeef")).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&packed.bytes).unwrap();
        assert_eq!(v["kind"], "portablePluginTreeRef");
        assert_eq!(v["treeManifestHash"], "deadbeef");
        assert_ne!(v["kind"], "portablePluginTree");
    }

    #[tokio::test]
    async fn builder_rejects_cross_target_items_before_cas_write() {
        let tmp = tempdir().unwrap();
        let store = ObjectStore::open(tmp.path()).unwrap();
        let cmd = tmp.path().join("hello.md");
        std::fs::write(&cmd, "# Hello\n\nbody\n").unwrap();
        let items = vec![sample_item(
            AgentTarget::Codex,
            PortableAssetKind::Command,
            "hello",
            Some(cmd.to_str().unwrap()),
        )];
        let err =
            build_portable_selection_envelope(&store, "device-a", AgentTarget::Claude, &items)
                .await
                .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("PORTABLE_PULL_TARGET_MISMATCH"), "{msg}");
    }

    #[tokio::test]
    async fn builder_packs_command_and_sets_snapshot_hash() {
        let tmp = tempdir().unwrap();
        let store = ObjectStore::open(tmp.path()).unwrap();
        let cmd = tmp.path().join("ship.md");
        std::fs::write(&cmd, "---\nname: ship\n---\nDo ship\n").unwrap();
        let items = vec![sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Command,
            "ship",
            Some(cmd.to_str().unwrap()),
        )];
        let built =
            build_portable_selection_envelope(&store, "device-a", AgentTarget::Claude, &items)
                .await
                .unwrap();
        assert_eq!(built.envelope.assets.len(), 1);
        assert_eq!(built.items.len(), 1);
        assert_eq!(built.envelope.snapshot_hash.len(), 64);
        assert!(!built.object_bytes.is_empty());
    }

    #[test]
    fn legacy_lossy_placeholder_detected() {
        let s = format!(r#"{{"env":{{"K":"{LEGACY_LOSSY_PLACEHOLDER}"}}}}"#);
        assert!(bytes_are_legacy_lossy(s.as_bytes()));
        assert!(!bytes_are_legacy_lossy(br#"{"env":{"K":"real"}}"#));
    }

    #[tokio::test]
    async fn mcp_legacy_lossy_flagged_without_adopting() {
        let tmp = tempdir().unwrap();
        let store = ObjectStore::open(tmp.path()).unwrap();
        let mcp = tmp.path().join("mcp.json");
        let body = format!(r#"{{"command":"npx","env":{{"TOKEN":"{LEGACY_LOSSY_PLACEHOLDER}"}}}}"#);
        std::fs::write(&mcp, &body).unwrap();
        let mut item = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Mcp,
            "old-peer",
            Some(mcp.to_str().unwrap()),
        );
        item.mcp_credential = Some(PortableMcpCredentialFactDto {
            present: true,
            hash: Some("x".into()),
        });
        let built =
            build_portable_selection_envelope(&store, "device-a", AgentTarget::Claude, &[item])
                .await
                .unwrap();
        assert!(built.items[0].legacy_lossy);
    }

    #[tokio::test]
    async fn builder_aborts_when_object_bytes_would_exceed_staging_limit() {
        // R2-P1-4：读取/累计过程中超限必须 PORTABLE_PULL_STAGING_LIMIT，不得整包读完再拒。
        let tmp = tempdir().unwrap();
        let store = ObjectStore::open(tmp.path()).unwrap();
        let oversized = tmp.path().join("huge.md");
        let body = vec![b'x'; (PORTABLE_SELECTION_MAX_OBJECT_BYTES as usize) + 1];
        std::fs::write(&oversized, &body).unwrap();
        let items = vec![sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Command,
            "huge",
            Some(oversized.to_str().unwrap()),
        )];
        let err =
            build_portable_selection_envelope(&store, "device-a", AgentTarget::Claude, &items)
                .await
                .unwrap_err();
        assert!(
            err.to_string().contains("PORTABLE_PULL_STAGING_LIMIT"),
            "expected staging limit abort, got {err}"
        );
    }
}
