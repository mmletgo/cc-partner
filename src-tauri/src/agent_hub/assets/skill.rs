//! agent_hub/assets/skill — Canonical PortableSkill 载荷
//!
//! Business Logic（为什么需要这个模块）:
//!     Skill 是含 SKILL.md + supporting files 的目录树资产；common 字段跨 target 共享，
//!     Codex/OpenCode 专属元数据进入 target_extensions，脚本不自动改写。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `PortableSkill`、校验 name/description/hash，并在 CAS TreeManifest 上
//!     验证存在 `SKILL.md`（不改写 supporting files）。

use crate::agent_hub::assets::diagnostics::PortabilityDiagnostic;
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::object_store::TreeManifest;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Canonical Skill 可移植载荷。
///
/// Business Logic（为什么需要这个结构体）:
///     Hub 以 typed JSON 保存 skill 身份与 CAS 引用；supporting files 只通过 tree hash 引用。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 字段；target_extensions 用 BTreeMap 保证确定性序列化键序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableSkill {
    /// Skill 逻辑名
    pub name: String,
    /// 描述（common）
    pub description: String,
    /// SKILL.md 正文 blob SHA-256 hex
    pub skill_markdown_hash: String,
    /// 目录 TreeManifest SHA-256 hex（含 SKILL.md 与 supporting files）
    pub tree_manifest_hash: String,
    /// 各 target 未归一化/专属扩展字段
    pub target_extensions: BTreeMap<AgentTarget, serde_json::Value>,
}

impl PortableSkill {
    /// 校验 skill 载荷基本字段。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     空名、空 hash 不得进入 revision。
    ///
    /// Code Logic（这个函数做什么）:
    ///     trim name；校验 64 hex hash 形态。
    pub fn validate(&self) -> Result<(), AppError> {
        if self.name.trim().is_empty() {
            return Err(AppError::validation(
                "agent_hub_portable_skill_empty_name".to_string(),
            ));
        }
        validate_sha256_hex("skill_markdown_hash", &self.skill_markdown_hash)?;
        validate_sha256_hex("tree_manifest_hash", &self.tree_manifest_hash)?;
        Ok(())
    }

    /// 校验 CAS 树包含 SKILL.md，且 skill_markdown_hash 与树条目一致。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Skill 目录资产必须以 SKILL.md 为锚；supporting files 原字节保留，不改写脚本。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 TreeManifest 中查找 path 以 `SKILL.md` 结尾（正斜杠相对路径）；
    ///     校验 blob_hash == skill_markdown_hash；缺失返回 Validation。
    pub fn validate_tree_manifest(&self, manifest: &TreeManifest) -> Result<(), AppError> {
        let skill_entry = manifest.entries.iter().find(|e| {
            let p = e.path.as_str();
            p == "SKILL.md" || p.ends_with("/SKILL.md")
        });
        let Some(entry) = skill_entry else {
            return Err(AppError::validation(
                "agent_hub_portable_skill_tree_missing_skill_md".to_string(),
            ));
        };
        if entry.blob_hash != self.skill_markdown_hash {
            return Err(AppError::validation(
                "agent_hub_portable_skill_markdown_hash_mismatch".to_string(),
            ));
        }
        Ok(())
    }

    /// 扫描 supporting 路径上的绝对路径/可执行依赖诊断（不改写内容）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     脚本引用绝对路径时只记诊断，不自动改写。
    ///
    /// Code Logic（这个函数做什么）:
    ///     检查 tree 路径组件是否像绝对路径（以 `/` 或盘符开头的 path 字符串本身）；
    ///     对可执行 bit 文件标记 targetExecutable 提示。
    pub fn collect_tree_diagnostics(&self, manifest: &TreeManifest) -> Vec<PortabilityDiagnostic> {
        let mut out = Vec::new();
        for entry in &manifest.entries {
            if is_absolute_like_path(&entry.path) {
                out.push(
                    PortabilityDiagnostic::absolute_path(format!("tree/{}", entry.path))
                        .with_value_metadata(&entry.path),
                );
            }
            if entry.executable {
                out.push(PortabilityDiagnostic::target_executable(format!(
                    "tree/{}",
                    entry.path
                )));
            }
        }
        out
    }
}

/// 校验 SHA-256 hex。
///
/// Business Logic: 非法 hash 不得写入 revision。
/// Code Logic: 长度 64 且仅 `[0-9a-f]`。
fn validate_sha256_hex(field: &str, hash: &str) -> Result<(), AppError> {
    if hash.len() != 64 || !hash.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
        return Err(AppError::validation(format!(
            "agent_hub_portable_skill_invalid_hash:{field}"
        )));
    }
    Ok(())
}

/// 路径是否像绝对路径（诊断用，不跟随）。
///
/// Business Logic: 绝对路径跨机不可移植。
/// Code Logic: Unix `/` 前缀或 Windows `X:` 前缀。
fn is_absolute_like_path(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    let p = Path::new(path);
    p.is_absolute()
        || (path.len() >= 2
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::object_store::{TreeEntry, TreeEntryType};

    fn sample_skill() -> PortableSkill {
        PortableSkill {
            name: "review".into(),
            description: "Review changes".into(),
            skill_markdown_hash: "a".repeat(64),
            tree_manifest_hash: "b".repeat(64),
            target_extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn rejects_empty_name() {
        let mut s = sample_skill();
        s.name = "  ".into();
        assert!(s.validate().is_err());
    }

    #[test]
    fn tree_missing_skill_md_fails() {
        let s = sample_skill();
        let manifest = TreeManifest {
            entries: vec![TreeEntry {
                path: "scripts/run.sh".into(),
                blob_hash: "c".repeat(64),
                entry_type: TreeEntryType::File,
                executable: true,
            }],
        };
        let err = s.validate_tree_manifest(&manifest).unwrap_err();
        assert!(err.to_string().contains("missing_skill_md"));
    }

    #[test]
    fn tree_with_skill_md_ok_and_executable_diagnostic() {
        let s = sample_skill();
        let manifest = TreeManifest {
            entries: vec![
                TreeEntry {
                    path: "SKILL.md".into(),
                    blob_hash: "a".repeat(64),
                    entry_type: TreeEntryType::File,
                    executable: false,
                },
                TreeEntry {
                    path: "scripts/run.sh".into(),
                    blob_hash: "c".repeat(64),
                    entry_type: TreeEntryType::File,
                    executable: true,
                },
            ],
        };
        s.validate_tree_manifest(&manifest).unwrap();
        let diags = s.collect_tree_diagnostics(&manifest);
        assert!(diags.iter().any(|d| d.code == "targetExecutable"));
    }
}
