//! 充电账本服务：模式、入账、多窗 1× 扣时。
//!
//! Business Logic（为什么需要这个模块）:
//!     命令层与健康/闪卡挂钩只应调用一组权威操作，避免各入口自己改余额。
//!
//! Code Logic（这个模块做什么）:
//!     组合 BatteryRepo + BatteryConfig + 进程内消耗窗集合。

use super::policy::{
    credit_delta_ms, credit_delta_ms_explicit, debit_delta_ms, evaluate_daily_reset, MS_PER_MINUTE,
};
use crate::config::{BatteryConfig, BatteryCreditSource};
use crate::error::AppError;
use crate::storage::battery_repo::{BatteryLedgerRow, BatteryRepo, BatteryStateRow};
use chrono::{Local, TimeZone};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// 前端快照。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BatterySnapshotDto {
    /// charging | unlimited
    pub mode: String,
    /// 剩余毫秒。
    pub remaining_ms: i64,
    /// 余额上限毫秒。
    pub max_balance_ms: i64,
    /// 今日已充毫秒。
    pub today_earned_ms: i64,
    /// 今日已用毫秒。
    pub today_spent_ms: i64,
    /// 当前是否正在扣时。
    pub consuming: bool,
    /// 本次入账分钟（仅 credit 事件有值）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_minutes: Option<i64>,
    /// 本次入账来源。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_source: Option<String>,
}

/// 进程内消耗窗与上次结算时间。
#[derive(Debug, Default)]
pub struct BatteryDrainRuntime {
    consuming: HashSet<String>,
    last_settle_ms: Option<i64>,
}

impl BatteryDrainRuntime {
    /// 空运行时。
    pub fn new() -> Self {
        Self::default()
    }
}

/// 全局 drain 运行时（桌面进程内一份）。
static DRAIN: OnceLock<Mutex<BatteryDrainRuntime>> = OnceLock::new();

fn drain_runtime() -> std::sync::MutexGuard<'static, BatteryDrainRuntime> {
    DRAIN
        .get_or_init(|| Mutex::new(BatteryDrainRuntime::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn local_day_bounds(now: i64) -> (i64, i64) {
    let local = Local
        .timestamp_opt(now, 0)
        .single()
        .unwrap_or_else(Local::now);
    let start = local
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|naive| naive.and_local_timezone(Local).single())
        .map(|dt| dt.timestamp())
        .unwrap_or(now);
    (start, start + 86_400)
}

fn source_kind(source: BatteryCreditSource) -> &'static str {
    match source {
        BatteryCreditSource::Flashcard => "credit_wordgame",
        BatteryCreditSource::GamePlugin => "credit_game_plugin",
        BatteryCreditSource::Health => "credit_health",
    }
}

fn source_prefix(source: BatteryCreditSource) -> &'static str {
    match source {
        BatteryCreditSource::Health => "habit:",
        BatteryCreditSource::Flashcard => "wordgame:",
        BatteryCreditSource::GamePlugin => "game-plugin:",
    }
}

fn source_token(source: BatteryCreditSource) -> &'static str {
    match source {
        BatteryCreditSource::Health => "health",
        BatteryCreditSource::Flashcard => "flashcard",
        BatteryCreditSource::GamePlugin => "game-plugin",
    }
}

/// 健康 completed 的幂等键。
pub fn habit_source_id(template_id: &str, habit_row_id: i64) -> String {
    format!("habit:{template_id}:{habit_row_id}")
}

/// 按模板计日上限时的 source_id 前缀。
pub fn habit_source_prefix(template_id: &str) -> String {
    format!("habit:{template_id}:")
}

/// 闪卡答对的幂等键。
pub fn wordgame_source_id(
    lemma: &str,
    question_type: &str,
    today: &str,
    correct_today: i64,
) -> String {
    format!("wordgame:{lemma}:{question_type}:{today}:{correct_today}")
}

/// 插件游戏完成的幂等键。
///
/// Business Logic（为什么需要这个函数）:
///     游戏可选择 sourceId 避免重复入账；不传则每次新 UUID，满足完全信任。
///
/// Code Logic（这个函数做什么）:
///     有非空 client source → `game-plugin:<id>:<source>`；否则带 UUID。
pub fn game_plugin_source_id(game_id: &str, client_source: Option<&str>) -> String {
    match client_source {
        Some(s) if !s.trim().is_empty() => format!("game-plugin:{game_id}:{}", s.trim()),
        _ => format!("game-plugin:{game_id}:{}", uuid::Uuid::new_v4()),
    }
}

/// 每日 8 点惰性重置：余额置为当前满值，与当前模式无关。
///
/// Business Logic（为什么需要这个函数）:
///     用户每天早晨应拿到满额工作余额，用不完也不累积；应用可能错过 8 点，
///     重置必须在其后任意入口调用时补账，且同一周期内不得重复触发。
///
/// Code Logic（这个函数做什么）:
///     `evaluate_daily_reset` 判定到期后把 `remaining_ms` 置为 `max_balance_minutes`
///     满值、推进 `last_daily_reset_at` 到当前周期边界并 upsert；余额实际变化时记
///     `daily_reset` 流水（source_id=`daily_reset:<boundary>` 幂等，并发双触发只落一条）。
async fn apply_daily_reset_if_due(
    repo: &BatteryRepo,
    config: &BatteryConfig,
    mut state: BatteryStateRow,
    now: i64,
) -> Result<BatteryStateRow, AppError> {
    let Some(boundary) = evaluate_daily_reset(state.last_daily_reset_at, now) else {
        return Ok(state);
    };
    let max_ms = config.max_balance_minutes.saturating_mul(MS_PER_MINUTE);
    let delta = max_ms - state.remaining_ms;
    state.remaining_ms = max_ms;
    state.last_daily_reset_at = boundary;
    state.updated_at = now;
    repo.upsert_state(&state).await?;
    if delta != 0 {
        let _ = repo
            .insert_ledger(&BatteryLedgerRow {
                id: 0,
                ts: now,
                kind: "daily_reset".into(),
                source_id: Some(format!("daily_reset:{boundary}")),
                delta_ms: delta,
                balance_after_ms: max_ms,
                note: None,
            })
            .await?;
    }
    Ok(state)
}

async fn snapshot_from(
    repo: &BatteryRepo,
    config: &BatteryConfig,
    state: &BatteryStateRow,
    now: i64,
    consuming: bool,
    credit_minutes: Option<i64>,
    credit_source: Option<String>,
) -> Result<BatterySnapshotDto, AppError> {
    let (day_start, day_end) = local_day_bounds(now);
    let (today_earned_ms, today_spent_ms) = repo.today_totals(day_start, day_end).await?;
    Ok(BatterySnapshotDto {
        mode: state.mode.clone(),
        remaining_ms: state.remaining_ms,
        max_balance_ms: config.max_balance_minutes.saturating_mul(MS_PER_MINUTE),
        today_earned_ms,
        today_spent_ms,
        consuming,
        credit_minutes,
        credit_source,
    })
}

/// 读取快照；必要时初始化默认行。
pub async fn get_snapshot(
    repo: &BatteryRepo,
    config: &BatteryConfig,
    now: i64,
) -> Result<BatterySnapshotDto, AppError> {
    let state = repo.ensure_default_state(now).await?;
    let state = apply_daily_reset_if_due(repo, config, state, now).await?;
    let consuming = !drain_runtime().consuming.is_empty() && state.mode == "charging";
    snapshot_from(repo, config, &state, now, consuming, None, None).await
}

/// 切换模式。不再发放首次充电欢迎赠送。
pub async fn set_mode(
    repo: &BatteryRepo,
    config: &BatteryConfig,
    mode: &str,
    now: i64,
) -> Result<BatterySnapshotDto, AppError> {
    if mode != "charging" && mode != "unlimited" {
        return Err(AppError::validation(
            "battery.mode 只能是 charging 或 unlimited",
        ));
    }
    let state = repo.ensure_default_state(now).await?;
    let mut state = apply_daily_reset_if_due(repo, config, state, now).await?;
    if state.mode == mode {
        let consuming = !drain_runtime().consuming.is_empty() && state.mode == "charging";
        return snapshot_from(repo, config, &state, now, consuming, None, None).await;
    }
    state.mode = mode.to_string();
    state.updated_at = now;
    repo.upsert_state(&state).await?;
    let _ = repo
        .insert_ledger(&BatteryLedgerRow {
            id: 0,
            ts: now,
            kind: "mode_change".into(),
            source_id: Some(format!("mode:{mode}:{now}")),
            delta_ms: 0,
            balance_after_ms: state.remaining_ms,
            note: Some(mode.into()),
        })
        .await?;
    if mode != "charging" {
        drain_runtime().last_settle_ms = None;
    }
    let consuming = !drain_runtime().consuming.is_empty() && state.mode == "charging";
    snapshot_from(repo, config, &state, now, consuming, None, None).await
}

/// 入账。source_id 已存在则返回当前快照且不改余额。
pub async fn credit(
    repo: &BatteryRepo,
    config: &BatteryConfig,
    source: BatteryCreditSource,
    source_id: &str,
    now: i64,
) -> Result<BatterySnapshotDto, AppError> {
    let state = repo.ensure_default_state(now).await?;
    let mut state = apply_daily_reset_if_due(repo, config, state, now).await?;
    let (day_start, day_end) = local_day_bounds(now);
    let today_count = repo
        .count_credits_today(
            source_kind(source),
            source_prefix(source),
            day_start,
            day_end,
        )
        .await?;
    let delta = credit_delta_ms(config, source, today_count, state.remaining_ms);
    if delta <= 0 {
        let consuming = !drain_runtime().consuming.is_empty() && state.mode == "charging";
        return snapshot_from(repo, config, &state, now, consuming, None, None).await;
    }
    let next_remaining = state.remaining_ms.saturating_add(delta);
    let inserted = repo
        .insert_ledger(&BatteryLedgerRow {
            id: 0,
            ts: now,
            kind: source_kind(source).into(),
            source_id: Some(source_id.to_string()),
            delta_ms: delta,
            balance_after_ms: next_remaining,
            note: Some(source_token(source).into()),
        })
        .await?;
    if inserted {
        state.remaining_ms = next_remaining;
        state.updated_at = now;
        repo.upsert_state(&state).await?;
    } else {
        state = repo
            .get_state()
            .await?
            .ok_or_else(|| AppError::generic("battery_state 丢失"))?;
    }
    let consuming = !drain_runtime().consuming.is_empty() && state.mode == "charging";
    snapshot_from(
        repo,
        config,
        &state,
        now,
        consuming,
        inserted.then_some(delta / MS_PER_MINUTE),
        inserted.then(|| source_token(source).into()),
    )
    .await
}

/// 按显式分钟入账（插件游戏）。不查日次数。
///
/// Business Logic（为什么需要这个函数）:
///     插件奖励以清单为准，产品要求无日上限；source_id 已存在则不改余额。
///
/// Code Logic（这个函数做什么）:
///     日重置后用 credit_delta_ms_explicit；其余与 credit 相同。
pub async fn credit_explicit(
    repo: &BatteryRepo,
    config: &BatteryConfig,
    source: BatteryCreditSource,
    source_id: &str,
    minutes: i64,
    now: i64,
) -> Result<BatterySnapshotDto, AppError> {
    let state = repo.ensure_default_state(now).await?;
    let mut state = apply_daily_reset_if_due(repo, config, state, now).await?;
    let delta = credit_delta_ms_explicit(config, state.remaining_ms, minutes);
    if delta <= 0 {
        let consuming = !drain_runtime().consuming.is_empty() && state.mode == "charging";
        return snapshot_from(repo, config, &state, now, consuming, None, None).await;
    }
    let next_remaining = state.remaining_ms.saturating_add(delta);
    let inserted = repo
        .insert_ledger(&BatteryLedgerRow {
            id: 0,
            ts: now,
            kind: source_kind(source).into(),
            source_id: Some(source_id.to_string()),
            delta_ms: delta,
            balance_after_ms: next_remaining,
            note: Some(source_token(source).into()),
        })
        .await?;
    if inserted {
        state.remaining_ms = next_remaining;
        state.updated_at = now;
        repo.upsert_state(&state).await?;
    } else {
        state = repo
            .get_state()
            .await?
            .ok_or_else(|| AppError::generic("battery_state 丢失"))?;
    }
    let consuming = !drain_runtime().consuming.is_empty() && state.mode == "charging";
    snapshot_from(
        repo,
        config,
        &state,
        now,
        consuming,
        inserted.then_some(delta / MS_PER_MINUTE),
        inserted.then(|| source_token(source).into()),
    )
    .await
}

/// 健康模板入账：按模板分钟与日上限，幂等键 `habit:{template_id}:{row}`。
///
/// Business Logic（为什么需要这个函数）:
///     每条健康提醒自己的额度与日上限不能再挤进 water/rest/kegel/custom 四个全局桶。
///
/// Code Logic（这个函数做什么）:
///     日重置后按 `habit:{template_id}:` 计今日次数；达 cap 则不入账，否则走 `credit_explicit`。
pub async fn credit_health_habit(
    repo: &BatteryRepo,
    config: &BatteryConfig,
    source: BatteryCreditSource,
    template_id: &str,
    habit_row_id: i64,
    minutes: i64,
    daily_cap: i64,
    now: i64,
) -> Result<BatterySnapshotDto, AppError> {
    let state = repo.ensure_default_state(now).await?;
    let state = apply_daily_reset_if_due(repo, config, state, now).await?;
    let (day_start, day_end) = local_day_bounds(now);
    let prefix = habit_source_prefix(template_id);
    let today_count = repo
        .count_credits_today("credit_health", &prefix, day_start, day_end)
        .await?;
    if daily_cap >= 0 && today_count >= daily_cap {
        let consuming = !drain_runtime().consuming.is_empty() && state.mode == "charging";
        return snapshot_from(repo, config, &state, now, consuming, None, None).await;
    }
    credit_explicit(
        repo,
        config,
        source,
        &habit_source_id(template_id, habit_row_id),
        minutes,
        now,
    )
    .await
}

/// 上报一扇窗是否消耗，并按墙钟结算一份扣时。
pub async fn report_focus(
    repo: &BatteryRepo,
    config: &BatteryConfig,
    window_label: &str,
    consuming: bool,
    now_ms: i64,
) -> Result<BatterySnapshotDto, AppError> {
    let now_s = now_ms / 1000;
    let state = repo.ensure_default_state(now_s).await?;
    let mut state = apply_daily_reset_if_due(repo, config, state, now_s).await?;
    let elapsed = {
        let mut rt = drain_runtime();
        let any_before = !rt.consuming.is_empty();
        if consuming {
            rt.consuming.insert(window_label.to_string());
        } else {
            rt.consuming.remove(window_label);
        }
        let any_after = !rt.consuming.is_empty();
        let elapsed = if state.mode == "charging" && any_before {
            rt.last_settle_ms
                .map(|prev| (now_ms - prev).max(0))
                .unwrap_or(0)
        } else {
            0
        };
        if any_after && state.mode == "charging" {
            rt.last_settle_ms = Some(now_ms);
        } else {
            rt.last_settle_ms = None;
        }
        elapsed
    };
    let debit = debit_delta_ms(
        state.mode == "charging",
        elapsed > 0,
        state.remaining_ms,
        elapsed,
    );
    if debit > 0 {
        state.remaining_ms -= debit;
        state.updated_at = now_s;
        repo.upsert_state(&state).await?;
        let _ = repo
            .insert_ledger(&BatteryLedgerRow {
                id: 0,
                ts: now_s,
                kind: "debit_tick".into(),
                source_id: None,
                delta_ms: -debit,
                balance_after_ms: state.remaining_ms,
                note: None,
            })
            .await?;
    }
    let consuming_now = !drain_runtime().consuming.is_empty() && state.mode == "charging";
    snapshot_from(repo, config, &state, now_s, consuming_now, None, None).await
}

/// 列出流水。
pub async fn list_ledger(
    repo: &BatteryRepo,
    limit: i64,
) -> Result<Vec<HashMap<String, serde_json::Value>>, AppError> {
    let rows = repo.list_ledger(limit).await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let mut m = HashMap::new();
            m.insert("id".into(), serde_json::json!(r.id));
            m.insert("ts".into(), serde_json::json!(r.ts));
            m.insert("kind".into(), serde_json::json!(r.kind));
            m.insert("sourceId".into(), serde_json::json!(r.source_id));
            m.insert("deltaMs".into(), serde_json::json!(r.delta_ms));
            m.insert(
                "balanceAfterMs".into(),
                serde_json::json!(r.balance_after_ms),
            );
            m.insert("note".into(), serde_json::json!(r.note));
            m
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn repo() -> BatteryRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        BatteryRepo::ensure_schema(&pool).await.unwrap();
        BatteryRepo::new(pool)
    }

    /// 进程内 DRAIN 是全局单例；并行测试互相 reset 会把 elapsed 算错。
    /// 用 tokio Mutex：guard 需要跨测试内的 await 持有（clippy await_holding_lock）。
    static SERVICE_TEST_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

    async fn lock_drain_for_test() -> tokio::sync::MutexGuard<'static, ()> {
        let guard = SERVICE_TEST_LOCK.lock().await;
        let mut rt = drain_runtime();
        rt.consuming.clear();
        rt.last_settle_ms = None;
        guard
    }

    #[tokio::test]
    async fn first_charging_does_not_grant_welcome() {
        // 欢迎赠送已下线：切 charging 只切模式，不另入账。
        let _lock = lock_drain_for_test().await;
        let repo = repo().await;
        let cfg = BatteryConfig::default();
        let full = cfg.max_balance_minutes * MS_PER_MINUTE;
        let snap = set_mode(&repo, &cfg, "charging", 1_700_000_000)
            .await
            .unwrap();
        assert_eq!(snap.mode, "charging");
        assert_eq!(snap.remaining_ms, full);
        assert_eq!(snap.credit_minutes, None);
        let again = set_mode(&repo, &cfg, "unlimited", 1_700_000_010)
            .await
            .unwrap();
        assert_eq!(again.remaining_ms, full);
        let back = set_mode(&repo, &cfg, "charging", 1_700_000_020)
            .await
            .unwrap();
        assert_eq!(back.remaining_ms, full);
        assert_eq!(back.credit_minutes, None);
    }

    #[tokio::test]
    async fn duplicate_habit_source_does_not_double_credit() {
        let _lock = lock_drain_for_test().await;
        let repo = repo().await;
        let cfg = BatteryConfig::default();
        let now = 1_700_000_100;
        // 先把状态置为「当前周期已重置、余额已消耗」，否则入口的每日重置会先置满、credit 被钳 0。
        let mut seeded = repo.ensure_default_state(now).await.unwrap();
        seeded.mode = "charging".into();
        seeded.remaining_ms = 0;
        seeded.last_daily_reset_at = evaluate_daily_reset(0, now).unwrap();
        repo.upsert_state(&seeded).await.unwrap();
        let id = wordgame_source_id("lemma", "spell", "2026-08-16", 1);
        let a = credit(&repo, &cfg, BatteryCreditSource::Flashcard, &id, now)
            .await
            .unwrap();
        let b = credit(&repo, &cfg, BatteryCreditSource::Flashcard, &id, now + 1)
            .await
            .unwrap();
        assert_eq!(a.remaining_ms, 3 * MS_PER_MINUTE);
        assert_eq!(b.remaining_ms, 3 * MS_PER_MINUTE);
        assert_eq!(b.credit_minutes, None);
    }

    #[test]
    fn habit_source_id_uses_template_id() {
        assert_eq!(habit_source_id("water", 9), "habit:water:9");
        assert_eq!(habit_source_id("custom-foo", 3), "habit:custom-foo:3");
        assert_eq!(habit_source_prefix("custom-foo"), "habit:custom-foo:");
    }

    #[tokio::test]
    async fn credit_health_habit_caps_per_template() {
        let _lock = lock_drain_for_test().await;
        let repo = repo().await;
        let cfg = BatteryConfig::default();
        let now = 1_700_000_100;
        let mut seeded = repo.ensure_default_state(now).await.unwrap();
        seeded.mode = "charging".into();
        seeded.remaining_ms = 0;
        seeded.last_daily_reset_at = evaluate_daily_reset(0, now).unwrap();
        repo.upsert_state(&seeded).await.unwrap();

        let first = credit_health_habit(
            &repo,
            &cfg,
            BatteryCreditSource::Health,
            "custom-a",
            1,
            10,
            1,
            now,
        )
        .await
        .unwrap();
        assert_eq!(first.remaining_ms, 10 * MS_PER_MINUTE);

        let capped = credit_health_habit(
            &repo,
            &cfg,
            BatteryCreditSource::Health,
            "custom-a",
            2,
            10,
            1,
            now + 1,
        )
        .await
        .unwrap();
        assert_eq!(capped.remaining_ms, 10 * MS_PER_MINUTE);
        assert_eq!(capped.credit_minutes, None);

        let other = credit_health_habit(
            &repo,
            &cfg,
            BatteryCreditSource::Health,
            "custom-b",
            3,
            10,
            1,
            now + 2,
        )
        .await
        .unwrap();
        assert_eq!(other.remaining_ms, 20 * MS_PER_MINUTE);
    }

    #[tokio::test]
    async fn two_windows_debit_once() {
        let _lock = lock_drain_for_test().await;
        let repo = repo().await;
        let cfg = BatteryConfig::default();
        set_mode(&repo, &cfg, "charging", 1_700_010_000)
            .await
            .unwrap();
        report_focus(&repo, &cfg, "main", true, 1_700_010_000_000)
            .await
            .unwrap();
        report_focus(&repo, &cfg, "workbench-1", true, 1_700_010_000_200)
            .await
            .unwrap();
        let after = report_focus(&repo, &cfg, "main", true, 1_700_010_001_200)
            .await
            .unwrap();
        assert_eq!(
            after.remaining_ms,
            cfg.max_balance_minutes * MS_PER_MINUTE - 1_200
        );
    }

    #[tokio::test]
    async fn daily_reset_fills_balance_on_first_read() {
        // 从未重置过（last=0）→ 任意入口首次调用即置满，且与模式无关；流水幂等只落一条。
        let _lock = lock_drain_for_test().await;
        let repo = repo().await;
        let cfg = BatteryConfig::default();
        let now = 1_700_000_000;
        let snap = get_snapshot(&repo, &cfg, now).await.unwrap();
        assert_eq!(snap.mode, "unlimited");
        assert_eq!(snap.remaining_ms, cfg.max_balance_minutes * MS_PER_MINUTE);
        // daily_reset 不计入今日已充。
        assert_eq!(snap.today_earned_ms, 0);
        let ledger = repo.list_ledger(10).await.unwrap();
        let daily: Vec<_> = ledger.iter().filter(|r| r.kind == "daily_reset").collect();
        assert_eq!(daily.len(), 1);
        // 二次读取不重复置满、不再落流水。
        let again = get_snapshot(&repo, &cfg, now + 60).await.unwrap();
        assert_eq!(again.remaining_ms, cfg.max_balance_minutes * MS_PER_MINUTE);
        let ledger2 = repo.list_ledger(10).await.unwrap();
        let daily2: Vec<_> = ledger2.iter().filter(|r| r.kind == "daily_reset").collect();
        assert_eq!(daily2.len(), 1);
    }

    #[tokio::test]
    async fn daily_reset_skips_within_same_period() {
        // 同周期内（已重置到当前边界）入口调用不得重置余额。
        let _lock = lock_drain_for_test().await;
        let repo = repo().await;
        let cfg = BatteryConfig::default();
        let now = 1_700_000_000;
        let mut seeded = repo.ensure_default_state(now).await.unwrap();
        seeded.mode = "charging".into();
        seeded.remaining_ms = 60_000;
        seeded.last_daily_reset_at = evaluate_daily_reset(0, now).unwrap();
        repo.upsert_state(&seeded).await.unwrap();
        let snap = get_snapshot(&repo, &cfg, now + 60).await.unwrap();
        assert_eq!(snap.remaining_ms, 60_000);
    }

    #[tokio::test]
    async fn daily_reset_refills_after_boundary_passes() {
        // 上一周期已重置且耗尽；跨过下一个 8 点边界后，首次读取补满。
        let _lock = lock_drain_for_test().await;
        let repo = repo().await;
        let cfg = BatteryConfig::default();
        let now = 1_700_000_000;
        let boundary = evaluate_daily_reset(0, now).unwrap();
        let mut seeded = repo.ensure_default_state(now).await.unwrap();
        seeded.mode = "charging".into();
        seeded.remaining_ms = 0;
        seeded.last_daily_reset_at = boundary;
        repo.upsert_state(&seeded).await.unwrap();
        // 边界 + 1 天 + 2 小时必然已进入下一周期（含 DST 偏移缓冲）。
        let next_period = boundary + 86_400 + 7_200;
        let snap = get_snapshot(&repo, &cfg, next_period).await.unwrap();
        assert_eq!(snap.remaining_ms, cfg.max_balance_minutes * MS_PER_MINUTE);
    }
}
