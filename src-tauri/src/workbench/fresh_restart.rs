//! workbench/fresh_restart.rs — 设备级「全新启动连接」。
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench 终端环境被三层固化——backend 进程环境冻结在启动它的那次登录、
//!     tmux server 环境冻结在首次 `new-session`（exit-empty off 常驻）、pane shell 非 login。
//!     系统级变更（rc/profile、usermod -aG、limits.conf）在终端里永不生效，用户被迫
//!     手动重连 SSH + 重启 backend + kill tmux server。本模块提供设备级一键动作：
//!     关闭该设备全部工作台终端会话，并以全新 PAM 登录环境重启 tmux server。
//!
//! Code Logic（这个模块做什么）:
//!     两阶段 API：`preview`（只读枚举 + loopback ssh 可用性探测）与
//!     `run`（逐会话关闭 → kill-server 决策 → loopback ssh 引导新 server）。
//!     引导通道仅在"全新启动"那一刻用一次 `ssh <user>@127.0.0.1`（sshd 完整走
//!     PAM 提供新组/limits，`sh -lc` 再补 login profile），之后终端体验不变。
//!     探测/引导失败时如实降级：结果 DTO 标注 degraded 原因并附可复制手动命令
//!     （`cc-partner-backend workbench-fresh`，用户在任意新 SSH 登录里执行），
//!     绝不以 backend 旧环境 fallback start-server 伪装成功。
//!     进程级设备 barrier（双闸点见 sessions.rs）挡住 kill/引导窗口内一切
//!     会触发 `ensure_workbench_tmux_server` 的路径，防止旧环境 server 抢跑。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::dependencies::{available_tmux_command, TmuxCommand};
use crate::workbench::models::WorkbenchSessionRow;
use crate::workbench::sessions::{kill_persisted_backend, TMUX_BACKEND};
use crate::workbench::tmux_persist::{
    tmux_kill_server_args, tmux_list_session_names_args, tmux_start_server_args,
    workbench_tmux_persist_conf_path,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

/// 降级手动命令：用户在任意新 SSH 登录里执行即获得全新环境引导。
pub(crate) const FRESH_RESTART_MANUAL_COMMAND: &str = "cc-partner-backend workbench-fresh";

/// preview 会话清单上限（DTO 有界，防止大设备响应膨胀）。
const FRESH_RESTART_PREVIEW_SESSION_LIMIT: usize = 100;
/// preview 非工作台 session 名展示上限。
const FRESH_RESTART_PREVIEW_FOREIGN_NAME_LIMIT: usize = 20;
/// 结果 DTO 中 terminated/skipped id 列表上限（计数仍为真实值）。
const FRESH_RESTART_RESULT_ID_LIMIT: usize = 100;
/// loopback ssh 连接超时（秒）。
const FRESH_RESTART_SSH_CONNECT_TIMEOUT_SECS: u64 = 5;
/// loopback ssh 引导/探测整体墙钟（探测含 ConnectTimeout 5s + 余量）。
const FRESH_RESTART_SSH_PROBE_TIMEOUT_SECS: u64 = 15;
const FRESH_RESTART_SSH_BOOTSTRAP_TIMEOUT_SECS: u64 = 20;

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

/// preview 阶段的单条会话摘要。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchFreshRestartSessionDto {
    pub session_id: String,
    pub project_id: String,
    pub name: String,
    pub backend: String,
}

/// 「全新启动连接」预检结果（只读）：将被终止的会话清单 + tmux 非工作台会话数
/// + loopback ssh 引导通道可用性。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchFreshRestartPreviewDto {
    pub sessions: Vec<WorkbenchFreshRestartSessionDto>,
    pub workbench_tmux_session_count: u32,
    pub foreign_session_count: u32,
    pub foreign_session_names: Vec<String>,
    /// None = 探测未执行或不确定；Some(false) + detail 说明不可用原因。
    pub ssh_bootstrap_available: Option<bool>,
    pub ssh_bootstrap_detail: Option<String>,
}

/// 新 tmux server 的引导方式。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FreshRestartBootstrap {
    /// loopback ssh 引导成功，server 运行于全新 PAM 登录环境。
    Ssh,
    /// ssh 引导不可用/失败，降级为手动命令指引（server 由惰性 start 兜底，环境为 backend 旧环境）。
    Manual,
    /// 未尝试引导（存在非工作台会话且用户未选择一并终止，或 close 失败跳过 kill-server）。
    Skipped,
}

/// 「全新启动连接」执行结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchFreshRestartResultDto {
    pub terminated_session_count: u32,
    pub terminated_session_ids: Vec<String>,
    /// 无法关闭的会话（如 raw PTY 缺 live handle）；保留在 SQLite 中供后续处理。
    pub skipped_session_ids: Vec<String>,
    pub server_restarted: bool,
    pub bootstrap: FreshRestartBootstrap,
    /// 稳定降级 token（见各 `fresh_restart_*` 常量）；成功引导时为 None。
    pub degraded_reason: Option<String>,
    /// 人类可读降级说明（已脱敏，不含 token/路径之外的用户数据）。
    pub degraded_detail: Option<String>,
    pub foreign_session_count: u32,
    /// 降级时非空：可复制的手动命令。
    pub manual_command: Option<String>,
}

/// loopback ssh 引导失败的稳定降级 token（仅 Windows 降级分支使用）。
#[cfg(windows)]
const DEGRADED_PLATFORM_UNSUPPORTED: &str = "fresh_restart_platform_unsupported";
const DEGRADED_SSH_UNAVAILABLE: &str = "fresh_restart_ssh_unavailable";
const DEGRADED_SSH_FAILED: &str = "fresh_restart_ssh_failed";
const DEGRADED_SSH_TIMEOUT: &str = "fresh_restart_ssh_timeout";
const DEGRADED_USER_UNKNOWN: &str = "fresh_restart_user_unknown";
const DEGRADED_FOREIGN_PRESENT: &str = "fresh_restart_foreign_present";
const DEGRADED_CLOSE_FAILED: &str = "fresh_restart_close_failed";
const DEGRADED_KILL_SERVER_FAILED: &str = "fresh_restart_kill_server_failed";

// ---------------------------------------------------------------------------
// 设备级 barrier（进程内 static + RAII）
// ---------------------------------------------------------------------------

/// 0 = 空闲；>0 = 有 fresh restart 正在执行（设备级单飞）。
static FRESH_RESTART_ACTIVE: AtomicU64 = AtomicU64::new(0);

/// Business Logic（为什么需要这个函数）:
///     kill-server 与 ssh 引导之间存在窗口，若并发 create/attach/restore 触发
///     `ensure_workbench_tmux_server`，会以 backend 旧环境抢先拉起 server，破坏
///     "全新登录环境"承诺。设备级闸必须在所有会触发 start-server 的入口前拦截。
///
/// Code Logic（这个函数做什么）:
///     CAS 0→1；已被占用返回 `conflict("workbench_fresh_restart_already_running")`。
pub(crate) fn begin_device_fresh_restart_barrier(
) -> Result<DeviceFreshRestartBarrierGuard, AppError> {
    FRESH_RESTART_ACTIVE
        .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
        .map_err(|_| AppError::conflict("workbench_fresh_restart_already_running".to_string()))?;
    Ok(DeviceFreshRestartBarrierGuard { armed: true })
}

/// Business Logic（为什么需要这个函数）:
///     create/attach/restore 等 start-server 入口（sessions.rs 闸点 A/B）在 fresh
///     restart 执行期间必须失败而不是排队或绕过，避免旧环境 server 抢跑。
///
/// Code Logic（这个函数做什么）:
///     barrier 活跃时返回 `unavailable("workbench_fresh_restart_barrier_active")`（可重试）。
pub(crate) fn require_device_not_fresh_restarting() -> Result<(), AppError> {
    if FRESH_RESTART_ACTIVE.load(Ordering::SeqCst) != 0 {
        return Err(AppError::unavailable(
            "workbench_fresh_restart_barrier_active".to_string(),
        ));
    }
    Ok(())
}

/// 测试/诊断用：barrier 是否活跃。
pub(crate) fn is_device_fresh_restart_active() -> bool {
    FRESH_RESTART_ACTIVE.load(Ordering::SeqCst) != 0
}

/// RAII 释放 guard：Drop（含 early-return/panic 路径）自动清 barrier。
pub(crate) struct DeviceFreshRestartBarrierGuard {
    armed: bool,
}

impl Drop for DeviceFreshRestartBarrierGuard {
    fn drop(&mut self) {
        if self.armed {
            FRESH_RESTART_ACTIVE.store(0, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
pub(crate) fn reset_device_fresh_restart_barrier_for_test() {
    FRESH_RESTART_ACTIVE.store(0, Ordering::SeqCst);
}

/// 并行测试下所有 begin/占用 barrier 的测试共享此锁，防止进程级 static 串扰。
#[cfg(test)]
pub(crate) static BARRIER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ---------------------------------------------------------------------------
// 纯决策函数
// ---------------------------------------------------------------------------

/// Business Logic（为什么需要这个函数）:
///     tmux server 上可能同时挂着用户自己的非工作台会话，kill-server 会一并终止；
///     必须先把"哪些 session 属于 cc-partner 工作台"与"哪些是用户的"分开。
///
/// Code Logic（这个函数做什么）:
///     以 SQLite `workbench_sessions.backend_id` 权威集合为准：在集合内的归 owned，
///     不在集合内的归 foreign；tmux_names 顺序保留（展示稳定）。
pub(crate) fn classify_tmux_sessions(
    workbench_names: &HashSet<String>,
    tmux_names: Vec<String>,
) -> (Vec<String>, Vec<String>) {
    let mut owned = Vec::new();
    let mut foreign = Vec::new();
    for name in tmux_names {
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        if workbench_names.contains(&name) {
            owned.push(name);
        } else {
            foreign.push(name);
        }
    }
    (owned, foreign)
}

/// kill-server 决策结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FreshRestartServerDecision {
    /// 无外部会话（或用户显式同意一并终止）且全部 close 成功 → kill-server + ssh 引导。
    KillServer,
    /// 存在用户非工作台会话且未同意终止 → 只关工作台会话，server 不重启（group/limits 类不生效）。
    SkipForeign,
    /// 有会话关闭失败 → fail-closed 不动 server（保留现场供排查）。
    SkipCloseFailed,
}

/// Business Logic（为什么需要这个函数）:
///     换 tmux server 环境必须 kill-server，但那只在该设备 tmux 上没有用户自己的
///     会话（或用户显式同意）且工作台会话全部干净关闭时才安全；close 失败时盲杀
///     server 会留下"元数据已删、tmux 仍活"的孤儿。
///
/// Code Logic（这个函数做什么）:
///     close_failures > 0 → SkipCloseFailed；foreign_count == 0 || include_foreign →
///     KillServer；否则 SkipForeign。
pub(crate) fn decide_server_restart(
    close_failures: usize,
    foreign_count: usize,
    include_foreign: bool,
) -> FreshRestartServerDecision {
    if close_failures > 0 {
        return FreshRestartServerDecision::SkipCloseFailed;
    }
    if foreign_count == 0 || include_foreign {
        return FreshRestartServerDecision::KillServer;
    }
    FreshRestartServerDecision::SkipForeign
}

/// POSIX 单引号转义：把任意字符串安全包进 `'...'`（`'` → `'\''`）。
pub(crate) fn shell_single_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Business Logic（为什么需要这个函数）:
///     loopback ssh 引导是新 tmux server 环境的唯一来源；argv 构造必须可脱离真实
///     ssh/tmux 做单元测试（BatchMode/ConnectTimeout/host、`sh -lc` login 包装、
///     TMUX_TMPDIR 防御 GUI 与 SSH 会话 TMPDIR 漂移连到不同 server）。
///
/// Code Logic（这个函数做什么）:
///     生成 `ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new
///     <user>@127.0.0.1 <remote>`；remote = `exec /bin/sh -lc '<inner>'`（ssh host cmd
///     非登录 shell，`-lc` 补 login profile；组/limits 由 ssh 连接本身的 PAM 会话赋予）；
///     inner = 可选 `TMUX_TMPDIR='<v>'` 前缀 + `exec <tmux program> -f <conf> start-server`
///     （各路径单引号转义；TMUX_TMPDIR 空值不注入——tmux 会把空串当作已设置）。
pub(crate) fn build_loopback_ssh_bootstrap_argv(
    user: &str,
    tmux: &TmuxCommand,
    persist_conf: &str,
    tmux_tmpdir: Option<&str>,
) -> Vec<String> {
    let start_args = tmux_start_server_args(&tmux.prefix_args, Some(persist_conf));
    let mut inner = String::from("exec ");
    inner.push_str(&shell_single_quote(&tmux.program));
    for arg in &start_args[tmux.prefix_args.len()..] {
        inner.push(' ');
        inner.push_str(&shell_single_quote(arg));
    }
    if let Some(tmpdir) = tmux_tmpdir.filter(|value| !value.is_empty()) {
        inner = format!("TMUX_TMPDIR={} {}", shell_single_quote(tmpdir), inner);
    }
    let remote = format!("exec /bin/sh -lc {}", shell_single_quote(&inner));
    vec![
        "ssh".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        format!("ConnectTimeout={FRESH_RESTART_SSH_CONNECT_TIMEOUT_SECS}"),
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
        format!("{user}@127.0.0.1"),
        remote,
    ]
}

/// loopback ssh 引导失败的分类（映射稳定降级 token）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FreshRestartDegraded {
    pub reason: &'static str,
    pub detail: String,
}

// ---------------------------------------------------------------------------
// ssh 用户解析 / 探测 / 引导执行
// ---------------------------------------------------------------------------

/// Business Logic（为什么需要这个函数）:
///     loopback ssh 需要显式用户名（GUI/sidecar 进程环境可能缺 USER）。
///
/// Code Logic（这个函数做什么）:
///     USER → LOGNAME → 有界 `id -un`（2s）依次回落；全部失败返回 None。
async fn resolve_loopback_ssh_user() -> Option<String> {
    for key in ["USER", "LOGNAME"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::process::Command::new("id").arg("-un").output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// 有界输出截断（ssh stderr 摘要进 degraded_detail，防止无界膨胀；按 char 边界截断）。
fn bounded_output_text(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout_text = String::from_utf8_lossy(stdout);
    let stderr_text = String::from_utf8_lossy(stderr);
    let combined = format!("{stdout_text}{stderr_text}");
    let mut cut = combined.len().min(512);
    while cut > 0 && !combined.is_char_boundary(cut) {
        cut -= 1;
    }
    combined[..cut].trim().to_string()
}

/// Business Logic（为什么需要这个函数）:
///     preview 阶段需要把"引导通道是否可用"如实告诉用户（不可用时 UI 直接展示
///     手动命令），避免用户执行后才发现降级。
///
/// Code Logic（这个函数做什么）:
///     Windows 一律不可用；其它平台对 127.0.0.1 跑一次有界 `ssh ... true`：
///     成功 Some(true)；ssh 缺失/连接拒绝/超时/非零退出 Some(false) + 简短原因。
async fn probe_loopback_ssh_available() -> (bool, String) {
    #[cfg(windows)]
    {
        (
            false,
            "Windows/WSL owner 不支持 loopback ssh 引导".to_string(),
        )
    }
    #[cfg(not(windows))]
    {
        let Some(user) = resolve_loopback_ssh_user().await else {
            return (
                false,
                "无法确定当前运行用户（USER/LOGNAME/id -un 均失败）".to_string(),
            );
        };
        let argv = vec![
            "ssh".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            format!("ConnectTimeout={FRESH_RESTART_SSH_CONNECT_TIMEOUT_SECS}"),
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
            format!("{user}@127.0.0.1"),
            "true".to_string(),
        ];
        match run_bounded_ssh(&argv, FRESH_RESTART_SSH_PROBE_TIMEOUT_SECS).await {
            Ok(_) => (true, String::new()),
            Err(degraded) => (false, degraded.detail),
        }
    }
}

/// 有界执行一条 ssh argv（kill_on_drop + select! 超时，镜像 dependency spawn_install 范式）。
async fn run_bounded_ssh(
    argv: &[String],
    timeout_secs: u64,
) -> Result<String, FreshRestartDegraded> {
    let Some((program, args)) = argv.split_first() else {
        return Err(FreshRestartDegraded {
            reason: DEGRADED_SSH_UNAVAILABLE,
            detail: "ssh 引导命令为空".to_string(),
        });
    };
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let child = match child {
        Ok(child) => child,
        Err(error) => {
            return Err(FreshRestartDegraded {
                reason: DEGRADED_SSH_UNAVAILABLE,
                detail: format!("无法启动 ssh（本机可能未安装或 PATH 不可见）: {error}"),
            });
        }
    };
    let output_future = child.wait_with_output();
    tokio::pin!(output_future);
    match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        &mut output_future,
    )
    .await
    {
        Err(_) => Err(FreshRestartDegraded {
            reason: DEGRADED_SSH_TIMEOUT,
            detail: format!("ssh 引导超时（{timeout_secs}s），可能未配置免密登录"),
        }),
        Ok(Err(error)) => Err(FreshRestartDegraded {
            reason: DEGRADED_SSH_FAILED,
            detail: format!("读取 ssh 结果失败: {error}"),
        }),
        Ok(Ok(output)) if output.status.success() => {
            Ok(bounded_output_text(&output.stdout, &output.stderr))
        }
        Ok(Ok(output)) => Err(FreshRestartDegraded {
            reason: DEGRADED_SSH_FAILED,
            detail: format!(
                "ssh 退出码 {}：{}",
                output.status,
                bounded_output_text(&output.stdout, &output.stderr)
            ),
        }),
    }
}

/// Business Logic（为什么需要这个函数）:
///     kill-server 后必须让新 server 由"全新登录进程链"拉起，否则下一个
///     `ensure_workbench_tmux_server` 会以 backend 旧环境重建 server，group/limits
///     类变更依然不生效。loopback ssh 是无人值守进程唯一免交互的 PAM 登录通道。
///
/// Code Logic（这个函数做什么）:
///     Windows 一律降级；解析用户 → 写 persist conf → 构造 argv（含 TMUX_TMPDIR
///     显式传递）→ 有界执行。失败返回分类降级（不 fallback 本机 start-server）。
async fn bootstrap_fresh_tmux_server_via_ssh(
    tmux: &TmuxCommand,
) -> Result<(), FreshRestartDegraded> {
    #[cfg(windows)]
    {
        let _ = tmux;
        return Err(FreshRestartDegraded {
            reason: DEGRADED_PLATFORM_UNSUPPORTED,
            detail: "Windows/WSL owner 不支持 loopback ssh 引导，请使用手动命令".to_string(),
        });
    }
    #[cfg(not(windows))]
    {
        let user = resolve_loopback_ssh_user()
            .await
            .ok_or_else(|| FreshRestartDegraded {
                reason: DEGRADED_USER_UNKNOWN,
                detail: "无法确定当前运行用户（USER/LOGNAME/id -un 均失败）".to_string(),
            })?;
        let conf = workbench_tmux_persist_conf_path()
            .map_err(|error| FreshRestartDegraded {
                reason: DEGRADED_SSH_FAILED,
                detail: format!("写入 tmux persist conf 失败: {error}"),
            })?
            .to_string_lossy()
            .to_string();
        let tmux_tmpdir = std::env::var("TMUX_TMPDIR").ok();
        let argv = build_loopback_ssh_bootstrap_argv(&user, tmux, &conf, tmux_tmpdir.as_deref());
        run_bounded_ssh(&argv, FRESH_RESTART_SSH_BOOTSTRAP_TIMEOUT_SECS)
            .await
            .map(|_| ())
    }
}

// ---------------------------------------------------------------------------
// tmux 枚举 / kill-server
// ---------------------------------------------------------------------------

/// 枚举 tmux server 上全部 session 名；无 server / tmux 不可用返回空表。
fn tmux_list_session_names(tmux: &TmuxCommand) -> Vec<String> {
    let args = tmux_list_session_names_args(&tmux.prefix_args);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut command = tmux.std_command();
    command.args(&arg_refs);
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        // 无 server 时 list-sessions 非零退出（"no server running"）→ 空表。
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// kill-server；already-gone（no server）视为成功。
fn run_tmux_kill_server(tmux: &TmuxCommand) -> Result<(), AppError> {
    let args = tmux_kill_server_args(&tmux.prefix_args);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut command = tmux.std_command();
    command.args(&arg_refs);
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if crate::workbench::sessions::tmux_destroy_exit_is_already_gone(&stdout, &stderr) {
        return Ok(());
    }
    Err(AppError::generic(format!(
        "tmux kill-server 失败: {}",
        stderr.trim()
    )))
}

// ---------------------------------------------------------------------------
// 计划
// ---------------------------------------------------------------------------

/// fresh restart 的枚举快照。
pub(crate) struct FreshRestartPlan {
    /// SQLite 全部持久化会话行（含 raw PTY）。
    pub rows: Vec<WorkbenchSessionRow>,
    /// tmux server 上属于工作台的 session 名。
    pub owned_tmux_names: Vec<String>,
    /// tmux server 上用户自己的非工作台 session 名。
    pub foreign_tmux_names: Vec<String>,
}

/// Business Logic（为什么需要这个函数）:
///     "该设备全部工作台终端"与"用户自己的 tmux 会话"必须以权威数据源区分：
///     session 名无固定前缀，SQLite `workbench_sessions` 全表是唯一权威清单；
///     tmux 侧 list-sessions 与之做差集得到非工作台会话。
///
/// Code Logic（这个函数做什么）:
///     `repo.list(None)` 全表 ∪ registry live-only id 防御性并入；tmux 侧枚举后
///     `classify_tmux_sessions` 分类。tmux 不可用时 tmux 名单为空（仍可关 raw 会话）。
async fn plan_fresh_restart(state: &AppState) -> Result<FreshRestartPlan, AppError> {
    let mut rows = state.workbench_session_repo.list(None).await?;
    let row_ids: HashSet<String> = rows.iter().map(|row| row.id.clone()).collect();
    // registry live 但 SQLite 缺行（防御）：逐个补进关闭清单。
    for live_row in state.workbench_sessions.list_live_session_rows() {
        if !row_ids.contains(&live_row.id) {
            rows.push(live_row);
        }
    }
    let workbench_names: HashSet<String> = rows
        .iter()
        .filter(|row| row.backend == TMUX_BACKEND)
        .filter_map(|row| row.backend_id.as_deref())
        .map(str::to_string)
        .collect();
    let (owned, foreign) = match available_tmux_command() {
        Some(tmux) => {
            let names = tokio::task::spawn_blocking(move || tmux_list_session_names(&tmux))
                .await
                .unwrap_or_default();
            classify_tmux_sessions(&workbench_names, names)
        }
        None => (Vec::new(), Vec::new()),
    };
    Ok(FreshRestartPlan {
        rows,
        owned_tmux_names: owned,
        foreign_tmux_names: foreign,
    })
}

/// 与 `close_sessions_for_worktree` 同构的单会话关闭阶梯：
/// registry.close → NotFound 时 close-intent（Conflict 重试）→ kill_persisted_backend
/// → repo.delete → finish_cleanup。kill/delete 失败时保留 barrier（Drop 不 finish）。
async fn close_one_workbench_session(
    state: &AppState,
    row: &WorkbenchSessionRow,
) -> Result<(), AppError> {
    let cleanup = match state.workbench_sessions.close(&row.id) {
        Ok(cleanup) => cleanup,
        Err(AppError::NotFound(_)) => {
            match state
                .workbench_sessions
                .begin_close_intent_for_missing_handle(&row.id, row.clone())
            {
                Ok(cleanup) => cleanup,
                Err(AppError::Conflict(_)) => match state.workbench_sessions.close(&row.id) {
                    Ok(cleanup) => cleanup,
                    Err(AppError::NotFound(_)) => state
                        .workbench_sessions
                        .begin_close_intent_for_missing_handle(&row.id, row.clone())?,
                    Err(error) => return Err(error),
                },
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    kill_persisted_backend(cleanup.row())?;
    state.workbench_session_repo.delete(&row.id).await?;
    cleanup.finish_cleanup();
    Ok(())
}

// ---------------------------------------------------------------------------
// 两阶段入口
// ---------------------------------------------------------------------------

/// Business Logic（为什么需要这个函数）:
///     终止整台设备的全部工作台终端是不可逆动作，用户确认前需要看到准确影响面
///     （会话清单、非工作台 tmux 会话数、引导通道可用性）再决定是否连带终止
///     用户自己的 tmux 会话。
///
/// Code Logic（这个函数做什么）:
///     require_owner → 只读 plan → 有界 ssh 探测 → 组装有界 preview DTO。
pub async fn preview_workbench_fresh_restart_for_state(
    state: &AppState,
) -> Result<WorkbenchFreshRestartPreviewDto, AppError> {
    state.runtime_role.require_owner()?;
    let plan = plan_fresh_restart(state).await?;
    let (ssh_available, ssh_detail) = probe_loopback_ssh_available().await;
    let sessions: Vec<WorkbenchFreshRestartSessionDto> = plan
        .rows
        .iter()
        .take(FRESH_RESTART_PREVIEW_SESSION_LIMIT)
        .map(|row| WorkbenchFreshRestartSessionDto {
            session_id: row.id.clone(),
            project_id: row.project_id.clone(),
            name: row.name.clone(),
            backend: row.backend.clone(),
        })
        .collect();
    Ok(WorkbenchFreshRestartPreviewDto {
        workbench_tmux_session_count: plan.owned_tmux_names.len() as u32,
        foreign_session_count: plan.foreign_tmux_names.len() as u32,
        foreign_session_names: plan
            .foreign_tmux_names
            .into_iter()
            .take(FRESH_RESTART_PREVIEW_FOREIGN_NAME_LIMIT)
            .collect(),
        ssh_bootstrap_available: Some(ssh_available),
        ssh_bootstrap_detail: if ssh_available {
            None
        } else {
            Some(ssh_detail)
        },
        sessions,
    })
}

/// Business Logic（为什么需要这个函数）:
///     用户确认后的设备级"全新启动连接"：关闭该设备全部工作台终端会话，并以
///     全新登录环境重启 tmux server，让系统级变更（组/limits/profile）在新终端
///     生效。任何一步失败都如实降级，不伪装成功。
///
/// Code Logic（这个函数做什么）:
///     require_owner → 设备 barrier → plan → 逐会话关闭（失败记 skipped 不中断）
///     → 复探 foreign → kill-server 决策 → KillServer 时 kill-server + ssh 引导
///     （失败降级 Manual，惰性 ensure 兜底可用性）→ 组装结果 DTO；Drop 释放 barrier。
pub async fn run_workbench_fresh_restart_for_state(
    state: &AppState,
    include_foreign_sessions: bool,
) -> Result<WorkbenchFreshRestartResultDto, AppError> {
    state.runtime_role.require_owner()?;
    let _barrier = begin_device_fresh_restart_barrier()?;
    let plan = plan_fresh_restart(state).await?;

    let mut terminated_ids: Vec<String> = Vec::new();
    let mut skipped_ids: Vec<String> = Vec::new();
    let mut close_failures = 0_usize;
    for row in &plan.rows {
        match close_one_workbench_session(state, row).await {
            Ok(()) => terminated_ids.push(row.id.clone()),
            Err(error) => {
                close_failures += 1;
                tracing::warn!(
                    session_id = %row.id,
                    error = %error,
                    "fresh restart 单会话关闭失败（保留元数据，跳过）"
                );
                skipped_ids.push(row.id.clone());
            }
        }
    }
    let terminated_session_count = terminated_ids.len() as u32;
    let terminated_session_ids = terminated_ids
        .into_iter()
        .take(FRESH_RESTART_RESULT_ID_LIMIT)
        .collect::<Vec<_>>();
    let skipped_session_ids = skipped_ids
        .into_iter()
        .take(FRESH_RESTART_RESULT_ID_LIMIT)
        .collect::<Vec<_>>();

    // 复探：close 后 server 上残留的非工作台会话（用户会话数是 kill-server 决策输入）。
    let foreign_after: Vec<String> = match available_tmux_command() {
        Some(tmux) => {
            let names = tokio::task::spawn_blocking(move || tmux_list_session_names(&tmux))
                .await
                .unwrap_or_default();
            let empty = HashSet::new();
            classify_tmux_sessions(&empty, names).1
        }
        None => Vec::new(),
    };
    let foreign_session_count = foreign_after.len() as u32;

    let decision = decide_server_restart(
        close_failures,
        foreign_session_count as usize,
        include_foreign_sessions,
    );
    let mut result = WorkbenchFreshRestartResultDto {
        terminated_session_count,
        terminated_session_ids,
        skipped_session_ids,
        server_restarted: false,
        bootstrap: FreshRestartBootstrap::Skipped,
        degraded_reason: None,
        degraded_detail: None,
        foreign_session_count,
        manual_command: None,
    };
    match decision {
        FreshRestartServerDecision::SkipCloseFailed => {
            result.degraded_reason = Some(DEGRADED_CLOSE_FAILED.to_string());
            result.degraded_detail =
                Some("有会话关闭失败，为避免孤儿现场未重启 tmux server".to_string());
            result.manual_command = Some(FRESH_RESTART_MANUAL_COMMAND.to_string());
        }
        FreshRestartServerDecision::SkipForeign => {
            result.degraded_reason = Some(DEGRADED_FOREIGN_PRESENT.to_string());
            result.degraded_detail = Some(format!(
                "tmux 上存在 {} 个非工作台会话，未获准一并终止；server 未重启，组/limits 类变更暂不生效",
                foreign_session_count
            ));
            result.manual_command = Some(FRESH_RESTART_MANUAL_COMMAND.to_string());
        }
        FreshRestartServerDecision::KillServer => {
            let Some(tmux) = available_tmux_command() else {
                result.degraded_reason = Some(DEGRADED_KILL_SERVER_FAILED.to_string());
                result.degraded_detail = Some("tmux 不可用，无法重启 server".to_string());
                result.manual_command = Some(FRESH_RESTART_MANUAL_COMMAND.to_string());
                return Ok(result);
            };
            if let Err(error) = tokio::task::spawn_blocking(move || run_tmux_kill_server(&tmux))
                .await
                .map_err(|join_error| {
                    AppError::generic(format!("kill-server 任务失败: {join_error}"))
                })
                .and_then(|outcome| outcome)
            {
                result.degraded_reason = Some(DEGRADED_KILL_SERVER_FAILED.to_string());
                result.degraded_detail = Some(format!("tmux kill-server 失败: {error}"));
                result.manual_command = Some(FRESH_RESTART_MANUAL_COMMAND.to_string());
                return Ok(result);
            }
            if let Some(tmux) = available_tmux_command() {
                match bootstrap_fresh_tmux_server_via_ssh(&tmux).await {
                    Ok(()) => {
                        result.server_restarted = true;
                        result.bootstrap = FreshRestartBootstrap::Ssh;
                    }
                    Err(degraded) => {
                        result.bootstrap = FreshRestartBootstrap::Manual;
                        result.degraded_reason = Some(degraded.reason.to_string());
                        result.degraded_detail = Some(degraded.detail);
                        result.manual_command = Some(FRESH_RESTART_MANUAL_COMMAND.to_string());
                    }
                }
            } else {
                result.bootstrap = FreshRestartBootstrap::Manual;
                result.degraded_reason = Some(DEGRADED_KILL_SERVER_FAILED.to_string());
                result.degraded_detail = Some("tmux 不可用，无法引导新 server".to_string());
                result.manual_command = Some(FRESH_RESTART_MANUAL_COMMAND.to_string());
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_single_quote_escapes_quotes() {
        assert_eq!(shell_single_quote("plain"), "'plain'");
        assert_eq!(shell_single_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_single_quote(""), "''");
    }

    #[test]
    fn classify_splits_owned_and_foreign() {
        let mut workbench = HashSet::new();
        workbench.insert("proj-main-a1b2c3d4e5f6".to_string());
        let (owned, foreign) = classify_tmux_sessions(
            &workbench,
            vec![
                "proj-main-a1b2c3d4e5f6".to_string(),
                "manual".to_string(),
                String::new(),
                "  spaced  ".to_string(),
            ],
        );
        assert_eq!(owned, vec!["proj-main-a1b2c3d4e5f6".to_string()]);
        assert_eq!(foreign, vec!["manual".to_string(), "spaced".to_string()]);
    }

    #[test]
    fn classify_empty_tmux_means_no_server() {
        let workbench = HashSet::new();
        let (owned, foreign) = classify_tmux_sessions(&workbench, Vec::new());
        assert!(owned.is_empty());
        assert!(foreign.is_empty());
    }

    #[test]
    fn decide_server_restart_truth_table() {
        use FreshRestartServerDecision::*;
        // close 失败永远优先 fail-closed。
        assert_eq!(decide_server_restart(1, 0, true), SkipCloseFailed);
        // 无外部会话 → kill-server。
        assert_eq!(decide_server_restart(0, 0, false), KillServer);
        // 外部会话 + 用户同意 → kill-server。
        assert_eq!(decide_server_restart(0, 2, true), KillServer);
        // 外部会话 + 未同意 → skip。
        assert_eq!(decide_server_restart(0, 2, false), SkipForeign);
    }

    #[test]
    fn loopback_argv_includes_login_wrapper_and_persist_conf() {
        let tmux = TmuxCommand::native("/opt/homebrew/bin/tmux");
        let argv = build_loopback_ssh_bootstrap_argv(
            "hans",
            &tmux,
            "/Users/hans/.cc-partner/tmux.conf",
            Some("/var/folders/ab/T/"),
        );
        assert_eq!(argv[0], "ssh");
        let joined = argv.join(" ");
        assert!(
            joined.contains("hans@127.0.0.1"),
            "argv 应指向 loopback: {joined}"
        );
        assert!(joined.contains("BatchMode=yes"));
        assert!(joined.contains("ConnectTimeout=5"));
        assert!(joined.contains("StrictHostKeyChecking=accept-new"));
        // login 包装 + TMUX_TMPDIR 前缀 + persist conf + start-server。
        assert!(joined.contains("exec /bin/sh -lc"));
        assert!(joined.contains("TMUX_TMPDIR="));
        assert!(joined.contains("start-server"));
        assert!(joined.contains("/Users/hans/.cc-partner/tmux.conf"));
        // tmux 程序与 conf 路径必须单引号转义。
        assert!(joined.contains("'/opt/homebrew/bin/tmux'"));
    }

    #[test]
    fn loopback_argv_omits_empty_tmpdir() {
        let tmux = TmuxCommand::native("tmux");
        let argv = build_loopback_ssh_bootstrap_argv("u", &tmux, "/tmp/tmux.conf", Some(""));
        assert!(!argv.join(" ").contains("TMUX_TMPDIR="));
    }

    #[test]
    fn barrier_begin_conflict_and_release() {
        let _lock = BARRIER_TEST_LOCK.lock().expect("barrier 测试锁中毒");
        reset_device_fresh_restart_barrier_for_test();
        assert!(!is_device_fresh_restart_active());
        let guard = begin_device_fresh_restart_barrier().expect("首次 begin 应成功");
        assert!(is_device_fresh_restart_active());
        assert!(begin_device_fresh_restart_barrier().is_err());
        assert!(require_device_not_fresh_restarting().is_err());
        drop(guard);
        assert!(!is_device_fresh_restart_active());
        assert!(require_device_not_fresh_restarting().is_ok());
        reset_device_fresh_restart_barrier_for_test();
    }

    #[test]
    fn manual_command_is_stable() {
        assert_eq!(
            FRESH_RESTART_MANUAL_COMMAND,
            "cc-partner-backend workbench-fresh"
        );
    }
}
