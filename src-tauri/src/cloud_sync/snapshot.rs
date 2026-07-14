//! cloud_sync/snapshot.rs — 工作区 JSON 文件 ↔ DB Row 导入导出
//!
//! Business Logic（为什么需要这个模块）:
//!     git 工作区是本地 SQLite 权威数据的"序列化镜像"。一次同步需先把工作区 JSON
//!     import 进本地（与本地 merge_* 合并），再把本地权威 export 写回工作区供 commit/push。
//!     工作区结构（workdir 下）：
//!       - prompts/<id>.json        → PromptRow（含 deleted，照写以传播软删除）
//!       - claude_history/<id>.json → ClaudeHistoryRow（含 deleted）
//!       - ssh_targets/<host>.json → SshTargetRow（含 deleted）
//!       - scratchpad/<hex(id)>.json  → ScratchpadRow（多页面，含 deleted）
//!
//! Code Logic（这个模块做什么）:
//!     - `id_to_filename` / `filename_to_id`：id → 安全文件名的可逆映射（hex 编码，
//!       对任意 id round-trip 一致，规避 Windows 非法字符 / 路径分隔符问题）。
//!     - `import_to_db`：扫描工作区 JSON → merge_* 进本地（仅变化才落库）→ 返回统计。
//!     - `export_from_db`：清空 prompts/ 与 claude_history/ 与 ssh_targets/ → 本地全量写回 → 返回统计。
//!
//! CLAUDE.md 不参与云端自动同步；它只由 CLAUDE.md 页面用户主动推送。
//! 复用既有 merge_prompt / merge_cc_history / merge_ssh_target，冲突解决与局域网同步完全一致。

use crate::cc::merger::merge_cc_history;
use crate::cc::models::ClaudeHistoryRow;
use crate::error::AppError;
use crate::models::prompt::PromptRow;
use crate::models::scratchpad::{ScratchpadRow, SCRATCHPAD_ID};
use crate::models::ssh_target::SshTargetRow;
use crate::state::AppState;
use crate::storage::ScratchpadRepo;
use crate::sync::merger::merge_prompt;
use crate::sync::scratchpad::{merge_scratchpad, scratchpad_changed};
use crate::sync::ssh_target::merge_ssh_target;
use std::fs;
use std::path::Path;

/// 工作区下 prompts 目录名。
const PROMPTS_DIR: &str = "prompts";
/// 工作区下 CC 历史目录名。
const CC_HISTORY_DIR: &str = "claude_history";
/// 工作区下 SSH 目标目录名。
const SSH_TARGETS_DIR: &str = "ssh_targets";
/// 工作区下速记本目录名。
const SCRATCHPAD_DIR: &str = "scratchpad";

/// 云端速记本 JSON 的兼容反序列化结构。
///
/// Business Logic: 旧版云端 `scratchpad/scratchpad.json` 没有 title 字段；升级后仍必须导入为默认页。
/// Code Logic: title 缺失时补“速记本”，deleted 缺失时补 false，再转换为 ScratchpadRow。
#[derive(Debug, serde::Deserialize)]
struct ScratchpadCloudRow {
    id: String,
    #[serde(default = "default_scratchpad_title")]
    title: String,
    content: String,
    created_at: String,
    updated_at: String,
    device_id: String,
    vector_clock: std::collections::HashMap<String, u64>,
    #[serde(default)]
    deleted: bool,
    /// 删除序号；旧云端 JSON 缺字段时为 0。
    #[serde(default)]
    delete_epoch: u64,
}

/// 旧速记本云端 JSON 缺 title 时使用的默认标题。
///
/// Business Logic: 旧单页内容升级后需要以“速记本”标题显示在第一页。
/// Code Logic: 返回固定中文标题，与 DB title 迁移默认值一致。
fn default_scratchpad_title() -> String {
    "速记本".to_string()
}

impl ScratchpadCloudRow {
    /// 转换为同步层使用的 ScratchpadRow。
    ///
    /// Business Logic: 云端导入后必须进入同一套 merge_scratchpad 逻辑。
    /// Code Logic: 字段原样搬运，title 已由 serde default 兜底。
    fn into_row(self) -> ScratchpadRow {
        ScratchpadRow {
            id: self.id,
            title: self.title,
            content: self.content,
            created_at: self.created_at,
            updated_at: self.updated_at,
            device_id: self.device_id,
            vector_clock: self.vector_clock,
            deleted: self.deleted,
            delete_epoch: self.delete_epoch,
        }
    }
}
/// import 统计：各类型实际落库条数。
#[derive(Debug, Clone, Default)]
pub struct ImportStats {
    /// prompts 实际合并产生变化的条数。
    pub prompts: u64,
    /// CC 历史实际合并产生变化的条数。
    pub cc_history: u64,
    /// SSH 目标实际合并产生变化的条数。
    pub ssh_targets: u64,
    /// 速记本实际合并产生变化的页面数。
    pub scratchpad: u64,
}

impl ImportStats {
    /// prompts + cc_history + ssh_targets + scratchpad 的总导入条数。
    ///
    /// Business Logic: engine 统计 pulled 条数时需包含所有自动云同步资源，避免新增类型漏计。
    /// Code Logic: 四字段相加。CLAUDE.md 仍不参与自动云同步。
    pub fn total(&self) -> u64 {
        self.prompts + self.cc_history + self.ssh_targets + self.scratchpad
    }
}

/// export 统计：各类型写出文件数。
#[derive(Debug, Clone, Default)]
pub struct ExportStats {
    /// prompts 写出文件数（含 deleted）。
    pub prompts: u64,
    /// CC 历史写出文件数（含 deleted）。
    pub cc_history: u64,
    /// SSH 目标写出文件数（含 deleted）。
    pub ssh_targets: u64,
    /// 速记本写出文件数（含 deleted 页面）。
    pub scratchpad: u64,
}

impl ExportStats {
    /// prompts + cc_history + ssh_targets + scratchpad 的总写出数。
    ///
    /// Business Logic: engine 统计 pushed 条数时需对多类型条目求和，集中在此避免散落的
    ///     `last_export.prompts + last_export.cc_history` 漏加新类型。
    /// Code Logic: 四字段相加。CLAUDE.md 不参与云端自动同步。
    pub fn total(&self) -> u64 {
        self.prompts + self.cc_history + self.ssh_targets + self.scratchpad
    }
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

/// 判断两条 SshTargetRow 是否在同步相关字段上有差异。
///
/// Business Logic: import 合并后只有当结果在同步相关字段（向量时钟/时间戳/可编辑内容/软删除）
///     上与本地不同才需落库，省 IO。字段集与局域网 ssh_target_sync_with_peer 的差异判定一致。
/// Code Logic: 逐字段 != 比对（含 port/label 等 SSH 特有可编辑字段）。
fn ssh_target_changed(merged: &SshTargetRow, local: &SshTargetRow) -> bool {
    merged.vector_clock != local.vector_clock
        || merged.updated_at != local.updated_at
        || merged.username != local.username
        || merged.port != local.port
        || merged.label != local.label
        || merged.deleted != local.deleted
}

/// 读取并解析单个 JSON 文件，失败时记录 warn 并返回 None。
///
/// Business Logic: cloud import 不能因单个损坏 JSON 中断整次同步，保持与其他资源导入容错一致。
/// Code Logic: read_to_string + serde_json::from_str；任何错误都 tracing::warn 后跳过。
fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                "cloud_sync import: 读取 {} 失败（跳过）: {e}",
                path.display()
            );
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                "cloud_sync import: 解析 {} 失败（跳过）: {e}",
                path.display()
            );
            None
        }
    }
}

/// 把工作区 JSON 文件 import 进本地 DB（与本地合并，仅变化落库）。
///
/// Business Logic: pull 后工作区有远端版本，需逐条与本地合并，使本地吸收远端变化，
///     供后续 export 写回统一版本。仅当合并结果与本地有差异时才 bulk_upsert（省 IO）。
///     CLAUDE.md 不参与云端自动同步，避免 GitHub 自动同步覆盖用户本机配置。
///
/// Code Logic:
/// 1. 扫 prompts/*.json 反解 id → PromptRow，逐条本地 get：None 直接收，Some 则 merge_prompt，
///    仅 prompt_changed 才收集；批量 bulk_upsert；
/// 2. claude_history/*.json：merge_cc_history，变化才 bulk_upsert；
/// 3. ssh_targets/*.json：merge_ssh_target，变化才 bulk_upsert（host 为主键，文件名 hex 还原）；
/// 4. scratchpad/*.json：按文件名还原 id，merge_scratchpad，变化才 upsert；兼容旧 scratchpad/scratchpad.json；
/// 5. 返回 ImportStats。
pub async fn import_to_db(state: &AppState, workdir: &Path) -> Result<ImportStats, AppError> {
    let mut stats = ImportStats::default();

    // 1. prompts import
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
                    tracing::warn!(
                        "cloud_sync import: 读取 {} 失败（跳过）: {e}",
                        path.display()
                    );
                    continue;
                }
            };
            let remote: PromptRow = match serde_json::from_str(&text) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "cloud_sync import: 解析 {} 失败（跳过）: {e}",
                        path.display()
                    );
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

    // 2. CC 历史 import
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
                    tracing::warn!(
                        "cloud_sync import: 读取 {} 失败（跳过）: {e}",
                        path.display()
                    );
                    continue;
                }
            };
            let remote: ClaudeHistoryRow = match serde_json::from_str(&text) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "cloud_sync import: 解析 {} 失败（跳过）: {e}",
                        path.display()
                    );
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

    // 3. SSH 目标 import（host 为主键，文件名 hex 还原）
    let ssh_dir = workdir.join(SSH_TARGETS_DIR);
    if ssh_dir.is_dir() {
        let mut to_upsert: Vec<SshTargetRow> = Vec::new();
        for entry in fs::read_dir(&ssh_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };
            let host = filename_to_id(stem);
            let text = match fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(
                        "cloud_sync import: 读取 {} 失败（跳过）: {e}",
                        path.display()
                    );
                    continue;
                }
            };
            let remote: SshTargetRow = match serde_json::from_str(&text) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "cloud_sync import: 解析 {} 失败（跳过）: {e}",
                        path.display()
                    );
                    continue;
                }
            };
            // 文件名还原的 host 与文件内容 host 应一致；以还原 host 为准做本地查找
            let lookup_host = if remote.host == host {
                host.clone()
            } else {
                host
            };
            match state.ssh_target_repo.get(&lookup_host).await? {
                None => {
                    // 本地没有 → 直接接收远端版本（确保 host 用还原值）
                    let mut r = remote;
                    r.host = lookup_host;
                    to_upsert.push(r);
                }
                Some(local) => {
                    let merged = merge_ssh_target(&local, &remote);
                    if ssh_target_changed(&merged, &local) {
                        to_upsert.push(merged);
                    }
                }
            }
        }
        if !to_upsert.is_empty() {
            let n = to_upsert.len() as u64;
            state.ssh_target_repo.bulk_upsert(&to_upsert).await?;
            stats.ssh_targets = n;
        }
    }

    stats.scratchpad =
        import_scratchpad_dir(&state.scratchpad_repo, &workdir.join(SCRATCHPAD_DIR)).await?;

    Ok(stats)
}

/// 把本地权威数据 export 写回工作区（覆盖式，含软删除）。
///
/// Business Logic: import 合并后本地是最新权威，需完整写回工作区供 commit/push。
///     每次全量覆盖（先清空 prompts/、claude_history/、ssh_targets/、scratchpad/ 目录内容，保留目录本身），
///     确保工作区与本地一一对应，不会残留本地已删除但远端曾有的文件。
///     CLAUDE.md 不参与云端自动同步，工作区内旧 claude_md 文件会保持原样但被本流程忽略。
///
/// Code Logic:
/// 1. 清空 prompts/、claude_history/、ssh_targets/、scratchpad/ 目录内容（保留目录）；
/// 2. prompt_repo.get_all_for_sync() 全量（含 deleted）逐条写 prompts/<id_to_filename>.json；
/// 3. cc_history 全量写 claude_history/<id_to_filename>.json；
/// 4. ssh_targets 全量写 ssh_targets/<id_to_filename(host)>.json；
/// 5. scratchpad 全量写 scratchpad/<hex(id)>.json；
/// 6. 返回 ExportStats。
pub async fn export_from_db(state: &AppState, workdir: &Path) -> Result<ExportStats, AppError> {
    let mut stats = ExportStats::default();

    let prompts_dir = workdir.join(PROMPTS_DIR);
    let cc_dir = workdir.join(CC_HISTORY_DIR);
    let ssh_dir = workdir.join(SSH_TARGETS_DIR);
    let scratchpad_dir = workdir.join(SCRATCHPAD_DIR);

    // 确保目录存在 + 清空 prompts 与 cc 历史与 ssh_targets 与 scratchpad（保留目录）
    fs::create_dir_all(&prompts_dir)?;
    fs::create_dir_all(&cc_dir)?;
    fs::create_dir_all(&ssh_dir)?;
    fs::create_dir_all(&scratchpad_dir)?;
    clear_dir_contents(&prompts_dir)?;
    clear_dir_contents(&cc_dir)?;
    clear_dir_contents(&ssh_dir)?;
    clear_dir_contents(&scratchpad_dir)?;

    // prompts 全量写出（含 deleted）
    let all_prompts = state.prompt_repo.get_all_for_sync().await?;
    for p in &all_prompts {
        let fname = format!("{}.json", id_to_filename(&p.id));
        let path = prompts_dir.join(&fname);
        let text = serde_json::to_string_pretty(p)?;
        fs::write(&path, text)?;
    }
    stats.prompts = all_prompts.len() as u64;

    // CC 历史全量写出（含 deleted）
    let all_cc = state.cc_history_repo.get_all_for_sync().await?;
    for h in &all_cc {
        let fname = format!("{}.json", id_to_filename(&h.id));
        let path = cc_dir.join(&fname);
        let text = serde_json::to_string_pretty(h)?;
        fs::write(&path, text)?;
    }
    stats.cc_history = all_cc.len() as u64;

    // SSH 目标全量写出（含 deleted，host 为主键）
    let all_ssh = state.ssh_target_repo.get_all_for_sync().await?;
    for s in &all_ssh {
        let fname = format!("{}.json", id_to_filename(&s.host));
        let path = ssh_dir.join(&fname);
        let text = serde_json::to_string_pretty(s)?;
        fs::write(&path, text)?;
    }
    stats.ssh_targets = all_ssh.len() as u64;

    stats.scratchpad = export_scratchpad_dir(&state.scratchpad_repo, &scratchpad_dir).await?;

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
            tracing::warn!(
                "cloud_sync export: 清理 {} 失败（继续）: {e}",
                path.display()
            );
        }
    }
    Ok(())
}

/// 从 scratchpad 目录导入多页面 JSON，并返回实际落库页面数。
///
/// Business Logic: 云同步升级为多文件后，需要读取 `scratchpad/<hex(id)>.json`；
///     旧版 `scratchpad/scratchpad.json` 也必须继续导入为默认页。
/// Code Logic: 文件 stem 通过 filename_to_id 还原，非法 hex 会回退原 stem；逐页 merge 后批量 upsert。
async fn import_scratchpad_dir(
    repo: &ScratchpadRepo,
    scratchpad_dir: &Path,
) -> Result<u64, AppError> {
    if !scratchpad_dir.is_dir() {
        return Ok(0);
    }

    let mut to_upsert: Vec<ScratchpadRow> = Vec::new();
    for entry in fs::read_dir(scratchpad_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let id = if stem == SCRATCHPAD_ID {
            SCRATCHPAD_ID.to_string()
        } else {
            filename_to_id(stem)
        };
        let remote = match read_json_file::<ScratchpadCloudRow>(&path) {
            Some(mut r) => {
                if r.id != id {
                    r.id = id;
                }
                r.into_row()
            }
            None => continue,
        };
        match repo.get(&remote.id).await? {
            None => to_upsert.push(remote),
            Some(local) => {
                let merged = merge_scratchpad(&local, &remote);
                if scratchpad_changed(&merged, &local) {
                    to_upsert.push(merged);
                }
            }
        }
    }

    let changed = to_upsert.len() as u64;
    if !to_upsert.is_empty() {
        repo.bulk_upsert(&to_upsert).await?;
    }
    Ok(changed)
}

/// 把全部 scratchpad 页面导出到 scratchpad 目录，并返回写出文件数。
///
/// Business Logic: 云端镜像应包含全部页面（含 deleted），让删除和重命名都能跨设备传播。
/// Code Logic: 每页写为 `scratchpad/<hex(id)>.json`，不再生成旧固定名 `scratchpad.json`。
async fn export_scratchpad_dir(
    repo: &ScratchpadRepo,
    scratchpad_dir: &Path,
) -> Result<u64, AppError> {
    let all_pages = repo.get_all_for_sync().await?;
    for page in &all_pages {
        let fname = format!("{}.json", id_to_filename(&page.id));
        let path = scratchpad_dir.join(&fname);
        let text = serde_json::to_string_pretty(page)?;
        fs::write(&path, text)?;
    }
    Ok(all_pages.len() as u64)
}

#[cfg(test)]
mod tests {
    //! snapshot 单测：重点覆盖 id_to_filename / filename_to_id 的可逆性
    //! （含冒号、斜杠等特殊字符的 id），以及辅助判定函数的行为。

    use super::*;
    use crate::models::scratchpad::SCRATCHPAD_ID;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::HashMap;
    use std::str::FromStr;

    /// 构造测试用临时目录。
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cc-partner-snapshot-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 创建 scratchpad 云同步 helper 测试需要的内存仓库。
    async fn setup_scratchpad_repo() -> ScratchpadRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS scratchpad (id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '速记本', content TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, delete_epoch INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        ScratchpadRepo::new(pool)
    }

    /// 构造测试用 ScratchpadRow。
    fn scratchpad_row(id: &str, title: &str, content: &str, vc_counter: u64) -> ScratchpadRow {
        let mut vector_clock = HashMap::new();
        vector_clock.insert("device-remote".to_string(), vc_counter);
        ScratchpadRow {
            id: id.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            created_at: "2024-01-01T00:00:00+00:00".to_string(),
            updated_at: "2024-01-02T00:00:00+00:00".to_string(),
            device_id: "device-remote".to_string(),
            vector_clock,
            deleted: false,
            delete_epoch: 0,
        }
    }

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
        assert!(!fname.contains(':'), "文件名不应含冒号: {fname}");
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
    fn sync_dir_constants_stable() {
        assert_eq!(PROMPTS_DIR, "prompts");
        assert_eq!(CC_HISTORY_DIR, "claude_history");
        assert_eq!(SSH_TARGETS_DIR, "ssh_targets");
        assert_eq!(SCRATCHPAD_DIR, "scratchpad");
    }

    #[test]
    fn export_stats_total_includes_scratchpad() {
        let stats = ExportStats {
            prompts: 1,
            cc_history: 2,
            ssh_targets: 3,
            scratchpad: 1,
        };

        assert_eq!(stats.total(), 7);
    }

    #[test]
    fn import_stats_total_includes_scratchpad() {
        let stats = ImportStats {
            prompts: 1,
            cc_history: 2,
            ssh_targets: 3,
            scratchpad: 1,
        };

        assert_eq!(stats.scratchpad, 1);
        assert_eq!(stats.total(), 7);
    }

    /// 旧云同步单文件 scratchpad/scratchpad.json 仍可导入为默认页。
    #[tokio::test]
    async fn import_accepts_legacy_single_scratchpad_file() {
        let repo = setup_scratchpad_repo().await;
        let workdir = temp_dir("legacy-import");
        let scratchpad_dir = workdir.join(SCRATCHPAD_DIR);
        std::fs::create_dir_all(&scratchpad_dir).unwrap();
        std::fs::write(
            scratchpad_dir.join("scratchpad.json"),
            serde_json::json!({
                "id": SCRATCHPAD_ID,
                "content": "legacy cloud",
                "created_at": "2024-01-01T00:00:00+00:00",
                "updated_at": "2024-01-02T00:00:00+00:00",
                "device_id": "device-remote",
                "vector_clock": { "device-remote": 1 },
                "deleted": false
            })
            .to_string(),
        )
        .unwrap();

        let changed = import_scratchpad_dir(&repo, &scratchpad_dir).await.unwrap();
        let got = repo.get(SCRATCHPAD_ID).await.unwrap().unwrap();

        assert_eq!(changed, 1);
        assert_eq!(got.title, "速记本");
        assert_eq!(got.content, "legacy cloud");
        let _ = std::fs::remove_dir_all(workdir);
    }

    /// 多页面 scratchpad export 使用 hex(id).json，并统计全部页面。
    #[tokio::test]
    async fn export_writes_multiple_scratchpad_pages_with_hex_filenames() {
        let repo = setup_scratchpad_repo().await;
        let workdir = temp_dir("multi-export");
        let scratchpad_dir = workdir.join(SCRATCHPAD_DIR);
        std::fs::create_dir_all(&scratchpad_dir).unwrap();
        let default_page = scratchpad_row(SCRATCHPAD_ID, "速记本", "default", 1);
        let second_page = scratchpad_row("page:two", "第二页", "second", 1);
        repo.upsert(&default_page).await.unwrap();
        repo.upsert(&second_page).await.unwrap();

        let exported = export_scratchpad_dir(&repo, &scratchpad_dir).await.unwrap();

        assert_eq!(exported, 2);
        assert!(scratchpad_dir
            .join(format!("{}.json", id_to_filename(SCRATCHPAD_ID)))
            .is_file());
        assert!(scratchpad_dir
            .join(format!("{}.json", id_to_filename("page:two")))
            .is_file());
        assert!(!scratchpad_dir.join("scratchpad.json").is_file());
        let _ = std::fs::remove_dir_all(workdir);
    }
}
