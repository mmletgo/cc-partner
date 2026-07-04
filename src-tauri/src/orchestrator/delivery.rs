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
use tokio::process::Command;

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
    let mut combined = String::new();
    for command in commands {
        let output = run_shell_command(cwd, command).await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let section = format!(
            "$ {command}\nexit: {}\nstdout:\n{stdout}\nstderr:\n{stderr}\n",
            output.status
        );
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
///     并把 stdout/stderr 全量捕获返回。
async fn run_shell_command(cwd: &Path, command: &str) -> Result<std::process::Output, AppError> {
    let shell_command = build_shell_command(command);
    let mut child = Command::new(&shell_command.program);
    child.args(&shell_command.args);
    child
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    child
        .output()
        .await
        .map_err(|err| AppError::generic(format!("启动验证命令失败: {command}: {err}")))
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
