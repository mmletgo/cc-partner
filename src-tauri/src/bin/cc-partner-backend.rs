/// 启动 cc-partner 独立后端 CLI。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要一个不启动桌面 GUI 的可执行入口，用于在远端设备启动和管理 headless 后端，
///     以及运行 `doctor` / `doctor --json` 健康检查。
///
/// Code Logic（这个函数做什么）:
///     调用库中的 `backend::cli::run_from_env()`，并把返回值作为进程退出码直接转发：
///     lifecycle 成功 0 / 失败 1；doctor healthy=0、degraded=1、unhealthy 或无法完成=2。
fn main() {
    std::process::exit(app_lib::backend::cli::run_from_env());
}
