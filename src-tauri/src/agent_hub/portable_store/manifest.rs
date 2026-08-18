//! portable_store/manifest — store 目录账本
//!
//! Business Logic（为什么需要这个模块）:
//!     盘点去重、附加状态与彻底删除需要稳定 storeId 与 attachment 列表；
//!     真源仍是磁盘软链/leaf，manifest 是可重建的索引。
//!
//! Code Logic（这个模块做什么）:
//!     读写 `manifest.json`；按 canonical 路径推导 storeId。

use super::{store_id_for, validate_store_native_id};
use crate::agent_hub::models::AgentTarget;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// store 资产类别（不含 Plugin）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortableStoreKind {
    /// Skill 目录
    Skill,
    /// Command markdown
    Command,
    /// MCP 目录 JSON
    Mcp,
}

impl PortableStoreKind {
    /// 稳定 token。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Command => "command",
            Self::Mcp => "mcp",
        }
    }
}

/// 某 Agent 上的附加记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestAttachment {
    /// 附加的 Hub target
    pub target: AgentTarget,
    /// 该 target 上的 native 路径（软链或配置文件）
    pub path: String,
}

/// 单条 store 账本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableStoreManifestEntry {
    /// `skill:foo` / `command:bar` / `mcp:id`
    pub id: String,
    /// 资产类别
    pub kind: PortableStoreKind,
    /// 原生 id（目录名 / stem / server key）
    pub native_id: String,
    /// 内容 hash（可空，扫描后回填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// 已知附加
    #[serde(default)]
    pub attachments: Vec<ManifestAttachment>,
}

/// store 根 manifest。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableStoreManifest {
    /// 格式版本
    pub version: u32,
    /// 条目
    #[serde(default)]
    pub entries: Vec<PortableStoreManifestEntry>,
}

impl Default for PortableStoreManifest {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

/// 读取 manifest；文件不存在则返回空账本。
///
/// Business Logic: 扫描/附加不能因为缺账本失败。
/// Code Logic: 缺文件 → Default；坏 JSON → Validation。
pub fn load_manifest(store_root: &Path) -> Result<PortableStoreManifest, AppError> {
    let path = store_root.join("manifest.json");
    if !path.is_file() {
        return Ok(PortableStoreManifest::default());
    }
    let bytes = fs::read(&path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| AppError::validation(format!("PORTABLE_STORE_MANIFEST_INVALID:{e}")))
}

/// 原子写回 manifest。
///
/// Business Logic: 附加/拆除/迁移后要留下可重建索引。
/// Code Logic: 写临时文件再 rename。
pub fn save_manifest(store_root: &Path, manifest: &PortableStoreManifest) -> Result<(), AppError> {
    fs::create_dir_all(store_root)?;
    let path = store_root.join("manifest.json");
    let tmp = store_root.join("manifest.json.tmp");
    let bytes = serde_json::to_vec_pretty(manifest)?;
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// 插入或更新一条账本，并记录 attachment。
///
/// Business Logic: 同一 storeId 只保留一行；同 target 路径覆盖。
/// Code Logic: 按 id 查找，更新 hash/attachment。
pub fn upsert_manifest_entry(
    store_root: &Path,
    kind: PortableStoreKind,
    native_id: &str,
    content_hash: Option<String>,
    attachment: Option<ManifestAttachment>,
) -> Result<PortableStoreManifestEntry, AppError> {
    validate_store_native_id(native_id)?;
    let id = store_id_for(kind, native_id);
    let mut manifest = load_manifest(store_root)?;
    let entry = if let Some(existing) = manifest.entries.iter_mut().find(|e| e.id == id) {
        if content_hash.is_some() {
            existing.content_hash = content_hash;
        }
        if let Some(att) = attachment {
            existing.attachments.retain(|a| a.target != att.target);
            existing.attachments.push(att);
        }
        existing.clone()
    } else {
        let mut attachments = Vec::new();
        if let Some(att) = attachment {
            attachments.push(att);
        }
        let created = PortableStoreManifestEntry {
            id,
            kind,
            native_id: native_id.to_string(),
            content_hash,
            attachments,
        };
        manifest.entries.push(created.clone());
        created
    };
    save_manifest(store_root, &manifest)?;
    Ok(entry)
}

/// 去掉某 target 的 attachment；条目仍保留（真树还在）。
pub fn remove_manifest_attachment(
    store_root: &Path,
    store_id: &str,
    target: AgentTarget,
) -> Result<(), AppError> {
    let mut manifest = load_manifest(store_root)?;
    if let Some(entry) = manifest.entries.iter_mut().find(|e| e.id == store_id) {
        entry.attachments.retain(|a| a.target != target);
    }
    save_manifest(store_root, &manifest)?;
    Ok(())
}

/// 彻底删除账本条目。
pub fn remove_manifest_entry(store_root: &Path, store_id: &str) -> Result<(), AppError> {
    let mut manifest = load_manifest(store_root)?;
    manifest.entries.retain(|e| e.id != store_id);
    save_manifest(store_root, &manifest)?;
    Ok(())
}

/// 从 canonical 路径推导 storeId。
///
/// Business Logic: 软链目标必须落在 `portable-store/{skills,commands,mcp}/<id>`。
/// Code Logic: 剥 store 根后按第一段目录判断 kind。
pub fn store_id_from_canonical(canonical: &Path, store_root: &Path) -> Option<String> {
    let store = fs::canonicalize(store_root).ok()?;
    let rel = canonical.strip_prefix(&store).ok()?;
    let mut parts = rel.iter();
    let kind_dir = parts.next()?.to_str()?;
    let name = parts.next()?.to_str()?;
    match kind_dir {
        "skills" => {
            validate_store_native_id(name).ok()?;
            Some(store_id_for(PortableStoreKind::Skill, name))
        }
        "commands" => {
            let id = name.strip_suffix(".md").unwrap_or(name);
            validate_store_native_id(id).ok()?;
            Some(store_id_for(PortableStoreKind::Command, id))
        }
        "mcp" => {
            let id = name.strip_suffix(".json").unwrap_or(name);
            validate_store_native_id(id).ok()?;
            Some(store_id_for(PortableStoreKind::Mcp, id))
        }
        _ => None,
    }
}
