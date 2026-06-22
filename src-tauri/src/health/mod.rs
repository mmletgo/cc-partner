//! 健康提醒模块:键鼠监测 + 工作/休息状态机 + 提醒触发。
//!
//! 子模块:
//! - `state`:工作/休息状态机(纯算法)
//! - `monitor`:键鼠采样(跨平台)
//! - `reminder`:提醒生命周期 + 免打扰
//! - daemon 入口 `start_health_daemon`(本文件)

pub mod monitor;
pub mod reminder;
pub mod state;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::AppError;
use crate::state::AppState;

use self::monitor::{ActivitySample, ActivitySampler, DeviceQuerySampler};
use self::reminder::is_in_dnd;
use self::state::{HealthStateMachine, HealthThresholds};

/// 健康监测运行时共享状态(跨 daemon task 与命令层)。
pub struct HealthRuntime {
    /// 工作/休息状态机(每分钟由 daemon 推进一拍;命令层也可读取展示当前相位)。
    pub machine: Mutex<HealthStateMachine>,
    /// 贪睡(手动暂停提醒)到期时间戳(秒);None 或 <= now 表示未贪睡。
    pub snooze_until: Mutex<Option<i64>>,
    /// 是否整体暂停监测(paused 状态由命令层置位,daemon 采样时据此跳过提醒)。
    pub paused: AtomicBool,
}
impl HealthRuntime {
    /// Business Logic: daemon 与命令层(前端「暂停/贪睡」按钮)需要共享同一份
    ///                  状态机/贪睡/暂停标记,该构造产出初始全空闲的运行时。
    /// Code Logic: 新建 Idle 初态状态机,贪睡置 None,暂停置 false。
    pub fn new() -> Self {
        Self {
            machine: Mutex::new(HealthStateMachine::new()),
            snooze_until: Mutex::new(None),
            paused: AtomicBool::new(false),
        }
    }
}

/// 启动健康监测后台 daemon。返回 `CancellationToken`,供应用退出时取消。
///
/// 一个 `std::thread` 采样(线程局部持有非 Send 的 `DeviceState`)
/// + 一个 tokio task 处理(写库 + 推进状态机 + emit 提醒)。
///
/// 架构:复用 `cc/collector.rs` 的 `select!{cancel, rx.recv()}` 范式——
/// 采样放原生线程(持有非 Send 的设备句柄),跨线程只传 `ActivitySample`(Send 纯数据)。
pub fn start_health_daemon(app: AppHandle, state: std::sync::Arc<AppState>) -> CancellationToken {
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel::<ActivitySample>(8);

    // 采样线程(线程局部持有 sampler,无需 Send)
    let cancel_s = cancel.clone();
    std::thread::spawn(move || {
        let mut sampler = DeviceQuerySampler::new();
        loop {
            if cancel_s.is_cancelled() {
                break;
            }
            let sample = sampler.sample();
            // 处理 task 被取消/退出后 rx 端关闭,blocking_send 返回 Err → 退出采样线程。
            if tx.blocking_send(sample).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_secs(60));
        }
    });

    // 处理 task:消费 ActivitySample,写库 → 推进状态机 → 满足条件 emit。
    let app_h = app.clone();
    let state_h = state.clone();
    let cancel_h = cancel.clone();
    // 用 `tauri::async_runtime::spawn`（非 `tokio::spawn`）：本函数在 lib.rs setup 闭包的
    // 同步段（block_on 之外）被调用，主线程无 Tokio reactor，`tokio::spawn` 会 panic
    // "there is no reactor running"；走 Tauri 全局 runtime handle 不依赖当前线程上下文
    // （与 cc/collector.rs / commands/updater.rs 的 spawn 范式一致）。
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel_h.cancelled() => break,
                Some(sample) = rx.recv() => {
                    if let Err(e) = handle_sample(&app_h, &state_h, sample).await {
                        tracing::warn!("健康采样处理失败: {e}");
                    }
                }
            }
        }
    });
    cancel
}

/// 处理一次采样:写库 → 推进状态机 → 满足条件 emit `health:reminder`。
///
/// 跨 await 不持 RwLockReadGuard:开头 `state.config.read().unwrap().health.clone()`
/// 先 clone 出配置副本并释放读锁,后续 await 安全。
async fn handle_sample(app: &AppHandle, state: &AppState, sample: ActivitySample) -> Result<(), AppError> {
    let cfg = state.config.read().unwrap().health.clone();
    let now = Utc::now().timestamp();
    // 对齐到分钟桶(同分钟重采覆盖),取该分钟起始时间戳。
    let minute_ts = now - now.rem_euclid(60);
    let active_for_reminder = cfg.enabled && !state.health.paused.load(Ordering::Relaxed);

    // 写活动记录(record_window_title=false 时不记标题,降级到只记进程名/活跃态)
    let rec = crate::storage::health_repo::ActivityRecord {
        ts: minute_ts,
        is_active: sample.is_active,
        process_name: sample.process_name.clone(),
        window_title: if cfg.record_window_title { sample.window_title.clone() } else { None },
    };
    state.health_repo.insert_activity(&rec).await?;

    // 未启用 / 已暂停:仅写库不触发提醒。
    if !active_for_reminder {
        return Ok(());
    }

    // 推进状态机(持锁区间内不 await,advance 是纯 CPU 计算)
    let thresholds = HealthThresholds {
        work_window_seconds: cfg.work_window_seconds,
        break_seconds: cfg.break_seconds,
    };
    let should_remind = {
        let mut m = state.health.machine.lock().unwrap();
        m.advance(sample.is_active, now, &thresholds).should_remind
    };

    if should_remind {
        // 贪睡未到期则静默;免打扰时段静默;notify_enabled 关闭则不 emit。
        let snoozed = state.health.snooze_until.lock().unwrap().is_some_and(|t| t > now);
        let dnd = is_in_dnd(now, cfg.dnd_start.as_deref(), cfg.dnd_end.as_deref());
        if !snoozed && !dnd && cfg.notify_enabled {
            // 仅 emit 事件载荷;系统通知由前端监听后弹出(文案走 i18n)。
            let _ = app.emit("health:reminder", serde_json::json!({ "workWindowSeconds": cfg.work_window_seconds }));
        }
    }

    // 数据清理(DELETE 幂等,成本低;每次跑可优化为跨天清理)
    let cutoff = now - cfg.retain_days * 86400;
    if let Err(e) = state.health_repo.cleanup_older_than(cutoff).await {
        tracing::warn!("活动记录清理失败: {e}");
    }
    Ok(())
}
