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
///     macOS/Linux 使用 `sh -lc`，Windows 使用 `cmd /C`，并把 stdout/stderr 全量捕获返回。
async fn run_shell_command(cwd: &Path, command: &str) -> Result<std::process::Output, AppError> {
    let mut child = if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.arg("-lc").arg(command);
        cmd
    };
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
}
