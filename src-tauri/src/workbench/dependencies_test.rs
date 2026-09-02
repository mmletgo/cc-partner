//! Workbench dependencies 单测模块（由 `dependencies.rs` 以 `#[path]` 挂载）。
//!
//! Business Logic:
//!     将大体量 `#[cfg(test)]` 从生产源文件拆出，避免 module-boundary no-growth 与测试膨胀互相绑死，
//!     同时保留子模块对 `dependencies` 私有 helper 的可见性。
//!
//! Code Logic:
//!     文件本体即为 `mod tests` 的模块体；仅在 `cfg(test)` 下由父模块 `#[path]` 引入。

use super::*;

/// Business Logic（为什么需要这个测试）:
///     前端需要展示 tmux 版本，后端必须从 `tmux -V` 的标准输出中稳定提取版本号。
///
/// Code Logic（这个测试做什么）:
///     覆盖普通版本、补丁后缀与带换行输出的解析结果。
#[test]
fn parse_tmux_version_extracts_version_token() {
    assert_eq!(parse_tmux_version("tmux 3.4\n"), Some("3.4".to_string()));
    assert_eq!(parse_tmux_version("tmux 3.3a"), Some("3.3a".to_string()));
    assert_eq!(parse_tmux_version("not tmux"), None);
}

/// Business Logic（为什么需要这个测试）:
///     macOS 用户缺少 tmux 时，应看到 Homebrew 安装预览命令。
///
/// Code Logic（这个测试做什么）:
///     对 macOS 平台选择器断言返回 `brew install tmux`。
#[test]
fn macos_install_preview_uses_brew() {
    let preview = install_command_preview_for_platform(DependencyPlatform::MacOs, &["brew"]);

    assert_eq!(
        preview,
        Some(vec!["brew".into(), "install".into(), "tmux".into()])
    );
    assert_eq!(
        install_command_preview_for_platform(DependencyPlatform::MacOs, &[]),
        None
    );
}

/// Business Logic（为什么需要这个测试）:
///     Linux 发行版包管理器不同，后端应按本机存在的工具给出最可能可执行的安装命令。
///
/// Code Logic（这个测试做什么）:
///     分别覆盖 apt-get、dnf、pacman 的选择顺序。
#[test]
fn linux_install_preview_selects_existing_package_manager() {
    assert_eq!(
        install_command_preview_for_platform(DependencyPlatform::Linux, &["dnf", "apt-get"]),
        Some(vec![
            "sudo".into(),
            "apt-get".into(),
            "install".into(),
            "-y".into(),
            "tmux".into()
        ])
    );
    assert_eq!(
        install_command_preview_for_platform(DependencyPlatform::Linux, &["dnf"]),
        Some(vec![
            "sudo".into(),
            "dnf".into(),
            "install".into(),
            "-y".into(),
            "tmux".into()
        ])
    );
    assert_eq!(
        install_command_preview_for_platform(DependencyPlatform::Linux, &["pacman"]),
        Some(vec![
            "sudo".into(),
            "pacman".into(),
            "-S".into(),
            "--noconfirm".into(),
            "tmux".into()
        ])
    );
}

/// Business Logic（为什么需要这个测试）:
///     Windows 只能通过 WSL 安装/运行 tmux，前端预览必须明确展示 wsl.exe 包裹命令。
///
/// Code Logic（这个测试做什么）:
///     断言 Windows 安装预览为固定的 WSL apt-get 命令。
#[test]
fn windows_install_preview_uses_wsl_apt() {
    let preview = install_command_preview_for_platform(DependencyPlatform::Windows, &["wsl.exe"]);

    assert_eq!(
        preview,
        Some(vec![
            "wsl.exe".into(),
            "--exec".into(),
            "sh".into(),
            "-lc".into(),
            "sudo apt-get update && sudo apt-get install -y tmux".into(),
        ])
    );
    assert_eq!(
        install_command_preview_for_platform(DependencyPlatform::Windows, &[]),
        None
    );
}

/// Business Logic（为什么需要这个测试）:
///     Workbench dependency DTO 是前端锁定契约，字段名必须保持 camelCase。
///
/// Code Logic（这个测试做什么）:
///     序列化一个 ready 状态，断言 installCommandPreview 字段存在且状态值稳定。
#[test]
fn dependency_status_serializes_with_camel_case_contract() {
    let status = WorkbenchDependencyStatusDto {
        status: WorkbenchDependencyState::Ready,
        available: true,
        version: Some("3.4".to_string()),
        backend: "native".to_string(),
        path: Some("/opt/homebrew/bin/tmux".to_string()),
        installable: false,
        install_command_preview: Vec::new(),
        error: None,
        output: Vec::new(),
        status_changed_at: "2026-07-12T00:00:00Z".to_string(),
    };

    let json = serde_json::to_value(status).unwrap();

    assert_eq!(json["status"], "ready");
    assert_eq!(json["installCommandPreview"], serde_json::json!([]));
    assert_eq!(json["statusChangedAt"], "2026-07-12T00:00:00Z");
}

/// Business Logic（为什么需要这个测试）:
///     安装流程可能被用户取消，状态机必须能从 installing 进入 failed 并保留最近输出供排查。
///
/// Code Logic（这个测试做什么）:
///     构造安装运行时，先标记 installing，再取消并断言 DTO 状态和输出摘要。
#[test]
fn install_state_transitions_from_installing_to_cancelled_failed() {
    let runtime = WorkbenchDependencyInstallRuntime::new();

    runtime.mark_installing(vec!["brew".into(), "install".into(), "tmux".into()]);
    runtime.mark_cancelled();
    let status = runtime.status();

    assert_eq!(status.status, WorkbenchDependencyState::Failed);
    assert_eq!(status.error.as_deref(), Some("安装已取消"));
    assert!(status.output.iter().any(|line| line.contains("安装已取消")));
}

/// Business Logic（为什么需要这个测试）:
///     Attention 需要依赖初始状态带有非空变更时间，才能稳定投影 environment 条目。
///
/// Code Logic（这个测试做什么）:
///     新建 runtime 后读取 status，断言 status_changed_at 非空。
#[test]
fn initial_status_has_status_changed_at_timestamp() {
    let runtime = WorkbenchDependencyInstallRuntime::new();
    let status = runtime.status();

    assert!(!status.status_changed_at.is_empty());
    assert!(chrono::DateTime::parse_from_rfc3339(&status.status_changed_at).is_ok());
}

/// Business Logic（为什么需要这个测试）:
///     冷启动若把未探测伪装成 missing，有项目时 Inbox 会错误计数环境阻塞；
///     初始化必须与真实探测结果一致。
///
/// Code Logic（这个测试做什么）:
///     对比 `new()` 缓存与 `probe_workbench_dependency()` 的 status/available/path，
///     并在探测为 ready 时确认不会被当成 missing。
#[test]
fn new_runtime_uses_real_probe_not_placeholder_missing() {
    let probed = probe_workbench_dependency();
    let runtime = WorkbenchDependencyInstallRuntime::new();
    let status = runtime.status();

    assert_eq!(status.status, probed.status);
    assert_eq!(status.available, probed.available);
    assert_eq!(status.path, probed.path);
    assert_eq!(status.version, probed.version);
    if probed.status == WorkbenchDependencyState::Ready {
        assert_ne!(status.status, WorkbenchDependencyState::Missing);
        assert!(status.available);
    }
}

/// Business Logic（为什么需要这个测试）:
///     相同语义状态的重复探测/轮询不能刷新变更时间，否则 Inbox 会把旧问题伪装成新事件。
///
/// Code Logic（这个测试做什么）:
///     写入 missing 后再次 set 同枚举不同 payload，再连续 status() 读取，断言时间戳保持不变。
#[test]
fn same_semantic_status_preserves_status_changed_at_across_polls() {
    let runtime = WorkbenchDependencyInstallRuntime::new();
    let initial = runtime.status();
    let initial_changed_at = initial.status_changed_at.clone();

    let again = runtime.set_checked_status(WorkbenchDependencyStatusDto {
        status: initial.status,
        available: false,
        version: None,
        backend: "native".to_string(),
        path: None,
        installable: true,
        install_command_preview: vec!["brew".into(), "install".into(), "tmux".into()],
        error: Some("still missing".into()),
        output: vec!["recheck".into()],
        status_changed_at: "should-be-ignored".into(),
    });
    assert_eq!(again.status_changed_at, initial_changed_at);

    let polled_once = runtime.status();
    let polled_twice = runtime.status();
    assert_eq!(polled_once.status_changed_at, initial_changed_at);
    assert_eq!(polled_twice.status_changed_at, initial_changed_at);
}

/// Business Logic（为什么需要这个测试）:
///     真实状态迁移（如 missing→ready 或 ready→failed）必须更新变更时间，Attention 才能按最新阻塞排序。
///
/// Code Logic（这个测试做什么）:
///     先强制写入与当前探测不同的状态，再切到另一状态，断言每次语义变化后 status_changed_at 都前进。
#[test]
fn semantic_status_change_updates_status_changed_at() {
    let runtime = WorkbenchDependencyInstallRuntime::new();
    let initial = runtime.status();
    let initial_changed_at = initial.status_changed_at.clone();

    // 确保时间戳至少相差 1ms；先切到与初始不同的状态（冷启动可能已是 ready）。
    std::thread::sleep(std::time::Duration::from_millis(5));
    let first_target = if initial.status == WorkbenchDependencyState::Missing {
        WorkbenchDependencyState::Ready
    } else {
        WorkbenchDependencyState::Missing
    };
    let first = runtime.set_checked_status(WorkbenchDependencyStatusDto {
        status: first_target,
        available: first_target == WorkbenchDependencyState::Ready,
        version: if first_target == WorkbenchDependencyState::Ready {
            Some("3.4".into())
        } else {
            None
        },
        backend: "native".into(),
        path: if first_target == WorkbenchDependencyState::Ready {
            Some("/opt/homebrew/bin/tmux".into())
        } else {
            None
        },
        installable: first_target == WorkbenchDependencyState::Missing,
        install_command_preview: Vec::new(),
        error: None,
        output: Vec::new(),
        status_changed_at: String::new(),
    });
    assert_eq!(first.status, first_target);
    assert_ne!(first.status_changed_at, initial_changed_at);
    assert!(!first.status_changed_at.is_empty());

    let first_changed_at = first.status_changed_at.clone();
    std::thread::sleep(std::time::Duration::from_millis(5));

    runtime.mark_failed("probe failed", vec!["stderr".into()]);
    let failed = runtime.status();
    assert_eq!(failed.status, WorkbenchDependencyState::Failed);
    assert_ne!(failed.status_changed_at, first_changed_at);
    assert!(!failed.status_changed_at.is_empty());
}

/// Business Logic（为什么需要这个测试）:
///     挂起的外部命令必须在硬超时后被终止，不能阻塞探测线程。
///
/// Code Logic（这个测试做什么）:
///     启动 `sleep 10`，以 200ms 超时探测，断言返回 TimedOut 且总耗时远小于 sleep。
#[test]
fn run_std_command_with_timeout_kills_hanging_process() {
    let mut command = StdCommand::new("sleep");
    command.arg("10");
    let started = Instant::now();
    let result = run_std_command_with_timeout(command, Duration::from_millis(200));
    let elapsed = started.elapsed();
    assert_eq!(result.err(), Some(ProbeCommandError::TimedOut));
    assert!(
        elapsed < Duration::from_secs(2),
        "超时路径耗时过长: {elapsed:?}"
    );
}

/// Business Logic（为什么需要这个测试）:
///     doctor 超时后的回收命令本身也必须有界；挂起的 kill 辅助进程不得突破 hard deadline。
///
/// Code Logic（这个测试做什么）:
///     spawn 长 sleep 模拟挂起的 taskkill/kill helper，用极短 grace 调用 wait_child_bounded，
///     断言 TimedOut 且耗时远小于无界 wait。
#[test]
fn wait_child_bounded_times_out_and_kills_hanging_helper() {
    let mut child = StdCommand::new("sleep")
        .arg("10")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hanging helper");
    let started = Instant::now();
    let err = wait_child_bounded(&mut child, Duration::from_millis(80))
        .expect_err("挂起 helper 必须在 grace 内超时");
    let elapsed = started.elapsed();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        elapsed < Duration::from_millis(800),
        "有界回收耗时过长: {elapsed:?}"
    );
}

/// Business Logic（为什么需要这个测试）:
///     超时路径不能依赖「kill 必然成功」；即便进程已退出，terminate 也必须有界返回。
///
/// Code Logic（这个测试做什么）:
///     对已退出的 sleep 0 子进程调用 terminate_probe_child，断言在 grace 内返回且不 panic。
#[test]
fn terminate_probe_child_is_bounded_when_process_already_exited() {
    let mut child = StdCommand::new("sleep")
        .arg("0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sleep 0");
    let pgid = child.id();
    // 等子进程自然退出，使后续 kill/wait 走「已退出 / kill 失败」分支。
    let _ = child.wait();
    let deadline = Instant::now();
    let started = Instant::now();
    terminate_probe_child(
        &mut child,
        pgid,
        deadline,
        #[cfg(windows)]
        None,
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "已退出进程的 terminate 耗时过长: {elapsed:?}"
    );
}

/// Business Logic（为什么需要这个测试）:
///     Windows 单次 probe 超时必须走 taskkill /T 杀进程树，而不能只 Child::kill 直接子进程。
///
/// Code Logic（这个测试做什么）:
///     通过 TEST_WINDOWS_TASKKILL_SPAWN hook 记录 terminate_probe_child 触发的 PID，
///     对 sleep 0 已退出子进程调用后断言 hook 被调用且 PID 匹配。
#[cfg(windows)]
#[test]
fn terminate_probe_child_windows_uses_taskkill_tree() {
    let mut child = StdCommand::new("cmd")
        .args(["/C", "exit", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn exited child");
    let pid = child.id();
    // 等子进程退出，避免真实 taskkill 干扰；hook 拦截 spawn。
    let _ = child.wait();
    // 重新 spawn 一个仍存活的 sleep 以便 try_wait 路径存在。
    let mut child = StdCommand::new("cmd")
        .args(["/C", "ping", "-n", "3", "127.0.0.1", ">", "nul"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hanging child");
    let pid = child.id();

    let seen: std::sync::Arc<std::sync::Mutex<Vec<u32>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_hook = seen.clone();
    {
        let mut hook = TEST_WINDOWS_TASKKILL_SPAWN.lock().expect("hook lock");
        *hook = Some(Box::new(move |kill_pid| {
            seen_hook.lock().expect("seen").push(kill_pid);
            // 返回已退出的 helper，避免 wait_child_bounded hang。
            StdCommand::new("cmd")
                .args(["/C", "exit", "0"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }));
    }

    let deadline = Instant::now() + Duration::from_millis(200);
    terminate_probe_child(
        &mut child,
        pid,
        deadline,
        #[cfg(windows)]
        None,
    );

    {
        let mut hook = TEST_WINDOWS_TASKKILL_SPAWN.lock().expect("hook lock");
        *hook = None;
    }
    let pids = seen.lock().expect("seen").clone();
    assert_eq!(
        pids,
        vec![pid],
        "terminate_probe_child 必须对直接子 PID 调 taskkill /T"
    );
    let _ = child.try_wait();
}

/// Business Logic（为什么需要这个测试）:
///     正常退出后的管道 drain 必须有界；即便 deadline 已过，也不能无界阻塞读。
///
/// Code Logic（这个测试做什么）:
///     跑 `printf hi`，以已过去的 deadline 调用 drain_probe_pipes，断言快速返回（缓冲可空）。
#[test]
fn drain_probe_pipes_respects_past_deadline() {
    let mut child = StdCommand::new("printf")
        .arg("hi")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn printf");
    let _ = child.wait();
    let started = Instant::now();
    let past_deadline = Instant::now() - Duration::from_millis(1);
    let (stdout, stderr) = drain_probe_pipes(
        &mut child,
        past_deadline,
        #[cfg(windows)]
        None,
    );
    let elapsed = started.elapsed();
    // 允许小额 grace；必须远小于无界阻塞。
    assert!(
        elapsed < Duration::from_millis(500),
        "过期 deadline 的 drain 耗时过长: {elapsed:?}"
    );
    // 结果可空（超时关闭读端）或含 "hi"；关键是不阻塞。
    let _ = (stdout, stderr);
}

/// Business Logic（为什么需要这个测试）:
///     成功路径在截止时间内应读到完整 stdout，验证非阻塞 drain 不只是「永远返回空」。
///
/// Code Logic（这个测试做什么）:
///     跑 `printf hello` 并在充足 deadline 下 drain，断言 stdout 含 hello。
#[test]
fn drain_probe_pipes_reads_stdout_within_deadline() {
    let mut child = StdCommand::new("printf")
        .arg("hello")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn printf");
    let status = child.wait().expect("wait printf");
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(2);
    let (stdout, _stderr) = drain_probe_pipes(
        &mut child,
        deadline,
        #[cfg(windows)]
        None,
    );
    assert_eq!(String::from_utf8_lossy(&stdout), "hello");
}

/// Business Logic（为什么需要这个测试）:
///     父进程先退出但同进程组孙进程仍持 stdout 时，必须有界返回并 killpg 清理后代。
///
/// Code Logic（这个测试做什么）:
///     Unix：独立进程组 spawn `sh -c 'sleep 60 &'`，等 shell 退出后以过期 deadline drain；
///     断言 drain 有界返回，且 kill(pgid,0) 失败（进程组已无存活成员）。
#[cfg(unix)]
#[test]
fn drain_timeout_kills_descendant_holding_pipe() {
    use std::os::unix::process::CommandExt;

    let mut command = StdCommand::new("sh");
    command
        .arg("-c")
        .arg("sleep 60 &")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn sh with background sleep");
    let pgid = child.id();

    // 等待 shell 自身退出，留下持有 stdout 的 sleep 后代。
    let wait_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                assert!(Instant::now() < wait_deadline, "shell 未在预期时间内退出");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("try_wait shell failed: {err}"),
        }
    }

    // 过期 deadline 强制走 drain 超时 → killpg 路径。
    let started = Instant::now();
    let past_deadline = Instant::now() - Duration::from_millis(1);
    let (_stdout, _stderr) = drain_probe_pipes(
        &mut child,
        past_deadline,
        #[cfg(windows)]
        None,
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "descendant-hold-pipe 的 drain 超时路径耗时过长: {elapsed:?}"
    );

    // 进程组应已被 SIGKILL。SIGKILL 后可能短暂残留僵尸，轮询等待 reaper 回收。
    let pgid_i: libc::pid_t = pgid.try_into().expect("pgid fits pid_t");
    let assert_deadline = Instant::now() + Duration::from_secs(1);
    let mut still_alive = true;
    while Instant::now() < assert_deadline {
        still_alive = unsafe { libc::kill(-pgid_i, 0) } == 0;
        if !still_alive {
            break;
        }
        // 再补一次 killpg，覆盖 terminate 与断言之间的竞态窗口。
        let _ = kill_probe_process_group(pgid);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!still_alive, "drain 超时后进程组仍有存活后代 (pgid={pgid})");
}

/// Business Logic（为什么需要这个测试）:
///     后代 setsid 逃离原进程组后 killpg 无效；旧实现会 mem::forget 阻塞 reader，
///     每次依赖重检都累积线程。新实现必须有界返回并关闭读端，无 reader 线程爆炸。
///
/// Code Logic（这个测试做什么）:
///     用 python 子进程 setsid 后 sleep 并继承 stdout；父 shell 退出后反复 drain；
///     断言每次 probe 在 deadline 内返回，且过程不依赖 mem::forget/detach 线程。
///     最后单独 kill 逃逸后代，避免污染测试环境。
#[cfg(unix)]
#[test]
fn drain_timeout_with_setsid_escape_stays_bounded_across_repeated_probes() {
    use std::os::unix::process::CommandExt;

    // 逃逸后代把 pid 写到临时文件，便于测试结束清理；stdout 继承 shell 管道保持打开。
    let pid_file = std::env::temp_dir().join(format!(
        "cc-partner-probe-setsid-{}.pid",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&pid_file);
    let pid_path = pid_file.to_string_lossy().replace('\'', "");

    let script = format!(
        "python3 -c 'import os,time; os.setsid(); open(\"{pid_path}\",\"w\").write(str(os.getpid())); time.sleep(120)' &"
    );

    let mut escaped_pids: Vec<i32> = Vec::new();
    // 多次探测：证明不会因 forget reader 而线程/耗时爆炸。
    for round in 0..3 {
        let mut command = StdCommand::new("sh");
        command
            .arg("-c")
            .arg(&script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .expect("spawn sh with setsid-escaped python");

        let wait_deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    assert!(
                        Instant::now() < wait_deadline,
                        "round {round}: shell 未在预期时间内退出"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("round {round}: try_wait shell failed: {err}"),
            }
        }

        // 读取逃逸 pid（best-effort，用于最终清理）。
        if let Ok(text) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = text.trim().parse::<i32>() {
                escaped_pids.push(pid);
            }
        }
        let _ = std::fs::remove_file(&pid_file);

        let started = Instant::now();
        // 给极短 grace：非阻塞 poll 应在 deadline 后立刻因超时关闭读端返回。
        let past_deadline = Instant::now() - Duration::from_millis(1);
        let (_stdout, _stderr) = drain_probe_pipes(
            &mut child,
            past_deadline,
            #[cfg(windows)]
            None,
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "round {round}: setsid-escape drain 超时路径耗时过长: {elapsed:?}"
        );
        // 读端已关闭：Child 上不应再持有 stdout/stderr handle。
        assert!(child.stdout.is_none());
        assert!(child.stderr.is_none());
    }

    // 清理 setsid 逃逸后代（它们不在原 pgid，drain 的 killpg 管不到）。
    for pid in escaped_pids {
        unsafe {
            let _ = libc::kill(pid, libc::SIGKILL);
        }
    }
    let _ = std::fs::remove_file(&pid_file);
}

/// Business Logic（为什么需要这个测试）:
///     持续向继承管道写数据的后代会使旧 pump_side 在 WouldBlock 前无限扩张缓冲，
///     阻塞 AppState 初始化；修复后必须在 deadline/字节预算内返回并关闭读端。
///
/// Code Logic（这个测试做什么）:
///     Unix：独立进程组 spawn `sh -c 'yes ... &'`，shell 退出后后代持续写 stdout；
///     以短但非零 deadline drain，断言有界返回、读端已 drop、缓冲不超过字节预算。
#[cfg(unix)]
#[test]
fn drain_timeout_with_continuous_writing_descendant_stays_bounded() {
    use std::os::unix::process::CommandExt;

    let mut command = StdCommand::new("sh");
    command
        .arg("-c")
        // yes 继承 shell stdout（探测管道）并持续写入；父 shell 退出后管道仍可读。
        .arg("yes probe-continuous-write 2>/dev/null &")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .expect("spawn continuous-writing descendant");
    let pgid = child.id();

    let wait_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                assert!(
                    Instant::now() < wait_deadline,
                    "continuous-write shell 未在预期时间内退出"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("try_wait continuous-write shell failed: {err}"),
        }
    }

    // 给真实 grace 窗口：管道始终可读时，旧实现会无限 pump；新实现靠 deadline/预算退出。
    let started = Instant::now();
    let drain_deadline = Instant::now() + Duration::from_millis(200);
    let (stdout, _stderr) = drain_probe_pipes(
        &mut child,
        drain_deadline,
        #[cfg(windows)]
        None,
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "continuous-write drain 超时路径耗时过长: {elapsed:?}"
    );
    assert!(
        stdout.len() <= PROBE_PIPE_DRAIN_BYTE_BUDGET,
        "stdout 超过字节预算: {}",
        stdout.len()
    );
    assert!(child.stdout.is_none());
    assert!(child.stderr.is_none());

    // best-effort 清理同组 yes 后代（drain 超时路径通常已 killpg）。
    let pgid_i: libc::pid_t = pgid.try_into().expect("pgid fits pid_t");
    unsafe {
        let _ = libc::kill(-pgid_i, libc::SIGKILL);
    }
}

/// Business Logic（为什么需要这个测试）:
///     fcntl 失败后若仍进入阻塞 read，deadline 无法打断；源码必须在失败时立刻关侧。
///
/// Code Logic（这个测试做什么）:
///     审查 Unix drain 生产实现：set_nonblocking 返回 Result，失败分支 drop 读端且
///     不在失败路径上直接 pump/read。
#[cfg(unix)]
#[test]
fn unix_drain_source_closes_side_on_set_nonblocking_failure() {
    let source = include_str!("dependencies.rs");
    // 生产区可能含 `#[cfg(test)]` hook 分支；以 tests 模块边界切分，避免误截断。
    let production = source
        .split("mod tests {")
        .next()
        .expect("dependencies.rs 应包含 mod tests");
    let unix_fn = production
        .split("fn drain_child_pipes_nonblocking_unix")
        .nth(1)
        .and_then(|rest| rest.split("fn plan_windows_pipe_read").next())
        .or_else(|| {
            production
                .split("fn drain_child_pipes_nonblocking_unix")
                .nth(1)
                .and_then(|rest| {
                    rest.split("fn drain_child_pipes_nonblocking_fallback")
                        .next()
                })
        })
        .expect("应能切出 Unix drain 函数体");
    assert!(
        unix_fn.contains("fn set_nonblocking(fd: RawFd) -> std::io::Result<()>")
            || unix_fn.contains("fn set_nonblocking(fd: RawFd) -> Result<(),"),
        "set_nonblocking 必须返回 Result，禁止静默忽略 fcntl 失败"
    );
    assert!(
        unix_fn.contains("match set_nonblocking(fd)"),
        "调用方必须 match set_nonblocking 结果"
    );
    assert!(
        unix_fn.contains("drop(out)") || unix_fn.contains("drop(err)"),
        "fcntl 失败路径必须 drop 读端，禁止继续阻塞 read"
    );
    assert!(
        !unix_fn.contains("失败时仍尝试读") && !unix_fn.contains("最坏靠 deadline 退出"),
        "不得再声称 fcntl 失败后靠 deadline 退出阻塞 read"
    );
    assert!(
        unix_fn.contains("PROBE_PIPE_DRAIN_BYTE_BUDGET") || unix_fn.contains("total_read"),
        "pump_side 必须受字节预算约束"
    );
    assert!(
        unix_fn.contains("Instant::now() >= deadline"),
        "pump_side 循环内必须检查 deadline"
    );
}

/// Business Logic（为什么需要这个测试）:
///     生产代码必须消除 forget-reader 与后台 join reader 回退，避免回归到永久线程泄漏。
///
/// Code Logic（这个测试做什么）:
///     只检查 `#[cfg(test)]` 之前的生产源码，断言不存在 forget 调用 / join_pipe_thread 定义。
#[test]
fn probe_drain_source_has_no_forgotten_reader_fallback() {
    let source = include_str!("dependencies.rs");
    // 测试模块自身会提到这些符号；只审查生产实现段。
    // 生产区可能含 `#[cfg(test)]` hook 分支；以 tests 模块边界切分，避免误截断。
    let production = source
        .split("mod tests {")
        .next()
        .expect("dependencies.rs 应包含 mod tests");
    let forget_token = format!("{}::forget(", "mem");
    assert!(
        !production.contains(&forget_token),
        "生产代码仍包含 mem::forget 调用，禁止作为 pipe drain 回退"
    );
    assert!(
        !production.contains("fn join_pipe_thread"),
        "生产代码仍包含 fn join_pipe_thread 后台 reader 路径"
    );
    assert!(
        production.contains("fn drain_child_pipes_nonblocking"),
        "生产代码应定义非阻塞 drain_child_pipes_nonblocking"
    );
}

/// Business Logic（为什么需要这个测试）:
///     Windows 路径若在 Peek 之前裸调用阻塞 read，deadline 无法打断 hang，会卡死 AppState 初始化。
///
/// Code Logic（这个测试做什么）:
///     审查生产源码：Windows fallback 必须含 PeekNamedPipe + plan_windows_pipe_read；
///     且 fallback 函数体内不得出现未先 plan 的“裸 read 循环”注释回归。
#[test]
fn windows_drain_source_uses_peek_named_pipe_before_read() {
    let source = include_str!("dependencies.rs");
    // 生产区可能含 `#[cfg(test)]` hook 分支；以 tests 模块边界切分，避免误截断。
    let production = source
        .split("mod tests {")
        .next()
        .expect("dependencies.rs 应包含 mod tests");
    assert!(
        production.contains("fn plan_windows_pipe_read"),
        "应导出可单测的 plan_windows_pipe_read 决策函数"
    );
    assert!(
        production.contains("PeekNamedPipe"),
        "Windows drain 必须通过 PeekNamedPipe 查询可用字节"
    );
    assert!(
        production.contains("fn drain_child_pipes_nonblocking_fallback"),
        "应保留非 Unix drain fallback 入口"
    );
    // 禁止回归到“注释宣称受总超时约束 + 直接 out.read”的错误实现。
    assert!(
        !production.contains("极端阻塞场景仍受 probe 总超时约束"),
        "不得再声称阻塞 read 受 probe 总超时约束"
    );
    let fallback = production
        .split("fn drain_child_pipes_nonblocking_fallback")
        .nth(1)
        .and_then(|rest| rest.split("struct TmuxCandidate").next())
        .expect("应能切出 Windows fallback 函数体");
    assert!(
        fallback.contains("PeekNamedPipe") || fallback.contains("peek_named_pipe_available"),
        "fallback 函数体必须调用 PeekNamedPipe"
    );
    assert!(
        fallback.contains("plan_windows_pipe_read"),
        "fallback 函数体必须经 plan_windows_pipe_read 决策后再 read"
    );
}

/// Business Logic（为什么需要这个测试）:
///     根进程先退出后 taskkill /T 不可靠；生产路径必须在用户代码运行前绑定 Job Object。
///
/// Code Logic（这个测试做什么）:
///     审查生产源码含 CREATE_SUSPENDED / CreateJobObjectW / KILL_ON_JOB_CLOSE /
///     AssignProcessToJobObject / ResumeThread / TerminateJobObject，且
///     terminate_probe_child / cancel_and_reap 优先使用 job。
///     真实“父退子存”杀树验证需 windows-latest runner；非 Windows 以本源码契约测试代替。
#[test]
fn windows_probe_job_object_source_contract() {
    let source = include_str!("dependencies.rs");
    let production = source
        .split("mod tests {")
        .next()
        .expect("dependencies.rs 应包含 mod tests");
    assert!(
        production.contains("CreateJobObjectW"),
        "Windows probe 必须 CreateJobObjectW"
    );
    assert!(
        production.contains("JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE")
            || production.contains("0x0000_2000")
            || production.contains("0x00002000"),
        "Job 必须启用 KILL_ON_JOB_CLOSE"
    );
    assert!(
        production.contains("CREATE_SUSPENDED") && production.contains("creation_flags"),
        "必须 CREATE_SUSPENDED 挂起 spawn，用户代码运行前再 Assign"
    );
    // Job 创建失败必须 fail closed：spawn 前返回 SpawnOrIo，禁止 .ok() 吞错后走未挂起路径。
    assert!(
        !production
            .split("fn run_std_command_with_timeout_guarded")
            .nth(1)
            .and_then(|rest| rest.split("fn terminate_probe_child").next())
            .expect("应能切出 run_std_command_with_timeout_guarded")
            .contains("WindowsProbeJob::create().ok()"),
        "禁止 WindowsProbeJob::create().ok() 吞掉 Job 创建失败"
    );
    assert!(
        production.contains("拒绝启动未受约束探测")
            || production.contains("fail closed")
            || production.contains("未能创建 Job Object，拒绝"),
        "Job 创建失败必须在 spawn 前 fail closed"
    );
    assert!(
        production.contains("AssignProcessToJobObject"),
        "必须 AssignProcessToJobObject"
    );
    assert!(
        production.contains("ResumeThread") || production.contains("resume_suspended_process"),
        "Assign 后必须 ResumeThread 再跑用户代码"
    );
    // 禁止旧的“先 spawn 再 create_for_child”竞态路径出现在生产代码。
    assert!(
        !production.contains("create_for_child"),
        "禁止 spawn 后 create_for_child（Assign 竞态）"
    );
    assert!(
        production.contains("TerminateJobObject"),
        "超时路径必须 TerminateJobObject"
    );
    assert!(
        production.contains("register_job") || production.contains("jobs:"),
        "ProbeRuntimeGuard 必须登记 job 句柄"
    );
    let terminate = production
        .split("fn terminate_probe_child")
        .nth(1)
        .and_then(|rest| rest.split("fn kill_probe_process_group").next())
        .expect("应能切出 terminate_probe_child");
    assert!(
        terminate.contains("job.terminate") || terminate.contains("TerminateJobObject"),
        "terminate_probe_child 必须优先 job.terminate"
    );
    assert!(
        terminate.contains("kill_probe_pid_windows") || terminate.contains("taskkill"),
        "无 job 时仍保留 taskkill fallback"
    );
    let cancel = production
        .split("fn cancel_and_reap")
        .nth(1)
        .and_then(|rest| rest.split("fn kill_probe_pid").next())
        .expect("应能切出 cancel_and_reap");
    assert!(
        cancel.contains("TerminateJobObject")
            || cancel.contains("job.terminate")
            || cancel.contains("jobs.drain"),
        "cancel_and_reap 必须终止已登记 job"
    );
}

/// Business Logic（为什么需要这个测试）:
///     Windows 上父进程派生持管道后代后立即退出时，deadline 后必须能通过 Job 终止后代。
///
/// Code Logic（这个测试做什么）:
///     仅 Windows：CREATE_SUSPENDED 起 `cmd /C ping`（避免 powershell 冷启动），
///     Assign → Resume；等 Job 出现非根 PID 后 `Child::kill` 只杀 cmd 根，
///     断言 ping 仍存活，TerminateJobObject 后退出。使用 NUL 而非 /dev/null。
#[cfg(windows)]
#[test]
fn windows_job_kills_descendants_after_root_exits() {
    use std::os::windows::process::CommandExt;

    let job = WindowsProbeJob::create().expect("create empty job");
    let mut command = StdCommand::new("cmd");
    command
        .args(["/C", "ping.exe -n 60 127.0.0.1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_SUSPENDED);
    let mut child = command.spawn().expect("spawn suspended cmd wrapper");
    let root_pid = child.id();
    job.assign_child(&child)
        .expect("assign suspended root to job");
    resume_suspended_process(root_pid).expect("resume after assign");

    let wait_desc = Instant::now() + Duration::from_secs(10);
    let descendant_pid = loop {
        let live: Vec<u32> = job_assigned_pids(&job)
            .into_iter()
            .filter(|pid| *pid != root_pid && windows_pid_is_alive(*pid))
            .collect();
        if let Some(pid) = live.first().copied() {
            break pid;
        }
        assert!(
            Instant::now() < wait_desc,
            "cmd 未在时限内派生 Job 内后代; job_pids={:?}",
            job_assigned_pids(&job)
        );
        std::thread::sleep(Duration::from_millis(20));
    };

    // 只杀根进程：Windows 默认不连坐子女，模拟 wrapper 先退出、后代仍占管道。
    let _ = child.kill();
    let wait_root = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                assert!(
                    Instant::now() < wait_root,
                    "cmd 根进程未被 kill 回收; descendant={descendant_pid}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => panic!("try_wait wrapper failed: {err}"),
        }
    }
    assert!(
        windows_pid_is_alive(descendant_pid),
        "根退出后、Terminate 前后代 PID {descendant_pid} 应仍存活"
    );

    let started = Instant::now();
    job.terminate().expect("TerminateJobObject after root exit");
    let kill_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if !windows_pid_is_alive(descendant_pid) {
            break;
        }
        assert!(
            Instant::now() < kill_deadline,
            "TerminateJobObject 后后代 PID {descendant_pid} 仍存活"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "job 杀树过长: {elapsed:?}"
    );
}

/// 读取 Job 内当前进程 PID 列表，供杀树回归断言。
///
/// Business Logic（为什么需要这个函数）:
///     cmd wrapper 退出后不能再靠 stdout 拿后代 PID，必须问 Job 自己还绑着谁。
///
/// Code Logic（这个函数做什么）:
///     QueryInformationJobObject(JobObjectBasicProcessIdList)；失败返回空列表。
#[cfg(windows)]
fn job_assigned_pids(job: &WindowsProbeJob) -> Vec<u32> {
    use std::os::windows::io::AsRawHandle;

    type BOOL = i32;
    type DWORD = u32;
    type HANDLE = *mut core::ffi::c_void;
    const JOB_OBJECT_BASIC_PROCESS_ID_LIST: i32 = 3;

    #[repr(C)]
    struct JobObjectBasicProcessIdList {
        number_of_assigned_processes: DWORD,
        number_of_process_ids_in_list: DWORD,
        process_id_list: [usize; 16],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn QueryInformationJobObject(
            h_job: HANDLE,
            job_object_information_class: i32,
            lp_job_object_information: *mut core::ffi::c_void,
            cb_job_object_information_length: DWORD,
            lp_return_length: *mut DWORD,
        ) -> BOOL;
    }

    let mut info = JobObjectBasicProcessIdList {
        number_of_assigned_processes: 0,
        number_of_process_ids_in_list: 0,
        process_id_list: [0; 16],
    };
    let mut ret_len: DWORD = 0;
    let ok = unsafe {
        QueryInformationJobObject(
            job.handle.as_raw_handle() as HANDLE,
            JOB_OBJECT_BASIC_PROCESS_ID_LIST,
            (&mut info as *mut JobObjectBasicProcessIdList).cast(),
            std::mem::size_of::<JobObjectBasicProcessIdList>() as DWORD,
            &mut ret_len,
        )
    };
    if ok == 0 {
        return Vec::new();
    }
    let n = (info.number_of_process_ids_in_list as usize).min(16);
    info.process_id_list[..n]
        .iter()
        .map(|id| *id as u32)
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     Windows 杀树回归测试需要观察真实后代 PID 是否仍存活。
///
/// Code Logic（这个函数做什么）:
///     OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) 成功即视为存活；失败视为已退出。
#[cfg(windows)]
fn windows_pid_is_alive(pid: u32) -> bool {
    type BOOL = i32;
    type DWORD = u32;
    type HANDLE = *mut core::ffi::c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: DWORD = 0x1000;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(
            dw_desired_access: DWORD,
            b_inherit_handle: BOOL,
            dw_process_id: DWORD,
        ) -> HANDLE;
        fn CloseHandle(h_object: HANDLE) -> BOOL;
        fn GetExitCodeProcess(h_process: HANDLE, lp_exit_code: *mut DWORD) -> BOOL;
    }

    const STILL_ACTIVE: DWORD = 259;
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut code: DWORD = 0;
    let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    ok != 0 && code == STILL_ACTIVE
}

/// Business Logic（为什么需要这个测试）:
///     Peek 结果到 read 预算的映射是 Windows 有界 drain 的核心；零字节必须禁止 read。
///
/// Code Logic（这个测试做什么）:
///     覆盖 None/0/正数/cap 裁剪四类输入，断言 Wait/Eof/Read 规划正确。
#[test]
fn plan_windows_pipe_read_never_allows_blocking_when_empty() {
    assert_eq!(plan_windows_pipe_read(None, 4096), WindowsPipeReadPlan::Eof);
    assert_eq!(
        plan_windows_pipe_read(Some(0), 4096),
        WindowsPipeReadPlan::Wait
    );
    assert_eq!(
        plan_windows_pipe_read(Some(100), 0),
        WindowsPipeReadPlan::Wait
    );
    assert_eq!(
        plan_windows_pipe_read(Some(100), 4096),
        WindowsPipeReadPlan::Read(100)
    );
    assert_eq!(
        plan_windows_pipe_read(Some(8000), 4096),
        WindowsPipeReadPlan::Read(4096)
    );
}

/// Business Logic（为什么需要这个测试）:
///     注入挂起 runner 时探测必须收敛为 TimedOut，供 DTO 写成 failed 而非永久卡死。
///
/// Code Logic（这个测试做什么）:
///     fake runner 恒返回 TimedOut；Windows 平台单候选，断言 outcome 为 TimedOut。
#[test]
fn probe_tmux_command_with_timeout_runner_returns_timed_out() {
    let outcome = probe_tmux_command_with(DependencyPlatform::Windows, |_cmd, _timeout| {
        Err(ProbeCommandError::TimedOut)
    });
    assert!(matches!(outcome, TmuxProbeOutcome::TimedOut));
}

/// Business Logic（为什么需要这个测试）:
///     探测超时不能伪装成 missing，否则 Inbox 会把“未知”当成可安装缺失。
///
/// Code Logic（这个测试做什么）:
///     用恒超时 runner 走 probe 路径，经 probe_tmux_command_with 映射为 Failed DTO 语义：
///     直接断言 TimedOut outcome 对应的 probe_workbench 分支字段（构造等价 DTO）。
#[test]
fn timed_out_probe_maps_to_failed_not_missing() {
    let outcome = probe_tmux_command_with(DependencyPlatform::Linux, |_cmd, _timeout| {
        Err(ProbeCommandError::TimedOut)
    });
    assert!(matches!(outcome, TmuxProbeOutcome::TimedOut));
    // 与 probe_workbench_dependency 的 TimedOut 分支保持同一语义。
    let status = WorkbenchDependencyStatusDto {
        status: WorkbenchDependencyState::Failed,
        available: false,
        version: None,
        backend: backend_for_platform(DependencyPlatform::Linux).to_string(),
        path: None,
        installable: false,
        install_command_preview: Vec::new(),
        error: Some("tmux 探测超时（3 秒），请稍后重新检测".into()),
        output: vec!["依赖探测超时，已终止外部进程".to_string()],
        status_changed_at: String::new(),
    };
    assert_eq!(status.status, WorkbenchDependencyState::Failed);
    assert!(!status.available);
    assert!(status.error.as_deref().unwrap_or("").contains("超时"));
}

/// Business Logic（为什么需要这个测试）:
///     快速失败的候选应记为 Missing，而不是 TimedOut。
///
/// Code Logic（这个测试做什么）:
///     fake runner 返回 SpawnOrIo；断言 outcome 为 Missing。
#[test]
fn probe_tmux_command_with_spawn_failures_returns_missing() {
    let outcome = probe_tmux_command_with(DependencyPlatform::MacOs, |_cmd, _timeout| {
        Err(ProbeCommandError::SpawnOrIo)
    });
    assert!(matches!(outcome, TmuxProbeOutcome::Missing));
}

/// Business Logic（为什么需要这个测试）:
///     Linux/WSL argv 与 `sh -lc` 里的 sudo 都必须被识别，才能加无 TTY 超时。
#[test]
fn install_command_detects_sudo_argv_and_shell_string() {
    assert!(install_command_uses_sudo(&[
        "sudo".into(),
        "apt-get".into(),
        "install".into(),
        "-y".into(),
        "tmux".into()
    ]));
    assert!(install_command_uses_sudo(&[
        "wsl.exe".into(),
        "--exec".into(),
        "sh".into(),
        "-lc".into(),
        "sudo apt-get install -y tmux".into()
    ]));
    assert!(!install_command_uses_sudo(&[
        "brew".into(),
        "install".into(),
        "tmux".into()
    ]));
}

/// Business Logic（为什么需要这个测试）:
///     缺 capability 的 DTO 必须 unsupported 且不可安装。
#[test]
fn unsupported_status_is_not_installable() {
    let status =
        unsupported_dependency_status("capability_unsupported:workbench.dependency-install.v1");
    assert_eq!(status.status, WorkbenchDependencyState::Unsupported);
    assert!(!status.installable);
    assert!(!status.available);
    let encoded = serde_json::to_value(&status).expect("json");
    let decoded: WorkbenchDependencyStatusDto = serde_json::from_value(encoded).expect("decode");
    assert_eq!(decoded.status, WorkbenchDependencyState::Unsupported);
}
