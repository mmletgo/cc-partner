use crate::config::config_dir;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tokio::sync::watch;

const CONTROL_FILE_NAME: &str = "backend-control.json";
const PID_FILE_NAME: &str = "backend.pid";
static SHUTDOWN_NOTIFIER: OnceLock<Mutex<Option<watch::Sender<bool>>>> = OnceLock::new();

/// 独立后端进程写入磁盘的控制文件内容。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 和后续 `cc-partner-backend` CLI 需要用同一份控制文件识别后端进程、HTTP 端口和设备身份，
///     从而支持 start/stop/status 的跨进程协作。
///
/// Code Logic（这个结构做什么）:
///     以 camelCase JSON 保存 pid、port、设备信息、启动时间和控制令牌；读写 helper 直接序列化/反序列化该结构。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackendControlFile {
    pub pid: u32,
    pub port: u16,
    pub device_id: String,
    pub device_name: String,
    pub started_at: String,
    pub control_token: String,
}

impl BackendControlFile {
    /// 构造测试用控制文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单元测试只关心状态分类所需的最小 pid/port/device_id，不应为无关字段重复造样板数据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用传入 pid、port、device_id 填充关键字段，并给 device_name、started_at、control_token 提供稳定占位值。
    #[cfg(test)]
    fn for_test(pid: u32, port: u16, device_id: &str) -> Self {
        Self {
            pid,
            port,
            device_id: device_id.to_string(),
            device_name: "test-device".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            control_token: "test-token".to_string(),
        }
    }
}

/// 后端状态分类。
///
/// Business Logic（为什么需要这个枚举）:
///     CLI status 和 GUI 生命周期管理需要把“没有控制文件、进程残留、健康检查失败、真实运行中”
///     这些情形压缩为用户可理解的固定状态。
///
/// Code Logic（这个枚举做什么）:
///     使用 camelCase 序列化输出 Running/Stopped/Stale/Error 四种状态，供后续命令层直接返回。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BackendStatusKind {
    Running,
    Stopped,
    Stale,
    Error,
}

/// 后端 status 查询结果。
///
/// Business Logic（为什么需要这个结构）:
///     用户执行 status 时不仅要看到最终状态，还需要在可用时看到控制文件细节，并在异常时看到错误原因。
///
/// Code Logic（这个结构做什么）:
///     包装状态枚举、可选控制文件和可选错误字符串，保持 camelCase JSON 输出契约。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendStatus {
    pub kind: BackendStatusKind,
    pub control: Option<BackendControlFile>,
    pub error: Option<String>,
}

/// 返回后端控制 JSON 文件路径。
///
/// Business Logic（为什么需要这个函数）:
///     GUI、CLI 与独立后端必须在同一用户配置目录下定位同一份控制文件，避免多处硬编码路径。
///
/// Code Logic（这个函数做什么）:
///     基于 `config_dir()` 派生 `backend-control.json` 的绝对路径。
pub fn control_file_path() -> PathBuf {
    config_dir().join(CONTROL_FILE_NAME)
}

/// 返回后端 pid 文件路径。
///
/// Business Logic（为什么需要这个函数）:
///     stop/status 等命令需要一个轻量 pid 文件与控制 JSON 并存，兼容只需读取进程号的后续逻辑。
///
/// Code Logic（这个函数做什么）:
///     基于 `config_dir()` 派生 `backend.pid` 的绝对路径。
pub fn pid_file_path() -> PathBuf {
    config_dir().join(PID_FILE_NAME)
}

/// 读取后端控制文件。
///
/// Business Logic（为什么需要这个函数）:
///     status 和 stop 需要先读取当前后端的进程、端口和控制令牌；文件不存在表示后端未启动。
///
/// Code Logic（这个函数做什么）:
///     若控制文件不存在返回 `Ok(None)`；存在则按 UTF-8 读取并反序列化为 `BackendControlFile`。
pub fn read_control_file() -> Result<Option<BackendControlFile>, AppError> {
    let path = control_file_path();
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&content)?))
}

/// 写入后端控制文件和 pid 文件。
///
/// Business Logic（为什么需要这个函数）:
///     独立后端启动成功后需要发布自己的运行信息，GUI/CLI 才能发现并管理这个后端实例。
///
/// Code Logic（这个函数做什么）:
///     确保配置目录存在，将控制结构写成 pretty JSON UTF-8，并把 pid 单独写入 `backend.pid`。
pub fn write_control_file(control: &BackendControlFile) -> Result<(), AppError> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join(CONTROL_FILE_NAME),
        serde_json::to_string_pretty(control)?,
    )?;
    fs::write(dir.join(PID_FILE_NAME), control.pid.to_string())?;
    Ok(())
}

/// 删除后端控制文件和 pid 文件。
///
/// Business Logic（为什么需要这个函数）:
///     后端停止或发现 stale 状态后需要清理控制文件，避免下一次 status/start 误判旧实例仍可管理。
///
/// Code Logic（这个函数做什么）:
///     分别尝试删除控制 JSON 与 pid 文件；文件不存在视为清理成功，其它 IO 错误向上返回。
pub fn remove_control_files() -> Result<(), AppError> {
    for path in [control_file_path(), pid_file_path()] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

/// 返回进程内后端关闭通知槽。
///
/// Business Logic（为什么需要这个函数）:
///     headless `serve` 进程需要让 HTTP control route 唤醒自身退出，但 route 层不能依赖 CLI 模块。
///
/// Code Logic（这个函数做什么）:
///     用 `OnceLock` 延迟初始化一个 `Mutex<Option<watch::Sender<bool>>>`，供 serve 安装、route 触发、退出清理。
fn shutdown_notifier_slot() -> &'static Mutex<Option<watch::Sender<bool>>> {
    SHUTDOWN_NOTIFIER.get_or_init(|| Mutex::new(None))
}

/// 安装 headless 后端关闭通知器。
///
/// Business Logic（为什么需要这个函数）:
///     `cc-partner-backend serve` 启动后必须把自己的 shutdown receiver 暴露给本进程 HTTP route，
///     这样 `stop` 命令才能通过本地 control API 请求它优雅关闭。
///
/// Code Logic（这个函数做什么）:
///     将 watch sender 写入全局通知槽；后续同进程 route 调 `request_backend_shutdown` 会发送 true。
pub fn install_shutdown_notifier(sender: watch::Sender<bool>) {
    let mut guard = shutdown_notifier_slot().lock().expect("后端关闭通知锁中毒");
    *guard = Some(sender);
}

/// 清理 headless 后端关闭通知器。
///
/// Business Logic（为什么需要这个函数）:
///     serve 退出后不应保留旧 sender，否则测试或后续同进程重启会向已经失效的控制通道发送信号。
///
/// Code Logic（这个函数做什么）:
///     将全局通知槽重置为 None。
pub fn clear_shutdown_notifier() {
    let mut guard = shutdown_notifier_slot().lock().expect("后端关闭通知锁中毒");
    *guard = None;
}

/// 请求 headless 后端优雅关闭。
///
/// Business Logic（为什么需要这个函数）:
///     本地 stop route 通过 token 校验后需要通知 serve 主循环退出，进而执行 runtime shutdown 和控制文件清理。
///
/// Code Logic（这个函数做什么）:
///     若已安装 watch sender，则发送 true 并返回是否发送成功；未安装 sender 时返回 false。
pub fn request_backend_shutdown() -> bool {
    let guard = shutdown_notifier_slot().lock().expect("后端关闭通知锁中毒");
    guard
        .as_ref()
        .map(|sender| sender.send(true).is_ok())
        .unwrap_or(false)
}

/// 根据控制文件、进程存活、健康检查和错误信息生成后端状态。
///
/// Business Logic（为什么需要这个函数）:
///     CLI/GUI 需要统一的状态判定口径：没有控制文件是停止，有错误优先报错，只有进程和健康检查都通过才是运行中。
///
/// Code Logic（这个函数做什么）:
///     按 `error -> Error`、`None -> Stopped`、`process_alive && health_ok -> Running`，
///     其它组合归为 `Stale`，并保留原控制文件与错误详情。
pub fn classify_status(
    control: Option<BackendControlFile>,
    process_alive: bool,
    health_ok: bool,
    error: Option<String>,
) -> BackendStatus {
    let kind = if error.is_some() {
        BackendStatusKind::Error
    } else if control.is_none() {
        BackendStatusKind::Stopped
    } else if process_alive && health_ok {
        BackendStatusKind::Running
    } else {
        BackendStatusKind::Stale
    };

    BackendStatus {
        kind,
        control,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证缺少控制文件时状态为停止。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     用户查询后端状态时，未启动过独立后端应看到 Stopped，而不是错误或残留状态。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入空控制文件、进程未存活、健康检查失败且无错误，断言分类结果为 Stopped 且不携带控制文件。
    #[test]
    fn classify_status_reports_stopped_without_control_file() {
        let status = classify_status(None, false, false, None);
        assert_eq!(status.kind, BackendStatusKind::Stopped);
        assert!(status.control.is_none());
    }

    /// 验证错误信息优先于缺失控制文件。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     status 查询过程中如果读取控制文件或健康检查发生错误，用户需要先看到 Error，避免真实故障被误报为未启动。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入空控制文件和错误字符串，断言 `classify_status` 返回 Error，并保留错误详情。
    #[test]
    fn classify_status_reports_error_when_error_exists_without_control_file() {
        let status = classify_status(None, false, false, Some("read failed".to_string()));
        assert_eq!(status.kind, BackendStatusKind::Error);
        assert_eq!(status.error.as_deref(), Some("read failed"));
    }

    /// 验证控制文件存在但 pid 不存活时状态为 stale。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     后端异常退出后可能留下控制文件，用户查询状态时应看到 Stale 以便后续清理残留。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造测试控制文件并传入进程未存活、健康检查失败且无错误，断言分类结果为 Stale。
    #[test]
    fn classify_status_reports_stale_when_pid_dead() {
        let control = BackendControlFile::for_test(1234, 62116, "device-a");
        let status = classify_status(Some(control), false, false, None);
        assert_eq!(status.kind, BackendStatusKind::Stale);
    }

    /// 验证只有 pid 和健康检查都正常时才是 running。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI/CLI 只有在后端进程存在且 HTTP 健康检查通过时才能把后端展示为可用。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造控制文件并传入进程存活、健康检查成功且无错误，断言分类结果为 Running 并保留控制文件内容。
    #[test]
    fn classify_status_reports_running_only_when_pid_and_health_are_ok() {
        let control = BackendControlFile::for_test(1234, 62116, "device-a");
        let status = classify_status(Some(control.clone()), true, true, None);
        assert_eq!(status.kind, BackendStatusKind::Running);
        assert_eq!(status.control.unwrap().pid, 1234);
    }
}
