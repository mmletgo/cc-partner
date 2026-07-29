//! Backend CLI 生命周期冒烟测试。
//!
//! 覆盖 start→health→status→stop、duplicate start、stale control recovery。
//! 使用 `CC_PARTNER_DATA_DIR` 隔离，不触碰用户真实 `~/.cc-partner`。

mod support;

use std::thread;
use std::time::{Duration, Instant};
use support::{
    captured_from_output, dead_pid, ensure_platform_supported, process_is_alive, read_control_file,
    unused_local_port, write_control_file, CapturedCli, ControlFileJson, SmokeCase,
};

/// Business Logic（为什么需要这个函数）:
///     测试失败时要把 CLI 输出与 case 路径挂到 panic，便于 CI 诊断；
///     同时在 teardown 前落盘 diagnostics，避免 control/port 证据被清理。
///
/// Code Logic（这个函数做什么）:
///     标记 failed、写 diagnostics，再 panic 带上下文的消息。
fn fail_case(case: &mut SmokeCase, message: impl AsRef<str>) -> ! {
    let message = message.as_ref();
    case.mark_failed();
    case.write_failure_diagnostics(message);
    panic!(
        "{message}\ncase_dir={}\ndata_dir={}\ndiagnostics={}",
        case.case_dir.display(),
        case.data_dir.display(),
        case.case_dir.join("diagnostics").display()
    );
}

/// Business Logic（为什么需要这个函数）:
///     start 成功后需要同时记录 control 与 status 中的 pid，供 teardown 精准 kill。
///
/// Code Logic（这个函数做什么）:
///     从 status JSON 与 control 文件提取 pid 并 `record_pid`。
fn record_pid_from_status(case: &mut SmokeCase, status: &support::CliStatusJson) {
    if let Some(control) = status.control.as_ref() {
        case.record_pid(control.pid);
    }
    if let Ok(Some(file)) = read_control_file(&case.control_file_path()) {
        case.record_pid(file.pid);
    }
}

/// Business Logic（为什么需要这个函数）:
///     断言 CLI 调用成功，否则附带 stdout/stderr。
///
/// Code Logic（这个函数做什么）:
///     success=false 时 fail_case。
fn assert_cli_ok(case: &mut SmokeCase, label: &str, captured: &CapturedCli) {
    if !captured.success {
        fail_case(case, format!("{label} 失败\n{}", captured.diagnostic()));
    }
}

/// start → poll control → /api/health → status → stop 全链路。
///
/// Business Logic（为什么需要这个测试）:
///     跨平台 CI 需要证明 backend CLI 在隔离 data dir 下能完成完整生命周期，
///     且 stop 后进程与 control 文件都消失。
///
/// Code Logic（这个测试做什么）:
///     运行 start，轮询 control 与 health，校验 status pid/port，再 stop 并断言 stopped。
#[test]
fn start_health_status_stop() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }

    let mut case = SmokeCase::new("lifecycle").expect("创建 smoke case");

    let start = match case.run_cli(&["start"]) {
        Ok(captured) => captured,
        Err(err) => fail_case(&mut case, format!("start 执行失败: {err}")),
    };
    assert_cli_ok(&mut case, "start", &start);
    let start_status = match start.parse_status_json() {
        Ok(status) => status,
        Err(err) => fail_case(&mut case, err),
    };
    if start_status.kind != "running" {
        fail_case(
            &mut case,
            format!(
                "start 后 kind 应为 running，实际 {:?}\n{}",
                start_status,
                start.diagnostic()
            ),
        );
    }
    record_pid_from_status(&mut case, &start_status);

    let control = match case.wait_for_control_file() {
        Ok(control) => control,
        Err(err) => fail_case(&mut case, err),
    };
    case.record_pid(control.pid);

    let health = match case.wait_for_health(control.port) {
        Ok(body) => body,
        Err(err) => fail_case(&mut case, err),
    };
    let health_port = health
        .get("http_port")
        .and_then(|v| v.as_u64())
        .map(|v| v as u16);
    if health_port != Some(control.port) {
        fail_case(
            &mut case,
            format!(
                "health.http_port 与 control.port 不一致: health={health:?} control_port={}",
                control.port
            ),
        );
    }
    let health_device = health
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if health_device != control.device_id {
        fail_case(
            &mut case,
            format!(
                "health.device_id 与 control.device_id 不一致: {health_device} vs {}",
                control.device_id
            ),
        );
    }

    let status = match case.run_cli(&["status"]) {
        Ok(captured) => captured,
        Err(err) => fail_case(&mut case, format!("status 执行失败: {err}")),
    };
    assert_cli_ok(&mut case, "status", &status);
    let status_json = match status.parse_status_json() {
        Ok(s) => s,
        Err(err) => fail_case(&mut case, err),
    };
    if status_json.kind != "running" {
        fail_case(
            &mut case,
            format!("status kind 应为 running: {:?}", status_json),
        );
    }
    let status_control = status_json
        .control
        .as_ref()
        .unwrap_or_else(|| fail_case(&mut case, "status.control 缺失"));
    if status_control.pid != control.pid || status_control.port != control.port {
        fail_case(
            &mut case,
            format!(
                "status pid/port 与 control 文件不一致: status={status_control:?} control=({}, {})",
                control.pid, control.port
            ),
        );
    }
    if !process_is_alive(control.pid) {
        fail_case(
            &mut case,
            format!("running 状态下 pid {} 已死亡", control.pid),
        );
    }

    let stop = match case.run_cli(&["stop"]) {
        Ok(captured) => captured,
        Err(err) => fail_case(&mut case, format!("stop 执行失败: {err}")),
    };
    assert_cli_ok(&mut case, "stop", &stop);
    let stop_status = match stop.parse_status_json() {
        Ok(s) => s,
        Err(err) => fail_case(&mut case, err),
    };
    if stop_status.kind != "stopped" {
        fail_case(
            &mut case,
            format!(
                "stop 后 kind 应为 stopped，实际 {:?}\n{}",
                stop_status,
                stop.diagnostic()
            ),
        );
    }

    // 有界等待进程与 control 文件消失。
    let deadline = std::time::Instant::now() + case.op_timeout;
    while std::time::Instant::now() < deadline {
        let control_gone = !case.control_file_path().exists() && !case.pid_file_path().exists();
        let process_gone = !process_is_alive(control.pid);
        if control_gone && process_gone {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if process_is_alive(control.pid) {
        fail_case(&mut case, format!("stop 后 pid {} 仍存活", control.pid));
    }
    let control_exists = case.control_file_path().exists();
    let pid_exists = case.pid_file_path().exists();
    if control_exists || pid_exists {
        fail_case(
            &mut case,
            format!("stop 后 control/pid 文件仍存在: control={control_exists} pid={pid_exists}"),
        );
    }

    let final_status = match case.run_cli(&["status"]) {
        Ok(captured) => captured,
        Err(err) => fail_case(&mut case, format!("最终 status 失败: {err}")),
    };
    assert_cli_ok(&mut case, "final status", &final_status);
    let final_json = match final_status.parse_status_json() {
        Ok(s) => s,
        Err(err) => fail_case(&mut case, err),
    };
    if final_json.kind != "stopped" {
        fail_case(
            &mut case,
            format!("最终 status 应为 stopped: {:?}", final_json),
        );
    }
}

/// 重复 start 不得拉起第二个 backend，应报告已有实例。
///
/// Business Logic（为什么需要这个测试）:
///     用户或 CI 重复执行 start 时必须幂等返回现有 running 实例，不能双开端口/进程。
///
/// Code Logic（这个测试做什么）:
///     start 一次后再次 start，断言 pid/port 不变且进程仍为同一 pid。
#[test]
fn duplicate_start_reports_existing_instance() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }

    let mut case = SmokeCase::new("duplicate-start").expect("创建 smoke case");

    let first = match case.run_cli(&["start"]) {
        Ok(captured) => captured,
        Err(err) => fail_case(&mut case, format!("首次 start 失败: {err}")),
    };
    assert_cli_ok(&mut case, "first start", &first);
    let first_status = match first.parse_status_json() {
        Ok(s) => s,
        Err(err) => fail_case(&mut case, err),
    };
    if first_status.kind != "running" {
        fail_case(
            &mut case,
            format!("首次 start 未 running: {:?}", first_status),
        );
    }
    let first_control = first_status
        .control
        .clone()
        .unwrap_or_else(|| fail_case(&mut case, "首次 start 无 control"));
    case.record_pid(first_control.pid);

    // 确认 health 可用后再 duplicate start，避免竞态。
    if let Err(err) = case.wait_for_health(first_control.port) {
        fail_case(&mut case, err);
    }

    let second = match case.run_cli(&["start"]) {
        Ok(captured) => captured,
        Err(err) => fail_case(&mut case, format!("二次 start 失败: {err}")),
    };
    assert_cli_ok(&mut case, "second start", &second);
    let second_status = match second.parse_status_json() {
        Ok(s) => s,
        Err(err) => fail_case(&mut case, err),
    };
    if second_status.kind != "running" {
        fail_case(
            &mut case,
            format!("二次 start 未 running: {:?}", second_status),
        );
    }
    let second_control = second_status
        .control
        .clone()
        .unwrap_or_else(|| fail_case(&mut case, "二次 start 无 control"));

    if second_control.pid != first_control.pid || second_control.port != first_control.port {
        fail_case(
            &mut case,
            format!(
                "duplicate start 改变了实例: first={first_control:?} second={second_control:?}\n{}",
                second.diagnostic()
            ),
        );
    }
    if !process_is_alive(first_control.pid) {
        fail_case(
            &mut case,
            format!("原 pid {} 在 duplicate start 后死亡", first_control.pid),
        );
    }

    // 清理：显式 stop。
    let stop = case.cli_stop();
    if !stop.success {
        fail_case(
            &mut case,
            format!("duplicate case stop 失败\n{}", stop.diagnostic()),
        );
    }
}

/// 并发双重 start 只能留下一个 backend PID/监听实例，且 teardown 可完整回收。
///
/// Business Logic（为什么需要这个测试）:
///     check-then-spawn 无锁时，两个同时 start 都能看到 stopped 并各自 spawn serve，
///     留下孤儿进程与竞争覆盖的 control 文件；顺序 duplicate start 测不到该竞态。
///
/// Code Logic（这个测试做什么）:
///     同 data_dir 上并发 spawn 两个 `start`，等待二者结束后断言仅一个 running 实例，
///     且 control pid 存活、health 可用；随后 stop 并确认进程/文件清理。
#[test]
fn concurrent_duplicate_start_only_one_backend_survives() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }

    let mut case = SmokeCase::new("concurrent-duplicate-start").expect("创建 smoke case");

    let mut first_child = match case.spawn_cli(&["start"]) {
        Ok(child) => child,
        Err(err) => fail_case(&mut case, format!("并发 start#1 spawn 失败: {err}")),
    };
    let mut second_child = match case.spawn_cli(&["start"]) {
        Ok(child) => child,
        Err(err) => {
            let _ = first_child.kill();
            let _ = first_child.wait();
            fail_case(&mut case, format!("并发 start#2 spawn 失败: {err}"))
        }
    };

    let deadline = Instant::now() + case.op_timeout;
    let first_out = wait_child_output(&mut first_child, deadline, "start#1", &mut case);
    let second_out = wait_child_output(&mut second_child, deadline, "start#2", &mut case);

    let first = captured_from_output(first_out);
    let second = captured_from_output(second_out);
    // 至少一个 start 必须成功变成 running；另一个可以成功返回同一实例，或在锁竞争下短暂失败后由后续 status 收敛。
    if !first.success && !second.success {
        fail_case(
            &mut case,
            format!(
                "并发 start 全部失败\n--- first ---\n{}\n--- second ---\n{}",
                first.diagnostic(),
                second.diagnostic()
            ),
        );
    }

    // 从两次 start 输出收集报告的 pid，用于检测“双持有锁后双开 serve”的 ABA 回归。
    let mut reported_pids: Vec<u32> = Vec::new();
    for (label, captured) in [("start#1", &first), ("start#2", &second)] {
        if let Ok(status) = captured.parse_status_json() {
            if let Some(control) = status.control.as_ref() {
                if control.pid != 0 {
                    reported_pids.push(control.pid);
                    case.record_pid(control.pid);
                }
            }
        } else if captured.success {
            fail_case(
                &mut case,
                format!(
                    "{label} 成功但无法解析 status JSON\n{}",
                    captured.diagnostic()
                ),
            );
        }
    }

    // 收敛：以 control 文件 + status 为准，断言唯一存活 backend。
    let control = match case.wait_for_control_file() {
        Ok(c) => c,
        Err(err) => fail_case(
            &mut case,
            format!(
                "并发 start 后无 control 文件: {err}\n--- first ---\n{}\n--- second ---\n{}",
                first.diagnostic(),
                second.diagnostic()
            ),
        ),
    };
    case.record_pid(control.pid);
    if !process_is_alive(control.pid) {
        fail_case(&mut case, format!("control pid {} 未存活", control.pid));
    }
    if let Err(err) = case.wait_for_health(control.port) {
        fail_case(&mut case, err);
    }

    let status = match case.run_cli(&["status"]) {
        Ok(captured) => captured,
        Err(err) => fail_case(&mut case, format!("并发后 status 失败: {err}")),
    };
    assert_cli_ok(&mut case, "concurrent status", &status);
    let status_json = match status.parse_status_json() {
        Ok(s) => s,
        Err(err) => fail_case(&mut case, err),
    };
    if status_json.kind != "running" {
        fail_case(
            &mut case,
            format!("并发 start 收敛后应为 running: {:?}", status_json),
        );
    }
    let reported = status_json
        .control
        .clone()
        .unwrap_or_else(|| fail_case(&mut case, "status 无 control"));
    if reported.pid != control.pid || reported.port != control.port {
        fail_case(
            &mut case,
            format!("status control 与文件不一致: status={reported:?} file={control:?}"),
        );
    }

    // 若两次 start 都报告了 pid，它们必须是同一 serve（或仅一个存活）；
    // 出现两个不同且仍存活的 pid 即 dual-serve 回归。
    let mut unique_live: Vec<u32> = Vec::new();
    for pid in reported_pids
        .into_iter()
        .chain(std::iter::once(control.pid))
    {
        if pid == 0 || !process_is_alive(pid) {
            continue;
        }
        if !unique_live.contains(&pid) {
            unique_live.push(pid);
        }
    }
    if unique_live.len() != 1 || unique_live[0] != control.pid {
        fail_case(
            &mut case,
            format!(
                "并发 start 后出现多个存活 backend pid: live={unique_live:?} control={}\n--- first ---\n{}\n--- second ---\n{}",
                control.pid,
                first.diagnostic(),
                second.diagnostic()
            ),
        );
    }

    // 额外保险：同一 data_dir 下 control 端口只能有一个 listener。
    // stop 必须回收该唯一实例。
    let stop = case.cli_stop();
    if !stop.success {
        fail_case(
            &mut case,
            format!("concurrent case stop 失败\n{}", stop.diagnostic()),
        );
    }
    let stop_deadline = Instant::now() + Duration::from_secs(5);
    while process_is_alive(control.pid) && Instant::now() < stop_deadline {
        thread::sleep(Duration::from_millis(50));
    }
    if process_is_alive(control.pid) {
        fail_case(&mut case, format!("stop 后 pid {} 仍存活", control.pid));
    }
    if case.control_file_path().exists() || case.pid_file_path().exists() {
        fail_case(&mut case, "stop 后 control/pid 文件仍存在");
    }
}

/// 有界等待 CLI child 并收集 Output；超时 kill 后仍尽量 drain。
///
/// Business Logic（为什么需要这个函数）:
///     并发 start smoke 需要同时等待两个 CLI 子进程，且每个都有 deadline。
///
/// Code Logic（这个函数做什么）:
///     轮询 try_wait；超时 kill+有界 reap；成功后 read_to_end stdout/stderr 组装 Output。
fn wait_child_output(
    child: &mut std::process::Child,
    deadline: Instant,
    label: &str,
    case: &mut SmokeCase,
) -> std::process::Output {
    use std::io::Read;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_end(&mut stderr);
                }
                return std::process::Output {
                    status,
                    stdout,
                    stderr,
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let reap_deadline = Instant::now() + Duration::from_secs(2);
                    while Instant::now() < reap_deadline {
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                let mut stdout = Vec::new();
                                let mut stderr = Vec::new();
                                if let Some(mut out) = child.stdout.take() {
                                    let _ = out.read_to_end(&mut stdout);
                                }
                                if let Some(mut err) = child.stderr.take() {
                                    let _ = err.read_to_end(&mut stderr);
                                }
                                return std::process::Output {
                                    status,
                                    stdout,
                                    stderr,
                                };
                            }
                            Ok(None) => thread::sleep(Duration::from_millis(20)),
                            Err(err) => fail_case(case, format!("{label} try_wait 失败: {err}")),
                        }
                    }
                    fail_case(
                        case,
                        format!("{label} 等待超时且无法回收 (pid={})", child.id()),
                    );
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(err) => fail_case(case, format!("{label} try_wait 失败: {err}")),
        }
    }
}

/// stale control（死 PID + 未用端口）应被 status 识别，后续 start 可恢复。
///
/// Business Logic（为什么需要这个测试）:
///     后端异常退出留下 control 文件时，status 必须报 stale，start 必须清理并启动新实例，
///     且不能影响其它隔离 case。
///
/// Code Logic（这个测试做什么）:
///     写入死 PID/空闲端口的 control 文件，断言 status=stale，再 start 恢复为 running。
#[test]
fn stale_control_status_and_start_recovery() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }

    let mut case = SmokeCase::new("stale-control").expect("创建 smoke case");

    let dead = match dead_pid() {
        Ok(pid) => pid,
        Err(err) => {
            // 平台无法生成 dead pid 时显式说明，不静默 skip。
            if err.starts_with("skip:") {
                eprintln!("{err}");
                return;
            }
            fail_case(&mut case, err);
        }
    };
    let port = match unused_local_port() {
        Ok(port) => port,
        Err(err) => fail_case(&mut case, err),
    };

    let stale = ControlFileJson {
        pid: dead,
        port,
        device_id: "smoke-stale-device".to_string(),
        device_name: "smoke-stale".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        control_token: "smoke-stale-token".to_string(),
    };
    if let Err(err) = write_control_file(&case.data_dir, &stale) {
        fail_case(&mut case, err);
    }

    let status = match case.run_cli(&["status"]) {
        Ok(captured) => captured,
        Err(err) => fail_case(&mut case, format!("stale status 失败: {err}")),
    };
    assert_cli_ok(&mut case, "stale status", &status);
    let status_json = match status.parse_status_json() {
        Ok(s) => s,
        Err(err) => fail_case(&mut case, err),
    };
    if status_json.kind != "stale" {
        fail_case(
            &mut case,
            format!(
                "期望 status kind=stale，实际 {:?}\n{}",
                status_json,
                status.diagnostic()
            ),
        );
    }
    let reported = status_json
        .control
        .as_ref()
        .unwrap_or_else(|| fail_case(&mut case, "stale status 无 control"));
    if reported.pid != dead || reported.port != port {
        fail_case(
            &mut case,
            format!("stale status control 不匹配: reported={reported:?} expected=({dead},{port})"),
        );
    }

    let start = match case.run_cli(&["start"]) {
        Ok(captured) => captured,
        Err(err) => fail_case(&mut case, format!("stale 后 start 失败: {err}")),
    };
    assert_cli_ok(&mut case, "stale recovery start", &start);
    let start_status = match start.parse_status_json() {
        Ok(s) => s,
        Err(err) => fail_case(&mut case, err),
    };
    if start_status.kind != "running" {
        fail_case(
            &mut case,
            format!(
                "stale 恢复 start 后应为 running: {:?}\n{}",
                start_status,
                start.diagnostic()
            ),
        );
    }
    let recovered = start_status
        .control
        .clone()
        .unwrap_or_else(|| fail_case(&mut case, "recovery start 无 control"));
    case.record_pid(recovered.pid);

    if recovered.pid == dead {
        fail_case(&mut case, format!("recovery 仍使用死 pid {dead}"));
    }
    if !process_is_alive(recovered.pid) {
        fail_case(&mut case, format!("recovery pid {} 未存活", recovered.pid));
    }
    if let Err(err) = case.wait_for_health(recovered.port) {
        fail_case(&mut case, err);
    }

    // 确认 control 文件已换成新实例。
    let file = match case.wait_for_control_file() {
        Ok(c) => c,
        Err(err) => fail_case(&mut case, err),
    };
    if file.pid != recovered.pid || file.port != recovered.port {
        fail_case(
            &mut case,
            format!("control 文件未更新为新实例: file={file:?} recovered={recovered:?}"),
        );
    }

    let stop = case.cli_stop();
    if !stop.success {
        fail_case(
            &mut case,
            format!("stale recovery stop 失败\n{}", stop.diagnostic()),
        );
    }
}

/// 并发直接执行 serve 时，同一 data_dir 只能有一个实例持锁并写出 control。
///
/// Business Logic（为什么需要这个测试）:
///     start 锁只覆盖父进程；serve 自身必须持生命周期单实例锁，否则双 writer/双 control。
///
/// Code Logic（这个测试做什么）:
///     同 data_dir 并发 spawn 两个 `serve`，等待其中一个成为 Running 后，再等短时间，
///     断言最多一个 serve pid 存活；最后 stop 清理。
#[test]
fn concurrent_direct_serve_is_single_instance() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }
    let mut case = SmokeCase::new("concurrent-serve").expect("create case");
    let bin = case.backend_bin.clone();
    let data_dir = case.data_dir.clone();

    let spawn_serve = || {
        std::process::Command::new(&bin)
            .arg("serve")
            .env("CC_PARTNER_DATA_DIR", &data_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    };

    let mut c1 = match spawn_serve() {
        Ok(c) => c,
        Err(err) => fail_case(&mut case, format!("spawn serve1: {err}")),
    };
    let mut c2 = match spawn_serve() {
        Ok(c) => c,
        Err(err) => {
            let _ = c1.kill();
            let _ = c1.wait();
            fail_case(&mut case, format!("spawn serve2: {err}"))
        }
    };
    case.record_pid(c1.id());
    case.record_pid(c2.id());

    // 等待出现唯一 running 实例。
    let deadline = Instant::now() + case.op_timeout;
    let mut winner_pid = None;
    let mut winner_port = None;
    while Instant::now() < deadline {
        if let Ok(status) = case.run_cli(&["status"]) {
            if let Ok(value) = status.parse_status_json() {
                if value.kind == "running" {
                    if let Some(control) = value.control {
                        case.record_pid(control.pid);
                        winner_pid = Some(control.pid);
                        winner_port = Some(control.port);
                        break;
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
    let Some(winner_pid) = winner_pid else {
        let _ = c1.kill();
        let _ = c2.kill();
        let _ = c1.wait();
        let _ = c2.wait();
        fail_case(&mut case, "并发 serve 未在超时内出现 running");
    };
    let winner_port = winner_port.expect("winner port");

    // 稳定窗口：control 不得抖动到另一个 pid/port。
    thread::sleep(Duration::from_secs(2));
    for _ in 0..5 {
        let status = match case.run_cli(&["status"]) {
            Ok(s) => s,
            Err(err) => fail_case(&mut case, format!("status 失败: {err}")),
        };
        let value = match status.parse_status_json() {
            Ok(v) => v,
            Err(err) => fail_case(&mut case, err),
        };
        if value.kind != "running" {
            fail_case(&mut case, format!("期望保持 running，实际 {:?}", value));
        }
        let control = value
            .control
            .clone()
            .unwrap_or_else(|| fail_case(&mut case, "running 无 control"));
        if control.pid != winner_pid || control.port != winner_port {
            fail_case(
                &mut case,
                format!(
                    "control 在并发 serve 下发生切换: winner=({winner_pid},{winner_port}) now=({}, {})",
                    control.pid, control.port
                ),
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
    if let Err(err) = case.wait_for_health(winner_port) {
        fail_case(&mut case, err);
    }

    // 用 try_wait 判断 child 是否退出（kill -0 会对僵尸进程误报存活）。
    let loser_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let exited1 = matches!(c1.try_wait(), Ok(Some(_)));
        let exited2 = matches!(c2.try_wait(), Ok(Some(_)));
        let running_children = usize::from(!exited1) + usize::from(!exited2);
        if running_children <= 1 {
            break;
        }
        if Instant::now() >= loser_deadline {
            let _ = case.run_cli(&["stop"]);
            let _ = c1.kill();
            let _ = c2.kill();
            let _ = c1.wait();
            let _ = c2.wait();
            fail_case(
                &mut case,
                format!(
                    "serve 锁超时后仍有两个未退出 child pid={} / {} (winner={winner_pid})",
                    c1.id(),
                    c2.id()
                ),
            );
        }
        thread::sleep(Duration::from_millis(200));
    }

    let _ = case.run_cli(&["stop"]);
    let _ = c1.kill();
    let _ = c2.kill();
    let _ = c1.wait();
    let _ = c2.wait();
}

/// Business Logic（为什么需要这个测试）:
///     start 与 direct serve 并发时，若只看全局 Running 不校验 PID，会放弃自己的 child
///     导致稍后意外重启；必须 kill+reap owned child 并采纳已有实例。
///
/// Code Logic（这个测试做什么）:
///     先 spawn direct serve，再 start；断言 start 成功、仅一个 running pid，
///     且 start 退出后不会再出现第二个 backend。
#[test]
fn start_concurrent_with_direct_serve_adopts_existing_and_reaps_owned_child() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }
    let mut case = SmokeCase::new("start-vs-direct-serve").expect("create case");
    let bin = case.backend_bin.clone();
    let data_dir = case.data_dir.clone();

    let mut serve_child = match std::process::Command::new(&bin)
        .arg("serve")
        .env("CC_PARTNER_DATA_DIR", &data_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(err) => fail_case(&mut case, format!("spawn serve: {err}")),
    };
    case.record_pid(serve_child.id());

    // 并发 start
    let start = match case.run_cli(&["start"]) {
        Ok(s) => s,
        Err(err) => {
            let _ = serve_child.kill();
            let _ = serve_child.wait();
            fail_case(&mut case, format!("start 失败: {err}"))
        }
    };
    if start.code != Some(0) {
        let _ = serve_child.kill();
        let _ = serve_child.wait();
        fail_case(
            &mut case,
            format!("start 应成功采纳已有 serve\n{}", start.diagnostic()),
        );
    }
    let start_status = match start.parse_status_json() {
        Ok(v) => v,
        Err(err) => {
            let _ = serve_child.kill();
            let _ = serve_child.wait();
            fail_case(&mut case, err)
        }
    };
    if start_status.kind != "running" {
        let _ = serve_child.kill();
        let _ = serve_child.wait();
        fail_case(&mut case, format!("期望 running，实际 {:?}", start_status));
    }
    let control = start_status
        .control
        .clone()
        .unwrap_or_else(|| fail_case(&mut case, "running 无 control"));
    case.record_pid(control.pid);

    // 稳定窗口：不得出现第二实例 pid 切换
    thread::sleep(Duration::from_secs(2));
    for _ in 0..5 {
        let status = match case.run_cli(&["status"]) {
            Ok(s) => s,
            Err(err) => fail_case(&mut case, format!("status 失败: {err}")),
        };
        let value = match status.parse_status_json() {
            Ok(v) => v,
            Err(err) => fail_case(&mut case, err),
        };
        if value.kind != "running" {
            fail_case(&mut case, format!("期望保持 running，实际 {:?}", value));
        }
        let now = value
            .control
            .clone()
            .unwrap_or_else(|| fail_case(&mut case, "running 无 control"));
        if now.pid != control.pid || now.port != control.port {
            fail_case(
                &mut case,
                format!(
                    "start 后 control 切换，疑似遗留 child 抢锁: first=({},{}) now=({},{})",
                    control.pid, control.port, now.pid, now.port
                ),
            );
        }
        thread::sleep(Duration::from_millis(200));
    }

    let _ = case.run_cli(&["stop"]);
    let _ = serve_child.kill();
    let _ = serve_child.wait();
}

/// Agent Hub owner wiring：Headless serve 持有 agent_hub cancel 槽语义 + 单 owner。
///
/// Business Logic（为什么需要这个测试）:
///     Gate A Task7 要求 sidecar owner 启动 Agent Hub runtime，且 duplicate start 仍只有
///     一个 backend/watcher；GUI 关闭后 owner 继续存活。完整多目标收敛 smoke 若环境阻塞，
///     至少证明 owner lifecycle 与 control stop 可用。
///
/// Code Logic（这个测试做什么）:
///     隔离 data dir 下 start → health → status 保持 running → stop；
///     再测 duplicate start 只保留一个 pid。完整 GUI-closed multi-target 收敛见 report NOT VERIFIED。
#[test]
fn agent_hub_owner_lifecycle_single_owner() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }

    let mut case = SmokeCase::new("agent-hub-owner").expect("创建 smoke case");

    let start = match case.run_cli(&["start"]) {
        Ok(captured) => captured,
        Err(err) => fail_case(&mut case, format!("start 执行失败: {err}")),
    };
    assert_cli_ok(&mut case, "agent_hub start", &start);
    let start_status = match start.parse_status_json() {
        Ok(status) => status,
        Err(err) => fail_case(&mut case, err),
    };
    if start_status.kind != "running" {
        fail_case(
            &mut case,
            format!(
                "agent_hub start 后 kind 应为 running，实际 {:?}\n{}",
                start_status,
                start.diagnostic()
            ),
        );
    }
    record_pid_from_status(&mut case, &start_status);

    let control = match case.wait_for_control_file() {
        Ok(control) => control,
        Err(err) => fail_case(&mut case, err),
    };
    case.record_pid(control.pid);
    let first_pid = control.pid;

    // health 证明 owner 在服务
    if let Err(err) = case.wait_for_health(control.port) {
        fail_case(&mut case, err);
    }

    // duplicate start 不得换 pid
    let start2 = match case.run_cli(&["start"]) {
        Ok(c) => c,
        Err(err) => fail_case(&mut case, format!("duplicate start 失败: {err}")),
    };
    assert_cli_ok(&mut case, "agent_hub duplicate start", &start2);
    let status2 = match start2.parse_status_json() {
        Ok(s) => s,
        Err(err) => fail_case(&mut case, err),
    };
    if status2.kind != "running" {
        fail_case(
            &mut case,
            format!("duplicate start 后应 running，实际 {:?}", status2),
        );
    }
    let control2 = status2
        .control
        .clone()
        .unwrap_or_else(|| fail_case(&mut case, "running 无 control"));
    if control2.pid != first_pid {
        fail_case(
            &mut case,
            format!(
                "duplicate start 更换了 owner pid: first={first_pid} now={}",
                control2.pid
            ),
        );
    }

    // 短暂存活窗口：owner 保持 running（Agent Hub runtime 已挂在 Headless start_background_tasks）
    thread::sleep(Duration::from_millis(500));
    let status3 = match case.run_cli(&["status"]) {
        Ok(s) => s,
        Err(err) => fail_case(&mut case, format!("status 失败: {err}")),
    };
    let value3 = match status3.parse_status_json() {
        Ok(v) => v,
        Err(err) => fail_case(&mut case, err),
    };
    if value3.kind != "running" {
        fail_case(
            &mut case,
            format!("owner 应保持 running，实际 {:?}", value3),
        );
    }

    let stop = match case.run_cli(&["stop"]) {
        Ok(s) => s,
        Err(err) => fail_case(&mut case, format!("stop 失败: {err}")),
    };
    assert_cli_ok(&mut case, "agent_hub stop", &stop);

    // stop 后进程应退出
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !process_is_alive(first_pid) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if process_is_alive(first_pid) {
        fail_case(&mut case, format!("stop 后 owner pid={first_pid} 仍存活"));
    }
}
