//! cloud_sync/scheduler.rs — 云端同步后台轮询调度
//!
//! Business Logic（为什么需要这个模块）:
//!     用户开启"自动同步"后，应用应在后台周期性地跑 trigger_cloud_sync，无需手动点按钮。
//!     scheduler 每 tick 重读 config，实时生效 enabled / auto 开关与 interval 间隔——
//!     这样前端改配置后无需重启 scheduler。setup 无条件启动它（内部按 config 决定是否真同步）。
//!
//! Code Logic（这个模块做什么）:
//!     `start(state)` spawn 一个后台任务，loop { select!{ cancel.cancelled() => break,
//!     sleep(interval) => 重读 config，若 !enabled || !auto 则 continue，否则跑 trigger_cloud_sync } }。
//!     首次先 sleep 再检查（不立即跑，避免启动瞬间 IO 风暴）。错误仅 tracing::error 不 panic。
//!     注意 spawn 入口用 `tauri::async_runtime::spawn` 而非 `tokio::spawn`（见 start 实现处注释）。

use crate::cloud_sync::engine::trigger_cloud_sync;
use crate::state::AppState;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// 启动云端同步后台轮询任务，返回取消令牌（应用退出时 cancel）。
///
/// Business Logic: setup 无条件调用此函数启动后台任务。任务内部每 tick 重读 config，
///     按 enabled+auto 决定是否真同步、按 interval_secs 决定间隔——故配置变更无需重启 scheduler。
///
/// Code Logic:
/// 1. 创建 CancellationToken；
/// 2. tauri::async_runtime::spawn：loop select! { cancel => break; sleep(interval) => tick }；
///    （spawn 入口必须用 tauri::async_runtime::spawn，不能直接用 tokio::spawn——本函数在 lib.rs
///    setup 同步段被调用，主线程无 Tokio reactor，详见下方实现处注释）
/// 3. 每 tick：读 config，若 !enabled || !auto → continue（继续按新 interval 等下一轮）；
///    否则 trigger_cloud_sync(&state).await（错误仅 tracing::error）；
/// 4. interval 每个 tick 重新读 config（实时生效）。
pub fn start(state: AppState) -> CancellationToken {
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    // 用 `tauri::async_runtime::spawn`（非 `tokio::spawn`）：本函数在 lib.rs setup 闭包的
    // 同步段（block_on 之外）被调用，主线程无 Tokio reactor，`tokio::spawn` 会 panic
    // "there is no reactor running"。`tauri::async_runtime::spawn` 走 Tauri 全局 runtime handle，
    // 不依赖当前线程上下文（与 cc/collector.rs / health/mod.rs 的 spawn 范式一致）。
    // 任务内部的 tokio::time::sleep / tokio::select! / CancellationToken 不受影响——它们运行在
    // spawn 出的 task 里，由 Tauri async runtime 的 worker thread 提供 reactor。
    tauri::async_runtime::spawn(async move {
        tracing::info!("cloud_sync scheduler 已启动");
        loop {
            // 每个 tick 重新读 interval（实时生效配置变更）
            let interval_secs = current_interval(&state);
            let interval = Duration::from_secs(interval_secs.max(1));

            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    tracing::info!("cloud_sync scheduler 已停止");
                    break;
                }
                _ = tokio::time::sleep(interval) => {
                    // tick：重读开关
                    let (enabled, auto) = current_flags(&state);
                    if !enabled || !auto {
                        // 未启用 / 未开自动 → 跳过本轮（仍按 interval 继续等待）
                        continue;
                    }
                    // 跑一次同步（错误仅记录，不中断 scheduler）
                    let result = trigger_cloud_sync(&state).await;
                    if !result.ok {
                        tracing::error!("cloud_sync scheduler 本轮同步失败: {}", result.note);
                    } else {
                        tracing::info!("cloud_sync scheduler 本轮同步: {}", result.note);
                    }
                }
            }
        }
    });

    cancel
}

/// 读取当前 config 的 interval_secs（scheduler 用）。
fn current_interval(state: &AppState) -> u64 {
    state
        .config
        .read()
        .expect("config 读锁中毒")
        .cloud_sync_interval_secs
}

/// 读取当前 config 的 enabled / auto 开关。
fn current_flags(state: &AppState) -> (bool, bool) {
    let cfg = state.config.read().expect("config 读锁中毒");
    (cfg.cloud_sync_enabled, cfg.cloud_sync_auto)
}
