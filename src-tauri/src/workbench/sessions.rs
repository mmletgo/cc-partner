//! workbench/sessions.rs — 工作台本机 PTY 会话注册表
//!
//! Business Logic（为什么需要这个模块）:
//!     工作台允许用户在同一项目下开启多个本机 Claude Code 交互式终端，会话只需要在应用运行期存在。
//!
//! Code Logic（这个模块做什么）:
//!     使用 portable-pty 创建 PTY，内存保存会话句柄，并通过 Tauri event 推送终端输出和状态变化。

#![allow(dead_code)]

use crate::error::AppError;
use crate::workbench::models::{WorkbenchProjectRow, WorkbenchSessionDto};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const DEFAULT_COLS: u16 = 98;
const DEFAULT_ROWS: u16 = 32;

/// 工作台终端输出事件 payload。
///
/// Business Logic（为什么需要这个结构体）:
///     前端 xterm 需要按会话接收增量输出，并用 seq 维持调试和乱序排查能力。
///
/// Code Logic（这个结构体做什么）:
///     序列化为 camelCase Tauri event payload，包含会话 ID、输出块、序号和毫秒时间戳。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutputEvent {
    session_id: String,
    chunk: String,
    seq: u64,
    ts: i64,
}

/// 工作台终端状态事件 payload。
///
/// Business Logic（为什么需要这个结构体）:
///     前端需要知道会话何时进入运行、退出或断开状态，以更新工作台右侧状态和终端 tab。
///
/// Code Logic（这个结构体做什么）:
///     序列化为 camelCase Tauri event payload，包含状态和可选退出码。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalStatusEvent {
    session_id: String,
    status: String,
    exit_code: Option<i32>,
    ts: i64,
}

/// PTY 进程资源。
///
/// Business Logic（为什么需要这个枚举）:
///     真实会话需要持有 PTY 资源；单元测试只验证 registry 纯内存行为，不应启动真实 CLI。
///
/// Code Logic（这个枚举做什么）:
///     区分真实 PTY 句柄与测试 fake 会话，让 list/filter/rename/close 可在无 PTY 环境下测试。
enum SessionProcess {
    Pty {
        master: Box<dyn portable_pty::MasterPty + Send>,
        writer: Box<dyn Write + Send>,
        child: Box<dyn portable_pty::Child + Send + Sync>,
    },
    #[allow(dead_code)]
    Fake,
}

/// 工作台终端会话运行态句柄。
///
/// Business Logic（为什么需要这个结构体）:
///     每个会话需要同时保存前端展示 DTO 和可操作的 PTY 进程资源。
///
/// Code Logic（这个结构体做什么）:
///     将 DTO 与 writer/master/child 聚合到单个 Mutex 保护的对象中，保证输入、resize、stop 串行访问。
struct WorkbenchSessionHandle {
    dto: WorkbenchSessionDto,
    process: SessionProcess,
}

/// 工作台 PTY 会话注册表。
///
/// Business Logic（为什么需要这个结构体）:
///     工作台会话是应用运行期内存状态，多个命令需要按 session_id 查找并操作同一 PTY。
///
/// Code Logic（这个结构体做什么）:
///     用 HashMap 保存 session_id 到会话句柄的映射；外层 Arc 允许后台读写线程更新状态。
pub struct WorkbenchSessionRegistry {
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>>,
}

impl WorkbenchSessionRegistry {
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 初始化时需要创建空的工作台会话注册表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造空 HashMap，并包裹 Arc<Mutex<_>> 供命令和后台线程共享。
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     前端工作台需要列出全部会话，或只列出某个项目下的会话。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取内存 registry，按可选 project_id 过滤并克隆 DTO 返回。
    pub fn list(&self, project_id: Option<&str>) -> Vec<WorkbenchSessionDto> {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        sessions
            .values()
            .filter_map(|handle| {
                let handle = handle.lock().expect("workbench session 锁中毒");
                if project_id
                    .map(|id| handle.dto.project_id == id)
                    .unwrap_or(true)
                {
                    Some(handle.dto.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户在工作台中创建本机 Claude Code 终端时，需要在项目目录中启动交互式 CLI。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 portable-pty pair，cwd 指向项目路径，spawn cli_path，并启动输出与退出监听线程。
    pub fn create(
        &self,
        app: AppHandle,
        project: WorkbenchProjectRow,
        cli_path: String,
    ) -> Result<WorkbenchSessionDto, AppError> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let dto = WorkbenchSessionDto {
            id: session_id.clone(),
            project_id: project.id.clone(),
            name: project.name.clone(),
            command: cli_path.clone(),
            status: "running".to_string(),
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            started_at: now,
            exited_at: None,
            exit_code: None,
        };

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: DEFAULT_ROWS,
                cols: DEFAULT_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| AppError::generic(format!("创建 PTY 失败: {error}")))?;
        let mut cmd = CommandBuilder::new(cli_path.clone());
        cmd.cwd(PathBuf::from(&project.path));
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|error| AppError::generic(format!("启动工作台终端失败: {error}")))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| AppError::generic(format!("创建 PTY reader 失败: {error}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| AppError::generic(format!("创建 PTY writer 失败: {error}")))?;

        let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
            dto: dto.clone(),
            process: SessionProcess::Pty {
                master: pair.master,
                writer,
                child,
            },
        }));
        self.sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .insert(session_id.clone(), handle.clone());

        emit_status(&app, &session_id, "running", None);
        spawn_reader_thread(app.clone(), session_id.clone(), reader);
        spawn_exit_watcher(app, self.sessions.clone(), session_id.clone(), handle);

        Ok(dto)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户在 xterm 中输入字符时，需要把输入发送给对应 PTY。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查找会话 writer，写入 UTF-8 字节并 flush；会话不存在或非运行态返回错误。
    pub fn write_input(&self, session_id: &str, data: &str) -> Result<(), AppError> {
        let handle = self.get_handle(session_id)?;
        let mut handle = handle.lock().expect("workbench session 锁中毒");
        if handle.dto.status != "running" {
            return Err(AppError::generic("工作台会话未运行"));
        }
        match &mut handle.process {
            SessionProcess::Pty { writer, .. } => {
                writer.write_all(data.as_bytes())?;
                writer.flush()?;
                Ok(())
            }
            SessionProcess::Fake => Ok(()),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     前端终端容器尺寸变化时，子进程需要收到新的 PTY 行列数。
    ///
    /// Code Logic（这个函数做什么）:
    ///     更新 DTO 尺寸，并调用 MasterPty::resize 通知底层 PTY。
    pub fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), AppError> {
        let handle = self.get_handle(session_id)?;
        let mut handle = handle.lock().expect("workbench session 锁中毒");
        handle.dto.cols = cols;
        handle.dto.rows = rows;
        match &mut handle.process {
            SessionProcess::Pty { master, .. } => master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|error| AppError::generic(format!("调整 PTY 尺寸失败: {error}"))),
            SessionProcess::Fake => Ok(()),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户需要主动停止某个 Claude Code 终端，但仍可保留会话记录直到关闭。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 child.kill() 终止进程；退出 watcher 会随后写入 exited 状态并发送事件。
    pub fn stop(&self, session_id: &str) -> Result<(), AppError> {
        let handle = self.get_handle(session_id)?;
        let mut handle = handle.lock().expect("workbench session 锁中毒");
        match &mut handle.process {
            SessionProcess::Pty { child, .. } => child.kill()?,
            SessionProcess::Fake => {}
        }
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户关闭终端 tab 后，该会话应从内存 registry 中移除并释放 PTY 资源。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 HashMap 删除句柄，尽力 kill 仍在运行的子进程；缺失会话返回错误。
    pub fn close(&self, session_id: &str) -> Result<(), AppError> {
        let handle = self
            .sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .remove(session_id)
            .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
        let mut handle = handle.lock().expect("workbench session 锁中毒");
        let was_running = handle.dto.status == "running";
        match &mut handle.process {
            SessionProcess::Pty { child, .. } => {
                if was_running {
                    let _ = child.kill();
                }
            }
            SessionProcess::Fake => {}
        }
        handle.dto.status = "disconnected".to_string();
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     应用退出或项目被移除时，所有仍运行的工作台 Claude Code 子进程都应被显式终止。
    ///
    /// Code Logic（这个函数做什么）:
    ///     drain registry 中全部会话句柄，逐个尽力 kill 仍运行的 PTY child，并返回被清理的数量。
    pub fn shutdown_all(&self) -> usize {
        let handles: Vec<Arc<Mutex<WorkbenchSessionHandle>>> = self
            .sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .drain()
            .map(|(_, handle)| handle)
            .collect();
        let count = handles.len();
        for handle in handles {
            let mut handle = handle.lock().expect("workbench session 锁中毒");
            let was_running = handle.dto.status == "running";
            match &mut handle.process {
                SessionProcess::Pty { child, .. } => {
                    if was_running {
                        let _ = child.kill();
                    }
                }
                SessionProcess::Fake => {}
            }
            handle.dto.status = "disconnected".to_string();
            handle.dto.exited_at = Some(chrono::Utc::now().to_rfc3339());
        }
        count
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户可以给多个终端会话改名，以区分不同工作流。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查找会话并更新 DTO name，返回更新后的 DTO；缺失会话返回错误。
    pub fn rename(&self, session_id: &str, name: &str) -> Result<WorkbenchSessionDto, AppError> {
        let handle = self.get_handle(session_id)?;
        let mut handle = handle.lock().expect("workbench session 锁中毒");
        handle.dto.name = name.trim().to_string();
        Ok(handle.dto.clone())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     多个会话操作都需要统一处理 session_id 不存在的错误。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 registry 中克隆 Arc 句柄；缺失时返回 AppError::NotFound。
    fn get_handle(&self, session_id: &str) -> Result<Arc<Mutex<WorkbenchSessionHandle>>, AppError> {
        self.sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .get(session_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("工作台会话不存在"))
    }

    #[cfg(test)]
    /// Business Logic（为什么需要这个函数）:
    ///     list 过滤测试需要构造不同项目的会话，但不应启动真实 PTY 或依赖本机 Claude CLI。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅在测试编译时插入 fake 会话句柄，覆盖 list/filter 纯内存逻辑。
    fn insert_fake_session_for_test(&self, session_id: &str, project_id: &str) {
        let dto = WorkbenchSessionDto {
            id: session_id.to_string(),
            project_id: project_id.to_string(),
            name: format!("session-{session_id}"),
            command: "claude".to_string(),
            status: "running".to_string(),
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            started_at: "2026-06-24T00:00:00Z".to_string(),
            exited_at: None,
            exit_code: None,
        };
        self.sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .insert(
                session_id.to_string(),
                Arc::new(Mutex::new(WorkbenchSessionHandle {
                    dto,
                    process: SessionProcess::Fake,
                })),
            );
    }
}

impl Default for WorkbenchSessionRegistry {
    /// Business Logic（为什么需要这个函数）:
    ///     需要默认值的测试或未来装配代码可直接构造空 registry。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `WorkbenchSessionRegistry::new()` 创建空注册表。
    fn default() -> Self {
        Self::new()
    }
}

/// Business Logic（为什么需要这个函数）:
///     前端终端需要持续接收 PTY stdout/stderr 合并后的输出。
///
/// Code Logic（这个函数做什么）:
///     在后台线程循环读取 reader，把每个字节块转换为 UTF-8 lossless chunk 后 emit。
fn spawn_reader_thread(app: AppHandle, session_id: String, mut reader: Box<dyn Read + Send>) {
    thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        let mut seq: u64 = 0;
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    seq += 1;
                    let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                    let event = TerminalOutputEvent {
                        session_id: session_id.clone(),
                        chunk,
                        seq,
                        ts: now_millis(),
                    };
                    if let Err(error) = app.emit("workbench:terminal-output", event) {
                        tracing::warn!("发送工作台终端输出事件失败: {error}");
                    }
                }
                Err(error) => {
                    tracing::debug!("读取工作台终端输出结束: {error}");
                    break;
                }
            }
        }
    });
}

/// Business Logic（为什么需要这个函数）:
///     终端进程退出后，前端需要收到状态变化并保留退出码。
///
/// Code Logic（这个函数做什么）:
///     后台线程短轮询 child.try_wait，退出时更新 DTO 并 emit exited 状态事件。
fn spawn_exit_watcher(
    app: AppHandle,
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>>,
    session_id: String,
    handle: Arc<Mutex<WorkbenchSessionHandle>>,
) {
    thread::spawn(move || loop {
        let status = {
            let mut handle = handle.lock().expect("workbench session 锁中毒");
            match &mut handle.process {
                SessionProcess::Pty { child, .. } => match child.try_wait() {
                    Ok(Some(status)) => Some(Ok(status.exit_code() as i32)),
                    Ok(None) => None,
                    Err(error) => Some(Err(error)),
                },
                SessionProcess::Fake => Some(Ok(0)),
            }
        };

        match status {
            Some(Ok(exit_code)) => {
                let still_registered = sessions
                    .lock()
                    .expect("workbench sessions 锁中毒")
                    .contains_key(&session_id);
                if still_registered {
                    let mut handle = handle.lock().expect("workbench session 锁中毒");
                    handle.dto.status = "exited".to_string();
                    handle.dto.exited_at = Some(chrono::Utc::now().to_rfc3339());
                    handle.dto.exit_code = Some(exit_code);
                    emit_status(&app, &session_id, "exited", Some(exit_code));
                }
                break;
            }
            Some(Err(error)) => {
                tracing::warn!("查询工作台终端退出状态失败: {error}");
                emit_status(&app, &session_id, "disconnected", None);
                break;
            }
            None => thread::sleep(Duration::from_millis(200)),
        }
    });
}

/// Business Logic（为什么需要这个函数）:
///     会话创建、退出和断开都需要以统一事件格式通知前端。
///
/// Code Logic（这个函数做什么）:
///     构造 `TerminalStatusEvent` 并通过 `workbench:terminal-status` 发送。
fn emit_status(app: &AppHandle, session_id: &str, status: &str, exit_code: Option<i32>) {
    let event = TerminalStatusEvent {
        session_id: session_id.to_string(),
        status: status.to_string(),
        exit_code,
        ts: now_millis(),
    };
    if let Err(error) = app.emit("workbench:terminal-status", event) {
        tracing::warn!("发送工作台终端状态事件失败: {error}");
    }
}

/// Business Logic（为什么需要这个函数）:
///     前端事件需要毫秒时间戳，用于排序、调试和状态展示。
///
/// Code Logic（这个函数做什么）:
///     返回当前 UTC 时间的 Unix 毫秒时间戳。
fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     新启动应用时还没有工作台终端，前端会话列表应为空数组。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造空 registry 并断言 list(None) 返回空。
    #[test]
    fn list_empty_registry_returns_empty() {
        let registry = WorkbenchSessionRegistry::new();

        assert!(registry.list(None).is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户重命名不存在的会话时，前端需要得到明确错误而不是创建幽灵会话。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对缺失 session_id 调用 rename 并断言返回 Err。
    #[test]
    fn rename_missing_session_returns_error() {
        let registry = WorkbenchSessionRegistry::new();

        assert!(registry.rename("missing", "name").is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户关闭不存在的会话时，应返回错误，避免前端误判关闭成功。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对缺失 session_id 调用 close 并断言返回 Err。
    #[test]
    fn close_missing_session_returns_error() {
        let registry = WorkbenchSessionRegistry::new();

        assert!(registry.close("missing").is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     工作台按项目切换时，只应展示当前项目下的终端会话。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入两个 fake 会话并断言 list(Some(project_id)) 只返回匹配项。
    #[test]
    fn list_filters_by_project_id() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s1", "p1");
        registry.insert_fake_session_for_test("s2", "p2");

        let listed = registry.list(Some("p1"));

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "s1");
        assert_eq!(listed[0].project_id, "p1");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     应用退出时必须清空所有运行期会话，避免留下不可见的后台终端进程。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 fake 会话后调用 shutdown_all，断言返回清理数量且 registry 变为空。
    #[test]
    fn shutdown_all_drains_registry() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s1", "p1");
        registry.insert_fake_session_for_test("s2", "p2");

        let cleaned = registry.shutdown_all();

        assert_eq!(cleaned, 2);
        assert!(registry.list(None).is_empty());
    }
}
