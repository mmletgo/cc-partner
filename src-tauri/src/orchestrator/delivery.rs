//! Orchestrator 验证与交付入口。
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent 完成后需要在任务 worktree 中执行项目验证命令，并把输出作为 evidence 保存。
//!
//! Code Logic（这个模块做什么）:
//!     提供验证命令执行 helper；后续 Task 7 交付流水线不得放在本模块本轮实现中。

use crate::error::AppError;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

const DEFAULT_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const PROCESS_REAP_GRACE_TIMEOUT: Duration = Duration::from_millis(200);
const PIPE_READER_JOIN_GRACE_TIMEOUT: Duration = Duration::from_millis(200);
const OUTPUT_TRUNCATED_MARKER: &str = "[output truncated]";
#[cfg(unix)]
const SIGKILL: std::os::raw::c_int = 9;

#[cfg(unix)]
unsafe extern "C" {
    fn killpg(pgrp: std::os::raw::c_int, sig: std::os::raw::c_int) -> std::os::raw::c_int;
}

/// 单条验证命令的 shell 调用规格。
///
/// Business Logic（为什么需要这个结构体）:
///     验证命令需要跨平台构造不同 shell 调用，同时测试要能稳定断言程序和参数。
///
/// Code Logic（这个结构体做什么）:
///     保存将要传给 tokio::process::Command 的 program 与 args。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellCommandSpec {
    program: String,
    args: Vec<String>,
}

/// 单条验证命令的执行结果。
///
/// Business Logic（为什么需要这个结构体）:
///     验证 evidence 需要同时展示命令退出状态、stdout/stderr 和是否截断，失败错误也复用同一份格式化输出。
///
/// Code Logic（这个结构体做什么）:
///     保存已按上限截断并转成 UTF-8 文本的 stdout/stderr、退出状态和截断标记。
struct VerificationCommandOutput {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    truncated: bool,
}

/// 单个输出流的受限读取结果。
///
/// Business Logic（为什么需要这个结构体）:
///     stdout/stderr 需要边读边丢弃超出预算的内容，避免验证命令产生海量输出时撑爆内存或 evidence。
///
/// Code Logic（这个结构体做什么）:
///     保存当前流在共享预算内保留下来的字节，以及该流是否发生截断。
struct LimitedPipeOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Business Logic（为什么需要这个函数）:
///     Agent 声称完成后，系统需要在对应 worktree 中执行项目配置的验证命令，并把输出保存为 evidence。
///
/// Code Logic（这个函数做什么）:
///     逐条用平台 shell 在 cwd 中执行命令，成功时返回包含命令、stdout、stderr 的合并文本；
///     任一命令非零退出时返回包含失败命令和输出的 AppError。
pub async fn run_verification_commands(
    cwd: &Path,
    commands: &[String],
) -> Result<String, AppError> {
    run_verification_commands_with_limits(
        cwd,
        commands,
        DEFAULT_VERIFICATION_TIMEOUT,
        DEFAULT_MAX_OUTPUT_BYTES,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     测试和未来配置需要能用短 timeout/小输出上限验证行为，而生产入口仍使用安全默认值。
///
/// Code Logic（这个函数做什么）:
///     逐条执行验证命令；每条命令应用 timeout 和 stdout+stderr 总量截断，非零退出返回包含截断输出的 AppError。
pub async fn run_verification_commands_with_limits(
    cwd: &Path,
    commands: &[String],
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<String, AppError> {
    let mut combined = String::new();
    for command in commands {
        let output = run_shell_command(cwd, command, timeout, max_output_bytes).await?;
        let section = format_verification_output(command, &output);
        combined.push_str(&section);
        combined.push('\n');
        if !output.status.success() {
            return Err(AppError::generic(format!(
                "验证命令失败: {command}\n{section}"
            )));
        }
    }
    Ok(combined)
}

/// Business Logic（为什么需要这个函数）:
///     用户配置的验证命令需要支持 shell 语法，如重定向、管道和环境变量展开。
///
/// Code Logic（这个函数做什么）:
///     macOS/Linux 优先使用 `$SHELL -lc`，空值回退 `sh -lc`；Windows 使用 `cmd /C`；
///     Unix/macOS 把 shell 放入独立进程组；子进程设置 kill_on_drop 兜底，stdout/stderr 由后台任务流式读取并共享总预算；
///     wait 由 timeout 包裹，超时终止进程树并给 reader 一个短 grace 后 abort。
async fn run_shell_command(
    cwd: &Path,
    command: &str,
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<VerificationCommandOutput, AppError> {
    let shell_command = build_shell_command(command);
    let mut child = Command::new(&shell_command.program);
    child.args(&shell_command.args);
    child
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    child.kill_on_drop(true);
    #[cfg(unix)]
    child.process_group(0);
    let mut process = child
        .spawn()
        .map_err(|err| AppError::generic(format!("启动验证命令失败: {command}: {err}")))?;
    let process_group_id = process.id();

    let stdout = process
        .stdout
        .take()
        .ok_or_else(|| AppError::generic(format!("捕获验证命令 stdout 失败: {command}")))?;
    let stderr = process
        .stderr
        .take()
        .ok_or_else(|| AppError::generic(format!("捕获验证命令 stderr 失败: {command}")))?;
    let remaining_budget = Arc::new(Mutex::new(max_output_bytes));
    let stdout_task = spawn_limited_pipe_reader(stdout, remaining_budget.clone());
    let stderr_task = spawn_limited_pipe_reader(stderr, remaining_budget);

    let status = match tokio::time::timeout(timeout, process.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(err)) => {
            stdout_task.abort();
            stderr_task.abort();
            return Err(AppError::generic(format!(
                "执行验证命令失败: {command}: {err}"
            )));
        }
        Err(_) => {
            terminate_shell_process_tree(&mut process, process_group_id).await;
            let _ = join_limited_pipe_output_with_grace(
                "stdout",
                stdout_task,
                PIPE_READER_JOIN_GRACE_TIMEOUT,
            )
            .await;
            let _ = join_limited_pipe_output_with_grace(
                "stderr",
                stderr_task,
                PIPE_READER_JOIN_GRACE_TIMEOUT,
            )
            .await;
            return Err(AppError::generic(format!(
                "验证命令超时: {command}（timeout={}秒）",
                timeout.as_secs_f64()
            )));
        }
    };

    let stdout_output = join_limited_pipe_output("stdout", stdout_task).await?;
    let stderr_output = join_limited_pipe_output("stderr", stderr_task).await?;
    let truncated = stdout_output.truncated || stderr_output.truncated;
    Ok(VerificationCommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout_output.bytes).to_string(),
        stderr: String::from_utf8_lossy(&stderr_output.bytes).to_string(),
        truncated,
    })
}

/// Business Logic（为什么需要这个函数）:
///     验证命令超时时，watch/dev server 等孙进程可能继续持有 stdout/stderr pipe，必须尽量终止整棵进程树。
///
/// Code Logic（这个函数做什么）:
///     Unix/macOS 先向独立进程组发送 SIGKILL，再对直接子进程执行 start_kill 兜底；所有平台都只等待短 grace 回收进程。
async fn terminate_shell_process_tree(process: &mut Child, process_group_id: Option<u32>) {
    #[cfg(unix)]
    if let Some(process_group_id) = process_group_id {
        let _ = kill_unix_process_group(process_group_id);
    }
    #[cfg(not(unix))]
    let _ = process_group_id;

    let _ = process.start_kill();
    let _ = tokio::time::timeout(PROCESS_REAP_GRACE_TIMEOUT, process.wait()).await;
}

/// Business Logic（为什么需要这个函数）:
///     Unix/macOS 上验证 shell 使用独立进程组，超时时需要用进程组信号覆盖后台子进程。
///
/// Code Logic（这个函数做什么）:
///     把 tokio 返回的子进程 pid 转成平台 c_int，并调用 POSIX killpg 发送 SIGKILL。
#[cfg(unix)]
fn kill_unix_process_group(process_group_id: u32) -> Result<(), std::io::Error> {
    let pgid: std::os::raw::c_int = process_group_id.try_into().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "process group id out of range",
        )
    })?;
    let result = unsafe { killpg(pgid, SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Business Logic（为什么需要这个函数）:
///     stdout 与 stderr 必须并发读取，避免某个 pipe 写满后阻塞子进程，导致验证命令假死。
///
/// Code Logic（这个函数做什么）:
///     为任意 AsyncRead pipe 启动 tokio 任务，调用 read_limited_pipe 按共享预算保存输出。
fn spawn_limited_pipe_reader<R>(
    reader: R,
    remaining_budget: Arc<Mutex<usize>>,
) -> JoinHandle<Result<LimitedPipeOutput, std::io::Error>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(read_limited_pipe(reader, remaining_budget))
}

/// Business Logic（为什么需要这个函数）:
///     验证流程需要把输出读取任务的 panic/IO 错误转换成 AppError，避免后台 join 错误泄漏为不清晰失败。
///
/// Code Logic（这个函数做什么）:
///     await JoinHandle，分别处理任务 join 失败和 pipe 读取 IO 失败，并补充 stdout/stderr 名称。
async fn join_limited_pipe_output(
    stream_name: &str,
    task: JoinHandle<Result<LimitedPipeOutput, std::io::Error>>,
) -> Result<LimitedPipeOutput, AppError> {
    task.await
        .map_err(|err| AppError::generic(format!("读取验证命令 {stream_name} 任务失败: {err}")))?
        .map_err(|err| AppError::generic(format!("读取验证命令 {stream_name} 失败: {err}")))
}

/// Business Logic（为什么需要这个函数）:
///     超时分支不能无限等待 stdout/stderr reader；即便管道仍被孙进程持有，也要快速让任务退出 Verifying。
///
/// Code Logic（这个函数做什么）:
///     对 JoinHandle 设置短 grace timeout；reader 正常结束则复用 join 错误映射，grace 超时则 abort 任务并返回 AppError。
async fn join_limited_pipe_output_with_grace(
    stream_name: &str,
    mut task: JoinHandle<Result<LimitedPipeOutput, std::io::Error>>,
    grace_timeout: Duration,
) -> Result<LimitedPipeOutput, AppError> {
    match tokio::time::timeout(grace_timeout, &mut task).await {
        Ok(result) => result
            .map_err(|err| {
                AppError::generic(format!("读取验证命令 {stream_name} 任务失败: {err}"))
            })?
            .map_err(|err| AppError::generic(format!("读取验证命令 {stream_name} 失败: {err}"))),
        Err(_) => {
            task.abort();
            Err(AppError::generic(format!(
                "读取验证命令 {stream_name} 超时"
            )))
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     验证命令输出需要在读取过程中执行预算限制，而不是等命令结束后再截断完整缓冲。
///
/// Code Logic（这个函数做什么）:
///     循环读取 pipe；每个 chunk 只把共享剩余预算内的字节追加到结果，超出部分继续读取但丢弃并标记 truncated。
async fn read_limited_pipe<R>(
    mut reader: R,
    remaining_budget: Arc<Mutex<usize>>,
) -> Result<LimitedPipeOutput, std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];

    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }

        let mut remaining = remaining_budget.lock().await;
        let keep = (*remaining).min(read);
        if keep > 0 {
            bytes.extend_from_slice(&buffer[..keep]);
            *remaining -= keep;
        }
        if keep < read {
            truncated = true;
        }
    }

    Ok(LimitedPipeOutput { bytes, truncated })
}

/// Business Logic（为什么需要这个函数）:
///     验证 evidence 和失败错误必须使用同一种文本格式，确保成功、失败、截断时前端展示一致。
///
/// Code Logic（这个函数做什么）:
///     格式化命令、exit、stdout、stderr；若输出被截断，在段落末尾追加固定 marker。
fn format_verification_output(command: &str, output: &VerificationCommandOutput) -> String {
    let mut section = format!(
        "$ {command}\nexit: {}\nstdout:\n{}\nstderr:\n{}\n",
        output.status, output.stdout, output.stderr
    );
    if output.truncated {
        section.push_str(OUTPUT_TRUNCATED_MARKER);
        section.push('\n');
    }
    section
}

/// Business Logic（为什么需要这个函数）:
///     Windows 用户的验证命令应沿用系统 cmd 语义，不受 Unix 用户 shell 逻辑影响。
///
/// Code Logic（这个函数做什么）:
///     构造 `cmd /C <command>` 的执行规格。
#[cfg(windows)]
fn build_shell_command(command: &str) -> ShellCommandSpec {
    ShellCommandSpec {
        program: "cmd".to_string(),
        args: vec!["/C".to_string(), command.to_string()],
    }
}

/// Business Logic（为什么需要这个函数）:
///     Unix/macOS 验证命令应复用用户登录 shell，以便加载用户 shell 可解析的配置和语法。
///
/// Code Logic（这个函数做什么）:
///     读取 SHELL 环境变量并交给纯 helper 归一化，构造 `<shell> -lc <command>`。
#[cfg(not(windows))]
fn build_shell_command(command: &str) -> ShellCommandSpec {
    build_shell_command_with_shell(command, std::env::var("SHELL").ok().as_deref())
}

/// Business Logic（为什么需要这个函数）:
///     Unix/macOS shell 选择需要可测试，避免单测依赖当前开发机真实 SHELL。
///
/// Code Logic（这个函数做什么）:
///     对传入 shell 环境值 trim；非空使用该 shell，否则回退 `sh`，参数固定为 `-lc <command>`。
#[cfg(not(windows))]
fn build_shell_command_with_shell(command: &str, shell_env: Option<&str>) -> ShellCommandSpec {
    let program = shell_env
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("sh")
        .to_string();
    ShellCommandSpec {
        program,
        args: vec!["-lc".to_string(), command.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个函数）:
    ///     验证命令成功时需要返回 stdout/stderr 合并输出，作为任务 evidence 展示给用户。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在临时目录中运行一条输出命令，断言结果包含命令文本和 stdout。
    #[tokio::test]
    async fn successful_command_returns_combined_output() {
        let dir = tempfile::tempdir().expect("tempdir");

        let output = run_verification_commands(dir.path(), &["printf success".to_string()])
            .await
            .expect("verification output");

        assert!(output.contains("$ printf success"));
        assert!(output.contains("stdout"));
        assert!(output.contains("success"));
        assert!(output.contains("stderr"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证命令失败时必须把失败命令与输出放进错误，方便 blocked UI 告知用户原因。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行一条非零退出命令，断言错误消息包含命令文本与 stderr 输出。
    #[tokio::test]
    async fn failing_command_error_contains_command_and_output() {
        let dir = tempfile::tempdir().expect("tempdir");

        let error =
            run_verification_commands(dir.path(), &["printf failure >&2; exit 7".to_string()])
                .await
                .expect_err("verification should fail");
        let message = error.to_string();

        assert!(message.contains("printf failure"));
        assert!(message.contains("failure"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证命令可能卡住，任务不能无限停留在 Verifying，超时需要终止子进程并返回可展示错误。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用极短 timeout 执行跨平台 sleep 命令，断言错误包含原命令和 timeout 信息。
    #[tokio::test]
    async fn verification_command_timeout_returns_error_with_command_and_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");

        let error = run_verification_commands_with_limits(
            dir.path(),
            &[sleep_command()],
            std::time::Duration::from_millis(50),
            1024,
        )
        .await
        .expect_err("sleep should timeout");
        let message = error.to_string();

        assert!(message.contains("timeout") || message.contains("超时"));
        assert!(message.contains("sleep") || message.contains("ping"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证命令超时时，即使命令启动的后台子进程继续持有 stdout/stderr pipe，任务也不能卡在 Verifying。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 Unix/macOS 上启动继承 pipe 的后台 sleep，并用外层短 timeout 断言验证 helper 自身会快速返回超时错误。
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_returns_even_when_child_process_keeps_pipe_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = "sleep 5 & echo child-started; wait".to_string();

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(800),
            run_verification_commands_with_limits(
                dir.path(),
                &[command.clone()],
                std::time::Duration::from_millis(50),
                1024,
            ),
        )
        .await;

        let error = result
            .expect("timeout branch should not wait for inherited pipe EOF")
            .expect_err("verification should return a timeout error");
        let message = error.to_string();

        assert!(message.contains("timeout") || message.contains("超时"));
        assert!(message.contains(&command));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证命令可能输出大量日志，evidence 需要有大小上限，避免 SQLite 和前端详情页被巨量文本拖垮。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行跨平台大输出命令并设置小输出上限，断言成功输出被截断且带 truncated 标记。
    #[tokio::test]
    async fn verification_command_output_is_truncated_with_marker() {
        let dir = tempfile::tempdir().expect("tempdir");

        let output = run_verification_commands_with_limits(
            dir.path(),
            &[large_output_command()],
            std::time::Duration::from_secs(5),
            64,
        )
        .await
        .expect("large output command should succeed");

        assert!(output.contains("[output truncated]"));
        assert!(output.len() < 512);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     超时测试需要一条不会依赖 Unix 工具的 Windows 等价命令，保证 CI 多平台稳定。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Windows 用 ping 本机延迟，Unix/macOS 用 sleep，返回 shell 可执行字符串。
    #[cfg(test)]
    fn sleep_command() -> String {
        if cfg!(windows) {
            "ping 127.0.0.1 -n 3 >NUL".to_string()
        } else {
            "sleep 2".to_string()
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     输出截断测试需要稳定制造超过上限的 stdout，且不依赖项目外部文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Windows 用 powershell 输出重复字符，Unix/macOS 用 yes+head 生成有限大输出。
    #[cfg(test)]
    fn large_output_command() -> String {
        if cfg!(windows) {
            "powershell -NoProfile -Command \"Write-Output ('x' * 2048)\"".to_string()
        } else {
            "yes x | head -n 2048".to_string()
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Unix/macOS 用户常在 zsh/bash/fish 中配置项目验证所需环境，验证命令应优先复用 `$SHELL`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     通过纯 helper 注入 shell 环境值，断言构造出的命令使用 trim 后的用户 shell 和 `-lc`。
    #[cfg(not(windows))]
    #[test]
    fn unix_shell_command_prefers_user_shell_env() {
        let command = build_shell_command_with_shell("cargo test", Some("  /bin/zsh  "));

        assert_eq!(command.program, "/bin/zsh");
        assert_eq!(
            command.args,
            vec!["-lc".to_string(), "cargo test".to_string()]
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户 shell 环境缺失或为空时仍需能运行验证命令，避免后台环境不完整导致验证入口不可用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     通过纯 helper 注入空白和缺失 shell 值，断言 Unix/macOS 回退到 `sh -lc`。
    #[cfg(not(windows))]
    #[test]
    fn unix_shell_command_falls_back_to_sh_when_shell_env_is_blank_or_missing() {
        let blank = build_shell_command_with_shell("cargo test", Some("  "));
        let missing = build_shell_command_with_shell("cargo test", None);

        assert_eq!(blank.program, "sh");
        assert_eq!(
            blank.args,
            vec!["-lc".to_string(), "cargo test".to_string()]
        );
        assert_eq!(missing.program, "sh");
        assert_eq!(
            missing.args,
            vec!["-lc".to_string(), "cargo test".to_string()]
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Windows 仍应使用 cmd 执行项目验证命令，避免 Unix shell 选择逻辑影响 Windows 用户。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 Windows 条件编译下断言 shell 命令保持 `cmd /C`。
    #[cfg(windows)]
    #[test]
    fn windows_shell_command_uses_cmd_c() {
        let command = build_shell_command("cargo test");

        assert_eq!(command.program, "cmd");
        assert_eq!(
            command.args,
            vec!["/C".to_string(), "cargo test".to_string()]
        );
    }
}
