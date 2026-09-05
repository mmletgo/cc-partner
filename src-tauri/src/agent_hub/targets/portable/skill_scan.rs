//! agent_hub/targets/portable/skill_scan — Skill 目录扫描与树 hash
//!
//! Business Logic（为什么需要这个模块）:
//!     各 CLI Agent 的 Skill 以「含 SKILL.md 的目录」为单位存放；Hub 需要只读扫描出
//!     独立发现记录（含 SKILL.md hash 与目录树 manifest hash），支持无根清单包的一层
//!     展开、store 软链跟随与逃逸软链 fail-closed；扫描不得写盘。
//!
//! Code Logic（这个模块做什么）:
//!     从原 portable.rs 拆出：`hash_skill_directory*` 树 hash（含只读增量缓存）、
//!     `scan_skill_dirs*` / `scan_disabled_skill_dirs*` 目录扫描、嵌套 skill 包展开与
//!     `discover_skill_at_path` 单目录解析；复用 frontmatter 解析、markdown_scan 的
//!     blocked_escape_skill 与父模块的 store_or_target_owner。

use crate::{
    agent_hub::{
        assets::{
            PortabilityDiagnostic, PortableAssetPayload, PortableSkill, CODE_UNKNOWN_SOURCE_FIELD,
        },
        models::{AgentTarget, AssetKind, ScopeKind},
        object_store::{sha256_hex, TreeEntry, TreeEntryType, TreeManifest},
        portable_store::{classify_store_link, StoreLinkClass},
        targets::tree_metadata::tree_metadata_fingerprint,
    },
    error::AppError,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use super::frontmatter::{parse_simple_frontmatter, unknown_fields_extension, KNOWN_SKILL_KEYS};
use super::markdown_scan::blocked_escape_skill;
use super::{
    store_or_target_owner, DiscoveredPortableAsset, PortableAssetOrigin, PortableDiscoveryStatus,
    PortableOriginKind,
};

/// 计算目录 TreeManifest（不写 CAS）并返回 manifest hash + skill_md hash。
///
/// Business Logic: discovery 需要稳定 content/tree hash，但 scan 不得写 objects 目录。
/// Code Logic: 逃逸软链根 fail-closed；其余 walk 文件；构建 sorted TreeManifest；hash(JSON) 与 SKILL.md 字节 hash。
pub fn hash_skill_directory(
    dir: &Path,
) -> Result<(String, String, TreeManifest, Vec<PortabilityDiagnostic>), AppError> {
    if matches!(classify_store_link(dir), StoreLinkClass::EscapeLink) {
        return Err(AppError::validation(
            "agent_hub_portable_skill_tree_symlink_escape".to_string(),
        ));
    }
    hash_skill_directory_unchecked(dir)
}

/// push/pull 打包专用：根目录是逃逸软链时跟随到仓库真树再 hash（全程只读）。
///
/// Business Logic: skill/command 常以「仓库真树 + Agent 软链」形式存在（如 `~/.agents/skills`），
///     跨机镜像必须能把这些资产送进对端 portable store，而不是 fail-closed 拒推；
///     本机写路径仍走 `hash_skill_directory` 的逃逸拒绝，不受影响。
/// Code Logic: 软链根 canonicalize 到目标目录（断链 → 逃逸错误）；StoreLink 用 canonical；
///     其余原样；然后与普通路径同一 walk/manifest 语义。
pub fn hash_skill_directory_dereferenced(
    dir: &Path,
) -> Result<(String, String, TreeManifest, Vec<PortabilityDiagnostic>), AppError> {
    let resolved = match classify_store_link(dir) {
        StoreLinkClass::Regular => dir.to_path_buf(),
        StoreLinkClass::EscapeLink => fs::canonicalize(dir).map_err(|_| {
            AppError::validation("agent_hub_portable_skill_tree_symlink_escape".to_string())
        })?,
        StoreLinkClass::StoreLink { canonical, .. } => canonical,
    };
    hash_skill_directory_unchecked(&resolved)
}

fn hash_skill_directory_unchecked(
    root: &Path,
) -> Result<(String, String, TreeManifest, Vec<PortabilityDiagnostic>), AppError> {
    let mut entries = Vec::new();
    let mut diagnostics = Vec::new();
    let mut skill_md_hash: Option<String> = None;
    walk_files(
        root,
        root,
        &mut entries,
        &mut diagnostics,
        &mut skill_md_hash,
    )?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let Some(skill_hash) = skill_md_hash else {
        return Err(AppError::validation(
            "agent_hub_portable_skill_tree_missing_skill_md".to_string(),
        ));
    };
    let manifest = TreeManifest { entries };
    let bytes = serde_json::to_vec(&manifest)
        .map_err(|e| AppError::generic(format!("tree manifest serialize: {e}")))?;
    let tree_hash = sha256_hex(&bytes);
    Ok((skill_hash, tree_hash, manifest, diagnostics))
}

type SkillHashResult = (String, String, TreeManifest, Vec<PortabilityDiagnostic>);

#[derive(Clone)]
struct CachedSkillHash {
    metadata_fingerprint: String,
    result: SkillHashResult,
}

/// 只读 discovery 专用增量 hash；adoption/action 仍调用未缓存函数重新验证内容。
fn hash_skill_directory_cached(dir: &Path) -> Result<SkillHashResult, AppError> {
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, CachedSkillHash>>> = OnceLock::new();
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let metadata_fingerprint = tree_metadata_fingerprint(dir)?;
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
        .filter(|entry| entry.metadata_fingerprint == metadata_fingerprint)
        .cloned()
    {
        return Ok(hit.result);
    }
    let result = hash_skill_directory(dir)?;
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.len() >= 2_048 && !guard.contains_key(&key) {
        guard.clear();
    }
    guard.insert(
        key,
        CachedSkillHash {
            metadata_fingerprint,
            result: result.clone(),
        },
    );
    Ok(result)
}

fn walk_files(
    root: &Path,
    current: &Path,
    entries: &mut Vec<TreeEntry>,
    diagnostics: &mut Vec<PortabilityDiagnostic>,
    skill_md_hash: &mut Option<String>,
) -> Result<(), AppError> {
    let read = match fs::read_dir(current) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let mut children: Vec<_> = read.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|e| e.file_name());
    for entry in children {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            match classify_store_link(&path) {
                StoreLinkClass::StoreLink { .. } => {
                    let followed = match fs::metadata(&path) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    if followed.is_dir() {
                        walk_files(root, &path, entries, diagnostics, skill_md_hash)?;
                        continue;
                    }
                    if followed.is_file() {
                        let bytes = fs::read(&path)?;
                        let hash = sha256_hex(&bytes);
                        let rel = relative_posix(root, &path);
                        if rel == "SKILL.md" || rel.ends_with("/SKILL.md") {
                            *skill_md_hash = Some(hash.clone());
                        }
                        entries.push(TreeEntry {
                            path: rel,
                            blob_hash: hash,
                            entry_type: TreeEntryType::File,
                            executable: is_executable(&followed),
                        });
                        continue;
                    }
                }
                StoreLinkClass::EscapeLink | StoreLinkClass::Regular => {
                    diagnostics.push(PortabilityDiagnostic::new(
                        "store_symlink_escape",
                        relative_posix(root, &path),
                        "symlink outside portable-store rejected",
                    ));
                    continue;
                }
            }
            continue;
        }
        if meta.is_dir() {
            walk_files(root, &path, entries, diagnostics, skill_md_hash)?;
            continue;
        }
        if !meta.is_file() {
            diagnostics.push(PortabilityDiagnostic::new(
                CODE_UNKNOWN_SOURCE_FIELD,
                relative_posix(root, &path),
                "unknown non-file entry in skill tree",
            ));
            continue;
        }
        let bytes = fs::read(&path)?;
        let hash = sha256_hex(&bytes);
        let rel = relative_posix(root, &path);
        if rel == "SKILL.md" || rel.ends_with("/SKILL.md") {
            *skill_md_hash = Some(hash.clone());
        }
        let executable = is_executable(&meta);
        if executable {
            diagnostics.push(PortabilityDiagnostic::target_executable(format!(
                "tree/{rel}"
            )));
        }
        entries.push(TreeEntry {
            path: rel,
            blob_hash: hash,
            entry_type: TreeEntryType::File,
            executable,
        });
    }
    Ok(())
}

fn is_executable(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

pub(super) fn relative_posix(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

/// 扫描含 SKILL.md 的子目录为 Skill 发现。
///
/// Business Logic: 每个子目录是独立 origin；同名目录在不同根上保持分离。
///     无根 SKILL.md 的 skill 包（如 superpowers）展开成带 SKILL.md 的子项，
///     这样 Grok 等会递归加载的嵌套 skill 会出现在 Hub 列表。
/// Code Logic: read_dir → 有 SKILL.md 则 parse + hash；否则展开一层子目录
///     及可选的 `skills/` 子目录。
pub fn scan_skill_dirs(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    scan_skill_dirs_with_mode(target, scope_kind, root, origin_kind, false)
}

/// Inventory 列表专用 Skill 扫描：读取 SKILL.md 身份，目录树延迟到动作 preview。
pub fn scan_skill_dirs_manifest_only(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    scan_skill_dirs_with_mode(target, scope_kind, root, origin_kind, true)
}

fn scan_skill_dirs_with_mode(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
    defer_tree_hash: bool,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    if !root.is_dir() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            match classify_store_link(&path) {
                StoreLinkClass::StoreLink { .. } => {
                    // 包根 store 软链：跟随并按目录扫描。
                }
                StoreLinkClass::EscapeLink | StoreLinkClass::Regular => {
                    out.push(blocked_escape_skill(target, scope_kind, origin_kind, &path));
                    continue;
                }
            }
        } else if !meta.is_dir() {
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        if path.join("SKILL.md").is_file() {
            if let Some(asset) = discover_skill_at_path(
                target,
                scope_kind,
                origin_kind,
                path,
                defer_tree_hash,
                Vec::new(),
            ) {
                out.push(asset);
            }
            continue;
        }
        out.extend(expand_skill_package_without_root_manifest(
            target,
            scope_kind,
            origin_kind,
            &path,
            defer_tree_hash,
        )?);
    }
    Ok(out)
}

/// 无根 SKILL.md 的包：把带清单的直接子目录（及可选 `skills/`）展开成独立 Skill。
///
/// Business Logic: Grok 等会递归加载 `.agents/skills/<包>/<子项>`；Hub 只扫一层会漏掉整包。
/// Code Logic: 不递归超过一层 + `skills/`；点目录跳过；有 SKILL.md 才产出。
fn expand_skill_package_without_root_manifest(
    target: AgentTarget,
    scope_kind: ScopeKind,
    origin_kind: PortableOriginKind,
    package: &Path,
    defer_tree_hash: bool,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let package_name = package
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("skill");
    let mut out = Vec::new();
    for child in nested_skill_candidate_dirs(package)? {
        if !child.join("SKILL.md").is_file() {
            continue;
        }
        let extra = vec![PortabilityDiagnostic::new(
            "nested_skill_package",
            "/origin/package",
            format!("skill package {package_name}"),
        )];
        if let Some(asset) = discover_skill_at_path(
            target,
            scope_kind,
            origin_kind,
            child,
            defer_tree_hash,
            extra,
        ) {
            out.push(asset);
        }
    }
    Ok(out)
}

/// 收集 skill 包内可作为子项的目录：直接子目录 + 无清单的 `skills/` 子目录。
fn nested_skill_candidate_dirs(package: &Path) -> Result<Vec<PathBuf>, AppError> {
    let mut out = Vec::new();
    let mut skills_subdir: Option<PathBuf> = None;
    for child in sorted_dir_children(package)? {
        let name = child
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if name.starts_with('.') || !is_scanable_skill_dir(&child) {
            continue;
        }
        if name == "skills" && !child.join("SKILL.md").is_file() {
            skills_subdir = Some(child);
            continue;
        }
        out.push(child);
    }
    if let Some(skills) = skills_subdir {
        for grandchild in sorted_dir_children(&skills)? {
            let name = grandchild
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if name.starts_with('.') || !is_scanable_skill_dir(&grandchild) {
                continue;
            }
            out.push(grandchild);
        }
    }
    Ok(out)
}

fn sorted_dir_children(dir: &Path) -> Result<Vec<PathBuf>, AppError> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    Ok(entries.into_iter().map(|e| e.path()).collect())
}

fn is_scanable_skill_dir(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return matches!(classify_store_link(path), StoreLinkClass::StoreLink { .. })
            && path.is_dir();
    }
    meta.is_dir()
}

/// 把含 SKILL.md 的目录解析成一条发现记录。
fn discover_skill_at_path(
    target: AgentTarget,
    scope_kind: ScopeKind,
    origin_kind: PortableOriginKind,
    path: PathBuf,
    defer_tree_hash: bool,
    extra_diags: Vec<PortabilityDiagnostic>,
) -> Option<DiscoveredPortableAsset> {
    let skill_md = path.join("SKILL.md");
    let skill_bytes = match fs::read(&skill_md) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::debug!(
                target = "agent_hub.portable",
                %error,
                path = %skill_md.display(),
                "skip unreadable skill manifest"
            );
            return None;
        }
    };
    let text = String::from_utf8(skill_bytes.clone()).unwrap_or_default();
    let (fields, _order, _body) = parse_simple_frontmatter(&text);
    let dir_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("skill")
        .to_string();
    let name = fields
        .get("name")
        .cloned()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| dir_name.clone());
    let description = fields.get("description").cloned().unwrap_or_default();
    let (skill_hash, tree_hash, payload_tree_hash, mut diags) = if defer_tree_hash {
        let skill_hash = sha256_hex(&skill_bytes);
        (
            skill_hash.clone(),
            None,
            format!("deferred:{skill_hash}"),
            extra_diags,
        )
    } else {
        let (skill_hash, tree_hash, _manifest, mut diagnostics) =
            match hash_skill_directory_cached(&path) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(
                        target = "agent_hub.portable",
                        error = %e,
                        "skip skill dir without valid tree"
                    );
                    return None;
                }
            };
        diagnostics.extend(extra_diags);
        (skill_hash, Some(tree_hash.clone()), tree_hash, diagnostics)
    };
    let (extensions, field_diags) =
        unknown_fields_extension(target, &fields, KNOWN_SKILL_KEYS, "/frontmatter");
    diags.extend(field_diags);
    let payload = PortableAssetPayload::Skill(PortableSkill {
        name: name.clone(),
        description,
        skill_markdown_hash: skill_hash.clone(),
        tree_manifest_hash: payload_tree_hash,
        target_extensions: extensions,
    });
    Some(DiscoveredPortableAsset {
        kind: AssetKind::Skill,
        semantic_name: name,
        scope_kind,
        payload,
        origin: PortableAssetOrigin {
            target,
            owned_by: store_or_target_owner(target, &path),
            path,
            origin_kind,
            native_id: dir_name,
            content_hash: skill_hash,
            tree_hash,
            status: PortableDiscoveryStatus::Active,
            native_output_candidate: origin_kind.is_native_output_candidate(),
            parent_plugin_id: None,
        },
        diagnostics: diags,
    })
}

/// 扫描 disabled 目录下的 skills（actualEnabled=false）。
///
/// Business Logic: active/disabled 路径必须映射为真实启用状态。
/// Code Logic: 复用 scan_skill_dirs 后强制 status=Disabled。
pub fn scan_disabled_skill_dirs(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let mut found = scan_skill_dirs(target, scope_kind, root, origin_kind)?;
    for d in &mut found {
        d.origin.status = PortableDiscoveryStatus::Disabled;
    }
    Ok(found)
}

/// Inventory 列表专用 disabled Skill 扫描；延迟目录 tree hash。
pub fn scan_disabled_skill_dirs_manifest_only(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &Path,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let mut found = scan_skill_dirs_manifest_only(target, scope_kind, root, origin_kind)?;
    for discovery in &mut found {
        discovery.origin.status = PortableDiscoveryStatus::Disabled;
    }
    Ok(found)
}
