//! workbench/tmux_persist.rs — 工作台 tmux server 持久化参数与 conf。
//!
//! Business Logic（为什么需要这个模块）:
//!     Dev.app codesign / sidecar 热更新会拆掉 attach 客户端；若 server 因空 session 退出，
//!     或 `start-server` 读到用户 `exit-empty on`，restore 只能看到 stale window。
//!     这些 helper 从 `sessions.rs` 拆出，避免会话注册表继续突破 no-growth 基线。
//!
//! Code Logic（这个模块做什么）:
//!     生成 session/server persist 的 tmux argv、默认 socket 上带 `-f conf` 的 start-server
//!     参数，以及写入 `<data_dir>/tmux.conf` 的 persist conf 路径。

use crate::error::AppError;
use std::path::PathBuf;

const WORKBENCH_TMUX_PERSIST_CONF: &str =
    "set -s exit-empty off\nset -g destroy-unattached off\nset -g mouse off\n";

/// Business Logic（为什么需要这个函数）:
///     sidecar 退出只会拆掉 attach 客户端；若 session 在最后一个 client 离开时自毁，
///     pane 里的 shell/Agent 会一起死，热更新后旧窗口无法重连。
///
/// Code Logic（这个函数做什么）:
///     生成 session-local `set-option -t <session> destroy-unattached off`。
pub(crate) fn tmux_session_persist_commands(session_name: &str) -> Vec<Vec<String>> {
    vec![vec![
        "set-option".to_string(),
        "-t".to_string(),
        session_name.to_string(),
        "destroy-unattached".to_string(),
        "off".to_string(),
    ]]
}

/// Business Logic（为什么需要这个函数）:
///     最后一个 session 销毁后 tmux server 默认 exit-empty 退出；热更新窗口期若短暂空 session，
///     后续 restore 会连到新 server，旧 window id 全部失效。
///
/// Code Logic（这个函数做什么）:
///     生成 server-level `set-option -s exit-empty off`。
pub(crate) fn tmux_server_persist_commands() -> Vec<Vec<String>> {
    vec![vec![
        "set-option".to_string(),
        "-s".to_string(),
        "exit-empty".to_string(),
        "off".to_string(),
    ]]
}

/// Business Logic（为什么需要这个函数）:
///     默认 socket 上 `tmux start-server` 会读用户 `~/.tmux.conf`，`exit-empty` 默认 on，
///     空 server 立刻退出。start-server 必须带 persist conf，且不得改用 `-S` 隔离 socket。
///
/// Code Logic（这个函数做什么）:
///     `prefix + 可选 -f <conf> + start-server`。
pub(crate) fn tmux_start_server_args(
    prefix_args: &[String],
    persist_conf: Option<&str>,
) -> Vec<String> {
    let mut args = prefix_args.to_vec();
    if let Some(conf) = persist_conf.filter(|path| !path.is_empty()) {
        args.push("-f".to_string());
        args.push(conf.to_string());
    }
    args.push("start-server".to_string());
    args
}

/// Business Logic（为什么需要这个函数）:
///     `start-server` 必须在创建 server 时就带上 `exit-empty off`，不能等 server
///     因空 session 退出后再 `set-option`。
///
/// Code Logic（这个函数做什么）:
///     把 persist conf 写到 `<data_dir>/tmux.conf`（文件，不是隔离 socket 目录）。
pub(crate) fn workbench_tmux_persist_conf_path() -> Result<PathBuf, AppError> {
    let path = crate::config::data_dir()?.join("tmux.conf");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(WORKBENCH_TMUX_PERSIST_CONF) {
        std::fs::write(&path, WORKBENCH_TMUX_PERSIST_CONF)?;
    }
    Ok(path)
}
