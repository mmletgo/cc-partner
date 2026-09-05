//! agent_hub/targets/portable/markdown_scan — Command/Agent Markdown 扫描
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude/OpenCode/Grok 等 Agent 的 Command 与 Agent 资产以单个 `*.md` 文件存放，
//!     Hub 需要只读扫描目录并把 frontmatter + 正文解析为 typed payload；文件或目录
//!     软链若逃逸 portable-store，必须记为 blocked 发现而不是跟随读取。
//!
//! Code Logic（这个模块做什么）:
//!     从原 portable.rs 拆出：`scan_command_markdown_dir` / `scan_agent_markdown_dir` /
//!     `scan_disabled_command_markdown_dir` 目录扫描、`parse_argument_hint` 参数提示
//!     解析、以及逃逸软链 blocked 记录 helper（blocked_escape_*，其中
//!     blocked_escape_skill 供 skill_scan 复用）；复用 frontmatter 解析与父模块的
//!     store_or_target_owner。

use crate::{
    agent_hub::{
        assets::{
            CommandArgument, PortabilityDiagnostic, PortableAgent, PortableAssetPayload,
            PortableCommand, PortableSkill,
        },
        models::{AgentTarget, AssetKind, ScopeKind},
        object_store::sha256_hex,
        portable_store::{classify_store_link, StoreLinkClass},
    },
    error::AppError,
};
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::Path};

use super::frontmatter::{
    parse_simple_frontmatter, unknown_fields_extension, KNOWN_AGENT_KEYS, KNOWN_COMMAND_KEYS,
};
use super::skill_scan::relative_posix;
use super::{
    store_or_target_owner, DiscoveredPortableAsset, PortableAssetOrigin, PortableAssetOwner,
    PortableDiscoveryStatus, PortableOriginKind,
};

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

/// 逃逸软链身份哈希：只记链目标字符串，不跟随正文。
///
/// Business Logic: 确认当前版本必须能把「当前就是逃逸链」记为基准，但不能把链外树当 SKILL.md hash。
/// Code Logic: `read_link` 原文 + 固定前缀；canonicalize 会跟随，禁止使用。
fn blocked_escape_identity_hash(path: &Path) -> String {
    let target = fs::read_link(path)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    sha256_hex(format!("store_symlink_escape\0{target}").as_bytes())
}

/// 逃逸 skill 包根：记 blocked，不跟随哈希。
pub(super) fn blocked_escape_skill(
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
    let content_hash = blocked_escape_identity_hash(path);
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
            content_hash,
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
            content_hash: blocked_escape_identity_hash(path),
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
