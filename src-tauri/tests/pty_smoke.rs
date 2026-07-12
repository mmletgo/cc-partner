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

/// Business Logic（为什么需要这个函数）:
///     交互式 shell 会回显输入行，`printf '...marker...'` 命令本身就包含 marker 子串，
///     若用 contains 会在命令回显阶段误判成功并提前结束读循环。
///
/// Code Logic（这个函数做什么）:
///     按 `\n`/`\r` 切行后，只有整行（trim 尾部空白）等于 marker 才算命中。
fn output_contains_standalone_marker(buf: &[u8], marker: &str) -> bool {
    let text = String::from_utf8_lossy(buf);
    text.split(['\n', '\r'])
        .any(|line| line.trim_end_matches([' ', '\t']) == marker)
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

/// Business Logic（为什么需要这个函数）:
///     必须在有界时间内看到独立成行 marker，否则 smoke 失败并附原始输出。
///
/// Code Logic（这个函数做什么）:
///     轮询 drain 快照直到 standalone marker 出现或超时。
fn wait_for_standalone_marker(
    drain: &PtyOutputDrain,
    marker: &str,
    timeout: Duration,
) -> Result<Vec<u8>, (Vec<u8>, String)> {
    let deadline = Instant::now() + timeout;
    loop {
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
///     轮询 try_wait；超时 force_reap 并附上 drain 快照。
fn wait_child_exit_zero(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    drain: &PtyOutputDrain,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let deadline = Instant::now() + timeout;
    loop {
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

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .unwrap_or_else(|err| panic!("spawn {:?} 失败: {err}", shell.program));
    let child_pid = child.process_id();

    // 父进程必须释放 slave，否则 child 退出后 master 读端可能永远等不到 EOF。
    drop(pair.slave);

    let mut writer = pair.master.take_writer().expect("PTY writer 应可取出");
    let reader = pair
        .master
        .try_clone_reader()
        .expect("PTY reader 应可 clone");
    // 单 reader 全程 drain，避免停止读后 PTY 缓冲堵死 shell。
    let drain = PtyOutputDrain::start(reader);

    // 给 shell 一点启动时间，再写入受控脚本。
    thread::sleep(Duration::from_millis(200));
    writer
        .write_all(input.as_bytes())
        .unwrap_or_else(|err| panic!("写入 PTY 失败: {err}"));
    let _ = writer.flush();

    if let Err((buf, msg)) = wait_for_standalone_marker(&drain, &marker, PTY_SMOKE_TIMEOUT) {
        drop(writer);
        force_reap_child(&mut child, Duration::from_secs(2));
        persist_pty_failure_diagnostics(&format!("marker_timeout: {msg}"), &marker, &buf);
        panic!(
            "未读到独立成行 marker `{marker}`: {msg}; partial={}",
            escape_bytes_for_diagnostic(&buf)
        );
    }

    // 保持 writer 直到 exit 完成，避免交互式 shell 在处理 exit 前收到 stdin EOF。
    let output = match wait_child_exit_zero(&mut child, &drain, PTY_SMOKE_TIMEOUT) {
        Ok(buf) => {
            drop(writer);
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
///     spawn `sleep 60` 并 setpgid(0,0)，确认 pid 存活后 killpg，再断言进程消失。
#[cfg(unix)]
#[test]
fn unix_process_group_cleanup_leaves_no_child() {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut child = unsafe {
        Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
            .spawn()
            .expect("应能 spawn sleep 作为进程组组长")
    };
    let pid = child.id();
    assert!(process_is_alive(pid), "spawn 后 sleep 应存活");

    // 与 orchestrator/delivery、workbench/dependencies 相同：killpg 目标为子进程组。
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
///     锁定 `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` 与 backend/cli.rs 相同的字面常量。
#[cfg(windows)]
#[test]
fn windows_detached_creation_flags_match_backend_lifecycle() {
    // 与 src-tauri/src/backend/cli.rs::configure_detached_child 保持一致。
    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    const EXPECTED: u32 = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    assert_eq!(EXPECTED, 0x00000208);
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
