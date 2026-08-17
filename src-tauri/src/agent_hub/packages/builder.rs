//! agent_hub/packages/builder — 确定性 managed package 布局与原子物化
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude/Codex 需要生成隔离 Plugin（marketplace 可激活）；OpenCode 使用原生路径树。
//!     package ID 只依赖 target/scope/逻辑资产 ID，不含 secret；同输入重建 alias 稳定。
//!
//! Code Logic（这个模块做什么）:
//!     渲染 target 可见内容到 sibling staging，校验 manifest，hash 目录树，原子 rename 到
//!     materialized-packages 根；记录 invocation alias / namespace 元数据。
//!     Gate D：可选 command/agent 文件与 residual 同 target 旁路写入。

use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::object_store::sha256_hex;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// 受管 marketplace 稳定名（CLI selector 的 marketplace 段）。
pub const MARKETPLACE_NAME: &str = "cc-partner";
/// 生成 Plugin 的稳定名。
pub const PLUGIN_NAME: &str = "cc-partner";
/// Claude/Codex 安装选择器：`plugin@cc-partner`。
pub const PLUGIN_SELECTOR: &str = "plugin@cc-partner";

/// 进入 package 的单条 Skill 输入。
///
/// Business Logic（为什么需要这个结构体）:
///     shared 与 targetOnly Skill 必须可过滤：只把该 target 可见的 skill 写入 package。
///
/// Code Logic（这个结构体做什么）:
///     携带逻辑 id、canonical name、markdown 正文、是否 targetOnly 及所属 targets。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageSkillInput {
    /// 逻辑资产 id（进入 package_id 派生，不含 secret）
    pub logical_asset_id: String,
    /// canonical skill 名
    pub name: String,
    /// 描述
    pub description: String,
    /// SKILL.md 正文（可含 frontmatter）
    pub skill_markdown: String,
    /// 是否 targetOnly
    pub target_only: bool,
    /// 可见 targets（targetOnly 时仅列出可见者；shared 可空表示全 target）
    pub visible_targets: Vec<AgentTarget>,
}

impl PackageSkillInput {
    /// 该 skill 是否应对给定 target 可见。
    ///
    /// Business Logic: targetOnly 必须严格隔离，shared 对所有 target 可见。
    /// Code Logic: !target_only 或 visible_targets 包含 target。
    pub fn visible_on(&self, target: AgentTarget) -> bool {
        if !self.target_only {
            return true;
        }
        self.visible_targets.contains(&target)
    }
}

/// package 内 Command 投影文件输入。
///
/// Business Logic: Gate D package render 可把 portable Command 一并物化进 managed package。
/// Code Logic: name + markdown 正文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageCommandInput {
    /// 逻辑资产 id
    pub logical_asset_id: String,
    /// 命令名
    pub name: String,
    /// commands/<name>.md 正文
    pub markdown: String,
}

/// package 内 Agent 投影文件输入。
///
/// Business Logic: Gate D package render 可把 portable Agent 一并物化。
/// Code Logic: name + markdown 正文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageAgentInput {
    /// 逻辑资产 id
    pub logical_asset_id: String,
    /// agent 名
    pub name: String,
    /// agents/<name>.md 正文
    pub markdown: String,
}

/// package 构建输入。
///
/// Business Logic（为什么需要这个结构体）:
///     builder 需要 data_dir、target、scope 与可见资产集合。
///
/// Code Logic（这个结构体做什么）:
///     聚合构建参数；commands/agents 默认空以保持 Gate B 兼容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageBuildInput {
    /// 数据根（`<data_dir>`）
    pub data_dir: PathBuf,
    /// 目标 CLI
    pub target: AgentTarget,
    /// scope 稳定 id（user / project / directory）
    pub scope_id: String,
    /// 进入 package 的 skills
    pub skills: Vec<PackageSkillInput>,
    /// 可选 commands
    #[serde(default)]
    pub commands: Vec<PackageCommandInput>,
    /// 可选 agents
    #[serde(default)]
    pub agents: Vec<PackageAgentInput>,
}

/// 物化元数据（写入 package 根 `.cc-partner-package.json`）。
///
/// Business Logic（为什么需要这个结构体）:
///     UI/preview 需要 invocation alias、namespace 与 package hash，且不得含 secret。
///
/// Code Logic（这个结构体做什么）:
///     camelCase JSON；记录 package_id、tree_hash、alias 与成员逻辑 id。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageMaterializationMeta {
    /// package id
    pub package_id: String,
    /// target
    pub target: AgentTarget,
    /// scope id
    pub scope_id: String,
    /// marketplace 名
    pub marketplace_name: String,
    /// plugin 名
    pub plugin_name: String,
    /// 安装 selector（Claude/Codex）
    pub plugin_selector: String,
    /// 调用命名空间
    pub invocation_namespace: String,
    /// 稳定 alias 映射：logical name → materialized invocation name
    pub invocation_aliases: BTreeMap<String, String>,
    /// 成员 logical asset id（排序）
    pub logical_asset_ids: Vec<String>,
    /// 目录树 hash
    pub tree_hash: String,
    /// package 绝对路径
    pub package_path: String,
}

/// 生成后的 target package 句柄。
///
/// Business Logic（为什么需要这个结构体）:
///     activator 与 projection 阶段消费 package 路径与元数据。
///
/// Code Logic（这个结构体做什么）:
///     持有 path + meta + 写入的相对路径清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedTargetPackage {
    /// 物化元数据
    pub meta: PackageMaterializationMeta,
    /// package 根绝对路径
    pub package_root: PathBuf,
    /// 写入的相对路径（正斜杠，排序）
    pub relative_paths: Vec<String>,
}

/// 派生 package id：`{target}-{scope}-{sha256(asset_ids)[0..16]}`。
///
/// Business Logic（为什么需要这个函数）:
///     package id 必须由 target/scope/逻辑资产 ID 派生，可重建且不含 secret。
///
/// Code Logic（这个函数做什么）:
///     排序 logical ids 后 hash，截断为 16 hex。
pub fn build_package_id(
    target: AgentTarget,
    scope_id: &str,
    logical_asset_ids: &[String],
) -> String {
    let mut ids = logical_asset_ids.to_vec();
    ids.sort();
    let joined = ids.join("\n");
    let digest = sha256_hex(joined.as_bytes());
    let short = &digest[..16.min(digest.len())];
    format!(
        "{}-{}-{}",
        target.as_str(),
        sanitize_id_part(scope_id),
        short
    )
}

/// 物化根：`<data_dir>/agent-hub/materialized-packages`。
///
/// Business Logic: 所有 target package 集中存放，便于 GC 与隔离。
/// Code Logic: 拼接固定相对路径。
pub fn package_materialized_root(data_dir: &Path) -> PathBuf {
    data_dir.join("agent-hub").join("materialized-packages")
}

/// 稳定 invocation alias：namespace 前缀 + canonical name。
///
/// Business Logic: Plugin namespace 改变调用名时，alias 必须跨 rebuild 稳定。
/// Code Logic: `cc-partner__{sanitized_name}`。
pub fn stable_invocation_alias(canonical_name: &str) -> String {
    format!("cc-partner__{}", sanitize_id_part(canonical_name))
}

/// 构建并原子物化 package。
///
/// Business Logic（为什么需要这个函数）:
///     staging → 校验 → tree hash → 原子替换 inactive 路径；失败时旧 package 仍可发现。
///
/// Code Logic（这个函数做什么）:
///     过滤可见 skill；按 target 写布局；rename staging over destination。
pub fn materialize_package(input: &PackageBuildInput) -> Result<GeneratedTargetPackage, AppError> {
    if input.scope_id.trim().is_empty() {
        return Err(AppError::validation(
            "agent_hub_package_empty_scope_id".to_string(),
        ));
    }
    let visible: Vec<&PackageSkillInput> = input
        .skills
        .iter()
        .filter(|s| s.visible_on(input.target))
        .collect();
    let mut logical_ids: Vec<String> = visible.iter().map(|s| s.logical_asset_id.clone()).collect();
    for c in &input.commands {
        logical_ids.push(c.logical_asset_id.clone());
    }
    for a in &input.agents {
        logical_ids.push(a.logical_asset_id.clone());
    }
    let package_id = build_package_id(input.target, &input.scope_id, &logical_ids);
    let dest = package_materialized_root(&input.data_dir)
        .join(input.target.as_str())
        .join(sanitize_id_part(&input.scope_id))
        .join(&package_id);

    let parent = dest
        .parent()
        .ok_or_else(|| AppError::generic("agent_hub_package_parent_missing".to_string()))?;
    fs::create_dir_all(parent)
        .map_err(|e| AppError::generic(format!("agent_hub_package_create_parent: {e}")))?;

    let staging = parent.join(format!(
        ".staging-{}-{}",
        package_id,
        Uuid::new_v4().simple()
    ));
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    fs::create_dir_all(&staging)
        .map_err(|e| AppError::generic(format!("agent_hub_package_create_staging: {e}")))?;

    let mut relative_paths = Vec::new();
    let mut aliases = BTreeMap::new();

    match input.target {
        AgentTarget::Claude => {
            write_claude_plugin(&staging, &visible, &mut relative_paths, &mut aliases)?;
            write_command_files(&staging, &input.commands, &mut relative_paths)?;
            write_agent_files(&staging, &input.agents, &mut relative_paths)?;
        }
        AgentTarget::Codex => {
            write_codex_plugin(&staging, &visible, &mut relative_paths, &mut aliases)?;
            write_command_files(&staging, &input.commands, &mut relative_paths)?;
            write_agent_files(&staging, &input.agents, &mut relative_paths)?;
        }
        AgentTarget::OpenCode => {
            write_opencode_native(&staging, &visible, &mut relative_paths, &mut aliases)?;
            write_command_files(&staging, &input.commands, &mut relative_paths)?;
            write_agent_files(&staging, &input.agents, &mut relative_paths)?;
        }
        AgentTarget::Grok | AgentTarget::Gemini | AgentTarget::Cursor | AgentTarget::Pi => {
            write_opencode_native(&staging, &visible, &mut relative_paths, &mut aliases)?;
            write_command_files(&staging, &input.commands, &mut relative_paths)?;
            write_agent_files(&staging, &input.agents, &mut relative_paths)?;
        }
    }

    // 拒绝任何禁止路径泄漏
    for p in &relative_paths {
        assert_not_forbidden_managed_path(p)?;
    }

    let tree_hash = hash_tree(&staging)?;
    let meta = PackageMaterializationMeta {
        package_id: package_id.clone(),
        target: input.target,
        scope_id: input.scope_id.clone(),
        marketplace_name: MARKETPLACE_NAME.to_string(),
        plugin_name: PLUGIN_NAME.to_string(),
        plugin_selector: PLUGIN_SELECTOR.to_string(),
        invocation_namespace: MARKETPLACE_NAME.to_string(),
        invocation_aliases: aliases,
        logical_asset_ids: {
            let mut ids = logical_ids;
            ids.sort();
            ids
        },
        tree_hash: tree_hash.clone(),
        package_path: dest.display().to_string(),
    };
    write_meta_file(&staging, &meta, &mut relative_paths)?;

    // 原子替换：若 dest 存在则先 rename 到 backup，再 staging→dest，成功后删 backup
    let backup = if dest.exists() {
        let b = parent.join(format!(
            ".backup-{}-{}",
            package_id,
            Uuid::new_v4().simple()
        ));
        fs::rename(&dest, &b)
            .map_err(|e| AppError::generic(format!("agent_hub_package_backup_rename: {e}")))?;
        Some(b)
    } else {
        None
    };

    match fs::rename(&staging, &dest) {
        Ok(()) => {
            if let Some(b) = backup {
                let _ = fs::remove_dir_all(b);
            }
        }
        Err(e) => {
            // 恢复 backup，保留旧 package
            if let Some(b) = backup {
                let _ = fs::rename(&b, &dest);
            }
            let _ = fs::remove_dir_all(&staging);
            return Err(AppError::generic(format!(
                "agent_hub_package_atomic_rename: {e}"
            )));
        }
    }

    relative_paths.sort();
    Ok(GeneratedTargetPackage {
        meta,
        package_root: dest,
        relative_paths,
    })
}

fn write_claude_plugin(
    root: &Path,
    skills: &[&PackageSkillInput],
    relative_paths: &mut Vec<String>,
    aliases: &mut BTreeMap<String, String>,
) -> Result<(), AppError> {
    // Claude marketplace plugin layout:
    //   .claude-plugin/plugin.json
    //   skills/<alias>/SKILL.md
    let manifest = serde_json::json!({
        "name": PLUGIN_NAME,
        "version": "1.0.0",
        "description": "cc-partner managed Claude plugin",
        "skills": "./skills"
    });
    write_bytes(
        root,
        ".claude-plugin/plugin.json",
        serde_json::to_vec_pretty(&manifest)
            .map_err(|e| AppError::generic(format!("claude plugin.json: {e}")))?,
        relative_paths,
    )?;
    for skill in skills {
        let alias = stable_invocation_alias(&skill.name);
        aliases.insert(skill.name.clone(), alias.clone());
        let body = skill_md_body(skill);
        write_bytes(
            root,
            &format!("skills/{alias}/SKILL.md"),
            body.into_bytes(),
            relative_paths,
        )?;
    }
    Ok(())
}

fn write_codex_plugin(
    root: &Path,
    skills: &[&PackageSkillInput],
    relative_paths: &mut Vec<String>,
    aliases: &mut BTreeMap<String, String>,
) -> Result<(), AppError> {
    // Codex plugin layout:
    //   .codex-plugin/plugin.json
    //   skills/<alias>/SKILL.md
    let manifest = serde_json::json!({
        "name": PLUGIN_NAME,
        "version": "1.0.0",
        "description": "cc-partner managed Codex plugin",
        "skills": "./skills"
    });
    write_bytes(
        root,
        ".codex-plugin/plugin.json",
        serde_json::to_vec_pretty(&manifest)
            .map_err(|e| AppError::generic(format!("codex plugin.json: {e}")))?,
        relative_paths,
    )?;
    for skill in skills {
        let alias = stable_invocation_alias(&skill.name);
        aliases.insert(skill.name.clone(), alias.clone());
        let body = skill_md_body(skill);
        write_bytes(
            root,
            &format!("skills/{alias}/SKILL.md"),
            body.into_bytes(),
            relative_paths,
        )?;
    }
    Ok(())
}

fn write_opencode_native(
    root: &Path,
    skills: &[&PackageSkillInput],
    relative_paths: &mut Vec<String>,
    aliases: &mut BTreeMap<String, String>,
) -> Result<(), AppError> {
    // OpenCode native: skills/commands/agents 直接在 package 根（投影到 config root / .opencode）
    for skill in skills {
        let alias = stable_invocation_alias(&skill.name);
        aliases.insert(skill.name.clone(), alias.clone());
        let body = skill_md_body(skill);
        write_bytes(
            root,
            &format!("skills/{alias}/SKILL.md"),
            body.into_bytes(),
            relative_paths,
        )?;
    }
    // 预留空 commands/agents 目录标记原生布局
    let commands = root.join("commands");
    let agents = root.join("agents");
    fs::create_dir_all(&commands)
        .map_err(|e| AppError::generic(format!("opencode commands dir: {e}")))?;
    fs::create_dir_all(&agents)
        .map_err(|e| AppError::generic(format!("opencode agents dir: {e}")))?;
    relative_paths.push("commands".to_string());
    relative_paths.push("agents".to_string());
    Ok(())
}

fn skill_md_body(skill: &PackageSkillInput) -> String {
    if skill.skill_markdown.trim().is_empty() {
        format!(
            "---\nname: {}\ndescription: {}\n---\n",
            skill.name, skill.description
        )
    } else if skill.skill_markdown.contains("---") {
        skill.skill_markdown.clone()
    } else {
        format!(
            "---\nname: {}\ndescription: {}\n---\n{}",
            skill.name, skill.description, skill.skill_markdown
        )
    }
}

/// 写入 commands/<name>.md。
///
/// Business Logic: managed package 需包含 portable Command 投影。
/// Code Logic: 相对路径 `commands/{name}.md`。
fn write_command_files(
    root: &Path,
    commands: &[PackageCommandInput],
    relative_paths: &mut Vec<String>,
) -> Result<(), AppError> {
    for cmd in commands {
        let rel = format!("commands/{}.md", sanitize_id_part(&cmd.name));
        write_bytes(root, &rel, cmd.markdown.as_bytes().to_vec(), relative_paths)?;
    }
    Ok(())
}

/// 写入 agents/<name>.md。
///
/// Business Logic: managed package 需包含 portable Agent 投影。
/// Code Logic: 相对路径 `agents/{name}.md`。
fn write_agent_files(
    root: &Path,
    agents: &[PackageAgentInput],
    relative_paths: &mut Vec<String>,
) -> Result<(), AppError> {
    for agent in agents {
        let rel = format!("agents/{}.md", sanitize_id_part(&agent.name));
        write_bytes(
            root,
            &rel,
            agent.markdown.as_bytes().to_vec(),
            relative_paths,
        )?;
    }
    Ok(())
}

fn write_meta_file(
    root: &Path,
    meta: &PackageMaterializationMeta,
    relative_paths: &mut Vec<String>,
) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(meta)
        .map_err(|e| AppError::generic(format!("package meta serialize: {e}")))?;
    write_bytes(root, ".cc-partner-package.json", bytes, relative_paths)
}

fn write_bytes(
    root: &Path,
    relative: &str,
    bytes: Vec<u8>,
    relative_paths: &mut Vec<String>,
) -> Result<(), AppError> {
    assert_not_forbidden_managed_path(relative)?;
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            AppError::generic(format!("package write mkdir {}: {e}", parent.display()))
        })?;
    }
    let mut f = File::create(&path)
        .map_err(|e| AppError::generic(format!("package write create {}: {e}", path.display())))?;
    f.write_all(&bytes)
        .map_err(|e| AppError::generic(format!("package write bytes: {e}")))?;
    f.sync_all()
        .map_err(|e| AppError::generic(format!("package write sync: {e}")))?;
    relative_paths.push(relative.replace('\\', "/"));
    Ok(())
}

/// managed 输出禁止落在 legacy 兼容路径语义下。
///
/// Business Logic: 相对路径不得等价于 `.claude/skills` 或 `.agents/skills` 终态。
/// Code Logic: 规范化后匹配禁止前缀。
fn assert_not_forbidden_managed_path(relative: &str) -> Result<(), AppError> {
    let norm = relative.replace('\\', "/");
    let trimmed = norm.trim_start_matches("./");
    if trimmed.starts_with(".claude/skills")
        || trimmed.starts_with(".agents/skills")
        || trimmed.contains("/.claude/skills/")
        || trimmed.contains("/.agents/skills/")
    {
        return Err(AppError::validation(format!(
            "agent_hub_package_forbidden_path:{trimmed}"
        )));
    }
    Ok(())
}

fn sanitize_id_part(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "x".to_string()
    } else {
        out
    }
}

/// 确定性目录树 hash：排序相对路径 + 内容 sha 串联。
fn hash_tree(root: &Path) -> Result<String, AppError> {
    let mut entries: Vec<(String, String)> = Vec::new();
    collect_files(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut buf = String::new();
    for (path, hash) in entries {
        buf.push_str(&path);
        buf.push('\0');
        buf.push_str(&hash);
        buf.push('\n');
    }
    Ok(sha256_hex(buf.as_bytes()))
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) -> Result<(), AppError> {
    let read =
        fs::read_dir(dir).map_err(|e| AppError::generic(format!("package hash readdir: {e}")))?;
    for entry in read {
        let entry = entry.map_err(|e| AppError::generic(format!("package hash entry: {e}")))?;
        let path = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| AppError::generic(format!("package hash ft: {e}")))?;
        if ft.is_dir() {
            collect_files(root, &path, out)?;
        } else if ft.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|_| AppError::generic("package hash strip_prefix".to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = fs::read(&path)
                .map_err(|e| AppError::generic(format!("package hash read: {e}")))?;
            out.push((rel, sha256_hex(&bytes)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn shared_skill() -> PackageSkillInput {
        PackageSkillInput {
            logical_asset_id: "asset-shared-review".into(),
            name: "review".into(),
            description: "shared review skill".into(),
            skill_markdown: "# Shared review\nDo careful review.\n".into(),
            target_only: false,
            visible_targets: vec![],
        }
    }

    fn target_only_claude() -> PackageSkillInput {
        PackageSkillInput {
            logical_asset_id: "asset-claude-only".into(),
            name: "claude-only".into(),
            description: "claude only".into(),
            skill_markdown: "# Claude only\n".into(),
            target_only: true,
            visible_targets: vec![AgentTarget::Claude],
        }
    }

    fn target_only_codex() -> PackageSkillInput {
        PackageSkillInput {
            logical_asset_id: "asset-codex-only".into(),
            name: "codex-only".into(),
            description: "codex only".into(),
            skill_markdown: "# Codex only\n".into(),
            target_only: true,
            visible_targets: vec![AgentTarget::Codex],
        }
    }

    fn tmp_data() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ah-pkg-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn package_layout_claude_has_manifest_and_only_claude_visible() {
        let data = tmp_data();
        let input = PackageBuildInput {
            data_dir: data.clone(),
            target: AgentTarget::Claude,
            scope_id: "user".into(),
            skills: vec![shared_skill(), target_only_claude(), target_only_codex()],
            commands: vec![],
            agents: vec![],
        };
        let pkg = materialize_package(&input).expect("claude package");
        let manifest = pkg.package_root.join(".claude-plugin/plugin.json");
        assert!(manifest.is_file(), "claude plugin manifest missing");
        let text = fs::read_to_string(&manifest).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["name"], PLUGIN_NAME);

        // 共享 + claude-only 在内；codex-only 不可见
        let names: Vec<_> = pkg.meta.invocation_aliases.keys().cloned().collect();
        assert!(names.iter().any(|n| n == "review"));
        assert!(names.iter().any(|n| n == "claude-only"));
        assert!(!names.iter().any(|n| n == "codex-only"));

        for p in &pkg.relative_paths {
            assert!(!p.contains(".claude/skills"));
            assert!(!p.contains(".agents/skills"));
        }
        assert!(!pkg
            .package_root
            .to_string_lossy()
            .contains("/.claude/skills"));
        let _ = fs::remove_dir_all(data);
    }

    #[test]
    fn package_layout_codex_has_codex_plugin_json_and_isolation() {
        let data = tmp_data();
        let input = PackageBuildInput {
            data_dir: data.clone(),
            target: AgentTarget::Codex,
            scope_id: "user".into(),
            skills: vec![shared_skill(), target_only_claude(), target_only_codex()],
            commands: vec![],
            agents: vec![],
        };
        let pkg = materialize_package(&input).expect("codex package");
        assert!(pkg.package_root.join(".codex-plugin/plugin.json").is_file());
        let names: Vec<_> = pkg.meta.invocation_aliases.keys().cloned().collect();
        assert!(names.iter().any(|n| n == "review"));
        assert!(names.iter().any(|n| n == "codex-only"));
        assert!(!names.iter().any(|n| n == "claude-only"));
        for p in &pkg.relative_paths {
            assert!(!p.contains(".claude/skills"));
            assert!(!p.contains(".agents/skills"));
        }
        let _ = fs::remove_dir_all(data);
    }

    #[test]
    fn package_layout_opencode_uses_native_skills_commands_agents() {
        let data = tmp_data();
        let input = PackageBuildInput {
            data_dir: data.clone(),
            target: AgentTarget::OpenCode,
            scope_id: "project-demo".into(),
            skills: vec![shared_skill(), target_only_claude()],
            commands: vec![],
            agents: vec![],
        };
        let pkg = materialize_package(&input).expect("opencode package");
        assert!(pkg.package_root.join("skills").is_dir());
        assert!(pkg.package_root.join("commands").is_dir());
        assert!(pkg.package_root.join("agents").is_dir());
        // 无 claude/codex plugin manifest
        assert!(!pkg.package_root.join(".claude-plugin").exists());
        assert!(!pkg.package_root.join(".codex-plugin").exists());
        // targetOnly claude 不泄漏
        assert!(!pkg.meta.invocation_aliases.contains_key("claude-only"));
        assert!(pkg.meta.invocation_aliases.contains_key("review"));
        for p in &pkg.relative_paths {
            assert!(!p.contains(".claude/skills"));
            assert!(!p.contains(".agents/skills"));
        }
        let _ = fs::remove_dir_all(data);
    }

    #[test]
    fn generated_aliases_stable_across_rebuilds() {
        let data = tmp_data();
        let input = PackageBuildInput {
            data_dir: data.clone(),
            target: AgentTarget::Claude,
            scope_id: "user".into(),
            skills: vec![shared_skill()],
            commands: vec![],
            agents: vec![],
        };
        let a = materialize_package(&input).unwrap();
        let b = materialize_package(&input).unwrap();
        assert_eq!(a.meta.package_id, b.meta.package_id);
        assert_eq!(a.meta.invocation_aliases, b.meta.invocation_aliases);
        assert_eq!(
            a.meta.invocation_aliases.get("review").unwrap(),
            "cc-partner__review"
        );
        assert_eq!(a.meta.tree_hash, b.meta.tree_hash);
        let _ = fs::remove_dir_all(data);
    }

    #[test]
    fn package_id_excludes_secret_payload() {
        let id = build_package_id(
            AgentTarget::Claude,
            "user",
            &["asset-1".into(), "asset-2".into()],
        );
        assert!(id.starts_with("claude-user-"));
        assert!(!id.contains("token"));
        assert!(!id.contains("Bearer"));
        // 顺序无关
        let id2 = build_package_id(
            AgentTarget::Claude,
            "user",
            &["asset-2".into(), "asset-1".into()],
        );
        assert_eq!(id, id2);
    }

    #[test]
    fn failed_atomic_replace_keeps_previous_package_when_dest_present() {
        let data = tmp_data();
        let input = PackageBuildInput {
            data_dir: data.clone(),
            target: AgentTarget::Claude,
            scope_id: "user".into(),
            skills: vec![shared_skill()],
            commands: vec![],
            agents: vec![],
        };
        let first = materialize_package(&input).unwrap();
        assert!(first
            .package_root
            .join(".claude-plugin/plugin.json")
            .is_file());
        // 第二次重建同 id 应覆盖成功（正常路径）
        let second = materialize_package(&input).unwrap();
        assert_eq!(first.package_root, second.package_root);
        assert!(second
            .package_root
            .join(".claude-plugin/plugin.json")
            .is_file());
        let _ = fs::remove_dir_all(data);
    }
}
