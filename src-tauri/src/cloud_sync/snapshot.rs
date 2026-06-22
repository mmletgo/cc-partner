//! cloud_sync/snapshot.rs — 工作区 JSON 文件 ↔ DB Row 导入导出
//!
//! Business Logic（为什么需要这个模块）:
//!     git 工作区是本地 SQLite 权威数据的"序列化镜像"。一次同步需先把工作区 JSON
//!     import 进本地（与本地 merge_* 合并），再把本地权威 export 写回工作区供 commit/push。
//!     工作区结构（workdir 下）：
//!       - prompts/<id>.json        → PromptRow（含 deleted，照写以传播软删除）
//!       - claude_md/claude_md.json → ClaudeMdRow（单例）
//!       - claude_history/<id>.json → ClaudeHistoryRow（含 deleted）
//!
//! Code Logic（这个模块做什么）:
//!     - `id_to_filename` / `filename_to_id`：id → 安全文件名的可逆映射（hex 编码，
//!       对任意 id round-trip 一致，规避 Windows 非法字符 / 路径分隔符问题）。
//!     - `import_to_db`：扫描工作区 JSON → merge_* 进本地（仅变化才落库）→ 返回统计。
//!     - `export_from_db`：清空 prompts/ 与 claude_history/ → 本地全量写回 → 返回统计。
//!
//! 复用既有 merge_prompt / merge_claude_md / merge_cc_history，冲突解决与局域网同步完全一致。

use crate::cc::merger::merge_cc_history;
use crate::cc::models::ClaudeHistoryRow;
use crate::error::AppError;
use crate::models::claude_md::ClaudeMdRow;
use crate::models::prompt::PromptRow;
use crate::state::AppState;
use crate::sync::claude_md::merge_claude_md;
use crate::sync::claude_md::{reconcile_from_file, write_file_if_changed};
use crate::sync::merger::merge_prompt;
use std::fs;
use std::path::Path;

/// 工作区下 prompts 目录名。
const PROMPTS_DIR: &str = "prompts";
/// 工作区下 CLAUDE.md 单例目录名。
const CLAUDE_MD_DIR: &str = "claude_md";
/// 工作区下 CC 历史目录名。
const CC_HISTORY_DIR: &str = "claude_history";
/// CLAUDE.md 单例文件名（id_to_filename 不适用，固定名）。
const CLAUDE_MD_FILE: &str = "claude_md.json";

/// import 统计：各类型实际落库条数 / CLAUDE.md 是否变更。
#[derive(Debug, Clone, Default)]
pub struct ImportStats {
    /// prompts 实际合并产生变化的条数。
    pub prompts: u64,
    /// CLAUDE.md 是否因合并而落库+写文件。
    pub claude_md_updated: bool,
    /// CC 历史实际合并产生变化的条数。
    pub cc_history: u64,
}

/// export 统计：各类型写出文件数。
#[derive(Debug, Clone, Default)]
pub struct ExportStats {
    /// prompts 写出文件数（含 deleted）。
    pub prompts: u64,
    /// 是否写出 CLAUDE.md 单例文件。
    pub claude_md: bool,
    /// CC 历史写出文件数（含 deleted）。
    pub cc_history: u64,
}

/// 把任意 id 编码为文件系统安全的文件名（不含扩展名）。
///
/// Business Logic: id 可能含 Windows 非法字符（CC 历史 id 是 `{session_id}:{uuid}` 含冒号，
///     prompts id 是 UUID 通常安全但不可假设）。需要一个对任意 id 都 round-trip 一致的映射，
///     保证 export 写出的文件名 import 时能精确还原 id。
/// Code Logic: 对 id 的 UTF-8 字节做 hex 编码（小写），输出仅含 [0-9a-f]，跨平台安全。
pub fn id_to_filename(id: &str) -> String {
    hex_encode(id.as_bytes())
}

/// 把文件名（hex 编码，去 .json）还原为原始 id。
///
/// Business Logic: import 扫描文件时需从文件名还原出原始 id 才能查本地 DB。
/// Code Logic: hex 解码文件名 stem 字节 → String。
pub fn filename_to_id(name: &str) -> String {
    // 调用方传入的是去 .json 后的 stem；这里直接 hex 解码
    hex_decode(name)
}

/// 字节序列 → 小写 hex 字符串。
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// hex 字符串 → 字节序列 → String（无效 hex 视为非法，返回原字符串避免 panic）。
fn hex_decode(hex: &str) -> String {
    if hex.len() % 2 != 0 {
        return hex.to_string();
    }
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect();
    match bytes {
        Some(b) => String::from_utf8_lossy(&b).into_owned(),
        None => hex.to_string(),
    }
}

/// 判断两条 PromptRow 是否在同步相关字段上有差异（决定是否需要落库）。
fn prompt_changed(merged: &PromptRow, local: &PromptRow) -> bool {
    merged.vector_clock != local.vector_clock
        || merged.updated_at != local.updated_at
        || merged.content != local.content
        || merged.title != local.title
        || merged.deleted != local.deleted
}

/// 判断两条 ClaudeHistoryRow 是否在同步相关字段上有差异。
fn cc_history_changed(merged: &ClaudeHistoryRow, local: &ClaudeHistoryRow) -> bool {
    merged.vector_clock != local.vector_clock
        || merged.updated_at != local.updated_at
        || merged.content != local.content
        || merged.deleted != local.deleted
}

/// 把工作区 JSON 文件 import 进本地 DB（与本地合并，仅变化落库）。
///
/// Business Logic: pull 后工作区有远端版本，需逐条与本地合并，使本地吸收远端变化，
///     供后续 export 写回统一版本。仅当合并结果与本地有差异时才 bulk_upsert（省 IO）。
///     CLAUDE.md 额外先 reconcile_from_file 一次（与局域网同步流程一致，纳入应用外编辑）。
///
/// Code Logic:
/// 1. 先 reconcile_from_file（CLAUDE.md 文件↔DB 对账）；
/// 2. 扫 prompts/*.json 反解 id → PromptRow，逐条本地 get：None 直接收，Some 则 merge_prompt，
///    仅 prompt_changed 才收集；批量 bulk_upsert；
/// 3. claude_md/claude_md.json 若存在：merge_claude_md，变化则 upsert + write_file_if_changed；
/// 4. claude_history/*.json：merge_cc_history，变化才 bulk_upsert；
/// 5. 返回 ImportStats。
pub async fn import_to_db(
    state: &AppState,
    workdir: &Path,
) -> Result<ImportStats, AppError> {
    let mut stats = ImportStats::default();

    // 1. CLAUDE.md 文件↔DB 对账（纳入应用外编辑）
    if let Err(e) = reconcile_from_file(state).await {
        tracing::warn!("cloud_sync import: CLAUDE.md 对账失败（继续）: {e}");
    }

    // 2. prompts import
    let prompts_dir = workdir.join(PROMPTS_DIR);
    if prompts_dir.is_dir() {
        let mut to_upsert: Vec<PromptRow> = Vec::new();
        for entry in fs::read_dir(&prompts_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };
            let id = filename_to_id(stem);
            let text = match fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("cloud_sync import: 读取 {} 失败（跳过）: {e}", path.display());
                    continue;
                }
            };
            let remote: PromptRow = match serde_json::from_str(&text) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("cloud_sync import: 解析 {} 失败（跳过）: {e}", path.display());
                    continue;
                }
            };
            // 文件名还原的 id 与文件内容 id 应一致；以还原 id 为准做本地查找
            let lookup_id = if remote.id == id { id.clone() } else { id };
            match state.prompt_repo.get(&lookup_id).await? {
                None => {
                    // 本地没有 → 直接接收远端版本（确保 id 用还原值）
                    let mut r = remote;
                    r.id = lookup_id;
                    to_upsert.push(r);
                }
                Some(local) => {
                    let merged = merge_prompt(&local, &remote);
                    if prompt_changed(&merged, &local) {
                        to_upsert.push(merged);
                    }
                }
            }
        }
        if !to_upsert.is_empty() {
            let n = to_upsert.len() as u64;
            state.prompt_repo.bulk_upsert(&to_upsert).await?;
            stats.prompts = n;
        }
    }

    // 3. CLAUDE.md import（单例）
    let claude_md_path = workdir.join(CLAUDE_MD_DIR).join(CLAUDE_MD_FILE);
    if claude_md_path.exists() {
        if let Ok(text) = fs::read_to_string(&claude_md_path) {
            if let Ok(remote) = serde_json::from_str::<ClaudeMdRow>(&text) {
                let local = state.claude_md_repo.get().await?;
                match local {
                    None => {
                        // 本地无 → 直接落库 + 写文件
                        state.claude_md_repo.upsert(&remote).await?;
                        write_file_if_changed(&remote.content).await?;
                        stats.claude_md_updated = true;
                    }
                    Some(local_row) => {
                        let merged = merge_claude_md(&local_row, &remote);
                        let changed = merged.content != local_row.content
                            || merged.vector_clock != local_row.vector_clock
                            || merged.updated_at != local_row.updated_at
                            || merged.device_id != local_row.device_id;
                        if changed {
                            state.claude_md_repo.upsert(&merged).await?;
                            write_file_if_changed(&merged.content).await?;
                            stats.claude_md_updated = true;
                        }
                    }
                }
            }
        }
    }

    // 4. CC 历史 import
    let cc_dir = workdir.join(CC_HISTORY_DIR);
    if cc_dir.is_dir() {
        let mut to_upsert: Vec<ClaudeHistoryRow> = Vec::new();
        for entry in fs::read_dir(&cc_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };
            let id = filename_to_id(stem);
            let text = match fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("cloud_sync import: 读取 {} 失败（跳过）: {e}", path.display());
                    continue;
                }
            };
            let remote: ClaudeHistoryRow = match serde_json::from_str(&text) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("cloud_sync import: 解析 {} 失败（跳过）: {e}", path.display());
                    continue;
                }
            };
            let lookup_id = if remote.id == id { id.clone() } else { id };
            match state.cc_history_repo.get(&lookup_id).await? {
                None => {
                    let mut r = remote;
                    r.id = lookup_id;
                    to_upsert.push(r);
                }
                Some(local) => {
                    let merged = merge_cc_history(&local, &remote);
                    if cc_history_changed(&merged, &local) {
                        to_upsert.push(merged);
                    }
                }
            }
        }
        if !to_upsert.is_empty() {
            let n = to_upsert.len() as u64;
            state.cc_history_repo.bulk_upsert(&to_upsert).await?;
            stats.cc_history = n;
        }
    }

    Ok(stats)
}

/// 把本地权威数据 export 写回工作区（覆盖式，含软删除）。
///
/// Business Logic: import 合并后本地是最新权威，需完整写回工作区供 commit/push。
///     每次全量覆盖（先清空 prompts/ 与 claude_history/ 目录内容，保留目录本身），
///     确保工作区与本地一一对应，不会残留本地已删除但远端曾有的文件。
///
/// Code Logic:
/// 1. 清空 prompts/ 与 claude_history/ 目录内容（保留目录）；
/// 2. prompt_repo.get_all_for_sync() 全量（含 deleted）逐条写 prompts/<id_to_filename>.json；
/// 3. claude_md_repo.get() 若 Some 写 claude_md/claude_md.json（无则不写）；
/// 4. cc_history 全量写 claude_history/<id_to_filename>.json；
/// 5. 返回 ExportStats。
pub async fn export_from_db(
    state: &AppState,
    workdir: &Path,
) -> Result<ExportStats, AppError> {
    let mut stats = ExportStats::default();

    let prompts_dir = workdir.join(PROMPTS_DIR);
    let claude_md_dir = workdir.join(CLAUDE_MD_DIR);
    let cc_dir = workdir.join(CC_HISTORY_DIR);

    // 确保目录存在 + 清空 prompts 与 cc 历史（保留目录）
    fs::create_dir_all(&prompts_dir)?;
    fs::create_dir_all(&claude_md_dir)?;
    fs::create_dir_all(&cc_dir)?;
    clear_dir_contents(&prompts_dir)?;
    clear_dir_contents(&cc_dir)?;

    // prompts 全量写出（含 deleted）
    let all_prompts = state.prompt_repo.get_all_for_sync().await?;
    for p in &all_prompts {
        let fname = format!("{}.json", id_to_filename(&p.id));
        let path = prompts_dir.join(&fname);
        let text = serde_json::to_string_pretty(p)?;
        fs::write(&path, text)?;
    }
    stats.prompts = all_prompts.len() as u64;

    // CLAUDE.md 单例
    if let Some(row) = state.claude_md_repo.get().await? {
        let path = claude_md_dir.join(CLAUDE_MD_FILE);
        let text = serde_json::to_string_pretty(&row)?;
        fs::write(&path, text)?;
        stats.claude_md = true;
    } else {
        // 本地无 CLAUDE.md：移除工作区可能残留的旧文件，避免传播过期内容
        let path = claude_md_dir.join(CLAUDE_MD_FILE);
        let _ = fs::remove_file(&path);
    }

    // CC 历史全量写出（含 deleted）
    let all_cc = state.cc_history_repo.get_all_for_sync().await?;
    for h in &all_cc {
        let fname = format!("{}.json", id_to_filename(&h.id));
        let path = cc_dir.join(&fname);
        let text = serde_json::to_string_pretty(h)?;
        fs::write(&path, text)?;
    }
    stats.cc_history = all_cc.len() as u64;

    Ok(stats)
}

/// 清空目录内的所有文件/子目录，但保留目录本身。
///
/// Business Logic: export 前需让工作区与本地一一对应，残留文件会传播过期/已删数据。
/// Code Logic: 读目录条目逐个 remove_file / remove_dir_all，跳过错误（并发/权限等不阻断）。
fn clear_dir_contents(dir: &Path) -> Result<(), AppError> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let res = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        if let Err(e) = res {
            tracing::warn!("cloud_sync export: 清理 {} 失败（继续）: {e}", path.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! snapshot 单测：重点覆盖 id_to_filename / filename_to_id 的可逆性
    //! （含冒号、斜杠等特殊字符的 id），以及辅助判定函数的行为。

    use super::*;

    #[test]
    fn id_to_filename_roundtrip_simple() {
        // 普通 UUID id：编码后解码一致
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let fname = id_to_filename(id);
        assert!(!fname.is_empty());
        // 文件名只含 hex 字符
        assert!(fname.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(filename_to_id(&fname), id);
    }

    #[test]
    fn id_to_filename_roundtrip_with_colon() {
        // CC 历史 id 含冒号（session_id:uuid），文件系统（尤其 Windows）不允许冒号
        let id = "abc123-def456:7890aabb";
        let fname = id_to_filename(id);
        assert!(
            !fname.contains(':'),
            "文件名不应含冒号: {fname}"
        );
        assert_eq!(filename_to_id(&fname), id);
    }

    #[test]
    fn id_to_filename_roundtrip_with_slash() {
        // 含路径分隔符的 id：编码后不含斜杠
        let id = "path/with/slash";
        let fname = id_to_filename(id);
        assert!(!fname.contains('/'));
        assert!(!fname.contains('\\'));
        assert_eq!(filename_to_id(&fname), id);
    }

    #[test]
    fn id_to_filename_roundtrip_unicode() {
        // 非 ASCII（中文）id
        let id = "用户-设备-001";
        let fname = id_to_filename(id);
        assert_eq!(filename_to_id(&fname), id);
    }

    #[test]
    fn id_to_filename_empty() {
        // 空 id：编码后空串，解码也空串
        assert_eq!(id_to_filename(""), "");
        assert_eq!(filename_to_id(""), "");
    }

    #[test]
    fn id_to_filename_is_deterministic() {
        // 相同 id 多次编码结果一致
        let id = "same:1";
        assert_eq!(id_to_filename(id), id_to_filename(id));
    }

    #[test]
    fn filename_to_id_invalid_hex_returns_input() {
        // 非 hex 字符串（奇数长度或非法字符）解码回退原串，不 panic
        let s = "not-hex!";
        let decoded = filename_to_id(s);
        assert_eq!(decoded, s);
    }

    #[test]
    fn claude_md_constants_stable() {
        assert_eq!(CLAUDE_MD_FILE, "claude_md.json");
        assert_eq!(CLAUDE_MD_DIR, "claude_md");
        assert_eq!(PROMPTS_DIR, "prompts");
        assert_eq!(CC_HISTORY_DIR, "claude_history");
    }
}
