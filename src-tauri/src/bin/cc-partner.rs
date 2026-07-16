/// 启动 cc-partner Agent 控制 CLI。
///
/// Business Logic（为什么需要这个函数）:
///     Agent 需要独立于 backend 生命周期命令的稳定控制面，用 JSON/exit code 编排
///     project/worktree/session/agent/task/experiment/attention/fleet/browser。
///
/// Code Logic（这个函数做什么）:
///     调用库中的 `agent_cli::run_from_env()`，并把返回值作为进程退出码直接转发。
fn main() {
    std::process::exit(app_lib::agent_cli::run_from_env());
}
