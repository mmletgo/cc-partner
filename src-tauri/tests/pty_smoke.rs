//! 原生 PTY echo/exit 冒烟测试。
//!
//! 覆盖跨平台 shell 输入构造、80×24 PTY 创建→echo token→exit 0，
//! 以及 Unix 进程组清理 / Windows detached creation flags 常量契约。
//! 不依赖 tmux / WSL / GUI。

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// PTY 读写与子进程退出的统一超时。
const PTY_SMOKE_TIMEOUT: Duration = Duration::from_secs(15);
/// xterm DSR「光标位置」查询；Windows ConPTY 在 TERM=xterm-256color 时启动就会发。
const CSI_CURSOR_POSITION_QUERY: &[u8] = b"\x1b[6n";
/// 合法 CPR 应答。行/列只要在窗口内即可，ConPTY 只要求格式正确才会继续读 stdin。
const CSI_CURSOR_POSITION_REPORT: &[u8] = b"\x1b[1;1R";

/// 平台 shell 描述：可执行文件 + 固定前缀参数。
///
/// Business Logic（为什么需要这个结构体）:
///     冒烟测试必须在 macOS/Linux 与 Windows 上用真实交互式 shell，且不能把任意用户输入拼进命令行。
///
/// Code Logic（这个结构体做什么）:
///     保存 program 与固定 args（如 cmd 的 /D /Q），供 CommandBuilder 原样使用。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlatformShell {
    program: String,
    args: Vec<String>,
}

/// Business Logic（为什么需要这个函数）:
///     冒烟 token 必须是字母数字，才能安全拼进 printf/echo 且避免 shell 元字符注入。
///
/// Code Logic（这个函数做什么）:
///     用当前时间纳秒与进程 pid 生成十六进制字符串（仅 [0-9a-f]）。
fn generate_alphanumeric_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", nanos, std::process::id())
}

/// Business Logic（为什么需要这个函数）:
///     输出中必须出现可断言的唯一标记，避免把 shell 提示符误判为成功。
///
/// Code Logic（这个函数做什么）:
///     把 token 包进固定前缀/后缀，形成 `__CC_PARTNER_<token>__`。
fn echo_marker(token: &str) -> String {
    format!("__CC_PARTNER_{token}__")
}

/// Business Logic（为什么需要这个函数）:
///     不同平台 shell 的行结束符不同，纯函数便于单测锁定契约。
///
/// Code Logic（这个函数做什么）:
///     Windows 返回 CRLF，其它平台返回 LF。
fn platform_newline() -> &'static str {
    if cfg!(windows) {
        "\r\n"
    } else {
        "\n"
    }
}

/// Business Logic（为什么需要这个函数）:
///     冒烟用例在缺少系统 shell 时应显式 skip，而不是 panic 成假失败。
///
/// Code Logic（这个函数做什么）:
///     Unix 要求 `/bin/sh` 存在；Windows 优先 `ComSpec`，否则探测 `cmd.exe` 常见路径。
fn resolve_platform_shell() -> Result<PlatformShell, String> {
    #[cfg(windows)]
    {
        let candidates = [
            std::env::var("ComSpec").ok(),
            Some(r"C:\Windows\System32\cmd.exe".to_string()),
            Some(r"C:\WINDOWS\system32\cmd.exe".to_string()),
        ];
        for candidate in candidates.into_iter().flatten() {
            let trimmed = candidate.trim();
            if trimmed.is_empty() {
                continue;
            }
            if Path::new(trimmed).exists() {
                return Ok(PlatformShell {
                    program: trimmed.to_string(),
                    args: vec!["/D".to_string(), "/Q".to_string()],
                });
            }
        }
        Err("skip: Windows cmd.exe / ComSpec 不可用，无法运行原生 PTY smoke".to_string())
    }
    #[cfg(not(windows))]
    {
        let path = "/bin/sh";
        if Path::new(path).exists() {
            Ok(PlatformShell {
                program: path.to_string(),
                args: Vec::new(),
            })
        } else {
            Err("skip: /bin/sh 不存在，无法运行原生 PTY smoke".to_string())
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     写入 PTY 的 echo/exit 脚本必须由受控 token 生成，禁止插值任意用户字符串。
///
/// Code Logic（这个函数做什么）:
///     Unix: `printf '__CC_PARTNER_<token>__\n'\nexit\n`；
///     Windows: `echo __CC_PARTNER_<token>__\r\nexit /b 0\r\n`。
fn build_echo_exit_input(token: &str) -> String {
    let marker = echo_marker(token);
    #[cfg(windows)]
    {
        format!("echo {marker}\r\nexit /b 0\r\n")
    }
    #[cfg(not(windows))]
    {
        format!("printf '{marker}\\n'\nexit\n")
    }
}

/// 去掉 CSI/OSC/简单 ESC，让 marker 扫描看到 ConPTY 真正打印的文本。
///
/// Business Logic（为什么需要这个函数）:
///     Windows ConPTY 会在 echo 输出前后夹 hide-cursor / CUP / OSC 标题，
///     直接按字节切行会把 marker 和提示符粘在同一“行”上。
///
/// Code Logic（这个函数做什么）:
///     ESC [ … 终字节(0x40–0x7E)、ESC ] … BEL/ST、其它 ESC+1 字节一律丢弃。
fn strip_terminal_control_sequences(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            if bytes[i] != 0x07 {
                out.push(bytes[i]);
            }
            i += 1;
            continue;
        }
        if i + 1 >= bytes.len() {
            break;
        }
        match bytes[i + 1] {
            b'[' => {
                i += 2;
                while i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    if (0x40..=0x7e).contains(&b) {
                        break;
                    }
                }
            }
            b']' => {
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            _ => i += 2,
        }
    }
    out
}

/// 一行是否是 echo 的结果行（不是 `echo MARKER` 命令回显）。
///
/// Business Logic（为什么需要这个函数）:
///     ConPTY 可能用 CUP 把下一行提示符叠到 marker 后面，整行不等于 marker，
///     但仍是真正的 echo 输出。
///
/// Code Logic（这个函数做什么）:
///     去尾空白后整行等于 marker，或 marker 出现在行首（前面只有空白）。
fn line_is_echo_result_marker(line: &str, marker: &str) -> bool {
    let trimmed = line.trim_end_matches([' ', '\t']);
    if trimmed == marker {
        return true;
    }
    let Some(idx) = trimmed.find(marker) else {
        return false;
    };
    trimmed[..idx].chars().all(|c| c == ' ' || c == '\t')
}

/// Business Logic（为什么需要这个函数）:
///     交互式 shell 会回显输入行，`printf '...marker...'` 命令本身就包含 marker 子串，
///     若用 contains 会在命令回显阶段误判成功并提前结束读循环。
///
/// Code Logic（这个函数做什么）:
///     先剥 CSI/OSC，再按 `\n`/`\r` 切行；命中 echo 结果行（行首 marker）才算成功。
fn output_contains_standalone_marker(buf: &[u8], marker: &str) -> bool {
    let stripped = strip_terminal_control_sequences(buf);
    let text = String::from_utf8_lossy(&stripped);
    text.split(['\n', '\r'])
        .any(|line| line_is_echo_result_marker(line, marker))
}

/// Business Logic（为什么需要这个函数）:
///     失败时需要把原始 PTY 输出挂到 panic，便于 CI 诊断，但控制字符会污染日志。
///
/// Code Logic（这个函数做什么）:
///     把非可打印字节转成 `\xNN` / 常见转义，保留可读 ASCII。
fn escape_bytes_for_diagnostic(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        match b {
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// Business Logic（为什么需要这个函数）:
///     PTY smoke 失败时若只 panic 到日志，CI 上传的 smoke root 可能没有文件证据；
///     需要在 `CC_PARTNER_SMOKE_ROOT` 下落盘 raw/escaped 输出。
///
/// Code Logic（这个函数做什么）:
///     若设置了 SMOKE_ROOT，创建唯一 case 目录并写 summary/raw/escaped；否则 no-op。
fn persist_pty_failure_diagnostics(reason: &str, marker: &str, output: &[u8]) {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    let Some(root) = std::env::var_os("CC_PARTNER_SMOKE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let case_dir = root.join(format!("pty-failure-{nanos}-{}", std::process::id()));
    let diag_dir = case_dir.join("diagnostics");
    if let Err(err) = fs::create_dir_all(&diag_dir) {
        eprintln!(
            "[pty-smoke] 创建 diagnostics 失败 path={} err={err}",
            diag_dir.display()
        );
        return;
    }
    let escaped = escape_bytes_for_diagnostic(output);
    let summary = format!(
        "reason={reason}\nmarker={marker}\noutput_len={}\ncase_dir={}\n",
        output.len(),
        case_dir.display()
    );
    let _ = fs::write(diag_dir.join("summary.txt"), summary);
    let _ = fs::write(diag_dir.join("output.raw"), output);
    let _ = fs::write(diag_dir.join("output.escaped.txt"), escaped);
    eprintln!(
        "[pty-smoke] 已写入 failure diagnostics path={}",
        diag_dir.display()
    );
}

/// Business Logic（为什么需要这个函数）:
///     失败清理时 `Child::wait` 可能因仍持有 slave 端而永久阻塞，冒烟必须有界。
///
/// Code Logic（这个函数做什么）:
///     先 kill，再轮询 try_wait 直到退出或超时；绝不调用无界 wait。
fn force_reap_child(child: &mut Box<dyn portable_pty::Child + Send + Sync>, timeout: Duration) {
    let _ = child.kill();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => return,
        }
    }
}

/// PTY 子进程 RAII 清理守卫：任意 panic/错误路径都 kill+有界 reap。
///
/// Business Logic（为什么需要这个结构）:
///     portable-pty Child Drop 不会自动杀进程；take_writer/try_clone_reader/write_all
///     等失败若直接 panic，会遗留 shell，workflow cleanup 也扫不到。
///
/// Code Logic（这个结构做什么）:
///     持有 child；`disarm` 表示测试已正常回收；Drop 时若仍 armed 则 force_reap。
struct PtyChildGuard {
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    armed: bool,
}

impl PtyChildGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     spawn 成功后必须立刻接管 child 生命周期。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装 child 并默认 armed=true。
    fn new(child: Box<dyn portable_pty::Child + Send + Sync>) -> Self {
        Self {
            child: Some(child),
            armed: true,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试主体需要可变借用 child 做 try_wait/kill。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 child 的可变引用；缺失时 panic（守卫构造后不应为空）。
    fn child_mut(&mut self) -> &mut Box<dyn portable_pty::Child + Send + Sync> {
        self.child
            .as_mut()
            .expect("PtyChildGuard 在 Drop 前应持有 child")
    }

    /// Business Logic（为什么需要这个函数）:
    ///     正常路径已确认子进程退出后，不应在 Drop 时再次 kill。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 armed 置 false。
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PtyChildGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     panic 或早期 return 时仍必须清理 shell 子进程。
    ///
    /// Code Logic（这个函数做什么）:
    ///     armed 时对 child force_reap（kill + 有界 try_wait）。
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(child) = self.child.as_mut() {
            force_reap_child(child, Duration::from_secs(2));
        }
    }
}

/// 持续 drain PTY 输出的共享缓冲。
///
/// Business Logic（为什么需要这个结构体）:
///     单 reader 必须贯穿 echo 与 exit 全程，停止读取会让交互式 shell 堵在 PTY 写缓冲。
///
/// Code Logic（这个结构体做什么）:
///     后台线程把 master 输出追加到 Mutex 缓冲，主线程只读快照做 marker/退出断言。
struct PtyOutputDrain {
    buffer: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    _join: Option<thread::JoinHandle<()>>,
}

impl PtyOutputDrain {
    /// Business Logic（为什么需要这个函数）:
    ///     冒烟启动后立刻接管 master 读端，避免输出积压阻塞 shell。
    ///
    /// Code Logic（这个函数做什么）:
    ///     启动后台 read 循环，直到 EOF 或 IO 错误；返回可 snapshot 的 drain 句柄。
    fn start(mut reader: Box<dyn Read + Send>) -> Self {
        use std::sync::{Arc, Mutex};

        let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
        let shared = Arc::clone(&buffer);
        let join = thread::spawn(move || {
            let mut chunk = [0u8; 1024];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut guard) = shared.lock() {
                            guard.extend_from_slice(&chunk[..n]);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });
        Self {
            buffer,
            _join: Some(join),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     主线程需要无锁拷贝当前输出，用于 marker 判定与失败诊断。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆 Mutex 内缓冲；锁失败时返回空 Vec。
    fn snapshot(&self) -> Vec<u8> {
        self.buffer.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

/// 统计缓冲里尚未处理的 CSI 6n 次数，供应答去重。
///
/// Business Logic（为什么需要这个函数）:
///     Windows ConPTY 可能连发多次光标查询；重复应答无害，漏应答会让 cmd 永久等 CPR。
///
/// Code Logic（这个函数做什么）:
///     滑窗统计 `\x1b[6n` 出现次数。
fn count_csi_cursor_position_queries(buf: &[u8]) -> usize {
    buf.windows(CSI_CURSOR_POSITION_QUERY.len())
        .filter(|window| *window == CSI_CURSOR_POSITION_QUERY)
        .count()
}

/// 把 ConPTY/xterm 的 DSR 查询答成 CPR，避免 shell 卡在启动握手。
///
/// Business Logic（为什么需要这个结构）:
///     工作台前端 xterm.js 会自动回 `\x1b[6n`；smoke 是 raw PTY master，必须自己扮终端。
///
/// Code Logic（这个结构做什么）:
///     按 drain 快照里未应答的 CSI 6n 次数，向 slave 写 `\x1b[1;1R`。
struct CursorPositionResponder {
    answered: usize,
}

impl CursorPositionResponder {
    /// Business Logic（为什么需要这个函数）:
    ///     每个 PTY 会话独立计数，避免跨测试串应答。
    ///
    /// Code Logic（这个函数做什么）:
    ///     answered 从 0 起。
    fn new() -> Self {
        Self { answered: 0 }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     查询可能在写入 echo 脚本前或后到达，主循环每次 poll 都要补应答。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对每个未应答的 CSI 6n 写一次 CPR 并 flush；写失败即停，留给上层超时诊断。
    fn pump(&mut self, drain: &PtyOutputDrain, writer: &mut dyn Write) {
        let seen = count_csi_cursor_position_queries(&drain.snapshot());
        while self.answered < seen {
            if writer.write_all(CSI_CURSOR_POSITION_REPORT).is_err() {
                return;
            }
            let _ = writer.flush();
            self.answered += 1;
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     必须在有界时间内看到独立成行 marker，否则 smoke 失败并附原始输出。
///
/// Code Logic（这个函数做什么）:
///     每次 poll 先应答未处理的 CSI 6n，再检查 standalone marker；超时返回 drain 快照。
fn wait_for_standalone_marker(
    drain: &PtyOutputDrain,
    marker: &str,
    timeout: Duration,
    writer: &mut dyn Write,
    responder: &mut CursorPositionResponder,
) -> Result<Vec<u8>, (Vec<u8>, String)> {
    let deadline = Instant::now() + timeout;
    loop {
        responder.pump(drain, writer);
        let buf = drain.snapshot();
        if output_contains_standalone_marker(&buf, marker) {
            return Ok(buf);
        }
        if Instant::now() >= deadline {
            return Err((
                buf.clone(),
                format!(
                    "timeout after {:?} waiting for standalone marker `{marker}`; output={}",
                    timeout,
                    escape_bytes_for_diagnostic(&buf)
                ),
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Business Logic（为什么需要这个函数）:
///     子进程必须在有界时间内以 0 退出，否则 smoke 失败并带诊断。
///
/// Code Logic（这个函数做什么）:
///     轮询 try_wait；每次 poll 继续应答 CSI 6n；超时 force_reap 并附上 drain 快照。
fn wait_child_exit_zero(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    drain: &PtyOutputDrain,
    timeout: Duration,
    writer: &mut dyn Write,
    responder: &mut CursorPositionResponder,
) -> Result<Vec<u8>, String> {
    let deadline = Instant::now() + timeout;
    loop {
        responder.pump(drain, writer);
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status.exit_code();
                let output = drain.snapshot();
                if code == 0 {
                    return Ok(output);
                }
                return Err(format!(
                    "child exited with code {code}; output={}",
                    escape_bytes_for_diagnostic(&output)
                ));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    force_reap_child(child, Duration::from_secs(2));
                    let output = drain.snapshot();
                    return Err(format!(
                        "timeout waiting for child exit; output={}",
                        escape_bytes_for_diagnostic(&output)
                    ));
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => {
                let output = drain.snapshot();
                return Err(format!(
                    "try_wait failed: {err}; output={}",
                    escape_bytes_for_diagnostic(&output)
                ));
            }
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     冒烟结束后必须确认子进程已消失，避免泄漏污染后续 case。
///
/// Code Logic（这个函数做什么）:
///     Unix 用 `kill -0`；Windows 用 `tasklist` 过滤 pid；其它平台返回 Ok 并说明无法探测。
fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::process::{Command, Stdio};
        Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        use std::process::{Command, Stdio};
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .map(|out| {
                let text = String::from_utf8_lossy(&out.stdout);
                text.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Business Logic（为什么需要这个函数）:
///     Unix 生产路径用 killpg 清理验证/探测进程组；smoke 需确认同组清理后无残留子进程。
///
/// Code Logic（这个函数做什么）:
///     调用 POSIX killpg(SIGKILL)；pgid 非法或系统错误时返回 io::Error。
#[cfg(unix)]
fn kill_unix_process_group(process_group_id: u32) -> Result<(), std::io::Error> {
    let pgid: libc::pid_t = process_group_id.try_into().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "process group id out of range",
        )
    })?;
    let result = unsafe { libc::killpg(pgid, libc::SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Business Logic（为什么需要这个测试）:
///     token→marker 的格式是跨平台断言契约，改动会让 CI 误判 echo 失败。
///
/// Code Logic（这个测试做什么）:
///     固定 token 断言 marker 与 newline helper 输出。
#[test]
fn marker_and_newline_helpers_are_stable() {
    assert_eq!(echo_marker("abc123"), "__CC_PARTNER_abc123__");
    if cfg!(windows) {
        assert_eq!(platform_newline(), "\r\n");
    } else {
        assert_eq!(platform_newline(), "\n");
    }
    // 命令回显含 marker 子串不得命中；printf 真正输出的独立行才算。
    let echoed_cmd = b"sh-3.2$ printf '__CC_PARTNER_abc123__\\n'\n";
    assert!(!output_contains_standalone_marker(
        echoed_cmd,
        "__CC_PARTNER_abc123__"
    ));
    let real_output = b"sh-3.2$ printf '__CC_PARTNER_abc123__\\n'\n__CC_PARTNER_abc123__\n";
    assert!(output_contains_standalone_marker(
        real_output,
        "__CC_PARTNER_abc123__"
    ));
    // Windows ConPTY 启动握手前缀不得挡住独立成行 marker。
    let with_dsr = b"\x1b[6n\r\n__CC_PARTNER_abc123__\r\n";
    assert!(output_contains_standalone_marker(
        with_dsr,
        "__CC_PARTNER_abc123__"
    ));
    // cmd 回显的 `echo MARKER` 不得算成功。
    let cmd_echo = b"C:\\Users\\runneradmin>echo __CC_PARTNER_abc123__\r\n";
    assert!(!output_contains_standalone_marker(
        cmd_echo,
        "__CC_PARTNER_abc123__"
    ));
    // windows-latest ConPTY：hide-cursor + marker + CUP 叠上下一提示符。
    let conpty_glued =
        b"\x1b[?25l__CC_PARTNER_abc123__\x1b[7;1HC:\\Users\\runneradmin>exit /b 0\r\n";
    assert!(output_contains_standalone_marker(
        conpty_glued,
        "__CC_PARTNER_abc123__"
    ));
}

/// Business Logic（为什么需要这个测试）:
///     必须按 hosted Windows ConPTY 的真实转义序列识别 echo 结果，避免再把成功当超时。
///
/// Code Logic（这个测试做什么）:
///     用 run 32871817645 的输出片段断言：命令回显不算，CUP 粘贴的结果行算。
#[test]
fn windows_conpty_glued_marker_is_detected() {
    let marker = "__CC_PARTNER_18cf1a46574f627c1838__";
    let output = b"\x1b[6n\x1b[?9001h\x1b[?1004h\x1b[m\x1b]0;C:\\Windows\\system32\\cmd.exe\x07\x1b[?25hMicrosoft Windows [Version 10.0.26100.33296]\r\n(c) Microsoft Corporation. All rights reserved.\r\n\x1b]0;Administrator: C:\\Windows\\system32\\cmd.exe\x07\r\nC:\\Users\\runneradmin>echo __CC_PARTNER_18cf1a46574f627c1838__\r\n\x1b[?25l__CC_PARTNER_18cf1a46574f627c1838__\x1b[7;1HC:\\Users\\runneradmin>exit /b 0\r\n\x1b]0;C:\\Windows\\system32\\cmd.exe\x07\x1b[?25h";
    assert!(
        output_contains_standalone_marker(output, marker),
        "stripped={}",
        escape_bytes_for_diagnostic(&strip_terminal_control_sequences(output))
    );
}

/// Business Logic（为什么需要这个测试）:
///     CSI 6n 计数是 Windows PTY 应答去重的契约，漏计会让 cmd 卡死，多计会灌多余 CPR。
///
/// Code Logic（这个测试做什么）:
///     覆盖空缓冲、单次、夹杂文本的两次查询，以及易混的 CSI 5n。
#[test]
fn csi_cursor_position_query_count_is_exact() {
    assert_eq!(count_csi_cursor_position_queries(b""), 0);
    assert_eq!(count_csi_cursor_position_queries(b"\x1b[6n"), 1);
    assert_eq!(
        count_csi_cursor_position_queries(b"hello\x1b[6nworld\x1b[6n"),
        2
    );
    assert_eq!(count_csi_cursor_position_queries(b"\x1b[5n"), 0);
    assert_eq!(
        count_csi_cursor_position_queries(CSI_CURSOR_POSITION_REPORT),
        0
    );
}

/// Business Logic（为什么需要这个测试）:
///     echo/exit 输入必须只由字母数字 token 驱动，防止 shell 注入与平台脚本漂移。
///
/// Code Logic（这个测试做什么）:
///     用固定 token 断言 Unix/Windows 脚本字节级内容。
#[test]
fn echo_exit_input_uses_controlled_token_only() {
    let token = "deadbeef";
    let input = build_echo_exit_input(token);
    assert!(input.contains("__CC_PARTNER_deadbeef__"));
    // token 本身不得含空白/元字符；脚本里的空格只允许出现在固定关键字处。
    assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
    #[cfg(windows)]
    {
        assert_eq!(input, "echo __CC_PARTNER_deadbeef__\r\nexit /b 0\r\n");
    }
    #[cfg(not(windows))]
    {
        assert_eq!(input, "printf '__CC_PARTNER_deadbeef__\\n'\nexit\n");
    }
}

/// Business Logic（为什么需要这个测试）:
///     token 生成器必须始终产出字母数字，才能安全进入 printf/echo。
///
/// Code Logic（这个测试做什么）:
///     生成 token 并断言每个字符都是 ASCII 字母数字。
#[test]
fn generated_token_is_alphanumeric() {
    let token = generate_alphanumeric_token();
    assert!(!token.is_empty());
    assert!(
        token.chars().all(|c| c.is_ascii_alphanumeric()),
        "token must be alphanumeric, got {token}"
    );
}

/// Business Logic（为什么需要这个测试）:
///     真实 runner 上必须证明 portable-pty 能驱动平台 shell 完成 echo 与干净退出。
///
/// Code Logic（这个测试做什么）:
///     80×24 打开 PTY，spawn 平台 shell，写入 echo+exit，读到 marker 后等待 exit 0；
///     失败时附带转义后的原始输出；结束后确认子进程已消失。
#[test]
fn native_pty_echo_token_and_exit_zero() {
    let shell = match resolve_platform_shell() {
        Ok(shell) => shell,
        Err(reason) => {
            eprintln!("{reason}");
            return;
        }
    };

    let token = generate_alphanumeric_token();
    let marker = echo_marker(&token);
    let input = build_echo_exit_input(&token);

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("创建 80x24 PTY 应成功");

    let mut cmd = CommandBuilder::new(&shell.program);
    for arg in &shell.args {
        cmd.arg(arg);
    }
    // 与 workbench 一致：避免继承 dumb TERM 导致异常控制序列。
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let child = pair
        .slave
        .spawn_command(cmd)
        .unwrap_or_else(|err| panic!("spawn {:?} 失败: {err}", shell.program));
    // spawn 成功后立刻安装 RAII 清理守卫：后续 take_writer/reader/write 失败 panic 也会回收 shell。
    let mut child_guard = PtyChildGuard::new(child);
    let child_pid = child_guard.child_mut().process_id();

    // 父进程必须释放 slave，否则 child 退出后 master 读端可能永远等不到 EOF。
    drop(pair.slave);

    let mut writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(err) => {
            let snap = Vec::new();
            persist_pty_failure_diagnostics(&format!("take_writer failed: {err}"), &marker, &snap);
            panic!("PTY writer 取出失败: {err}");
        }
    };
    let reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(err) => {
            drop(writer);
            let snap = Vec::new();
            persist_pty_failure_diagnostics(
                &format!("try_clone_reader failed: {err}"),
                &marker,
                &snap,
            );
            panic!("PTY reader clone 失败: {err}");
        }
    };
    // 单 reader 全程 drain，避免停止读后 PTY 缓冲堵死 shell。
    let drain = PtyOutputDrain::start(reader);
    let mut responder = CursorPositionResponder::new();

    // Windows ConPTY 常在首包就发 CSI 6n 并阻塞读 stdin；先应答再写 echo。
    // Unix 通常无此查询：等到短窗口结束再写，行为与原先 200ms 启动等待一致。
    let prewrite = if cfg!(windows) {
        Duration::from_secs(1)
    } else {
        Duration::from_millis(200)
    };
    let prewrite_deadline = Instant::now() + prewrite;
    loop {
        responder.pump(&drain, &mut writer);
        if cfg!(windows) && responder.answered > 0 {
            break;
        }
        if Instant::now() >= prewrite_deadline {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    if let Err(err) = writer.write_all(input.as_bytes()) {
        drop(writer);
        let snap = drain.snapshot();
        persist_pty_failure_diagnostics(&format!("write_all failed: {err}"), &marker, &snap);
        panic!(
            "写入 PTY 失败: {err}; output={}",
            escape_bytes_for_diagnostic(&snap)
        );
    }
    let _ = writer.flush();

    if let Err((buf, msg)) = wait_for_standalone_marker(
        &drain,
        &marker,
        PTY_SMOKE_TIMEOUT,
        &mut writer,
        &mut responder,
    ) {
        drop(writer);
        // Drop 守卫会 force_reap；此处先落盘诊断再 panic。
        persist_pty_failure_diagnostics(&format!("marker_timeout: {msg}"), &marker, &buf);
        panic!(
            "未读到独立成行 marker `{marker}`: {msg}; partial={}",
            escape_bytes_for_diagnostic(&buf)
        );
    }

    // 保持 writer 直到 exit 完成，避免交互式 shell 在处理 exit 前收到 stdin EOF。
    let output = match wait_child_exit_zero(
        child_guard.child_mut(),
        &drain,
        PTY_SMOKE_TIMEOUT,
        &mut writer,
        &mut responder,
    ) {
        Ok(buf) => {
            drop(writer);
            child_guard.disarm();
            buf
        }
        Err(err) => {
            drop(writer);
            let snap = drain.snapshot();
            persist_pty_failure_diagnostics(
                &format!("exit_timeout_or_nonzero: {err}"),
                &marker,
                &snap,
            );
            // 保持 armed，Drop 再做一次有界 reap。
            panic!("{err}");
        }
    };

    if let Some(pid) = child_pid {
        // 退出后短暂轮询，确认进程已回收。
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_is_alive(pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !process_is_alive(pid),
            "cleanup 后子进程 pid={pid} 仍存活; output={}",
            escape_bytes_for_diagnostic(&output)
        );
    }

    // 显式 drop master，避免 reader 线程在测试结束后仍持有句柄。
    drop(pair.master);

    println!(
        "pty smoke ok: shell={} marker={marker} output_len={}",
        shell.program,
        output.len()
    );
}

/// Business Logic（为什么需要这个测试）:
///     Unix 生产路径依赖独立进程组 + killpg；smoke 需证明 helper 目标是 spawn 出的组且清理后无子进程。
///
/// Code Logic（这个测试做什么）:
///     使用生产 seam `apply_unix_detached_pre_exec`（setsid）spawn `sleep 60`，
///     确认 pid 存活后 killpg，再断言进程消失。
#[cfg(unix)]
#[test]
fn unix_process_group_cleanup_leaves_no_child() {
    use std::process::{Command, Stdio};

    let mut command = Command::new("sleep");
    command
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // 必须走生产 detached seam，禁止测试内复制 setpgid/setsid。
    app_lib::backend::cli::apply_unix_detached_pre_exec(&mut command);
    let mut child = command
        .spawn()
        .expect("应能 spawn sleep 作为 detached session/进程组组长");
    let pid = child.id();
    assert!(process_is_alive(pid), "spawn 后 sleep 应存活");

    // setsid 后 child 是新 session/pg 组长，killpg(pid) 目标即该组。
    kill_unix_process_group(pid).expect("killpg 应能向 spawn 的进程组发 SIGKILL");

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("killpg 后 sleep 未在超时内退出");
            }
            Err(err) => panic!("try_wait 失败: {err}"),
        }
    }

    assert!(!process_is_alive(pid), "进程组清理后 pid={pid} 不应再存活");
}

/// Business Logic（为什么需要这个测试）:
///     Windows 冒烟矩阵要求验证 backend lifecycle 的 detached creation flags，而不是 WSL/tmux。
///
/// Code Logic（这个测试做什么）:
///     直接调用生产 seam `app_lib::backend::cli::windows_detached_creation_flags`，
///     断言仍为 DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP；生产改坏会让本测试失败。
#[cfg(windows)]
#[test]
fn windows_detached_creation_flags_match_backend_lifecycle() {
    // 必须绑定生产 helper，禁止在测试内复制字面量。
    let flags = app_lib::backend::cli::windows_detached_creation_flags();
    assert_eq!(
        flags, 0x00000208,
        "生产 windows_detached_creation_flags 必须是 DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP"
    );
    // 路径契约：Windows shell 解析必须能找到 cmd.exe（ComSpec 或系统路径）。
    let shell = resolve_platform_shell().expect("Windows smoke 需要可用的 cmd.exe");
    assert!(
        shell.program.to_ascii_lowercase().ends_with("cmd.exe"),
        "backend lifecycle Windows shell 应为 cmd.exe，实际 {}",
        shell.program
    );
    assert_eq!(shell.args, vec!["/D".to_string(), "/Q".to_string()]);
}

/// Business Logic（为什么需要这个测试）:
///     非 Windows runner 上 Windows lifecycle 断言 N/A，必须打印明确 skip reason（禁止静默）。
///
/// Code Logic（这个测试做什么）:
///     在非 Windows 上 eprintln 说明原因后直接返回。
#[cfg(not(windows))]
#[test]
fn windows_detached_lifecycle_skipped_on_non_windows() {
    eprintln!(
        "skip: Windows detached lifecycle flags 仅在 windows runner 验证（DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP）"
    );
}

/// Business Logic（为什么需要这个测试）:
///     非 Unix runner 上进程组清理 N/A，必须打印明确 skip reason。
///
/// Code Logic（这个测试做什么）:
///     在非 Unix 上 eprintln 说明原因后直接返回。
#[cfg(not(unix))]
#[test]
fn unix_process_group_lifecycle_skipped_on_non_unix() {
    eprintln!(
        "skip: Unix process-group lifecycle 仅在 macOS/Linux runner 验证（setpgid + killpg）"
    );
}
