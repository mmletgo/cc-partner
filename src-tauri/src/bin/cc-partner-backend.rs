/// 启动 cc-partner 独立后端 CLI。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要一个不启动桌面 GUI 的可执行入口，用于在远端设备启动和管理 headless 后端。
///
/// Code Logic（这个函数做什么）:
///     调用库中的 `backend::cli::run_from_env()`，并把返回值作为进程退出码。
fn main() {
    std::process::exit(app_lib::backend::cli::run_from_env());
}
