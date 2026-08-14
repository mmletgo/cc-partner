//! commands/game_plugin.rs — 列出并给插件游戏入账。
//!
//! Business Logic（为什么需要这个模块）:
//!     大厅只通过 invoke 读插件目录；完成一局的分钟数必须由后端按清单入账。
//!
//! Code Logic（这个模块做什么）:
//!     list 扫描并在缺目录时创建；credit 只信 game.json 的 rewardMinutes。

use crate::error::AppError;
use crate::game_plugin::GamePluginSummary;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::State;

/// 插件列表 DTO。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GamePluginListDto {
    pub dir: String,
    pub games: Vec<GamePluginSummary>,
}

/// 列出 plugin 目录里的游戏。
///
/// Business Logic: 大厅打开时要看到内置记单词以外的用户游戏。
/// Code Logic: 读 config.game_plugin_dir，缺则创建，再扫描一级子目录。
#[tauri::command]
pub async fn list_game_plugins(state: State<'_, AppState>) -> Result<GamePluginListDto, AppError> {
    let dir = state.config.read().unwrap().game_plugin_dir.clone();
    let games = crate::game_plugin::list_or_create(&PathBuf::from(&dir))?;
    Ok(GamePluginListDto { dir, games })
}

/// 插件完成入账入参。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditGamePluginInput {
    pub game_id: String,
    pub source_id: Option<String>,
}

/// 按清单分钟给电池入账。
///
/// Business Logic: 游戏只能自报完成，分钟数不能由消息指定。
/// Code Logic: 扫描找到 playable 游戏；reward=0 只返回快照；否则 credit_explicit。
#[tauri::command]
pub async fn credit_game_plugin(
    state: State<'_, AppState>,
    input: CreditGamePluginInput,
) -> Result<crate::battery::BatterySnapshotDto, AppError> {
    let (dir, battery_cfg) = {
        let cfg = state.config.read().unwrap();
        (cfg.game_plugin_dir.clone(), cfg.battery.clone())
    };
    let games = crate::game_plugin::scan_game_plugins(&PathBuf::from(&dir))?;
    let game = games
        .into_iter()
        .find(|g| g.id == input.game_id)
        .ok_or_else(|| AppError::validation("找不到这个游戏"))?;
    if !game.playable {
        return Err(AppError::validation("这个游戏还不能玩"));
    }
    let battery_repo =
        crate::storage::BatteryRepo::with_gate(state.db.clone(), state.maintenance_gate.clone());
    let now = chrono::Utc::now().timestamp();
    if game.reward_minutes <= 0 {
        return crate::battery::get_snapshot(&battery_repo, &battery_cfg, now).await;
    }
    let source_id = crate::battery::game_plugin_source_id(
        &game.id,
        input.source_id.as_deref(),
    );
    let snapshot = crate::battery::credit_explicit(
        &battery_repo,
        &battery_cfg,
        crate::config::BatteryCreditSource::GamePlugin,
        &source_id,
        game.reward_minutes,
        now,
    )
    .await?;
    state.emit_event("battery:changed", snapshot.clone());
    Ok(snapshot)
}
