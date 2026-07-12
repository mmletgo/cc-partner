//! 跨平台 smoke 集成测试 harness。
//!
//! 提供隔离数据目录、超时轮询、CLI 输出捕获、按 PID 清理等共享能力，
//! 供 `backend_cli_smoke` 等 integration test 复用。

use serde::Deserialize;
use std::fs;
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// smoke 单次操作默认超时。
pub const DEFAULT_OP_TIMEOUT: Duration = Duration::from_secs(20);

/// 轮询间隔。
pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// CLI status JSON（与 `backend::cli` 输出契约对齐，不含 token）。
///
/// Business Logic（为什么需要这个结构）:
///     smoke 需要解析 `start|status|stop` 的机器可读 JSON，断言 kind/pid/port。
///
/// Code Logic（这个结构做什么）:
///     反序列化 camelCase `{kind, control?, error?}`；control 仅含 pid/port。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CliStatusJson {
    pub kind: String,
    pub control: Option<CliControlJson>,
    pub error: Option<String>,
}

/// CLI status 中的控制摘要。
///
/// Business Logic（为什么需要这个结构）:
///     断言 start/status 返回的 pid/port 与 control 文件、health 一致。
///
/// Code Logic（这个结构做什么）:
///     反序列化 `{pid, port}`。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CliControlJson {
    pub pid: u32,
    pub port: u16,
}

/// 磁盘上的 backend-control.json（完整字段）。
///
/// Business Logic（为什么需要这个结构）:
///     smoke 轮询 control 文件、构造 stale control、校验 token 路径隔离时需要完整字段。
///
/// Code Logic（这个结构做什么）:
///     反序列化 camelCase 控制文件；序列化时同样使用 camelCase。
#[derive(Debug, Clone, serde::Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ControlFileJson {
    pub pid: u32,
    pub port: u16,
    pub device_id: String,
    pub device_name: String,
    pub started_at: String,
    pub control_token: String,
}

/// CLI 一次调用的捕获结果。
///
/// Business Logic（为什么需要这个结构）:
///     失败时要把 stdout/stderr/exit 挂到诊断，避免 silent failure。
///
/// Code Logic（这个结构做什么）:
///     保存 exit status、stdout、stderr 原始文本。
#[derive(Debug, Clone)]
pub struct CapturedCli {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CapturedCli {
    /// Business Logic（为什么需要这个函数）:
    ///     测试断言失败时需要可读的 CLI 输出摘要。
    ///
    /// Code Logic（这个函数做什么）:
    ///     拼装 exit/stdout/stderr 的多行诊断字符串。
    pub fn diagnostic(&self) -> String {
        format!(
            "exit={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.code, self.stdout, self.stderr
        )
    }

    /// Business Logic（为什么需要这个函数）:
    ///     start/status 成功后需要解析 JSON 状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 stdout 取最后一行非空文本并反序列化为 `CliStatusJson`。
    pub fn parse_status_json(&self) -> Result<CliStatusJson, String> {
        let line = self
            .stdout
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .ok_or_else(|| format!("CLI stdout 无 JSON 行\n{}", self.diagnostic()))?;
        serde_json::from_str(line).map_err(|err| {
            format!(
                "解析 status JSON 失败: {err}\nline={line}\n{}",
                self.diagnostic()
            )
        })
    }
}

/// 单个 smoke case 的隔离环境与清理守卫。
///
/// Business Logic（为什么需要这个结构）:
///     每个 lifecycle case 必须使用独立 data dir/control 文件，失败时保留诊断，
///     成功且未要求 keep 时清理；teardown 只能 kill 本 case 记录的 PID。
///
/// Code Logic（这个结构做什么）:
///     创建唯一 case 根目录，设置 `CC_PARTNER_DATA_DIR`，记录 observed PID，
///     Drop 时先 CLI stop，再按 PID 有界 kill，最后按策略删除目录。
pub struct SmokeCase {
    pub name: String,
    pub case_dir: PathBuf,
    pub data_dir: PathBuf,
    pub backend_bin: PathBuf,
    pub op_timeout: Duration,
    keep: bool,
    failed: bool,
    recorded_pid: Option<u32>,
    _temp_root: Option<tempfile::TempDir>,
}

impl SmokeCase {
    /// Business Logic（为什么需要这个函数）:
    ///     smoke 必须在隔离目录写数据，不接触用户真实 `~/.cc-partner`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 `CC_PARTNER_SMOKE_ROOT` 或 tempfile 下创建唯一 case 目录与 data 子目录，
    ///     返回持有清理策略的守卫。
    pub fn new(name: &str) -> Result<Self, String> {
        Self::new_with_timeout(name, DEFAULT_OP_TIMEOUT)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     不同 case 可能需要不同操作超时，但共享同一隔离根规则。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成唯一 case 根，创建 data 子目录，记录 keep 策略与 backend 二进制路径。
    pub fn new_with_timeout(name: &str, op_timeout: Duration) -> Result<Self, String> {
        let keep = env_truthy("CC_PARTNER_SMOKE_KEEP");
        let backend_bin = PathBuf::from(env!("CARGO_BIN_EXE_cc-partner-backend"));
        if !backend_bin.exists() {
            return Err(format!(
                "backend 二进制不存在: {}（请先 cargo test --test backend_cli_smoke）",
                backend_bin.display()
            ));
        }

        let unique = unique_suffix();
        let (case_dir, temp_root) = match std::env::var_os("CC_PARTNER_SMOKE_ROOT") {
            Some(root) => {
                let root = PathBuf::from(root);
                fs::create_dir_all(&root).map_err(|e| format!("创建 SMOKE_ROOT 失败: {e}"))?;
                let case_dir = root.join(format!("{name}-{unique}"));
                fs::create_dir_all(&case_dir)
                    .map_err(|e| format!("创建 case 目录失败 {}: {e}", case_dir.display()))?;
                (case_dir, None)
            }
            None => {
                let temp = tempfile::Builder::new()
                    .prefix(&format!("cc-partner-smoke-{name}-"))
                    .tempdir()
                    .map_err(|e| format!("创建 tempfile 失败: {e}"))?;
                let case_dir = temp.path().to_path_buf();
                (case_dir, Some(temp))
            }
        };

        let data_dir = case_dir.join("data");
        fs::create_dir_all(&data_dir)
            .map_err(|e| format!("创建 data_dir 失败 {}: {e}", data_dir.display()))?;

        Ok(Self {
            name: name.to_string(),
            case_dir,
            data_dir,
            backend_bin,
            op_timeout,
            keep,
            failed: false,
            recorded_pid: None,
            _temp_root: temp_root,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     失败时需保留 case 目录供 CI 上传诊断。
    ///
    /// Code Logic（这个函数做什么）:
    ///     标记 failed=true，Drop 时不删除目录。
    pub fn mark_failed(&mut self) {
        self.failed = true;
    }

    /// Business Logic（为什么需要这个函数）:
    ///     teardown 只能 kill 本 case 启动过的 backend PID，禁止按进程名扫杀。
    ///
    /// Code Logic（这个函数做什么）:
    ///     记录最近一次观察到的 backend pid。
    pub fn record_pid(&mut self, pid: u32) {
        if pid != 0 {
            self.recorded_pid = Some(pid);
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     control 文件路径必须落在隔离 data_dir 下。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `<data_dir>/backend-control.json`。
    pub fn control_file_path(&self) -> PathBuf {
        self.data_dir.join("backend-control.json")
    }

    /// Business Logic（为什么需要这个函数）:
    ///     pid 文件与 control 文件并存，清理断言需要同时检查。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `<data_dir>/backend.pid`。
    pub fn pid_file_path(&self) -> PathBuf {
        self.data_dir.join("backend.pid")
    }

    /// Business Logic（为什么需要这个函数）:
    ///     所有 CLI 调用必须继承隔离 data dir，避免污染用户 home。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造指向 backend 二进制的 Command，注入 `CC_PARTNER_DATA_DIR`，
    ///     清空继承污染并捕获 stdout/stderr。
    pub fn backend_command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.backend_bin);
        cmd.args(args)
            .env("CC_PARTNER_DATA_DIR", &self.data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Business Logic（为什么需要这个函数）:
    ///     smoke 需要同步拿到 CLI 输出并解析 JSON；每个 CLI 调用必须有硬超时，
    ///     否则异常阻塞会拖到 job 超时，导致 cleanup/artifact 无法可靠执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保留 Child 句柄；侧线程分别 drain stdout/stderr，主线程 `try_wait` 轮询。
    ///     超时走 Child API kill + 有界 reap（禁止无界 `Command::status()` kill/taskkill），
    ///     再 join drain 线程收集已缓冲输出。
    pub fn run_cli(&self, args: &[&str]) -> Result<CapturedCli, String> {
        let mut child = self
            .backend_command(args)
            .spawn()
            .map_err(|e| format!("spawn {:?} 失败: {e}", args))?;
        let child_pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_drain = thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stdout {
                use std::io::Read;
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        });
        let stderr_drain = thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stderr {
                use std::io::Read;
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        });

        let deadline = Instant::now() + self.op_timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        // 只终止本 CLI 子进程；detached serve 由 control/PID teardown 管理。
                        let _ = child.kill();
                        let status = reap_child_with_timeout(
                            &mut child,
                            Duration::from_secs(2),
                            child_pid,
                            args,
                        )?;
                        let stdout = join_drain_with_timeout(stdout_drain, Duration::from_secs(1))
                            .unwrap_or_default();
                        let stderr = join_drain_with_timeout(stderr_drain, Duration::from_secs(1))
                            .unwrap_or_default();
                        let captured = captured_from_output(Output {
                            status,
                            stdout,
                            stderr,
                        });
                        return Err(format!(
                            "CLI {:?} 超时 ({}s)\n{}\ncase={}",
                            args,
                            self.op_timeout.as_secs(),
                            captured.diagnostic(),
                            self.case_dir.display()
                        ));
                    }
                    thread::sleep(POLL_INTERVAL);
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = reap_child_with_timeout(
                        &mut child,
                        Duration::from_secs(1),
                        child_pid,
                        args,
                    );
                    return Err(format!("try_wait {:?} 失败: {e}", args));
                }
            }
        };

        let stdout = join_drain_with_timeout(stdout_drain, Duration::from_secs(2))
            .map_err(|e| format!("stdout drain {:?}: {e}", args))?;
        let stderr = join_drain_with_timeout(stderr_drain, Duration::from_secs(2))
            .map_err(|e| format!("stderr drain {:?}: {e}", args))?;
        Ok(captured_from_output(Output {
            status,
            stdout,
            stderr,
        }))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     有些场景需要异步启动 CLI 并稍后 wait，但当前 smoke 以同步为主。
    ///
    /// Code Logic（这个函数做什么）:
    ///     spawn 后返回 Child，调用方负责 wait/kill。
    #[allow(dead_code)]
    pub fn spawn_cli(&self, args: &[&str]) -> Result<Child, String> {
        self.backend_command(args)
            .spawn()
            .map_err(|e| format!("spawn {:?} 失败: {e}", args))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     start 后 control 文件由 serve 异步写出，必须有界轮询。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 deadline 内轮询读取并解析 control 文件，超时返回诊断。
    pub fn wait_for_control_file(&self) -> Result<ControlFileJson, String> {
        let path = self.control_file_path();
        let deadline = Instant::now() + self.op_timeout;
        let mut last_err = String::from("control 文件尚未出现");
        while Instant::now() < deadline {
            match read_control_file(&path) {
                Ok(Some(control)) => return Ok(control),
                Ok(None) => last_err = format!("control 文件不存在: {}", path.display()),
                Err(err) => last_err = err,
            }
            thread::sleep(POLL_INTERVAL);
        }
        Err(format!(
            "等待 control 文件超时 ({}): {last_err}\ncase={}",
            self.op_timeout.as_secs(),
            self.case_dir.display()
        ))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     health 必须证明 control 端口上是真实 cc-partner 后端。
    ///
    /// Code Logic（这个函数做什么）:
    ///     有界轮询本机 GET `/api/health`（std TCP，避免额外 blocking HTTP 依赖），
    ///     成功且 JSON ok=true 即返回 body。
    pub fn wait_for_health(&self, port: u16) -> Result<serde_json::Value, String> {
        let deadline = Instant::now() + self.op_timeout;
        let mut last = String::from("尚未请求");
        while Instant::now() < deadline {
            match http_get_json(&format!("127.0.0.1:{port}"), "/api/health") {
                Ok(body) if body.get("ok").and_then(|v| v.as_bool()) == Some(true) => {
                    return Ok(body);
                }
                Ok(body) => last = format!("health body 非 ok: {body}"),
                Err(err) => last = err,
            }
            thread::sleep(POLL_INTERVAL);
        }
        Err(format!(
            "等待 /api/health 超时 port={port}: {last}\ncase={}",
            self.case_dir.display()
        ))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试结束必须尽量优雅停止本 case 的 backend。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 CLI stop；忽略已 stopped 的结果，失败时保留诊断字符串。
    pub fn cli_stop(&self) -> CapturedCli {
        match self.run_cli(&["stop"]) {
            Ok(captured) => captured,
            Err(err) => CapturedCli {
                success: false,
                code: None,
                stdout: String::new(),
                stderr: err,
            },
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     CLI stop 失败时只能按本 case 记录的 PID 有界 kill，绝不能按进程名扫杀。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 recorded_pid 发 SIGTERM/taskkill，轮询直到进程退出或超时，再 SIGKILL。
    pub fn force_kill_recorded_pid(&self) {
        let Some(pid) = self.recorded_pid else {
            return;
        };
        if !process_is_alive(pid) {
            return;
        }
        let _ = terminate_pid(pid);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if !process_is_alive(pid) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = kill_pid_hard(pid);
        let hard_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < hard_deadline {
            if !process_is_alive(pid) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     stop 后必须确认 control/pid 文件消失，避免污染下一 case。
    ///
    /// Code Logic（这个函数做什么）:
    ///     删除 control 与 pid 文件；不存在视为成功。
    pub fn remove_control_files(&self) -> Result<(), String> {
        for path in [self.control_file_path(), self.pid_file_path()] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(format!("删除 {} 失败: {err}", path.display())),
            }
        }
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     失败/超时后必须在 teardown 清掉 control 文件前落盘诊断，
    ///     否则 CI artifact 只剩空目录，无法复现 port/pid/control 状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 case_dir/diagnostics 写入 control 快照、pid 存活、环境与路径摘要。
    pub fn write_failure_diagnostics(&self, reason: &str) {
        let diag_dir = self.case_dir.join("diagnostics");
        if let Err(err) = fs::create_dir_all(&diag_dir) {
            eprintln!(
                "[smoke] 创建 diagnostics 目录失败 path={} err={err}",
                diag_dir.display()
            );
            return;
        }

        let control_path = self.control_file_path();
        let pid_path = self.pid_file_path();
        let control_raw = fs::read_to_string(&control_path).unwrap_or_default();
        let pid_raw = fs::read_to_string(&pid_path).unwrap_or_default();
        if !control_raw.is_empty() {
            let _ = fs::write(diag_dir.join("backend-control.json"), &control_raw);
        }
        if !pid_raw.is_empty() {
            let _ = fs::write(diag_dir.join("backend.pid"), &pid_raw);
        }

        let mut pid = self.recorded_pid.unwrap_or(0);
        let mut port = 0u16;
        if let Ok(Some(control)) = read_control_file(&control_path) {
            pid = control.pid;
            port = control.port;
        }
        let alive = if pid == 0 {
            false
        } else {
            process_is_alive(pid)
        };

        let summary = format!(
            "reason={reason}\n\
name={}\n\
case_dir={}\n\
data_dir={}\n\
backend_bin={}\n\
op_timeout_secs={}\n\
recorded_pid={:?}\n\
control_pid={pid}\n\
control_port={port}\n\
process_alive={alive}\n\
control_path={}\n\
pid_path={}\n\
CC_PARTNER_SMOKE_ROOT={}\n\
CC_PARTNER_SMOKE_KEEP={}\n\
RUST_BACKTRACE={}\n\
timestamp_unix_ns={}\n",
            self.name,
            self.case_dir.display(),
            self.data_dir.display(),
            self.backend_bin.display(),
            self.op_timeout.as_secs(),
            self.recorded_pid,
            control_path.display(),
            pid_path.display(),
            std::env::var("CC_PARTNER_SMOKE_ROOT").unwrap_or_default(),
            std::env::var("CC_PARTNER_SMOKE_KEEP").unwrap_or_default(),
            std::env::var("RUST_BACKTRACE").unwrap_or_default(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        if let Err(err) = fs::write(diag_dir.join("summary.txt"), summary) {
            eprintln!(
                "[smoke] 写 diagnostics/summary.txt 失败 path={} err={err}",
                diag_dir.display()
            );
        }

        // 进程/端口快照（best-effort，仅诊断，不用于扫杀）。
        let mut snapshot = String::from("--- process ---\n");
        #[cfg(unix)]
        {
            if pid != 0 {
                if let Ok(output) = Command::new("ps")
                    .args(["-p", &pid.to_string(), "-o", "pid,ppid,stat,command"])
                    .output()
                {
                    snapshot.push_str(&String::from_utf8_lossy(&output.stdout));
                    snapshot.push('\n');
                }
            }
            if port != 0 {
                if let Ok(output) = Command::new("lsof")
                    .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
                    .output()
                {
                    snapshot.push_str("--- lsof port ---\n");
                    snapshot.push_str(&String::from_utf8_lossy(&output.stdout));
                    snapshot.push('\n');
                }
            }
        }
        #[cfg(windows)]
        {
            if pid != 0 {
                if let Ok(output) = Command::new("tasklist")
                    .args(["/FI", &format!("PID eq {pid}")])
                    .output()
                {
                    snapshot.push_str(&String::from_utf8_lossy(&output.stdout));
                    snapshot.push('\n');
                }
            }
        }
        let _ = fs::write(diag_dir.join("process-port.txt"), snapshot);
        eprintln!(
            "[smoke] 已写入 failure diagnostics path={}",
            diag_dir.display()
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Drop 与显式 teardown 共用同一清理序列。
    ///
    /// Code Logic（这个函数做什么）:
    ///     CLI stop → 按 recorded PID kill → 删除 control 文件；目录保留策略见 Drop。
    pub fn teardown_processes(&mut self) {
        // 若 control 仍在，先尝试把 pid 记入 recorded，便于后续有界 kill。
        if self.recorded_pid.is_none() {
            if let Ok(Some(control)) = read_control_file(&self.control_file_path()) {
                self.record_pid(control.pid);
            }
        }
        let _ = self.cli_stop();
        // stop 后若仍存活，仅 kill 记录的 PID。
        if let Some(pid) = self.recorded_pid {
            if process_is_alive(pid) {
                self.force_kill_recorded_pid();
            }
        }
        let _ = self.remove_control_files();
    }
}

impl Drop for SmokeCase {
    /// Business Logic（为什么需要这个函数）:
    ///     panic 或测试结束时也必须清理本 case 的 backend，避免残留进程/端口；
    ///     失败/panic 时还要先落盘诊断再 teardown，保证 CI artifact 有证据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     若 keep/failed/panicking 则写 diagnostics；再 teardown；仅保留时留下 case 目录。
    fn drop(&mut self) {
        let panicking = std::thread::panicking();
        let retain = self.keep || self.failed || panicking;
        if retain {
            let reason = if self.failed {
                "marked_failed"
            } else if panicking {
                "panic_unwinding"
            } else {
                "keep_requested"
            };
            self.write_failure_diagnostics(reason);
        }
        self.teardown_processes();
        if retain {
            eprintln!(
                "[smoke] 保留 case 目录 name={} path={} keep={} failed={} panicking={}",
                self.name,
                self.case_dir.display(),
                self.keep,
                self.failed,
                panicking
            );
            // 防止 TempDir Drop 删除保留目录：泄漏 TempDir 所有权。
            if let Some(temp) = self._temp_root.take() {
                let _ = temp.keep();
            }
            return;
        }
        // tempfile::TempDir 会自动删；若使用 SMOKE_ROOT 则手动删。
        if self._temp_root.is_none() {
            let _ = fs::remove_dir_all(&self.case_dir);
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     smoke 需探测本机 `/api/health`，但 integration test 不引入 blocking HTTP 依赖。
///
/// Code Logic（这个函数做什么）:
///     用 std TcpStream 发最小 HTTP/1.1 GET，解析状态行与 body JSON。
fn http_get_json(host_port: &str, path: &str) -> Result<serde_json::Value, String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let mut stream =
        TcpStream::connect(host_port).map_err(|e| format!("连接 {host_port} 失败: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("设置读超时失败: {e}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("设置写超时失败: {e}"))?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("写 HTTP 请求失败: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("读 HTTP 响应失败: {e}"))?;
    let text = String::from_utf8_lossy(&raw);
    let (header, body) = text
        .split_once("\r\n\r\n")
        .or_else(|| text.split_once("\n\n"))
        .ok_or_else(|| format!("HTTP 响应无 header/body 分隔: {text}"))?;
    let status_line = header.lines().next().unwrap_or("");
    if !(status_line.contains(" 200 ") || status_line.ends_with(" 200")) {
        return Err(format!("HTTP 非 200: {status_line}"));
    }
    serde_json::from_str(body.trim())
        .map_err(|e| format!("解析 health JSON 失败: {e}\nbody={body}"))
}

/// Business Logic（为什么需要这个函数）:
///     将 `std::process::Output` 统一转成诊断友好结构。
///
/// Code Logic（这个函数做什么）:
///     解码 stdout/stderr 为有损 UTF-8 字符串。
pub fn captured_from_output(output: Output) -> CapturedCli {
    CapturedCli {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     smoke 读写磁盘 control 文件时需要统一解析。
///
/// Code Logic（这个函数做什么）:
///     文件不存在返回 Ok(None)；存在则反序列化为 `ControlFileJson`。
pub fn read_control_file(path: &Path) -> Result<Option<ControlFileJson>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)
        .map_err(|e| format!("读取 control 失败 {}: {e}", path.display()))?;
    let control = serde_json::from_str(&content)
        .map_err(|e| format!("解析 control 失败 {}: {e}\n{content}", path.display()))?;
    Ok(Some(control))
}

/// Business Logic（为什么需要这个函数）:
///     stale-control case 需要写入死 PID + 未使用端口的控制文件。
///
/// Code Logic（这个函数做什么）:
///     确保父目录存在，pretty JSON 写入 control，并同步写 pid 文件。
pub fn write_control_file(data_dir: &Path, control: &ControlFileJson) -> Result<(), String> {
    fs::create_dir_all(data_dir).map_err(|e| format!("创建 data_dir 失败: {e}"))?;
    let control_path = data_dir.join("backend-control.json");
    let pid_path = data_dir.join("backend.pid");
    let body =
        serde_json::to_string_pretty(control).map_err(|e| format!("序列化 control 失败: {e}"))?;
    fs::write(&control_path, body)
        .map_err(|e| format!("写 control 失败 {}: {e}", control_path.display()))?;
    fs::write(&pid_path, control.pid.to_string())
        .map_err(|e| format!("写 pid 失败 {}: {e}", pid_path.display()))?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     stale case 需要一个确定未使用的本地端口，避免误判为 running。
///
/// Code Logic（这个函数做什么）:
///     bind `127.0.0.1:0` 取系统分配端口后立即释放。
pub fn unused_local_port() -> Result<u16, String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("绑定临时端口失败: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("读取临时端口失败: {e}"))?
        .port();
    drop(listener);
    Ok(port)
}

/// Business Logic（为什么需要这个函数）:
///     stale case 需要一个确定已死亡的 PID。
///
/// Code Logic（这个函数做什么）:
///     spawn 立即退出的子进程并 wait，返回其 pid。
pub fn dead_pid() -> Result<u32, String> {
    #[cfg(unix)]
    {
        let mut child = Command::new("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn true 失败: {e}"))?;
        let pid = child.id();
        let _ = child.wait();
        // 确保内核已回收。
        thread::sleep(Duration::from_millis(20));
        if process_is_alive(pid) {
            return Err(format!("期望死 PID 仍存活: {pid}"));
        }
        Ok(pid)
    }
    #[cfg(windows)]
    {
        let mut child = Command::new("cmd")
            .args(["/C", "exit", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn cmd exit 失败: {e}"))?;
        let pid = child.id();
        let _ = child.wait();
        thread::sleep(Duration::from_millis(20));
        if process_is_alive(pid) {
            return Err(format!("期望死 PID 仍存活: {pid}"));
        }
        Ok(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err("skip: 当前平台无法生成 dead PID 用于 stale-control smoke".to_string())
    }
}

/// Business Logic（为什么需要这个函数）:
///     teardown 与断言都需要判断 PID 是否仍存活。
///
/// Code Logic（这个函数做什么）:
///     Unix 用 `kill -0`；Windows 用 `tasklist` 过滤 PID。
///     探测命令本身也走有界 spawn+try_wait，避免 probe 挂起拖死 smoke。
pub fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        let mut cmd = Command::new("kill");
        cmd.arg("-0")
            .arg(pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        run_signal_command_bounded(cmd, Duration::from_secs(2), &format!("kill -0 {pid}")).is_ok()
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("tasklist");
        cmd.args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        match spawn_and_collect_output_bounded(cmd, Duration::from_secs(2)) {
            Ok(output) => {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Business Logic（为什么需要这个函数）:
///     run_cli 超时后必须有界 reap child，避免无限 `wait` 卡住 smoke。
///
/// Code Logic（这个函数做什么）:
///     在 timeout 内轮询 `try_wait`；超时返回 Err，绝不调用无界 `wait`/`status`。
fn reap_child_with_timeout(
    child: &mut Child,
    timeout: Duration,
    pid: u32,
    args: &[&str],
) -> Result<std::process::ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    // 再补一次 Child::kill；仍不阻塞。
                    let _ = child.kill();
                    // 最后一次非阻塞 probe。
                    if let Ok(Some(status)) = child.try_wait() {
                        return Ok(status);
                    }
                    return Err(format!(
                        "CLI {:?} 超时后无法在 {:?} 内 reap (pid={pid})",
                        args, timeout
                    ));
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("CLI {:?} try_wait 失败: {e}", args)),
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     stdout/stderr drain 线程在子进程被 kill 后通常会很快结束，但仍需有界 join，
///     防止极端 pipe 状态让 smoke 永久卡住。
///
/// Code Logic（这个函数做什么）:
///     轮询 `JoinHandle::is_finished`；完成后 join 取 Vec；超时返回 Err。
fn join_drain_with_timeout(
    handle: thread::JoinHandle<Vec<u8>>,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            // 无法安全 abort 标准库线程；超时后丢弃 handle（detach）并返回空缓冲诊断。
            // 调用方用 unwrap_or_default 时得到空输出；此处返回 Err 让上层决定。
            std::mem::forget(handle);
            return Err(format!("drain 线程在 {timeout:?} 内未结束"));
        }
        thread::sleep(Duration::from_millis(10));
    }
    handle.join().map_err(|_| "drain 线程 panic".to_string())
}

/// Business Logic（为什么需要这个函数）:
///     优雅停止失败后需要先发可恢复的终止信号。
///
/// Code Logic（这个函数做什么）:
///     Unix SIGTERM；Windows taskkill 不带 /F。所有 kill/taskkill 经有界 spawn+try_wait，
///     禁止 `Command::status()` 无界阻塞。
fn terminate_pid(pid: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut cmd = Command::new("kill");
        cmd.arg("-TERM")
            .arg(pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        run_signal_command_bounded(cmd, Duration::from_secs(2), &format!("kill -TERM {pid}"))
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        run_signal_command_bounded(cmd, Duration::from_secs(2), &format!("taskkill {pid}"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        Ok(())
    }
}

/// Business Logic（为什么需要这个函数）:
///     有界等待后仍存活的测试 PID 必须强杀，避免污染后续 case。
///
/// Code Logic（这个函数做什么）:
///     Unix SIGKILL；Windows `taskkill /F`。经有界 spawn+try_wait，禁止无界 `status()`。
fn kill_pid_hard(pid: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut cmd = Command::new("kill");
        cmd.arg("-KILL")
            .arg(pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        run_signal_command_bounded(cmd, Duration::from_secs(2), &format!("kill -KILL {pid}"))
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        run_signal_command_bounded(cmd, Duration::from_secs(2), &format!("taskkill /F {pid}"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        Ok(())
    }
}

/// Business Logic（为什么需要这个函数）:
///     kill/taskkill 本身也可能挂起；smoke 清理路径必须全程有界。
///
/// Code Logic（这个函数做什么）:
///     spawn 信号命令后在 timeout 内 `try_wait`；超时 `Child::kill` 再 probe，
///     成功退出码非 0 时返回 error（进程已死时 kill 也可能非 0，调用方通常忽略）。
fn run_signal_command_bounded(
    mut command: Command,
    timeout: Duration,
    label: &str,
) -> io::Result<()> {
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                return Err(io::Error::other(format!("{label} 失败: {status}")));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    // 短 grace 再 probe，仍不阻塞。
                    let grace = Instant::now() + Duration::from_millis(200);
                    while Instant::now() < grace {
                        if let Ok(Some(_)) = child.try_wait() {
                            break;
                        }
                        thread::sleep(Duration::from_millis(20));
                    }
                    return Err(io::Error::other(format!("{label} 在 {timeout:?} 内未结束")));
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => return Err(err),
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     Windows `tasklist` 等 probe 需要读 stdout，同样不能用无界 `output()`/`status()`。
///
/// Code Logic（这个函数做什么）:
///     spawn 后侧线程 drain stdout，主线程有界 `try_wait`；超时 kill+reap 并返回 error。
#[cfg(windows)]
fn spawn_and_collect_output_bounded(mut command: Command, timeout: Duration) -> io::Result<Output> {
    let mut child = command.spawn()?;
    let stdout = child.stdout.take();
    let drain = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stdout {
            use std::io::Read;
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let grace = Instant::now() + Duration::from_millis(200);
                    while Instant::now() < grace {
                        if let Ok(Some(status)) = child.try_wait() {
                            let stdout = join_drain_with_timeout(drain, Duration::from_millis(200))
                                .unwrap_or_default();
                            return Ok(Output {
                                status,
                                stdout,
                                stderr: Vec::new(),
                            });
                        }
                        thread::sleep(Duration::from_millis(20));
                    }
                    return Err(io::Error::other(format!(
                        "probe 命令在 {timeout:?} 内未结束"
                    )));
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => return Err(err),
        }
    };
    let stdout = join_drain_with_timeout(drain, Duration::from_secs(1)).unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

/// Business Logic（为什么需要这个函数）:
///     并行/重复运行时 case 目录名必须唯一。
///
/// Code Logic（这个函数做什么）:
///     用时间纳秒 + pid 生成后缀。
fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{}", std::process::id())
}

/// Business Logic（为什么需要这个函数）:
///     keep 开关用真值语义，避免只认 `"1"`。
///
/// Code Logic（这个函数做什么）:
///     环境变量为 1/true/yes/on（忽略大小写）时返回 true。
fn env_truthy(key: &str) -> bool {
    match std::env::var(key) {
        Ok(value) => {
            let v = value.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        }
        Err(_) => false,
    }
}

/// Business Logic（为什么需要这个函数）:
///     平台若完全无法运行 CLI smoke，必须显式说明原因而不是静默 skip。
///
/// Code Logic（这个函数做什么）:
///     当前 unix/windows 返回 Ok；其它平台返回 skip reason。
pub fn ensure_platform_supported() -> Result<(), String> {
    if cfg!(any(unix, windows)) {
        Ok(())
    } else {
        Err("skip: 当前平台非 unix/windows，无法运行 backend CLI lifecycle smoke".to_string())
    }
}
