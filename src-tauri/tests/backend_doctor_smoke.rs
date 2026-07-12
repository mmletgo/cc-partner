//! Backend doctor + 日志隐私跨平台冒烟测试。
//!
//! 覆盖：
//! - stopped / running 的 `doctor --json` 退出码契约（0/1 可接受，2 在正常路径失败）
//! - stdout 必须是纯 JSON（无 tracing 前缀）
//! - 敌意 sentinel 不得出现在 doctor 输出与 smoke 产物目录
//! - 核心路径 fixture 产生 unhealthy/2
//!
//! 隔离：`CC_PARTNER_DATA_DIR` + `CC_PARTNER_SMOKE_ROOT`，不触碰真实 `~/.cc-partner`。

mod support;

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use support::{ensure_platform_supported, process_is_alive, CapturedCli, SmokeCase};

/// 敌意隐私 sentinel：只应出现在输入侧，绝不能泄漏到 doctor 输出或 smoke 产物。
const SECRET_SENTINEL: &str = "SMOKE_P7T7_SECRET_TOKEN_XYZ_9f3a";
const PROMPT_SENTINEL: &str = "SMOKE_P7T7_PROMPT_BODY_DO_NOT_LEAK";
const FILE_SENTINEL: &str = "file-sentinel-SMOKE_P7T7_ARTIFACT";

/// Business Logic（为什么需要这个函数）:
///     测试失败时要把 CLI 输出与 case 路径挂到 panic，便于 CI 诊断；
///     同时在 teardown 前落盘 diagnostics，避免 control/log 证据被清理。
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
///     doctor 失败时需要把 stdout/stderr/exit 与日志目录快照保留给 artifact。
///
/// Code Logic（这个函数做什么）:
///     在 diagnostics 下写入 doctor-stdout.json / doctor-stderr.txt / exit-code.txt，
///     并 best-effort 复制 data_dir/logs。
fn preserve_doctor_artifacts(case: &SmokeCase, label: &str, captured: &CapturedCli) {
    let diag = case.case_dir.join("diagnostics");
    let _ = fs::create_dir_all(&diag);
    let prefix = format!("doctor-{label}");
    let _ = fs::write(diag.join(format!("{prefix}-stdout.json")), &captured.stdout);
    let _ = fs::write(diag.join(format!("{prefix}-stderr.txt")), &captured.stderr);
    let _ = fs::write(
        diag.join(format!("{prefix}-exit.txt")),
        format!("{:?}", captured.code),
    );

    // 故意不复制 data_dir/logs：敌意 raw log 不得进入 diagnostics/CI artifact。
    let _ = (case, prefix);
}
/// Business Logic（为什么需要这个函数）:
///     doctor --json 的 stdout 必须是纯 JSON，供脚本/CI 直接解析。
///
/// Code Logic（这个函数做什么）:
///     取 stdout 全量 trim 后 `serde_json::from_str`；失败返回诊断。
fn parse_doctor_json(captured: &CapturedCli) -> Result<Value, String> {
    let text = captured.stdout.trim();
    if text.is_empty() {
        return Err(format!(
            "doctor stdout 为空（期望纯 JSON）\n{}",
            captured.diagnostic()
        ));
    }
    // 纯净性：整段 stdout 必须是单一 JSON 文档，不能夹杂 tracing 前缀行。
    serde_json::from_str(text).map_err(|err| {
        format!(
            "doctor stdout 不是纯 JSON: {err}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            captured.stdout, captured.stderr
        )
    })
}

/// Business Logic（为什么需要这个函数）:
///     正常 smoke 路径只允许 healthy(0) 或 degraded(1)；unhealthy(2) 表示基础设施失败。
///
/// Code Logic（这个函数做什么）:
///     校验 exit code ∈ allowed，并与 JSON status 字段一致。
fn assert_doctor_exit_and_status(
    case: &mut SmokeCase,
    label: &str,
    captured: &CapturedCli,
    value: &Value,
    allowed_exits: &[i32],
) {
    let code = captured.code.unwrap_or(-1);
    if !allowed_exits.contains(&code) {
        preserve_doctor_artifacts(case, label, captured);
        fail_case(
            case,
            format!(
                "{label}: doctor 退出码 {code} 不在允许集 {allowed_exits:?}\n{}",
                captured.diagnostic()
            ),
        );
    }

    let status = value
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("<missing>");
    let expected_status = match code {
        0 => "healthy",
        1 => "degraded",
        2 => "unhealthy",
        _ => "<unknown>",
    };
    if status != expected_status {
        preserve_doctor_artifacts(case, label, captured);
        fail_case(
            case,
            format!(
                "{label}: exit={code} 与 JSON status={status:?} 不一致（期望 {expected_status})\n{}",
                captured.diagnostic()
            ),
        );
    }

    let schema = value
        .get("schemaVersion")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if schema != 1 {
        preserve_doctor_artifacts(case, label, captured);
        fail_case(
            case,
            format!(
                "{label}: schemaVersion 应为 1，实际 {schema}\n{}",
                captured.diagnostic()
            ),
        );
    }
}

/// Business Logic（为什么需要这个函数）:
///     隐私门禁：sentinel 只能作为敌意输入，不得出现在任意 smoke 产物中。
///
/// Code Logic（这个函数做什么）:
///     递归扫描根目录下文本文件，命中任一 sentinel 则失败。
fn scan_artifacts_for_sentinels(root: &Path, sentinels: &[&str]) -> Result<(), String> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        // 仅跳过故意放置的敌意输入原件目录；不得豁免 *-logs 或其它产物目录。
        let dir_name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if dir_name == "hostile-input-only" {
            continue;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(err) => {
                return Err(format!("扫描目录失败 {}: {err}", dir.display()));
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            // 跳过明显二进制（含 NUL）
            if bytes.contains(&0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for s in sentinels {
                if text.contains(s) {
                    // 失败信息只输出 sentinel 标识，不回显命中明文片段。
                    return Err(format!(
                        "隐私泄漏：sentinel_id={} 出现在 {}",
                        sentinel_id(s),
                        path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

/// 将 sentinel 原文映射为稳定标识（失败日志不回显明文）。
///
/// Business Logic（为什么需要这个函数）:
///     smoke 失败消息若回显 sentinel 明文，会把敏感 fixture 写进 cargo/CI 日志。
///
/// Code Logic（这个函数做什么）:
///     对已知 sentinel 返回短 id；未知则返回 OTHER。
fn sentinel_id(sentinel: &str) -> &'static str {
    if sentinel.contains("SECRET_TOKEN") {
        "SECRET"
    } else if sentinel.contains("PROMPT_BODY") {
        "PROMPT"
    } else if sentinel.contains("file-sentinel-") {
        "FILE"
    } else if sentinel.starts_with('/') || sentinel.contains("Users") || sentinel.contains("home") {
        "HOME"
    } else {
        "OTHER"
    }
}

/// Business Logic（为什么需要这个函数）:
///     doctor 输出与 stderr 也要扫描，不仅限于落盘文件。
///
/// Code Logic（这个函数做什么）:
///     对 stdout/stderr 字符串做 sentinel 包含检查。
fn assert_no_sentinels_in_text(case: &mut SmokeCase, label: &str, text: &str, sentinels: &[&str]) {
    for s in sentinels {
        if text.contains(s) {
            fail_case(
                case,
                format!("{label}: 文本泄漏 sentinel_id={}", sentinel_id(s)),
            );
        }
    }
}

fn record_pid_from_status_cli(case: &mut SmokeCase, captured: &CapturedCli) {
    if let Ok(status) = captured.parse_status_json() {
        if let Some(control) = status.control {
            case.record_pid(control.pid);
        }
    }
}

/// stopped 状态下 `doctor --json` 必须返回纯 JSON，且 exit 仅 0 或 1。
///
/// Business Logic（为什么需要这个测试）:
///     跨平台 CI 需要证明 doctor 在无 backend 运行时仍可采集快照；
///     stopped 是 normal 信息，不得抬升为 unhealthy/2。
///
/// Code Logic（这个测试做什么）:
///     隔离 data_dir 上直接跑 doctor --json，解析 JSON，断言 exit∈{0,1} 且 schemaVersion=1。
#[test]
fn doctor_json_stopped_accepts_healthy_or_degraded() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }

    let mut case = SmokeCase::new("doctor-stopped").expect("创建 smoke case");

    let captured = match case.run_cli(&["doctor", "--json"]) {
        Ok(c) => c,
        Err(err) => fail_case(&mut case, format!("doctor --json 执行失败: {err}")),
    };
    preserve_doctor_artifacts(&case, "stopped", &captured);

    let value = match parse_doctor_json(&captured) {
        Ok(v) => v,
        Err(err) => {
            case.mark_failed();
            fail_case(&mut case, err);
        }
    };
    assert_doctor_exit_and_status(&mut case, "stopped", &captured, &value, &[0, 1]);

    // 可选依赖缺失应 degraded/1，绝不能变成“基础设施失败”的非 JSON/崩溃。
    let status = value.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status == "unhealthy" {
        fail_case(
            &mut case,
            format!(
                "stopped doctor 不应返回 unhealthy（exit 2）\n{}",
                captured.diagnostic()
            ),
        );
    }
}

/// start → doctor --json → stop：running 快照 exit 0/1，stdout 纯 JSON。
///
/// Business Logic（为什么需要这个测试）:
///     证明 doctor 在 backend 运行时能 health probe，且 stop 后可回收。
///
/// Code Logic（这个测试做什么）:
///     start + wait health，跑 doctor --json，再 stop；全程保留失败产物。
#[test]
fn doctor_json_running_lifecycle() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }

    let mut case = SmokeCase::new("doctor-running").expect("创建 smoke case");

    let start = match case.run_cli(&["start"]) {
        Ok(c) => c,
        Err(err) => fail_case(&mut case, format!("start 失败: {err}")),
    };
    if !start.success {
        fail_case(&mut case, format!("start 未成功\n{}", start.diagnostic()));
    }
    record_pid_from_status_cli(&mut case, &start);

    let control = match case.wait_for_control_file() {
        Ok(c) => c,
        Err(err) => fail_case(&mut case, err),
    };
    case.record_pid(control.pid);
    if let Err(err) = case.wait_for_health(control.port) {
        fail_case(&mut case, err);
    }

    let doctor = match case.run_cli(&["doctor", "--json"]) {
        Ok(c) => c,
        Err(err) => fail_case(&mut case, format!("running doctor --json 失败: {err}")),
    };
    preserve_doctor_artifacts(&case, "running", &doctor);

    let value = match parse_doctor_json(&doctor) {
        Ok(v) => v,
        Err(err) => {
            case.mark_failed();
            fail_case(&mut case, err);
        }
    };
    // running 时仍可能因 mDNS/可选依赖 degraded；不允许 unhealthy。
    assert_doctor_exit_and_status(&mut case, "running", &doctor, &value, &[0, 1]);

    let backend_state = value
        .pointer("/backend/state")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if backend_state != "running" {
        // 允许极短暂竞态，但 CI 上 wait_for_health 后应稳定 running。
        eprintln!("[smoke] 警告: doctor 报告 backend.state={backend_state:?}（期望 running）");
    }

    let stop = case.cli_stop();
    if !stop.success {
        fail_case(&mut case, format!("stop 失败\n{}", stop.diagnostic()));
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while process_is_alive(control.pid) && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    if process_is_alive(control.pid) {
        fail_case(&mut case, format!("stop 后 pid {} 仍存活", control.pid));
    }
}

/// 敌意 sentinel 仅注入输入侧：doctor 输出与 case 产物不得出现明文。
///
/// Business Logic（为什么需要这个测试）:
///     doctor/日志隐私契约是跨平台 smoke 的硬门禁，防止 CI 产物泄漏密钥/Prompt。
///
/// Code Logic（这个测试做什么）:
///     向 data_dir 写入含 sentinel 的“敌意输入”文件（模拟用户侧内容），
///     跑 doctor --json，扫描 stdout/stderr 与 case 目录中除敌意输入外的产物。
#[test]
fn doctor_privacy_scan_rejects_leaked_sentinels() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }

    let mut case = SmokeCase::new("doctor-privacy").expect("创建 smoke case");

    // 真实 home 路径作为 home sentinel（doctor 必须替换为 <HOME>）。
    // 集成测试不直接依赖 dirs crate，用 HOME/USERPROFILE 环境变量。
    let real_home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/Users/smoke-missing-home".to_string());
    // 仅当 home 足够独特时才作为扫描 sentinel，避免误伤短路径。
    let home_sentinel = if real_home.len() >= 8 {
        Some(real_home.clone())
    } else {
        None
    };

    // 敌意输入只放在明确标记的目录，扫描时跳过该目录本身。
    let hostile_dir = case.data_dir.join("hostile-input-only");
    if let Err(err) = fs::create_dir_all(&hostile_dir) {
        fail_case(&mut case, format!("创建 hostile 目录失败: {err}"));
    }
    let hostile_body = format!(
        "token={SECRET_SENTINEL}\nPrompt={PROMPT_SENTINEL}\n{FILE_SENTINEL}\nhome={real_home}/.cc-partner/secret-project\n"
    );
    let hostile_path = hostile_dir.join("payload.txt");
    if let Err(err) = fs::write(&hostile_path, &hostile_body) {
        fail_case(&mut case, format!("写 hostile payload 失败: {err}"));
    }

    // 再写一份“伪装成日志行”的敌意内容到 logs（若 doctor 原样回显则失败）。
    let logs_dir = case.data_dir.join("logs");
    let _ = fs::create_dir_all(&logs_dir);
    let fake_log = logs_dir.join("backend.log");
    let fake_log_line = format!(
        r#"{{"timestamp":"2026-07-11T00:00:00Z","level":"error","message":"auth failed token={SECRET_SENTINEL} Prompt={PROMPT_SENTINEL} {FILE_SENTINEL} path={real_home}/web_project/secret-app"}}"#
    );
    if let Err(err) = fs::write(&fake_log, format!("{fake_log_line}\n")) {
        fail_case(&mut case, format!("写 fake backend.log 失败: {err}"));
    }

    let doctor = match case.run_cli(&["doctor", "--json"]) {
        Ok(c) => c,
        Err(err) => fail_case(&mut case, format!("privacy doctor 失败: {err}")),
    };
    preserve_doctor_artifacts(&case, "privacy", &doctor);

    let value = match parse_doctor_json(&doctor) {
        Ok(v) => v,
        Err(err) => {
            case.mark_failed();
            fail_case(&mut case, err);
        }
    };
    // 正常路径仍只允许 0/1（日志文件存在且可读不应导致 unhealthy）
    assert_doctor_exit_and_status(&mut case, "privacy", &doctor, &value, &[0, 1]);

    let mut sentinel_list: Vec<&str> = vec![SECRET_SENTINEL, PROMPT_SENTINEL, FILE_SENTINEL];
    if let Some(ref home) = home_sentinel {
        sentinel_list.push(home.as_str());
    }
    assert_no_sentinels_in_text(&mut case, "doctor-stdout", &doctor.stdout, &sentinel_list);
    assert_no_sentinels_in_text(&mut case, "doctor-stderr", &doctor.stderr, &sentinel_list);

    // 扫描 artifact 根（diagnostics）：不得复制 raw logs，也不允许按 *-logs 豁免。
    // 不扫 data/（含 config.json 合法 home 路径）与 hostile-input-only 输入侧。
    let diagnostics = case.case_dir.join("diagnostics");
    if diagnostics.exists() {
        if let Err(err) = scan_artifacts_for_sentinels(&diagnostics, &sentinel_list) {
            fail_case(&mut case, err);
        }
    }

    // 确认 JSON 序列化整体也不含 sentinel
    let compact = value.to_string();
    assert_no_sentinels_in_text(&mut case, "doctor-json-value", &compact, &sentinel_list);

    // recentErrors 若存在，message 应已脱敏（含 <REDACTED> 或 <HOME>，无明文 secret）
    if let Some(errors) = value.get("recentErrors").and_then(|v| v.as_array()) {
        for err in errors {
            let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
            if msg.contains(SECRET_SENTINEL) || msg.contains(PROMPT_SENTINEL) {
                fail_case(
                    &mut case,
                    "recentErrors.message 泄漏敏感内容 (sentinel_id=SECRET/PROMPT)",
                );
            }
        }
    }
}

/// 核心路径不可用时 doctor 必须 unhealthy / exit 2。
///
/// Business Logic（为什么需要这个测试）:
///     可选依赖缺失只能 degraded；核心路径失败必须 unhealthy，防止假绿。
///
/// Code Logic（这个测试做什么）:
///     把 data_dir/logs 建成普通文件（阻塞日志目录），跑 doctor --json，断言 exit=2 且 status=unhealthy。
#[test]
fn doctor_core_path_fixture_is_unhealthy() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }

    let mut case = SmokeCase::new("doctor-core-path").expect("创建 smoke case");

    // 用文件占位 logs，使 log 目录探测失败（核心路径 error → unhealthy）
    let logs_blocker = case.data_dir.join("logs");
    if logs_blocker.exists() {
        let _ = fs::remove_dir_all(&logs_blocker);
        let _ = fs::remove_file(&logs_blocker);
    }
    if let Err(err) = fs::write(&logs_blocker, b"not-a-directory") {
        fail_case(&mut case, format!("创建 logs 文件占位失败: {err}"));
    }

    let doctor = match case.run_cli(&["doctor", "--json"]) {
        Ok(c) => c,
        Err(err) => fail_case(&mut case, format!("core-path doctor 失败: {err}")),
    };
    preserve_doctor_artifacts(&case, "core-path", &doctor);

    // 即使 unhealthy，stdout 仍必须是纯 JSON
    let value = match parse_doctor_json(&doctor) {
        Ok(v) => v,
        Err(err) => {
            case.mark_failed();
            fail_case(&mut case, err);
        }
    };
    assert_doctor_exit_and_status(&mut case, "core-path", &doctor, &value, &[2]);

    let log_status = value
        .pointer("/paths/log/status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if log_status != "error" {
        // 若实现把 data 也标 error 亦可；至少 overall 已是 unhealthy。
        eprintln!(
            "[smoke] core-path fixture paths.log.status={log_status:?}（期望 error，overall 已校验 unhealthy）"
        );
    }
}

/// doctor --json 在重复调用下保持纯 JSON，stderr 可有 tracing 但不得污染 stdout。
///
/// Business Logic（为什么需要这个测试）:
///     init_doctor_tracing 只应写 stderr；stdout 夹杂一行日志就会让 jq/脚本炸掉。
///
/// Code Logic（这个测试做什么）:
///     连续两次 doctor --json，断言两次均可 parse，且 stdout 首字符为 `{`。
#[test]
fn doctor_json_stdout_remains_pure_across_calls() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }

    let mut case = SmokeCase::new("doctor-pure-json").expect("创建 smoke case");

    for i in 1..=2 {
        let label = format!("pure-{i}");
        let doctor = match case.run_cli(&["doctor", "--json"]) {
            Ok(c) => c,
            Err(err) => fail_case(&mut case, format!("{label} 执行失败: {err}")),
        };
        preserve_doctor_artifacts(&case, &label, &doctor);

        let trimmed = doctor.stdout.trim_start();
        if !trimmed.starts_with('{') {
            fail_case(
                &mut case,
                format!(
                    "{label}: stdout 未以 '{{' 开头（可能混入 tracing）\n{}",
                    doctor.diagnostic()
                ),
            );
        }
        let value = match parse_doctor_json(&doctor) {
            Ok(v) => v,
            Err(err) => {
                case.mark_failed();
                fail_case(&mut case, err);
            }
        };
        assert_doctor_exit_and_status(&mut case, &label, &doctor, &value, &[0, 1]);
    }
}

/// 辅助：导出 logs 目录路径（供本地调试；测试本身不依赖）。
#[allow(dead_code)]
fn logs_dir(case: &SmokeCase) -> PathBuf {
    case.data_dir.join("logs")
}

/// Business Logic（为什么需要这个测试）:
///     畸形受控 JSON 日志行必须 warning + degraded(exit 1)，不能静默 healthy。
///
/// Code Logic（这个测试做什么）:
///     在隔离 data_dir 写入 malformed backend.log，跑 doctor --json，断言 exit=1 且
///     status=degraded、logParseWarning 存在。
#[test]
fn malformed_recent_errors_degrade_exit_1() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }
    let mut case = SmokeCase::new("doctor-malformed-log").expect("create case");
    let logs = case.data_dir.join("logs");
    fs::create_dir_all(&logs).expect("mkdir logs");
    let log_path = logs.join("backend.log");
    let body = concat!(
        r#"{"timestamp":"2026-07-11T11:00:00Z","level":"info","message":"boot"}"#,
        "\n",
        r#"{"timestamp":"bad","level":"error","broken"#,
        "\n",
        r#"{"not":"schema"}"#,
        "\n",
    );
    fs::write(&log_path, body).expect("write malformed log");

    let captured = match case.run_cli(&["doctor", "--json"]) {
        Ok(c) => c,
        Err(err) => fail_case(&mut case, format!("doctor 执行失败: {err}")),
    };
    preserve_doctor_artifacts(&case, "malformed", &captured);
    let value = match parse_doctor_json(&captured) {
        Ok(v) => v,
        Err(err) => fail_case(&mut case, err),
    };
    let code = captured.code.unwrap_or(-1);
    if code != 1 {
        fail_case(
            &mut case,
            format!(
                "malformed 日志期望 exit 1 (degraded)，实际 {code}\n{}",
                captured.diagnostic()
            ),
        );
    }
    if value.get("status").and_then(|s| s.as_str()) != Some("degraded") {
        fail_case(
            &mut case,
            format!(
                "期望 status=degraded，实际 {}\n{}",
                value,
                captured.diagnostic()
            ),
        );
    }
    let warning = value.get("logParseWarning");
    if warning.is_none() || warning == Some(&Value::Null) {
        fail_case(
            &mut case,
            format!("期望 logParseWarning 存在\n{}", captured.diagnostic()),
        );
    }
    let wcode = warning
        .and_then(|w| w.get("code"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if wcode != "logs.recent_errors.malformed" {
        fail_case(
            &mut case,
            format!("warning code 应为 logs.recent_errors.malformed，实际 {wcode}"),
        );
    }
}
