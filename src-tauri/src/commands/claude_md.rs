//! commands/claude_md.rs — user 级 CLAUDE.md 读写命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 /claude-md 页通过 invoke 调用这两个命令完成 CLAUDE.md 的读取与编辑。
//!     CLAUDE.md 既是磁盘文件（~/.claude/CLAUDE.md，用户可能用任意编辑器改），
//!     也是 DB 单例记录（同步的权威来源），故读时需先对账（文件→DB），写时需双写（文件+DB）。
//!
//! Code Logic（这个模块做什么）:
//!     `get_claude_md`：先 `reconcile_from_file`（失败仅记日志不阻断）再读 DB 行；
//!         DB 无行时返回空 DTO（content/updatedAt 为空串，vectorClock 为空 map）。
//!     `update_claude_md`：先 create_dir_all + write 文件，再推进 vector_clock，
//!         upsert DB 行后返回 DTO（update 刚写过文件，无需再对账）。
//!     返回类型用 camelCase 的 `ClaudeMdDto`，对齐前端 TS 类型。

use crate::error::AppError;
use crate::models::claude_md::{ClaudeMdDto, ClaudeMdRow, CLAUDE_MD_ID};
use crate::state::AppState;
use chrono::Utc;
use std::collections::HashMap;
use std::fs;
use tauri::State;

/// 读取 CLAUDE.md：先对账（文件→DB），再返回 DB 权威版本。
///
/// Business Logic: 用户可能在应用外（编辑器、Claude Code 自身）修改 ~/.claude/CLAUDE.md，
///     读取前需把这份"外部真相"对账进 DB，否则会用过期 DB 内容覆盖前端展示。
///     对账失败不阻断读取（仅记 warn 日志），降级返回 DB 当前版本，保证 UI 可用。
/// Code Logic: reconcile_from_file → claude_md_repo.get → None 返回空 DTO、Some 返回 to_dto。
#[tauri::command]
pub async fn get_claude_md(state: State<'_, AppState>) -> Result<ClaudeMdDto, AppError> {
    // 先对账：文件被应用外编辑时以文件为准推进 DB 向量时钟
    if let Err(e) = crate::sync::claude_md::reconcile_from_file(state.inner()).await {
        tracing::warn!("claude_md 对账失败: {e}");
    }
    let row = state.claude_md_repo.get().await?;
    match row {
        Some(r) => Ok(r.to_dto()),
        None => {
            // DB 无行：返回空 DTO（设备 ID 仍填本机，便于前端展示归属）
            let device_id = state.device_id.as_str().to_string();
            Ok(ClaudeMdDto {
                content: String::new(),
                updated_at: String::new(),
                device_id,
                vector_clock: HashMap::new(),
            })
        }
    }
}

/// 更新 CLAUDE.md：写文件 + 推进 vector_clock + upsert DB，返回最新 DTO。
///
/// Business Logic: 用户在前端编辑保存时调用。CLAUDE.md 需同时落盘（供 Claude Code 等工具读取）
///     与落库（供跨设备同步），落库时推进本设备 vector_clock 使对端感知本次编辑。
///     update 刚写过文件，content 必然一致，无需再 reconcile。
/// Code Logic:
///     1. 读旧行取旧 vector_clock（无行则空 map）；
///     2. create_dir_all 父目录 + write 文件；
///     3. increment(old_vc, device_id) 推进时钟；
///     4. 构造 ClaudeMdRow（id 恒为 CLAUDE_MD_ID）+ upsert；
///     5. 返回 to_dto。
#[tauri::command]
pub async fn update_claude_md(
    state: State<'_, AppState>,
    content: String,
) -> Result<ClaudeMdDto, AppError> {
    let old = state.claude_md_repo.get().await?;
    let old_vc = old
        .as_ref()
        .map(|r| r.vector_clock.clone())
        .unwrap_or_default();

    // 写文件：先确保父目录存在（~/.claude 可能尚未创建）
    let path = crate::sync::claude_md::claude_md_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, &content)?;

    let device_id = state.device_id.as_str().to_string();
    let now = Utc::now().to_rfc3339();
    let new_vc = crate::sync::vector_clock::increment(&old_vc, &device_id);

    let row = ClaudeMdRow {
        id: CLAUDE_MD_ID.into(),
        content: content.clone(),
        updated_at: now,
        device_id: device_id.clone(),
        vector_clock: new_vc,
    };
    state.claude_md_repo.upsert(&row).await?;
    Ok(row.to_dto())
}
