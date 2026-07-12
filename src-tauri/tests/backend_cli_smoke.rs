//! Backend CLI 生命周期冒烟测试。
//!
//! 覆盖 start→health→status→stop、duplicate start、stale control recovery。
//! 使用 `CC_PARTNER_DATA_DIR` 隔离，不触碰用户真实 `~/.cc-partner`。

mod support;

use support::{
    dead_pid, ensure_platform_supported, process_is_alive, read_control_file, unused_local_port,
    write_control_file, CapturedCli, ControlFileJson, SmokeCase,
};
use std::thread;
use std::time::Duration;

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
        fail_case(
            case,
            format!("{label} 失败\n{}", captured.diagnostic()),
        );
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
        fail_case(
            &mut case,
            format!("stop 后 pid {} 仍存活", control.pid),
        );
    }
    let control_exists = case.control_file_path().exists();
    let pid_exists = case.pid_file_path().exists();
    if control_exists || pid_exists {
        fail_case(
            &mut case,
            format!(
                "stop 后 control/pid 文件仍存在: control={control_exists} pid={pid_exists}"
            ),
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
        fail_case(
            &mut case,
            format!("recovery 仍使用死 pid {dead}"),
        );
    }
    if !process_is_alive(recovered.pid) {
        fail_case(
            &mut case,
            format!("recovery pid {} 未存活", recovered.pid),
        );
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
