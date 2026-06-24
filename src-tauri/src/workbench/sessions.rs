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
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const DEFAULT_COLS: u16 = 98;
const DEFAULT_ROWS: u16 = 32;
const MIN_TERMINAL_COLS: u16 = 20;
const MIN_TERMINAL_ROWS: u16 = 6;

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

/// 工作台终端 UTF-8 流式解码器。
///
/// Business Logic（为什么需要这个结构体）:
///     Claude Code 终端会输出中文、符号和状态栏文本，PTY read 可能把一个 UTF-8 字符拆到两个 chunk。
///
/// Code Logic（这个结构体做什么）:
///     保存上次 chunk 末尾未完成的字节序列，下次 decode 时先拼回去；真实非法字节仍输出替换符。
#[derive(Debug, Default)]
struct TerminalUtf8Decoder {
    pending: Vec<u8>,
}

impl TerminalUtf8Decoder {
    /// Business Logic（为什么需要这个函数）:
    ///     PTY reader 每个会话启动时都需要一个新的解码状态容器。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回没有 pending 字节的流式 UTF-8 解码器。
    fn new() -> Self {
        Self::default()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     终端输出事件必须保持文本完整，否则前端 xterm 会显示 �，影响命令行状态栏阅读。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将新字节与 pending 拼接后解码；遇到末尾不完整 UTF-8 时暂存，遇到非法字节时输出替换符。
    fn decode(&mut self, bytes: &[u8]) -> String {
        if self.pending.is_empty() {
            return decode_utf8_chunk(bytes, &mut self.pending);
        }

        let mut combined = Vec::with_capacity(self.pending.len() + bytes.len());
        combined.append(&mut self.pending);
        combined.extend_from_slice(bytes);
        decode_utf8_chunk(&combined, &mut self.pending)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     PTY 关闭前如果仍有残留字节，前端应收到可诊断的占位文本而不是静默丢失。
    ///
    /// Code Logic（这个函数做什么）:
    ///     取出 pending 并用 lossy 解码；没有 pending 时返回 None。
    fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        let pending = std::mem::take(&mut self.pending);
        Some(String::from_utf8_lossy(&pending).into_owned())
    }
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
///     将 DTO 与 writer/master/child 聚合到单个 Mutex 保护的对象中，保证输入、resize、close 串行访问。
struct WorkbenchSessionHandle {
    dto: WorkbenchSessionDto,
    process: SessionProcess,
}

/// Business Logic（为什么需要这个函数）:
///     工作台终端首屏需要按前端真实可见尺寸启动，避免 Claude Code 先按默认列宽绘制后错位。
///
/// Code Logic（这个函数做什么）:
///     对前端传入的可选 cols/rows 做下限裁剪；缺失时回退默认 PTY 尺寸。
fn initial_terminal_size(cols: Option<u16>, rows: Option<u16>) -> (u16, u16) {
    (
        cols.map(|value| value.max(MIN_TERMINAL_COLS))
            .unwrap_or(DEFAULT_COLS),
        rows.map(|value| value.max(MIN_TERMINAL_ROWS))
            .unwrap_or(DEFAULT_ROWS),
    )
}

/// Business Logic（为什么需要这个函数）:
///     用户关闭终端或应用退出清理时，Claude Code 子进程可能已经自然退出并被系统回收，此时 kill 返回 No such process 不应打扰用户。
///
/// Code Logic（这个函数做什么）:
///     将底层 child.kill() 的结果归一化；进程已不存在视为 Ok，其他 IO 错误继续转换为 AppError。
fn normalize_terminal_kill_result(result: std::io::Result<()>) -> Result<(), AppError> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if is_terminal_process_already_gone(&error) => Ok(()),
        Err(error) => Err(AppError::from(error)),
    }
}

/// Business Logic（为什么需要这个函数）:
///     不同平台或 portable-pty 后端对“进程不存在”可能给出 ErrorKind::NotFound 或原始 ESRCH 码。
///
/// Code Logic（这个函数做什么）:
///     检查 IO 错误是否表示目标进程已不存在；macOS/Linux 的 ESRCH 是 raw os error 3。
fn is_terminal_process_already_gone(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::NotFound) || error.raw_os_error() == Some(3)
}

/// Business Logic（为什么需要这个函数）:
///     PTY reader 只能拿到字节流，工作台事件需要发送 UTF-8 字符串给前端 xterm。
///
/// Code Logic（这个函数做什么）:
///     从给定字节切片中尽可能解出完整 UTF-8 文本，把末尾不完整序列写入 pending。
fn decode_utf8_chunk(bytes: &[u8], pending: &mut Vec<u8>) -> String {
    let mut output = String::new();
    let mut offset = 0;

    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(valid) => {
                output.push_str(valid);
                break;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                if valid_up_to > 0 {
                    let valid = std::str::from_utf8(&bytes[offset..offset + valid_up_to])
                        .expect("valid_up_to guarantees this prefix is valid UTF-8");
                    output.push_str(valid);
                    offset += valid_up_to;
                }

                match error.error_len() {
                    Some(invalid_len) => {
                        output.push('\u{FFFD}');
                        offset += invalid_len;
                    }
                    None => {
                        pending.extend_from_slice(&bytes[offset..]);
                        break;
                    }
                }
            }
        }
    }

    output
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
        initial_cols: Option<u16>,
        initial_rows: Option<u16>,
    ) -> Result<WorkbenchSessionDto, AppError> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let (cols, rows) = initial_terminal_size(initial_cols, initial_rows);
        let dto = WorkbenchSessionDto {
            id: session_id.clone(),
            project_id: project.id.clone(),
            name: project.name.clone(),
            command: cli_path.clone(),
            status: "running".to_string(),
            cols,
            rows,
            started_at: now,
            exited_at: None,
            exit_code: None,
        };

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
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
                    if let Err(error) = normalize_terminal_kill_result(child.kill()) {
                        tracing::debug!("关闭工作台终端时 kill 失败: {error}");
                    }
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
                        if let Err(error) = normalize_terminal_kill_result(child.kill()) {
                            tracing::debug!("清理工作台终端时 kill 失败: {error}");
                        }
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
        let mut decoder = TerminalUtf8Decoder::default();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    emit_terminal_output(&app, &session_id, &mut seq, decoder.decode(&buf[..n]));
                }
                Err(error) => {
                    tracing::debug!("读取工作台终端输出结束: {error}");
                    break;
                }
            }
        }
        if let Some(chunk) = decoder.finish() {
            emit_terminal_output(&app, &session_id, &mut seq, chunk);
        }
    });
}

/// Business Logic（为什么需要这个函数）:
///     终端输出事件需要统一递增 seq，且纯 pending chunk 未完成时不应发送空事件。
///
/// Code Logic（这个函数做什么）:
///     非空 chunk 才构造 `TerminalOutputEvent` 并通过 `workbench:terminal-output` emit。
fn emit_terminal_output(app: &AppHandle, session_id: &str, seq: &mut u64, chunk: String) {
    if chunk.is_empty() {
        return;
    }
    *seq += 1;
    let event = TerminalOutputEvent {
        session_id: session_id.to_string(),
        chunk,
        seq: *seq,
        ts: now_millis(),
    };
    if let Err(error) = app.emit("workbench:terminal-output", event) {
        tracing::warn!("发送工作台终端输出事件失败: {error}");
    }
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
    ///     Claude Code 状态栏会输出中文和符号，PTY read 可能把多字节 UTF-8 拆到相邻 chunk。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造一个被拆开的中文字符串，断言流式解码器能跨 chunk 保留完整字符。
    #[test]
    fn terminal_utf8_decoder_preserves_split_multibyte_characters() {
        let mut decoder = TerminalUtf8Decoder::new();
        let text = "思考: xhigh\n";
        let bytes = text.as_bytes();
        let split_at = "思".len() + 1;

        let first = decoder.decode(&bytes[..split_at]);
        let second = decoder.decode(&bytes[split_at..]);

        assert_eq!(format!("{first}{second}"), text);
        assert_eq!(decoder.finish(), None);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     工作台打开终端时需要先按前端可见区域启动 PTY，避免 Claude Code 首屏按默认列宽绘制后错位。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言初始终端尺寸优先使用前端传入值，并对过小或缺失值回退到安全默认值。
    #[test]
    fn initial_terminal_size_uses_frontend_size_with_safe_minimums() {
        assert_eq!(initial_terminal_size(Some(140), Some(42)), (140, 42));
        assert_eq!(
            initial_terminal_size(Some(2), Some(1)),
            (MIN_TERMINAL_COLS, MIN_TERMINAL_ROWS),
        );
        assert_eq!(
            initial_terminal_size(None, None),
            (DEFAULT_COLS, DEFAULT_ROWS)
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户关闭终端或退出应用时，Claude Code 子进程可能已被系统回收，底层 kill 会返回 No such process。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 macOS/Linux 常见 ESRCH(os error 3)，断言终端 kill 归一化逻辑把它视为已停止。
    #[test]
    fn terminal_kill_treats_no_such_process_as_already_stopped() {
        let error = std::io::Error::from_raw_os_error(3);

        assert!(normalize_terminal_kill_result(Err(error)).is_ok());
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
