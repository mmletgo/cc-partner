//! 充电账本服务：模式、入账、多窗 1× 扣时。
//!
//! Business Logic（为什么需要这个模块）:
//!     命令层与健康/闪卡挂钩只应调用一组权威操作，避免各入口自己改余额。
//!
//! Code Logic（这个模块做什么）:
//!     组合 BatteryRepo + BatteryConfig + 进程内消耗窗集合。

use super::policy::{credit_delta_ms, debit_delta_ms, MS_PER_MINUTE};
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
        _ => "credit_health",
    }
}

fn source_prefix(source: BatteryCreditSource) -> &'static str {
    match source {
        BatteryCreditSource::Water => "habit:water:",
        BatteryCreditSource::Rest => "habit:rest:",
        BatteryCreditSource::Kegel => "habit:kegel:",
        BatteryCreditSource::Custom => "habit:custom:",
        BatteryCreditSource::Flashcard => "wordgame:",
    }
}

fn source_token(source: BatteryCreditSource) -> &'static str {
    match source {
        BatteryCreditSource::Water => "water",
        BatteryCreditSource::Rest => "rest",
        BatteryCreditSource::Kegel => "kegel",
        BatteryCreditSource::Custom => "custom",
        BatteryCreditSource::Flashcard => "flashcard",
    }
}

/// 健康 completed 的幂等键。
pub fn habit_source_id(template_id: &str, habit_row_id: i64) -> String {
    let bucket = match BatteryCreditSource::from_health_template_id(template_id) {
        BatteryCreditSource::Water => "water",
        BatteryCreditSource::Rest => "rest",
        BatteryCreditSource::Kegel => "kegel",
        _ => "custom",
    };
    format!("habit:{bucket}:{habit_row_id}")
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
    let consuming = !drain_runtime().consuming.is_empty() && state.mode == "charging";
    snapshot_from(repo, config, &state, now, consuming, None, None).await
}

/// 切换模式；首次进入充电且未赠送则入账欢迎分钟。
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
    let mut state = repo.ensure_default_state(now).await?;
    if state.mode == mode {
        let consuming = !drain_runtime().consuming.is_empty() && state.mode == "charging";
        return snapshot_from(repo, config, &state, now, consuming, None, None).await;
    }
    state.mode = mode.to_string();
    state.updated_at = now;
    let mut credit_minutes = None;
    if mode == "charging" && !state.welcome_granted && config.welcome_grant_minutes > 0 {
        let grant = config
            .welcome_grant_minutes
            .saturating_mul(MS_PER_MINUTE)
            .min(config.max_balance_minutes.saturating_mul(MS_PER_MINUTE) - state.remaining_ms)
            .max(0);
        if grant > 0 {
            state.remaining_ms += grant;
            state.welcome_granted = true;
            let inserted = repo
                .insert_ledger(&BatteryLedgerRow {
                    id: 0,
                    ts: now,
                    kind: "credit_welcome".into(),
                    source_id: Some("welcome".into()),
                    delta_ms: grant,
                    balance_after_ms: state.remaining_ms,
                    note: None,
                })
                .await?;
            if inserted {
                credit_minutes = Some(grant / MS_PER_MINUTE);
            }
        } else {
            state.welcome_granted = true;
        }
    }
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
    snapshot_from(
        repo,
        config,
        &state,
        now,
        consuming,
        credit_minutes,
        credit_minutes.map(|_| "welcome".into()),
    )
    .await
}

/// 入账。source_id 已存在则返回当前快照且不改余额。
pub async fn credit(
    repo: &BatteryRepo,
    config: &BatteryConfig,
    source: BatteryCreditSource,
    source_id: &str,
    now: i64,
) -> Result<BatterySnapshotDto, AppError> {
    let mut state = repo.ensure_default_state(now).await?;
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

/// 上报一扇窗是否消耗，并按墙钟结算一份扣时。
pub async fn report_focus(
    repo: &BatteryRepo,
    config: &BatteryConfig,
    window_label: &str,
    consuming: bool,
    now_ms: i64,
) -> Result<BatterySnapshotDto, AppError> {
    let now_s = now_ms / 1000;
    let mut state = repo.ensure_default_state(now_s).await?;
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
    static SERVICE_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_drain_for_test() -> std::sync::MutexGuard<'static, ()> {
        let guard = SERVICE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut rt = drain_runtime();
        rt.consuming.clear();
        rt.last_settle_ms = None;
        guard
    }

    #[tokio::test]
    async fn first_charging_grants_welcome_once() {
        let _lock = lock_drain_for_test();
        let repo = repo().await;
        let cfg = BatteryConfig::default();
        let snap = set_mode(&repo, &cfg, "charging", 1_700_000_000)
            .await
            .unwrap();
        assert_eq!(snap.mode, "charging");
        assert_eq!(snap.remaining_ms, 25 * MS_PER_MINUTE);
        assert_eq!(snap.credit_minutes, Some(25));
        let again = set_mode(&repo, &cfg, "unlimited", 1_700_000_010)
            .await
            .unwrap();
        assert_eq!(again.remaining_ms, 25 * MS_PER_MINUTE);
        let back = set_mode(&repo, &cfg, "charging", 1_700_000_020)
            .await
            .unwrap();
        assert_eq!(back.remaining_ms, 25 * MS_PER_MINUTE);
        assert_eq!(back.credit_minutes, None);
    }

    #[tokio::test]
    async fn duplicate_habit_source_does_not_double_credit() {
        let _lock = lock_drain_for_test();
        let repo = repo().await;
        let cfg = BatteryConfig::default();
        let id = habit_source_id("water", 9);
        let a = credit(&repo, &cfg, BatteryCreditSource::Water, &id, 1_700_000_100)
            .await
            .unwrap();
        let b = credit(&repo, &cfg, BatteryCreditSource::Water, &id, 1_700_000_101)
            .await
            .unwrap();
        assert_eq!(a.remaining_ms, 8 * MS_PER_MINUTE);
        assert_eq!(b.remaining_ms, 8 * MS_PER_MINUTE);
        assert_eq!(b.credit_minutes, None);
    }

    #[tokio::test]
    async fn two_windows_debit_once() {
        let _lock = lock_drain_for_test();
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
        assert_eq!(after.remaining_ms, 25 * MS_PER_MINUTE - 1_200);
    }
}
