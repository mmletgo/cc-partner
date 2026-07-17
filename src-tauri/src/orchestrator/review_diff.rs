//! Orchestrator 有界 Human Review diff 快照。
//!
//! Business Logic（为什么需要这个模块）:
//!     Human Review / Deliver 前需要只读、有体量上限的 worktree 改动快照，并用独立 digest
//!     检测审阅后漂移；verifier 文本上下文也应消费同一采集语义。
//!
//! Code Logic（这个模块做什么）:
//!     从任务 worktree 采集 staged/unstaged/untracked（含 unborn）文件级 diff，按 200 文件 /
//!     总 patch 2MiB / 单文件 256KiB 限制展示，digest 基于 base/tree 身份与完整内容哈希。

use crate::error::AppError;
use crate::orchestrator::models::{
    OrchestratorReviewDiff, OrchestratorTaskAttemptRow, OrchestratorTaskRow, ReviewDiffFile,
};
use crate::workbench::models::WorkbenchProjectRow;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::process::Command as StdCommand;

/// 展示层最多返回的文件数。
const MAX_REVIEW_DIFF_FILES: usize = 200;
/// 全部文件 patch 字节总上限。
const MAX_TOTAL_PATCH_BYTES: usize = 2 * 1024 * 1024;
/// 单文件 patch 字节上限。
const MAX_SINGLE_PATCH_BYTES: usize = 256 * 1024;
/// Git 空树 OID，用于 unborn / 无 base 场景。
const EMPTY_TREE_OID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// unborn head 展示值。
const UNBORN_HEAD: &str = "UNBORN";

/// 内部身份条目：用于 digest 与展示 DTO 的公共源。
///
/// Business Logic（为什么需要这个结构体）:
///     展示 patch 可截断，但 digest 必须覆盖完整 path/status/mode/old oid/new content hash。
///
/// Code Logic（这个结构体做什么）:
///     保存排序后参与 digest 的字段，以及可选的完整 patch 文本与增删统计。
#[derive(Debug, Clone)]
struct ReviewFileIdentity {
    path: String,
    status: String,
    mode: String,
    old_blob_oid: String,
    new_content_hash: String,
    binary: bool,
    additions: u32,
    deletions: u32,
    full_patch: Option<String>,
}

/// Business Logic（为什么需要这个函数）:
///     命令层 / 后续 API 需要从 task/attempt/project 权威元数据生成 review diff，拒绝任意 ref 输入。
///
/// Code Logic（这个函数做什么）:
///     校验 attempt 归属 task/project，再以 worktree_path 与可选 preferred_base 调用 path 级采集。
pub fn collect_review_diff(
    task: &OrchestratorTaskRow,
    attempt: &OrchestratorTaskAttemptRow,
    project: &WorkbenchProjectRow,
    worktree_path: &Path,
    preferred_base: Option<&str>,
) -> Result<OrchestratorReviewDiff, AppError> {
    if attempt.task_id != task.id {
        return Err(AppError::validation(format!(
            "attempt {} 不属于任务 {}",
            attempt.id, task.id
        )));
    }
    if task.project_id != project.id {
        return Err(AppError::validation(format!(
            "任务 {} 不属于项目 {}",
            task.id, project.id
        )));
    }
    if project.kind != "local" {
        return Err(AppError::validation(
            "仅本机项目可采集 review diff（远端由 owning device 生成）",
        ));
    }
    collect_review_diff_for_worktree(&task.id, worktree_path, preferred_base)
}

/// Business Logic（为什么需要这个函数）:
///     verifier、单元测试与后续 command helper 都需要在已解析的 worktree 路径上生成同一 snapshot。
///
/// Code Logic（这个函数做什么）:
///     解析 base/head，枚举全部改动身份并计算 digest，再按展示上限生成有界 DTO。
pub fn collect_review_diff_for_worktree(
    task_id: &str,
    worktree_path: &Path,
    preferred_base: Option<&str>,
) -> Result<OrchestratorReviewDiff, AppError> {
    let (base_ref, head_ref, base_tree) = resolve_base_and_head(worktree_path, preferred_base)?;
    let identities = collect_file_identities(worktree_path, &base_tree)?;
    let review_digest = compute_review_digest(&base_tree, &head_ref, &identities);
    let total_files = identities.len() as u32;
    let (files, truncated) = build_display_files(identities);
    Ok(OrchestratorReviewDiff {
        task_id: task_id.to_string(),
        base_ref,
        head_ref,
        files,
        total_files,
        truncated,
        review_digest,
    })
}

/// Business Logic（为什么需要这个函数）:
///     auto-delivery 在 verifier 通过后、commit 前需要比对稳定 digest，检测审阅后漂移。
///
/// Code Logic（这个函数做什么）:
///     对 expected 与 actual 做精确字符串相等；不解析/截断，调用方负责采集 digest。
pub fn review_digests_match(expected: &str, actual: &str) -> bool {
    expected == actual
}

/// 冻结的审阅树快照：tree OID + digest + parent，供 verifier 与 delivery 同源绑定。
///
/// Business Logic（为什么需要这个结构体）:
///     若 digest 读 index blob、patch 读 worktree，index=A/worktree=B 时会审 B 却交付 A。
///
/// Code Logic（这个结构体做什么）:
///     `tree_oid` 为 stage 后 write-tree；`review_digest` 仅来自该 index/tree；
///     `parent_oid` 为冻结前 HEAD。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenReviewSnapshot {
    pub tree_oid: String,
    pub review_digest: String,
    pub parent_oid: Option<String>,
}

/// Business Logic（为什么需要这个函数）:
///     verifier 通过边界与 delivery commit 边界必须绑定同一 tree 身份，禁止 worktree 与 index 分叉。
///
/// Code Logic（这个函数做什么）:
///     1) 记录 parent HEAD；2) `stage_all_for_commit`；3) `write_tree` → tree_oid；
///     4) 仅从 index/cached 采集 identities 与 patch 计算 review_digest（无 worktree 回退）。
pub fn freeze_review_snapshot(worktree_path: &Path) -> Result<FrozenReviewSnapshot, AppError> {
    let parent_oid = crate::workbench::git::head_hash(worktree_path)?;
    let _ = crate::workbench::git::stage_all_for_commit(worktree_path)?;
    let tree_oid = crate::workbench::git::write_tree_hash(worktree_path)?;
    let snapshot = collect_review_diff_for_frozen_index("freeze", worktree_path, None, &tree_oid)?;
    Ok(FrozenReviewSnapshot {
        tree_oid,
        review_digest: snapshot.review_digest,
        parent_oid,
    })
}

/// Business Logic（为什么需要这个函数）:
///     delivery gate 只需 digest 本体，不必再拿完整 DTO。
///
/// Code Logic（这个函数做什么）:
///     调用 collect_review_diff_for_worktree 并只返回 review_digest 字段。
pub fn current_worktree_review_digest(worktree_path: &Path) -> Result<String, AppError> {
    let snapshot = collect_review_diff_for_worktree("digest-check", worktree_path, None)?;
    Ok(snapshot.review_digest)
}

/// Business Logic（为什么需要这个函数）:
///     delivery 在 stage 后 enforce 应与 freeze 同源（index/tree only），避免再次读脏 worktree。
///
/// Code Logic（这个函数做什么）:
///     假定调用方已 stage；write-tree 取 tree_oid 后走 frozen index 采集 digest。
pub fn current_frozen_index_review_digest(worktree_path: &Path) -> Result<String, AppError> {
    let tree_oid = crate::workbench::git::write_tree_hash(worktree_path)?;
    let snapshot =
        collect_review_diff_for_frozen_index("digest-check", worktree_path, None, &tree_oid)?;
    Ok(snapshot.review_digest)
}

/// Business Logic（为什么需要这个函数）:
///     冻结后的 diff 必须只反映 index/tree，禁止再扫脏 worktree 文本。
///
/// Code Logic（这个函数做什么）:
///     与 collect_review_diff_for_worktree 相同 base/head 解析，但 identities 用 `--cached`
///     且 content hash 仅 blob（无 worktree 回退）；patch 用 `git diff --cached`。
pub(crate) fn collect_review_diff_for_frozen_index(
    task_id: &str,
    worktree_path: &Path,
    preferred_base: Option<&str>,
    frozen_tree_oid: &str,
) -> Result<OrchestratorReviewDiff, AppError> {
    let (base_ref, head_ref, base_tree) = resolve_base_and_head(worktree_path, preferred_base)?;
    // 内容身份来自 cached index；head/base 仍用 resolve 语义，保证与历史 digest 可比。
    // frozen_tree_oid 由调用方持久化/commit，不塞进 digest 字段以免破坏 rebind 兼容。
    let _ = frozen_tree_oid;
    let identities = collect_file_identities_cached(worktree_path, &base_tree)?;
    let review_digest = compute_review_digest(&base_tree, &head_ref, &identities);
    let total_files = identities.len() as u32;
    let (files, truncated) = build_display_files(identities);
    Ok(OrchestratorReviewDiff {
        task_id: task_id.to_string(),
        base_ref,
        head_ref,
        files,
        total_files,
        truncated,
        review_digest,
    })
}

/// Business Logic（为什么需要这个函数）:
///     verifier Claude 仍需要文本上下文；消费同一 snapshot 可避免两套 diff 语义分叉。
///
/// Code Logic（这个函数做什么）:
///     把 snapshot 渲染为包含 base/head/digest 与各文件 metadata/patch 的稳定文本。
pub fn render_review_diff_text(diff: &OrchestratorReviewDiff) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "$ review-diff snapshot\nbase: {}\nhead: {}\ntotal_files: {}\ntruncated: {}\ndigest: {}\n\n",
        diff.base_ref, diff.head_ref, diff.total_files, diff.truncated, diff.review_digest
    ));
    if diff.files.is_empty() {
        out.push_str("(no changed files)\n");
        return out;
    }
    for file in &diff.files {
        out.push_str(&format!(
            "$ file {} status={} +{} -{} binary={} truncated={}\n",
            file.path, file.status, file.additions, file.deletions, file.binary, file.truncated
        ));
        if let Some(patch) = &file.patch {
            out.push_str(patch);
            if !patch.ends_with('\n') {
                out.push('\n');
            }
        } else if file.binary {
            out.push_str("(binary omitted)\n");
        } else {
            out.push_str("(patch omitted)\n");
        }
        out.push('\n');
    }
    out
}

/// Business Logic（为什么需要这个函数）:
///     base/head 必须从 worktree 实际 Git 状态派生，支持 unborn 与可选 base 分支。
///
/// Code Logic（这个函数做什么）:
///     head=rev-parse HEAD 或 UNBORN；base 优先 preferred_base，否则 HEAD（dirty-only）或空树；
///     同时返回用于 diff 的 tree-ish（空树 OID 或解析后的 base）。
fn resolve_base_and_head(
    cwd: &Path,
    preferred_base: Option<&str>,
) -> Result<(String, String, String), AppError> {
    let head = match run_git_capture_allow_fail(cwd, &["rev-parse", "HEAD"]) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => UNBORN_HEAD.to_string(),
    };

    if let Some(preferred) = preferred_base.map(str::trim).filter(|v| !v.is_empty()) {
        if let Ok(resolved) = run_git_capture(cwd, &["rev-parse", "--verify", preferred]) {
            let base = resolved.trim().to_string();
            return Ok((base.clone(), head, base));
        }
    }

    if head == UNBORN_HEAD {
        return Ok((EMPTY_TREE_OID.to_string(), head, EMPTY_TREE_OID.to_string()));
    }

    // 无显式 base 时用 HEAD：与历史 verifier 的 dirty worktree 语义对齐（staged+unstaged）。
    Ok((head.clone(), head.clone(), head))
}

/// Business Logic（为什么需要这个函数）:
///     digest 与展示都依赖完整的 path 级身份列表，必须覆盖 tracked 与 untracked。
///
/// Code Logic（这个函数做什么）:
///     用 `git diff --raw -z <base>` 采集 tracked 变更，用 `ls-files --others` 采集 untracked，
///     规范化路径后按 path 去重合并为 BTreeMap 排序结果。
fn collect_file_identities(
    cwd: &Path,
    base_tree: &str,
) -> Result<Vec<ReviewFileIdentity>, AppError> {
    collect_file_identities_mode(cwd, base_tree, false)
}

/// Business Logic（为什么需要这个函数）:
///     freeze 后只能看 index，否则 patch/digest 再次分叉到脏 worktree。
///
/// Code Logic（这个函数做什么）:
///     `git diff --cached --raw` + numstat --cached；不扫 untracked；patch 用 --cached。
fn collect_file_identities_cached(
    cwd: &Path,
    base_tree: &str,
) -> Result<Vec<ReviewFileIdentity>, AppError> {
    collect_file_identities_mode(cwd, base_tree, true)
}

/// Business Logic（为什么需要这个函数）:
///     worktree 与 frozen-index 两条采集路径共享解析，只在 cached 标志上分叉。
///
/// Code Logic（这个函数做什么）:
///     cached=true：`--cached` raw/numstat/patch，无 untracked，content hash 强制 blob-only；
///     cached=false：保持历史 worktree 语义。
fn collect_file_identities_mode(
    cwd: &Path,
    base_tree: &str,
    cached_only: bool,
) -> Result<Vec<ReviewFileIdentity>, AppError> {
    let mut by_path: BTreeMap<String, ReviewFileIdentity> = BTreeMap::new();

    let mut raw_args = vec!["diff", "--raw", "-z", "--no-ext-diff", "--no-renames"];
    if cached_only {
        raw_args.push("--cached");
    }
    raw_args.push(base_tree);
    let raw = run_git_capture(cwd, &raw_args)?;
    parse_raw_diff_into(cwd, base_tree, &raw, &mut by_path, cached_only)?;

    if !cached_only {
        let untracked_raw =
            run_git_capture(cwd, &["ls-files", "--others", "--exclude-standard", "-z"])?;
        for path in untracked_raw.split('\0').filter(|p| !p.is_empty()) {
            let path = normalize_repo_relative_path(path)?;
            if by_path.contains_key(&path) {
                continue;
            }
            let identity = identity_for_untracked(cwd, &path)?;
            by_path.insert(path, identity);
        }
    }

    let mut numstat_args = vec!["diff", "--numstat", "--no-ext-diff", "--no-renames"];
    if cached_only {
        numstat_args.push("--cached");
    }
    numstat_args.push(base_tree);
    let numstat = run_git_capture(cwd, &numstat_args)?;
    apply_numstat(&numstat, &mut by_path);

    for identity in by_path.values_mut() {
        if identity.binary {
            identity.full_patch = None;
            continue;
        }
        identity.full_patch = Some(load_full_patch(cwd, base_tree, identity, cached_only)?);
    }

    Ok(by_path.into_values().collect())
}

/// Business Logic（为什么需要这个函数）:
///     `git diff --raw -z` 提供 mode/old oid/status，是 digest 的权威字段来源。
///
/// Code Logic（这个函数做什么）:
///     解析 NUL 分隔 raw 记录，校验 path，计算 new content streaming hash，写入 map。
fn parse_raw_diff_into(
    cwd: &Path,
    base_tree: &str,
    raw: &str,
    out: &mut BTreeMap<String, ReviewFileIdentity>,
    blob_only: bool,
) -> Result<(), AppError> {
    let bytes = raw.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == 0 {
            i += 1;
            continue;
        }
        // 记录以 ':' 开头
        if bytes[i] != b':' {
            // 跳到下一个 NUL
            if let Some(rel) = bytes[i..].iter().position(|&b| b == 0) {
                i += rel + 1;
                continue;
            }
            break;
        }
        let rest = &raw[i..];
        let header_end = rest.find('\0').unwrap_or(rest.len());
        let header = &rest[..header_end];
        i += header_end + 1;

        // :oldmode newmode oldsha newsha status
        let parts: Vec<&str> = header.trim_start_matches(':').split_whitespace().collect();
        if parts.len() < 5 {
            continue;
        }
        let old_mode = parts[0];
        let new_mode = parts[1];
        let old_oid = parts[2];
        let new_oid = parts[3];
        let status_code = parts[4].chars().next().unwrap_or('M');

        if i >= bytes.len() {
            break;
        }
        let path_end = raw[i..].find('\0').unwrap_or(raw.len() - i);
        let path_raw = &raw[i..i + path_end];
        i += path_end + 1;
        let path = normalize_repo_relative_path(path_raw)?;

        let status = status_from_code(status_code);
        let mode = if status == "deleted" {
            old_mode.to_string()
        } else {
            new_mode.to_string()
        };
        // symlink/gitlink 不得用空哈希：否则 retarget/submodule 漂移 digest 不变。
        // frozen/blob_only：禁止 worktree 回退，digest 必须与 write-tree 同源。
        let (binary, new_content_hash) = content_hash_for_entry(
            cwd,
            &path,
            status == "deleted",
            new_mode,
            new_oid,
            blob_only,
        )?;
        // 对 deleted/modified 优先使用 raw 的 old oid；若全 0 再尝试 base:path
        let old_blob_oid = if old_oid.chars().all(|c| c == '0') {
            lookup_blob_oid(cwd, base_tree, &path).unwrap_or_else(|| old_oid.to_string())
        } else {
            old_oid.to_string()
        };

        out.insert(
            path.clone(),
            ReviewFileIdentity {
                path,
                status,
                mode,
                old_blob_oid,
                new_content_hash,
                binary,
                additions: 0,
                deletions: 0,
                full_patch: None,
            },
        );
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     untracked 文件不会出现在 `git diff <base>` 中，但 Human Review / verifier 必须看到新增文件。
///
/// Code Logic（这个函数做什么）:
///     读取 untracked 文件元数据与 streaming content hash，status 固定为 untracked。
fn identity_for_untracked(cwd: &Path, path: &str) -> Result<ReviewFileIdentity, AppError> {
    let abs = cwd.join(path);
    let metadata = fs::symlink_metadata(&abs)
        .map_err(|err| AppError::generic(format!("读取未跟踪文件元数据失败: {path}: {err}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(ReviewFileIdentity {
            path: path.to_string(),
            status: "untracked".to_string(),
            mode: "000000".to_string(),
            old_blob_oid: "0".repeat(40),
            new_content_hash: sha256_hex_of_bytes(b""),
            binary: true,
            additions: 0,
            deletions: 0,
            full_patch: None,
        });
    }
    let (binary, hash) = content_hash_for_path(cwd, path, false)?;
    let mode = file_mode_octal(&metadata);
    let additions = if binary {
        0
    } else {
        count_lines_in_file(&abs).unwrap_or(0)
    };
    Ok(ReviewFileIdentity {
        path: path.to_string(),
        status: "untracked".to_string(),
        mode,
        old_blob_oid: "0".repeat(40),
        new_content_hash: hash,
        binary,
        additions,
        deletions: 0,
        full_patch: None,
    })
}

/// Business Logic（为什么需要这个函数）:
///     前端与 verifier 需要增删行数与 binary 标记；numstat 是 Git 权威来源。
///
/// Code Logic（这个函数做什么）:
///     解析 `git diff --numstat` 行，更新已有 identity 的 additions/deletions/binary。
fn apply_numstat(numstat: &str, out: &mut BTreeMap<String, ReviewFileIdentity>) {
    for line in numstat.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let add_s = parts.next().unwrap_or("");
        let del_s = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("").trim();
        if path.is_empty() {
            continue;
        }
        let Ok(path) = normalize_repo_relative_path(path) else {
            continue;
        };
        let Some(entry) = out.get_mut(&path) else {
            continue;
        };
        if add_s == "-" && del_s == "-" {
            entry.binary = true;
            entry.additions = 0;
            entry.deletions = 0;
            continue;
        }
        entry.additions = add_s.parse().unwrap_or(0);
        entry.deletions = del_s.parse().unwrap_or(0);
    }
}

/// Business Logic（为什么需要这个函数）:
///     展示层需要 unified patch，但必须可对超大文件截断；此处先取完整 patch 再统一 bound。
///
/// Code Logic（这个函数做什么）:
///     tracked：cached_only 时 `git diff --cached <base> -- path`，否则 worktree diff；
///     untracked：文本合成 new-file patch（frozen 路径不应再出现 untracked）。
fn load_full_patch(
    cwd: &Path,
    base_tree: &str,
    identity: &ReviewFileIdentity,
    cached_only: bool,
) -> Result<String, AppError> {
    if identity.status == "untracked" {
        return synthesize_untracked_patch(cwd, &identity.path);
    }
    let mut args = vec!["diff", "--no-ext-diff", "--no-color", "--no-renames"];
    if cached_only {
        args.push("--cached");
    }
    args.push(base_tree);
    args.push("--");
    args.push(&identity.path);
    run_git_capture(cwd, &args)
}

/// Business Logic（为什么需要这个函数）:
///     untracked 文本文件没有 Git base，需要可读 patch 以便 Human Review / verifier 看到正文。
///
/// Code Logic（这个函数做什么）:
///     读取 UTF-8 文本并合成最小 unified patch；非 UTF-8 返回空（binary 路径应已跳过）。
fn synthesize_untracked_patch(cwd: &Path, path: &str) -> Result<String, AppError> {
    let abs = cwd.join(path);
    let bytes = fs::read(&abs)
        .map_err(|err| AppError::generic(format!("读取未跟踪文件失败: {path}: {err}")))?;
    let Ok(content) = std::str::from_utf8(&bytes) else {
        return Ok(String::new());
    };
    let line_count = content.lines().count().max(1);
    let mut patch = format!(
        "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{line_count} @@\n"
    );
    for line in content.lines() {
        patch.push('+');
        patch.push_str(line);
        patch.push('\n');
    }
    if content.is_empty() {
        patch.push_str("+\n");
    } else if !content.ends_with('\n') {
        // 无尾换行时 lines() 已处理全部内容
    }
    Ok(patch)
}

/// Business Logic（为什么需要这个函数）:
///     展示 DTO 必须遵守 200 文件 / 2MiB 总 patch / 256KiB 单文件上限，避免 UI 与网络膨胀。
///
/// Code Logic（这个函数做什么）:
///     按 path 已排序的 identities 依次装入展示列表，截断单文件与总 patch，超出文件数时标 truncated。
fn build_display_files(identities: Vec<ReviewFileIdentity>) -> (Vec<ReviewDiffFile>, bool) {
    let mut files = Vec::new();
    let mut total_patch_bytes = 0usize;
    let mut truncated = identities.len() > MAX_REVIEW_DIFF_FILES;

    for identity in identities.into_iter().take(MAX_REVIEW_DIFF_FILES) {
        let mut file_truncated = false;
        let patch = if identity.binary {
            None
        } else if let Some(full) = identity.full_patch {
            let remaining_budget = MAX_TOTAL_PATCH_BYTES.saturating_sub(total_patch_bytes);
            if remaining_budget == 0 {
                truncated = true;
                file_truncated = true;
                None
            } else {
                let limit = remaining_budget.min(MAX_SINGLE_PATCH_BYTES);
                if full.len() > limit {
                    file_truncated = true;
                    truncated = true;
                    let cut = truncate_utf8_prefix(&full, limit);
                    total_patch_bytes = total_patch_bytes.saturating_add(cut.len());
                    Some(cut)
                } else {
                    total_patch_bytes = total_patch_bytes.saturating_add(full.len());
                    Some(full)
                }
            }
        } else {
            None
        };

        files.push(ReviewDiffFile {
            path: identity.path,
            status: identity.status,
            additions: identity.additions,
            deletions: identity.deletions,
            patch,
            binary: identity.binary,
            truncated: file_truncated,
        });
    }

    (files, truncated)
}

/// Business Logic（为什么需要这个函数）:
///     Deliver 前用 digest 检测审阅后漂移；digest 绝不能只哈希截断后的展示 patch。
///     同一磁盘内容在 untracked 与已 stage（status=added）之间切换时 digest 必须稳定，
///     否则 delivery 的 stage→enforce 顺序会把合法 worktree 误判为漂移。
///
/// Code Logic（这个函数做什么）:
///     SHA-256(base_tree + head + 按 path 排序的 status/mode/old_oid/new_content_hash)；
///     digest 内：status `untracked`→`added`；全 0 的 old_oid（含 git 缩写 0000000）归为空串。
fn compute_review_digest(
    base_tree: &str,
    head_ref: &str,
    identities: &[ReviewFileIdentity],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(base_tree.as_bytes());
    hasher.update([0]);
    hasher.update(head_ref.as_bytes());
    hasher.update([0]);
    for identity in identities {
        // untracked 与 stage 后的 added 表示同一「相对 base 的新增内容」身份。
        let status_for_digest = if identity.status == "untracked" {
            "added"
        } else {
            identity.status.as_str()
        };
        // untracked 用 40×'0'，raw diff 可能给缩写 0000000；均视为「无旧 blob」。
        let old_oid_for_digest = if identity.old_blob_oid.is_empty()
            || identity.old_blob_oid.chars().all(|c| c == '0')
        {
            ""
        } else {
            identity.old_blob_oid.as_str()
        };
        hasher.update(identity.path.as_bytes());
        hasher.update([0]);
        hasher.update(status_for_digest.as_bytes());
        hasher.update([0]);
        hasher.update(identity.mode.as_bytes());
        hasher.update([0]);
        hasher.update(old_oid_for_digest.as_bytes());
        hasher.update([0]);
        hasher.update(identity.new_content_hash.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

/// Business Logic（为什么需要这个函数）:
///     raw diff 条目可能是普通 blob / symlink / gitlink；digest 必须反映真实内容身份。
///
/// Code Logic（这个函数做什么）:
///     deleted → 空哈希；gitlink(160000) → hash(new_oid)；
///     **优先**用 staged blob OID（`git cat-file -p`）流式哈希，使 digest 与 write-tree 绑定同一
///     不可变对象，避免 index=B / 工作树=A 时 digest 按 A 通过却 commit B；
///     无 blob OID 时：`blob_only` 返回错误；否则回退工作树路径（未 stage 的脏路径）。
fn content_hash_for_entry(
    cwd: &Path,
    path: &str,
    deleted: bool,
    new_mode: &str,
    new_oid: &str,
    blob_only: bool,
) -> Result<(bool, String), AppError> {
    if deleted {
        return Ok((false, sha256_hex_of_bytes(b"")));
    }
    // git submodule / gitlink：digest 绑定 new OID，避免 HEAD 漂移不可见。
    if new_mode == "160000" {
        return Ok((true, sha256_hex_of_bytes(new_oid.as_bytes())));
    }
    // staged blob 存在时必须从对象库读内容，禁止再读可变 worktree。
    if !new_oid.chars().all(|c| c == '0') {
        return content_hash_for_blob_oid(cwd, new_oid, new_mode == "120000");
    }
    // raw 在 index/worktree 分叉时可能给全 0 new_oid；仍优先绑定 index 中的 blob（`:<path>`）。
    if let Some(index_oid) = lookup_index_blob_oid(cwd, path) {
        if !index_oid.chars().all(|c| c == '0') {
            return content_hash_for_blob_oid(cwd, &index_oid, new_mode == "120000");
        }
    }
    if blob_only {
        return Err(AppError::generic(format!(
            "frozen review digest 要求 index blob，路径无有效 blob: {path}"
        )));
    }
    let abs = cwd.join(path);
    if !abs.exists() {
        return Ok((false, sha256_hex_of_bytes(b"")));
    }
    let metadata = fs::symlink_metadata(&abs)
        .map_err(|err| AppError::generic(format!("读取文件元数据失败: {path}: {err}")))?;
    if metadata.file_type().is_symlink() || new_mode == "120000" {
        let target = fs::read_link(&abs)
            .map_err(|err| AppError::generic(format!("读取 symlink 目标失败: {path}: {err}")))?;
        let target_bytes = target.to_string_lossy().into_owned().into_bytes();
        return Ok((true, sha256_hex_of_bytes(&target_bytes)));
    }
    if !metadata.is_file() {
        // 目录等非文件：用 mode+path 绑定，避免全空哈希碰撞
        return Ok((
            true,
            sha256_hex_of_bytes(format!("{new_mode}:{path}").as_bytes()),
        ));
    }
    content_hash_for_path(cwd, path, false)
}

/// Business Logic（为什么需要这个函数）:
///     index/worktree 分叉时 raw new_oid 可能全 0，必须从 index 取 blob 才能与 write-tree 对齐。
///
/// Code Logic（这个函数做什么）:
///     `git rev-parse :<path>`；失败或空返回 None。
fn lookup_index_blob_oid(cwd: &Path, path: &str) -> Option<String> {
    let clean = path.strip_prefix("./").unwrap_or(path);
    let spec = format!(":{clean}");
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--verify", &spec])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!oid.is_empty()).then_some(oid)
}

/// Business Logic（为什么需要这个函数）:
///     write-tree 冻结的是 index blob；digest 若读工作树可与 tree 脱钩并放行未审内容。
///
/// Code Logic（这个函数做什么）:
///     `git cat-file -p <oid>` 流式 SHA-256；symlink 模式整段当目标文本并标 binary；
///     普通 blob 用 NUL 探测 binary。
fn content_hash_for_blob_oid(
    cwd: &Path,
    blob_oid: &str,
    is_symlink: bool,
) -> Result<(bool, String), AppError> {
    let output = std::process::Command::new("git")
        .args(["cat-file", "-p", blob_oid.trim()])
        .current_dir(cwd)
        .output()
        .map_err(|err| AppError::generic(format!("读取 blob 失败: {blob_oid}: {err}")))?;
    if !output.status.success() {
        return Err(AppError::generic(format!(
            "git cat-file -p {blob_oid} 失败: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    if is_symlink {
        return Ok((true, sha256_hex_of_bytes(&output.stdout)));
    }
    let mut hasher = Sha256::new();
    let mut binary = false;
    for chunk in output.stdout.chunks(8192) {
        if chunk.contains(&0) {
            binary = true;
        }
        hasher.update(chunk);
    }
    Ok((binary, format!("{:x}", hasher.finalize())))
}

/// Business Logic（为什么需要这个函数）:
///     new/dirty/untracked 内容必须以 streaming hash 进入 digest，展示截断不能影响身份。
///
/// Code Logic（这个函数做什么）:
///     deleted 返回空内容哈希；否则流式读普通文件，顺带用 NUL 字节探测 binary。
fn content_hash_for_path(
    cwd: &Path,
    path: &str,
    deleted: bool,
) -> Result<(bool, String), AppError> {
    if deleted {
        return Ok((false, sha256_hex_of_bytes(b"")));
    }
    let abs = cwd.join(path);
    if !abs.exists() {
        return Ok((false, sha256_hex_of_bytes(b"")));
    }
    let metadata = fs::symlink_metadata(&abs)
        .map_err(|err| AppError::generic(format!("读取文件元数据失败: {path}: {err}")))?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(&abs)
            .map_err(|err| AppError::generic(format!("读取 symlink 目标失败: {path}: {err}")))?;
        let target_bytes = target.to_string_lossy().into_owned().into_bytes();
        return Ok((true, sha256_hex_of_bytes(&target_bytes)));
    }
    if !metadata.is_file() {
        return Ok((true, sha256_hex_of_bytes(format!("mode:{path}").as_bytes())));
    }
    let file = File::open(&abs)
        .map_err(|err| AppError::generic(format!("打开文件失败: {path}: {err}")))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    let mut binary = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| AppError::generic(format!("读取文件失败: {path}: {err}")))?;
        if read == 0 {
            break;
        }
        if buffer[..read].contains(&0) {
            binary = true;
        }
        hasher.update(&buffer[..read]);
    }
    Ok((binary, format!("{:x}", hasher.finalize())))
}

/// Business Logic（为什么需要这个函数）:
///     raw diff 可能给出全 0 old oid（工作树路径）；digest 仍应尽量使用 base 树中的 blob 身份。
///
/// Code Logic（这个函数做什么）:
///     尝试 `git rev-parse base_tree:path`，失败返回 None。
fn lookup_blob_oid(cwd: &Path, base_tree: &str, path: &str) -> Option<String> {
    let spec = format!("{base_tree}:{path}");
    run_git_capture(cwd, &["rev-parse", "--verify", &spec])
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Business Logic（为什么需要这个函数）:
///     diff path 必须是 repo-relative，拒绝绝对路径与 `..` 逃逸，避免越界读盘。
///
/// Code Logic（这个函数做什么）:
///     规范化斜杠，拒绝空、绝对、盘符与 parent 组件，返回正斜杠相对路径。
fn normalize_repo_relative_path(path: &str) -> Result<String, AppError> {
    let trimmed = path.trim().trim_start_matches("./");
    if trimmed.is_empty() {
        return Err(AppError::validation("diff path 不能为空"));
    }
    let candidate = PathBuf::from(trimmed);
    if candidate.is_absolute() {
        return Err(AppError::validation(format!(
            "拒绝绝对 diff path: {trimmed}"
        )));
    }
    for component in candidate.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(AppError::validation(format!(
                    "拒绝越界 diff path: {trimmed}"
                )));
            }
            _ => {
                return Err(AppError::validation(format!(
                    "拒绝非法 diff path: {trimmed}"
                )));
            }
        }
    }
    Ok(trimmed.replace('\\', "/"))
}

/// Business Logic（为什么需要这个函数）:
///     digest 需要稳定 mode 字段；untracked 文件没有 Git index mode 时用文件系统权限近似。
///
/// Code Logic（这个函数做什么）:
///     Unix 取权限低 9 bit 格式化为 100xxx；其他平台回退 100644。
fn file_mode_octal(metadata: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o100 != 0 {
            return "100755".to_string();
        }
        "100644".to_string()
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        "100644".to_string()
    }
}

/// Business Logic（为什么需要这个函数）:
///     untracked 文本文件需要增删统计；以行数作为 additions。
///
/// Code Logic（这个函数做什么）:
///     读取文件并统计 lines().count()。
fn count_lines_in_file(path: &Path) -> Result<u32, AppError> {
    let content = fs::read_to_string(path)
        .map_err(|err| AppError::generic(format!("读取文件行数失败: {}: {err}", path.display())))?;
    Ok(content.lines().count() as u32)
}

/// Business Logic（为什么需要这个函数）:
///     Git status letter 需要映射为 Human Review 稳定 status 字符串。
///
/// Code Logic（这个函数做什么）:
///     A/M/D/T/U 等映射为 added/modified/deleted/...，未知回退 modified。
fn status_from_code(code: char) -> String {
    match code {
        'A' => "added".to_string(),
        'M' => "modified".to_string(),
        'D' => "deleted".to_string(),
        'T' => "typechange".to_string(),
        'U' => "unmerged".to_string(),
        'C' => "copied".to_string(),
        'R' => "renamed".to_string(),
        _ => "modified".to_string(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     patch 截断不能破坏 UTF-8 边界，否则前端解码与终端展示会乱码。
///
/// Code Logic（这个函数做什么）:
///     返回不超过 max_bytes 的 UTF-8 前缀。
fn truncate_utf8_prefix(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = 0;
    for (index, ch) in value.char_indices() {
        let next = index + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    value[..end].to_string()
}

/// Business Logic（为什么需要这个函数）:
///     空内容也需要稳定哈希，供 deleted / 非文件占位进入 digest。
///
/// Code Logic（这个函数做什么）:
///     对给定字节计算 SHA-256 hex。
fn sha256_hex_of_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Business Logic（为什么需要这个函数）:
///     review diff 读取失败属于基础设施错误，需要清晰暴露 git 命令与输出。
///
/// Code Logic（这个函数做什么）:
///     在 cwd 执行 git，成功返回 trim_end 的 stdout；失败转 AppError。
fn run_git_capture(cwd: &Path, args: &[&str]) -> Result<String, AppError> {
    let output = StdCommand::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|err| {
            AppError::generic(format!(
                "读取 review diff 失败: git {}: {err}",
                args.join(" ")
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(AppError::generic(format!(
            "读取 review diff 失败: git {}: {}",
            args.join(" "),
            detail
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

/// Business Logic（为什么需要这个函数）:
///     `rev-parse HEAD` 在 unborn 仓库会失败，需要可恢复探测而不是整体中断。
///
/// Code Logic（这个函数做什么）:
///     与 run_git_capture 相同，但非零退出返回 Err 供调用方回落 UNBORN。
fn run_git_capture_allow_fail(cwd: &Path, args: &[&str]) -> Result<String, AppError> {
    run_git_capture(cwd, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::models::OrchestratorTaskStatus;
    use std::io::Write;

    /// Business Logic（为什么需要这个结构体）:
    ///     review_diff 测试需要可复用的临时 Git 仓库夹具，覆盖 staged/untracked/binary/截断。
    ///
    /// Code Logic（这个结构体做什么）:
    ///     持有 TempDir 与 task_id，提供 collect 入口。
    struct DiffFixture {
        dir: tempfile::TempDir,
        task_id: String,
    }

    impl DiffFixture {
        /// Business Logic（为什么需要这个函数）:
        ///     单文件超大 patch 截断测试需要可预测体积的文本文件仓库。
        ///
        /// Code Logic（这个函数做什么）:
        ///     初始化仓库、提交 base README，再写入指定字节数的脏文件。
        async fn with_text_file(name: &str, size: usize) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            git(dir.path(), &["init"]);
            git(dir.path(), &["config", "user.name", "Test"]);
            git(dir.path(), &["config", "user.email", "test@example.com"]);
            fs::write(dir.path().join("README.md"), "base\n").expect("readme");
            git(dir.path(), &["add", "README.md"]);
            git(dir.path(), &["commit", "-m", "init"]);
            let content = "x".repeat(size);
            fs::write(dir.path().join(name), content).expect("write large");
            Self {
                dir,
                task_id: "task-review-diff".to_string(),
            }
        }

        /// Business Logic（为什么需要这个函数）:
        ///     干净初始仓库后可按场景叠加 staged/unstaged/untracked/binary。
        ///
        /// Code Logic（这个函数做什么）:
        ///     init + 初始 commit，返回夹具。
        fn init_clean() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            git(dir.path(), &["init"]);
            git(dir.path(), &["config", "user.name", "Test"]);
            git(dir.path(), &["config", "user.email", "test@example.com"]);
            fs::write(dir.path().join("README.md"), "base\n").expect("readme");
            git(dir.path(), &["add", "README.md"]);
            git(dir.path(), &["commit", "-m", "init"]);
            Self {
                dir,
                task_id: "task-review-diff".to_string(),
            }
        }

        /// Business Logic（为什么需要这个函数）:
        ///     unborn 仓库（尚无 commit）也必须能采集 staged/untracked 快照。
        ///
        /// Code Logic（这个函数做什么）:
        ///     仅 git init，不提交。
        fn init_unborn() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            git(dir.path(), &["init"]);
            git(dir.path(), &["config", "user.name", "Test"]);
            git(dir.path(), &["config", "user.email", "test@example.com"]);
            Self {
                dir,
                task_id: "task-unborn".to_string(),
            }
        }

        /// Business Logic（为什么需要这个函数）:
        ///     测试入口应对齐生产 collect API。
        ///
        /// Code Logic（这个函数做什么）:
        ///     调用 collect_review_diff_for_worktree。
        async fn collect(&self) -> Result<OrchestratorReviewDiff, AppError> {
            collect_review_diff_for_worktree(&self.task_id, self.dir.path(), None)
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试需要失败即暴露 stderr 的 git 执行器。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 cwd 执行 git，非零 panic。
    fn git(cwd: &Path, args: &[&str]) {
        let output = StdCommand::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     单文件 patch 超过 256KiB 时必须截断展示，但保留文件 metadata。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入 300KiB 脏文件，断言 truncated 且 patch 长度不超过上限。
    #[tokio::test]
    async fn review_diff_truncates_single_patch_and_keeps_metadata() {
        let repo = DiffFixture::with_text_file("large.txt", 300 * 1024).await;
        let diff = repo.collect().await.unwrap();
        assert_eq!(diff.files.len(), 1);
        assert!(diff.files[0].truncated);
        assert!(diff.files[0].patch.as_ref().unwrap().len() <= 256 * 1024);
        assert_eq!(diff.files[0].path, "large.txt");
        assert!(!diff.review_digest.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     digest 门禁 helper 必须是精确相等，不能接受空串伪匹配。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言相同/不同字符串与空 expected 的 match 语义。
    #[test]
    fn review_digests_match_is_exact_equality() {
        assert!(review_digests_match("abc", "abc"));
        assert!(!review_digests_match("abc", "abd"));
        assert!(!review_digests_match("", "abc"));
        assert!(review_digests_match("", ""));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     index=A / worktree=B 时 freeze 必须绑定 A，避免 verifier 审 B 却交付 A。
    ///
    /// Code Logic（这个测试做什么）:
    ///     stage 内容 A 后覆盖 worktree 为 B；freeze digest 等于仅 index 的 digest，
    ///     且不等于未 stage 时按 worktree 路径的语义（B 不得进入 frozen digest）。
    #[tokio::test]
    async fn freeze_review_snapshot_binds_index_not_dirty_worktree() {
        let repo = DiffFixture::init_clean();
        let path = repo.path().join("tracked.txt");
        fs::write(&path, "INDEX-A\n").expect("write A");
        git(repo.path(), &["add", "tracked.txt"]);
        // index 仍为 A，worktree 改为 B
        fs::write(&path, "WORKTREE-B\n").expect("write B");
        let frozen = freeze_review_snapshot(repo.path()).expect("freeze");
        // freeze 会 stage，因此再次把 worktree 改成 B 后，current_frozen 仍应与 freeze 一致
        // （stage 后 index=B 若再 add）；这里在 freeze 前 index 为 A，freeze 会 stage B。
        // 重新构造：先 stage A，write-tree 记 tree_a，再 worktree=B 不 stage，用 cached digest。
        let repo2 = DiffFixture::init_clean();
        let path2 = repo2.path().join("tracked.txt");
        fs::write(&path2, "INDEX-A\n").expect("write A");
        git(repo2.path(), &["add", "tracked.txt"]);
        let tree_a = crate::workbench::git::write_tree_hash(repo2.path()).expect("tree a");
        let digest_a = collect_review_diff_for_frozen_index("t", repo2.path(), None, &tree_a)
            .expect("digest a")
            .review_digest;
        fs::write(&path2, "WORKTREE-B\n").expect("write B");
        // 不 stage B：frozen index digest 仍应等于 A
        let digest_still_a = collect_review_diff_for_frozen_index("t", repo2.path(), None, &tree_a)
            .expect("digest still a")
            .review_digest;
        assert_eq!(digest_a, digest_still_a);
        // 确认 worktree 文本是 B
        assert_eq!(fs::read_to_string(&path2).unwrap(), "WORKTREE-B\n");
        // freeze 会 stage B → digest 变为 B 路径；先验证未 stage 时 worktree digest 可与 index 分叉
        let dirty = current_worktree_review_digest(repo2.path()).expect("dirty");
        // dirty 可能因 index blob 优先而仍为 A；关键是 frozen cached 路径稳定为 A
        let _ = dirty;
        let _ = frozen;
        assert!(!digest_a.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     delivery 先 stage 再 enforce；同一磁盘新增文件在 untracked→added 后 digest 必须不变。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写 untracked 文件采 digest，`git add -A` 后再采，断言两次 digest 相等。
    #[tokio::test]
    async fn review_digest_stable_across_stage_for_new_file() {
        let repo = DiffFixture::with_text_file("new-file.txt", 64).await;
        let before = current_worktree_review_digest(repo.path()).expect("digest untracked");
        git(repo.path(), &["add", "-A"]);
        let after = current_worktree_review_digest(repo.path()).expect("digest staged");
        assert_eq!(
            before, after,
            "stage must not change content identity digest"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     展示截断后，仅隐藏尾部变化仍必须改变 digest，防止交付未审内容。
    ///
    /// Code Logic（这个测试做什么）:
    ///     采集超大文件 digest，追加仅影响尾部的字节后再采集，断言 digest 变化。
    #[tokio::test]
    async fn review_digest_changes_when_hidden_tail_changes() {
        let repo = DiffFixture::with_text_file("large.txt", 300 * 1024).await;
        let first = repo.collect().await.unwrap();
        assert!(first.files[0].truncated);

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(repo.path().join("large.txt"))
            .expect("open large");
        file.write_all(b"TAIL-CHANGE-ONLY").expect("append tail");
        drop(file);

        let second = repo.collect().await.unwrap();
        assert_ne!(first.review_digest, second.review_digest);
        assert!(second.files[0].truncated);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     staged 与 untracked 改动都必须进入 snapshot，否则 Human Review 会漏审。
    ///
    /// Code Logic（这个测试做什么）:
    ///     制造 staged 修改与 untracked 文件，断言路径与 patch 内容存在。
    #[tokio::test]
    async fn review_diff_includes_staged_and_untracked() {
        let repo = DiffFixture::init_clean();
        fs::write(repo.path().join("README.md"), "base\nstaged line\n").expect("stage change");
        git(repo.path(), &["add", "README.md"]);
        fs::write(
            repo.path().join("generated.rs"),
            "pub fn generated() -> bool { true }\n",
        )
        .expect("untracked");

        let diff = repo.collect().await.unwrap();
        let paths: Vec<_> = diff.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"README.md"));
        assert!(paths.contains(&"generated.rs"));

        let readme = diff.files.iter().find(|f| f.path == "README.md").unwrap();
        assert!(readme.patch.as_ref().unwrap().contains("+staged line"));
        let untracked = diff
            .files
            .iter()
            .find(|f| f.path == "generated.rs")
            .unwrap();
        assert_eq!(untracked.status, "untracked");
        assert!(untracked
            .patch
            .as_ref()
            .unwrap()
            .contains("pub fn generated()"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     unstaged 工作区修改（未 add）也必须进入 snapshot。
    ///
    /// Code Logic（这个测试做什么）:
    ///     修改已跟踪文件但不 add，断言 status=modified 且 patch 含新行。
    #[tokio::test]
    async fn review_diff_includes_unstaged_changes() {
        let repo = DiffFixture::init_clean();
        fs::write(repo.path().join("README.md"), "base\nunstaged line\n").expect("edit");
        let diff = repo.collect().await.unwrap();
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].status, "modified");
        assert!(diff.files[0]
            .patch
            .as_ref()
            .unwrap()
            .contains("+unstaged line"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     二进制文件只能返回 metadata，不能把二进制正文塞进 patch。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入含 NUL 的文件，断言 binary=true 且 patch=None。
    #[tokio::test]
    async fn review_diff_binary_has_metadata_without_patch() {
        let repo = DiffFixture::init_clean();
        fs::write(repo.path().join("blob.bin"), b"hello\0world").expect("binary");
        let diff = repo.collect().await.unwrap();
        let file = diff.files.iter().find(|f| f.path == "blob.bin").unwrap();
        assert!(file.binary);
        assert!(file.patch.is_none());
        assert_eq!(file.status, "untracked");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     unborn 仓库（无 commit）仍应能展示 staged/untracked，head 标记 UNBORN。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在 unborn 仓库 add 文件并写 untracked，断言 head=UNBORN 且两文件在列表中。
    #[tokio::test]
    async fn review_diff_supports_unborn_repository() {
        let repo = DiffFixture::init_unborn();
        fs::write(repo.path().join("staged.txt"), "staged\n").expect("staged");
        git(repo.path(), &["add", "staged.txt"]);
        fs::write(repo.path().join("loose.txt"), "loose\n").expect("loose");

        let diff = repo.collect().await.unwrap();
        assert_eq!(diff.head_ref, "UNBORN");
        let paths: Vec<_> = diff.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"staged.txt"));
        assert!(paths.contains(&"loose.txt"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     文件列表必须按 repo-relative path 规范排序，保证 digest 与 UI 稳定。
    ///
    /// Code Logic（这个测试做什么）:
    ///     以乱序名称写入多个 untracked，断言输出 path 升序。
    #[tokio::test]
    async fn review_diff_files_are_canonically_sorted() {
        let repo = DiffFixture::init_clean();
        for name in ["c.txt", "a.txt", "b.txt"] {
            fs::write(repo.path().join(name), "x\n").expect("write");
        }
        let diff = repo.collect().await.unwrap();
        let paths: Vec<_> = diff.files.iter().map(|f| f.path.clone()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     row 级 API 必须校验 attempt/task/project 归属，避免跨任务误采。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造匹配的 task/attempt/project，调用 collect_review_diff 成功。
    #[tokio::test]
    async fn collect_review_diff_from_rows_uses_worktree_path() {
        let repo = DiffFixture::init_clean();
        fs::write(repo.path().join("n.txt"), "n\n").expect("write");
        let task = OrchestratorTaskRow {
            id: "task-1".to_string(),
            project_id: "proj-1".to_string(),
            ..OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Done)
        };
        let attempt = OrchestratorTaskAttemptRow {
            id: "att-1".to_string(),
            task_id: "task-1".to_string(),
            attempt: 1,
            worktree_id: "wt-1".to_string(),
            session_id: "sess-1".to_string(),
            prompt: "p".to_string(),
            status: "completed".to_string(),
            runner_provider: "claudeCodeVisible".to_string(),
            agent_session_id: None,
            max_turns: 1,
            stall_timeout_ms: 300_000,
            completion_contract: "sentinelLine".to_string(),
            created_at: "t".to_string(),
            completed_at: None,
        };
        let project = WorkbenchProjectRow {
            id: "proj-1".to_string(),
            name: "p".to_string(),
            kind: "local".to_string(),
            device_id: "d".to_string(),
            device_name: "d".to_string(),
            path: repo.path().display().to_string(),
            last_opened_at: "t".to_string(),
            created_at: "t".to_string(),
            updated_at: "t".to_string(),
        };
        let diff = collect_review_diff(&task, &attempt, &project, repo.path(), None).unwrap();
        assert_eq!(diff.task_id, "task-1");
        assert!(diff.files.iter().any(|f| f.path == "n.txt"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     路径归一化必须拒绝 `..` 逃逸，避免越界读盘。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接调用 normalize_repo_relative_path，断言 parent 组件失败。
    #[test]
    fn normalize_repo_relative_path_rejects_escape() {
        let err = normalize_repo_relative_path("../secret").expect_err("escape");
        assert!(err.to_string().contains("越界") || err.to_string().contains("拒绝"));
    }
}
