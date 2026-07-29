//! agent_hub/assets — Gate B 可移植资产 typed payload
//!
//! Business Logic（为什么需要这个模块）:
//!     Skill/Command/Agent/MCP 需要跨 CLI 的 canonical 形态：common 字段 + target_extensions；
//!     凭据原文进入 CAS，诊断/错误不得回显 secret。
//!
//! Code Logic（这个模块做什么）:
//!     导出各 payload 类型、`PortableAssetPayload` 标签枚举、确定性 canonical 序列化，
//!     以及与 ObjectStore/repo 协作的校验辅助。

pub mod agent;
pub mod command;
pub mod diagnostics;
pub mod mcp;
pub mod skill;

pub use agent::PortableAgent;
pub use command::{CommandArgument, PortableCommand};
pub use diagnostics::{
    format_validation_error_safe, redact_sensitive_text, PortabilityDiagnostic, CODE_ABSOLUTE_PATH,
    CODE_MATERIALIZED_ALIAS, CODE_MODEL_NOT_PORTABLE, CODE_PERMISSION_NOT_PORTABLE,
    CODE_TARGET_EXECUTABLE, CODE_UNKNOWN_SOURCE_FIELD, CODE_UNSUPPORTED_INTERPOLATION,
};
pub use mcp::{McpTransport, PortableMcpServer};
pub use skill::PortableSkill;

use crate::agent_hub::models::AssetKind;
use crate::agent_hub::object_store::{ObjectStore, TreeManifest};
use crate::error::AppError;
use serde::{Deserialize, Serialize};

/// 可移植资产标签载荷（写入 revision payload blob 的唯一形态）。
///
/// Business Logic（为什么需要这个枚举）:
///     revision.payload_hash 指向 typed canonical JSON；Skill supporting files 只在 tree hash。
///
/// Code Logic（这个枚举做什么）:
///     内部 tag `kind` + rename_all camelCase；变体 Skill/Command/Agent/Mcp。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PortableAssetPayload {
    /// Skill 目录资产
    Skill(PortableSkill),
    /// 斜杠 Command
    Command(PortableCommand),
    /// Agent 定义
    Agent(PortableAgent),
    /// MCP server
    Mcp(PortableMcpServer),
}

impl PortableAssetPayload {
    /// 载荷对应的 AssetKind。
    ///
    /// Business Logic: 持久化前拒绝 kind/tag 错配。
    /// Code Logic: 枚举映射。
    pub fn asset_kind(&self) -> AssetKind {
        match self {
            Self::Skill(_) => AssetKind::Skill,
            Self::Command(_) => AssetKind::Command,
            Self::Agent(_) => AssetKind::Agent,
            Self::Mcp(_) => AssetKind::Mcp,
        }
    }

    /// 稳定 kind 标签字符串。
    ///
    /// Business Logic: 日志与错误码用。
    /// Code Logic: 与 serde tag 一致。
    pub fn kind_tag(&self) -> &'static str {
        match self {
            Self::Skill(_) => "skill",
            Self::Command(_) => "command",
            Self::Agent(_) => "agent",
            Self::Mcp(_) => "mcp",
        }
    }

    /// 校验载荷字段。
    ///
    /// Business Logic: 空名/重复参数/非法 transport 不得写入。
    /// Code Logic: 分派到各类型 validate。
    pub fn validate(&self) -> Result<(), AppError> {
        match self {
            Self::Skill(s) => s.validate(),
            Self::Command(c) => c.validate(),
            Self::Agent(a) => a.validate(),
            Self::Mcp(m) => m.validate(),
        }
    }

    /// 若为 Skill 返回 tree_manifest_hash。
    ///
    /// Business Logic: revision.tree_manifest_hash 仅 Skill 填充。
    /// Code Logic: Option 抽取。
    pub fn tree_manifest_hash(&self) -> Option<&str> {
        match self {
            Self::Skill(s) => Some(s.tree_manifest_hash.as_str()),
            _ => None,
        }
    }

    /// 收集 portability 诊断（不含 credential 原文）。
    ///
    /// Business Logic: 投影 partial 标记依赖诊断。
    /// Code Logic: 分派 collect_*。
    pub fn collect_diagnostics(&self) -> Vec<PortabilityDiagnostic> {
        match self {
            Self::Skill(_) => Vec::new(),
            Self::Command(c) => c.collect_diagnostics(),
            Self::Agent(a) => a.collect_diagnostics(),
            Self::Mcp(m) => m.collect_diagnostics(),
        }
    }

    /// Skill：用 CAS 树校验 SKILL.md。
    ///
    /// Business Logic: 支持文件不改写，但必须含 SKILL.md。
    /// Code Logic: get_tree + validate_tree_manifest。
    pub async fn validate_skill_tree_if_needed(
        &self,
        store: &ObjectStore,
    ) -> Result<Vec<PortabilityDiagnostic>, AppError> {
        let Self::Skill(skill) = self else {
            return Ok(self.collect_diagnostics());
        };
        skill.validate()?;
        let manifest = store.get_tree(&skill.tree_manifest_hash).await?;
        skill.validate_tree_manifest(&manifest)?;
        let mut diags = skill.collect_tree_diagnostics(&manifest);
        diags.extend(self.collect_diagnostics());
        Ok(diags)
    }
}

/// 将载荷序列化为确定性 canonical JSON 字节。
///
/// Business Logic（为什么需要这个函数）:
///     同一语义载荷跨设备必须得到相同 payload_hash；BTreeMap 键序保证确定性。
///
/// Code Logic（这个函数做什么）:
///     serde_json::to_vec（map 键序由 BTreeMap 保证；结构体字段序由定义固定）。
pub fn canonical_bytes(payload: &PortableAssetPayload) -> Result<Vec<u8>, AppError> {
    payload.validate()?;
    serde_json::to_vec(payload)
        .map_err(|e| AppError::generic(format!("agent_hub_portable_payload_serialize:{e}")))
}

/// 从 canonical JSON 字节反序列化。
///
/// Business Logic: load_portable_asset 从 CAS blob 还原 typed 载荷。
/// Code Logic: from_slice + validate。
pub fn from_canonical_bytes(bytes: &[u8]) -> Result<PortableAssetPayload, AppError> {
    let payload: PortableAssetPayload = serde_json::from_slice(bytes)
        .map_err(|e| AppError::generic(format!("agent_hub_portable_payload_deserialize:{e}")))?;
    payload.validate()?;
    Ok(payload)
}

/// 校验 AssetKind 与 payload tag 一致。
///
/// Business Logic: 错配不得开启 SQL 事务。
/// Code Logic: 比较 enum。
pub fn ensure_kind_matches_payload(
    kind: AssetKind,
    payload: &PortableAssetPayload,
) -> Result<(), AppError> {
    if kind != payload.asset_kind() {
        return Err(AppError::validation(format!(
            "agent_hub_portable_asset_kind_mismatch:asset={},payload={}",
            kind.as_str(),
            payload.kind_tag()
        )));
    }
    Ok(())
}

/// 辅助：断言 manifest 含 SKILL.md（单测/调用方无需 store 时）。
///
/// Business Logic: Skill 树缺 SKILL.md 应失败。
/// Code Logic: 委托 PortableSkill::validate_tree_manifest。
pub fn validate_skill_tree(skill: &PortableSkill, manifest: &TreeManifest) -> Result<(), AppError> {
    skill.validate_tree_manifest(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::AgentTarget;
    use crate::agent_hub::object_store::{TreeEntry, TreeEntryType, TreeManifest};
    use std::collections::BTreeMap;

    fn skill_payload() -> PortableAssetPayload {
        PortableAssetPayload::Skill(PortableSkill {
            name: "review".into(),
            description: "d".into(),
            skill_markdown_hash: "a".repeat(64),
            tree_manifest_hash: "b".repeat(64),
            target_extensions: BTreeMap::from([(
                AgentTarget::Codex,
                serde_json::json!({"agentsYaml": true}),
            )]),
        })
    }

    fn command_payload() -> PortableAssetPayload {
        PortableAssetPayload::Command(PortableCommand {
            name: "ship".into(),
            description: None,
            prompt_template: "do it".into(),
            arguments: vec![CommandArgument {
                name: "tag".into(),
                description: None,
                required: false,
            }],
            target_extensions: BTreeMap::new(),
        })
    }

    fn agent_payload() -> PortableAssetPayload {
        PortableAssetPayload::Agent(PortableAgent {
            name: "bot".into(),
            description: None,
            instructions: "help".into(),
            mode_intent: None,
            tool_intents: vec![],
            target_extensions: BTreeMap::new(),
        })
    }

    fn mcp_payload() -> PortableAssetPayload {
        PortableAssetPayload::Mcp(PortableMcpServer {
            key: "k".into(),
            transport: McpTransport::Stdio {
                command: "uvx".into(),
                args: vec!["srv".into()],
                cwd: None,
            },
            env: BTreeMap::from([("Z".into(), "1".into()), ("A".into(), "2".into())]),
            enabled: true,
            tool_allow: vec![],
            tool_deny: vec![],
            target_extensions: BTreeMap::new(),
        })
    }

    #[test]
    fn round_trip_all_payload_kinds() {
        for p in [
            skill_payload(),
            command_payload(),
            agent_payload(),
            mcp_payload(),
        ] {
            let bytes = canonical_bytes(&p).unwrap();
            let back = from_canonical_bytes(&bytes).unwrap();
            assert_eq!(back, p);
        }
    }

    #[test]
    fn btreemap_key_order_is_deterministic() {
        let p = mcp_payload();
        let a = canonical_bytes(&p).unwrap();
        let b = canonical_bytes(&p).unwrap();
        assert_eq!(a, b);
        let text = String::from_utf8(a).unwrap();
        // env keys A before Z
        let pos_a = text.find("\"A\"").unwrap();
        let pos_z = text.find("\"Z\"").unwrap();
        assert!(pos_a < pos_z);
    }

    #[test]
    fn kind_mismatch_rejected() {
        let p = skill_payload();
        let err = ensure_kind_matches_payload(AssetKind::Command, &p).unwrap_err();
        assert!(err.to_string().contains("kind_mismatch"));
    }

    #[test]
    fn skill_tree_missing_skill_md() {
        let PortableAssetPayload::Skill(skill) = skill_payload() else {
            panic!("expected skill");
        };
        let manifest = TreeManifest {
            entries: vec![TreeEntry {
                path: "readme.md".into(),
                blob_hash: "c".repeat(64),
                entry_type: TreeEntryType::File,
                executable: false,
            }],
        };
        assert!(validate_skill_tree(&skill, &manifest).is_err());
    }

    #[test]
    fn skill_tree_ok() {
        let PortableAssetPayload::Skill(skill) = skill_payload() else {
            panic!("expected skill");
        };
        let manifest = TreeManifest {
            entries: vec![TreeEntry {
                path: "SKILL.md".into(),
                blob_hash: "a".repeat(64),
                entry_type: TreeEntryType::File,
                executable: false,
            }],
        };
        validate_skill_tree(&skill, &manifest).unwrap();
    }
}
