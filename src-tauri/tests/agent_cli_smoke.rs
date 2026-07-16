//! Agent-first CLI 隔离冒烟测试。
//!
//! 覆盖：隔离 backend start → `cc-partner --json` project/session/task/agent/browser 查询、
//! 幂等 task create（stdin）、backend lifecycle 仍可用、Tauri externalBin 仍仅 backend。

mod support;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use support::{ensure_platform_supported, CapturedCli, SmokeCase};

/// Business Logic（为什么需要这个函数）:
///     失败时要附 case 诊断，便于 CI。
///
/// Code Logic（这个函数做什么）:
///     mark_failed + panic 带路径。
fn fail_case(case: &mut SmokeCase, message: impl AsRef<str>) -> ! {
    let message = message.as_ref();
    case.mark_failed();
    case.write_failure_diagnostics(message);
    panic!(
        "{message}\ncase_dir={}\ndata_dir={}",
        case.case_dir.display(),
        case.data_dir.display()
    );
}

/// Business Logic（为什么需要这个函数）:
///     agent CLI 与 backend 二进制分离，必须用独立 CARGO_BIN_EXE。
///
/// Code Logic（这个函数做什么）:
///     返回 `CARGO_BIN_EXE_cc-partner` 路径。
fn agent_cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cc-partner"))
}

/// Business Logic（为什么需要这个函数）:
///     运行 agent CLI 并继承隔离 data dir。
///
/// Code Logic（这个函数做什么）:
///     Command + CC_PARTNER_DATA_DIR；可选 stdin 写入 body。
fn run_agent_cli(
    case: &SmokeCase,
    args: &[&str],
    stdin_body: Option<&str>,
) -> Result<CapturedCli, String> {
    let mut cmd = Command::new(agent_cli_bin());
    cmd.args(args)
        .env("CC_PARTNER_DATA_DIR", &case.data_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin_body.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn cc-partner {:?} 失败: {e}", args))?;
    if let Some(body) = stdin_body {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(body.as_bytes())
                .map_err(|e| format!("write stdin: {e}"))?;
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait cc-partner: {e}"))?;
    Ok(support::captured_from_output(output))
}

/// Business Logic（为什么需要这个函数）:
///     断言 JSON envelope ok=true 且 exit 0。
///
/// Code Logic（这个函数做什么）:
///     解析 stdout JSON；校验 schemaVersion/ok。
fn assert_json_ok(case: &mut SmokeCase, label: &str, captured: &CapturedCli) -> serde_json::Value {
    if captured.code != Some(0) {
        fail_case(
            case,
            format!("{label} exit 非 0\n{}", captured.diagnostic()),
        );
    }
    let line = captured
        .stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let body: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => fail_case(
            case,
            format!("{label} JSON 解析失败: {e}\n{}", captured.diagnostic()),
        ),
    };
    if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        fail_case(
            case,
            format!("{label} ok!=true: {body}\n{}", captured.diagnostic()),
        );
    }
    if body.get("schemaVersion").and_then(|v| v.as_u64()) != Some(1) {
        fail_case(case, format!("{label} schemaVersion!=1: {body}"));
    }
    // --json 时 stderr 应尽量干净（诊断可空）
    body
}

/// start backend → project list / attention / fleet / agent list 查询 + task create stdin。
///
/// Business Logic（为什么需要这个测试）:
///     证明独立 `cc-partner` 二进制可经 control plane 读 owner 状态，且 lifecycle CLI 不受影响。
///
/// Code Logic（这个测试做什么）:
///     隔离 start → health → agent queries → stop；检查 externalBin 配置仍仅 backend。
#[test]
fn agent_cli_queries_and_idempotent_create_against_isolated_backend() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }

    let mut case = SmokeCase::new("agent-cli").expect("创建 smoke case");

    // Tauri externalBin 仍仅 backend
    let conf = include_str!("../tauri.conf.json");
    assert!(
        conf.contains("binaries/cc-partner-backend"),
        "externalBin must keep backend sidecar"
    );
    assert!(
        !conf.contains("binaries/cc-partner\""),
        "cc-partner agent CLI must not be externalBin"
    );

    let start = match case.run_cli(&["start"]) {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    if !start.success {
        fail_case(&mut case, format!("backend start 失败\n{}", start.diagnostic()));
    }
    let control = match case.wait_for_control_file() {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    case.record_pid(control.pid);
    if let Err(e) = case.wait_for_health(control.port) {
        fail_case(&mut case, e);
    }

    // project list
    let list = match run_agent_cli(&case, &["--json", "project", "list"], None) {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    let body = assert_json_ok(&mut case, "project list", &list);
    assert!(
        body.get("data").is_some(),
        "project list missing data: {body}"
    );

    // attention list
    let att = match run_agent_cli(&case, &["--json", "attention", "list"], None) {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    let _ = assert_json_ok(&mut case, "attention list", &att);

    // fleet snapshot
    let fleet = match run_agent_cli(&case, &["--json", "fleet", "snapshot"], None) {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    let _ = assert_json_ok(&mut case, "fleet snapshot", &fleet);

    // 经 control workbench 添加本机项目，供 task create 幂等 smoke
    let project_dir = case.case_dir.join("sample-project");
    if let Err(e) = std::fs::create_dir_all(&project_dir) {
        fail_case(&mut case, format!("create sample project dir: {e}"));
    }
    // 最小 git repo（部分 workbench add 路径会检查目录存在即可）
    let _ = Command::new("git")
        .args(["init"])
        .current_dir(&project_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let project = match add_local_project_via_control(
        &control.control_token,
        control.port,
        &project_dir,
    ) {
        Ok(p) => p,
        Err(e) => fail_case(&mut case, e),
    };
    let project_id = project
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if project_id.is_empty() {
        fail_case(&mut case, format!("projects.add missing id: {project}"));
    }

    // session list（无 session 也须 envelope ok）
    let sessions = match run_agent_cli(
        &case,
        &["--json", "session", "list", "--project", &format!("id:{project_id}")],
        None,
    ) {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    let _ = assert_json_ok(&mut case, "session list", &sessions);

    // agent list
    let agents = match run_agent_cli(
        &case,
        &["--json", "agent", "list", "--project", &format!("id:{project_id}")],
        None,
    ) {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    let _ = assert_json_ok(&mut case, "agent list", &agents);

    // task create stdin + 同 clientRequestId 重放 → 同一 task
    let create_body = format!(
        r#"{{"title":"smoke task","goal":"g","acceptanceCriteria":"a","clientRequestId":"smoke-req-1"}}"#
    );
    let create1 = match run_agent_cli(
        &case,
        &[
            "--json",
            "task",
            "create",
            "--project",
            &format!("id:{project_id}"),
            "--input-json",
            "-",
        ],
        Some(&create_body),
    ) {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    let body1 = assert_json_ok(&mut case, "task create #1", &create1);
    let task_id_1 = extract_task_id(&body1["data"]).unwrap_or_default();
    if task_id_1.is_empty() {
        fail_case(&mut case, format!("task create #1 missing task id: {body1}"));
    }
    let create2 = match run_agent_cli(
        &case,
        &[
            "--json",
            "task",
            "create",
            "--project",
            &format!("id:{project_id}"),
            "--input-json",
            "-",
        ],
        Some(&create_body),
    ) {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    let body2 = assert_json_ok(&mut case, "task create #2 (idempotent)", &create2);
    let task_id_2 = extract_task_id(&body2["data"]).unwrap_or_default();
    if task_id_1 != task_id_2 {
        fail_case(
            &mut case,
            format!("idempotent create mismatch: {task_id_1} vs {task_id_2}\n{body1}\n{body2}"),
        );
    }

    // 缺 clientRequestId → usage exit 2
    let missing_rid = match run_agent_cli(
        &case,
        &[
            "--json",
            "task",
            "create",
            "--project",
            &format!("id:{project_id}"),
            "--input-json",
            "-",
        ],
        Some(r#"{"title":"x","goal":"y","acceptanceCriteria":"z"}"#),
    ) {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    if missing_rid.code != Some(2) {
        fail_case(
            &mut case,
            format!(
                "missing clientRequestId should exit 2\n{}",
                missing_rid.diagnostic()
            ),
        );
    }

    // usage error exit 2（缺 action）
    let usage = match run_agent_cli(&case, &["--json", "project"], None) {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    if usage.code != Some(2) {
        // clap missing subcommand may be 2
        if usage.code != Some(2) && !usage.stdout.contains("\"ok\":false") {
            fail_case(
                &mut case,
                format!("expected usage exit 2, got\n{}", usage.diagnostic()),
            );
        }
    }

    // backend status 仍可用
    let status = match case.run_cli(&["status"]) {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    if !status.success {
        fail_case(
            &mut case,
            format!("backend status 失败\n{}", status.diagnostic()),
        );
    }

    let stop = match case.run_cli(&["stop"]) {
        Ok(c) => c,
        Err(e) => fail_case(&mut case, e),
    };
    if !stop.success {
        fail_case(&mut case, format!("backend stop 失败\n{}", stop.diagnostic()));
    }
}

/// Business Logic（为什么需要这个函数）:
///     smoke 需要经 control plane 注册本机项目，不能绕开 workbench control。
///
/// Code Logic（这个函数做什么）:
///     curl POST `/api/backend/control/workbench` op=projects.add。
fn add_local_project_via_control(
    token: &str,
    port: u16,
    path: &std::path::Path,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({
        "controlToken": token,
        "op": "projects.add",
        "payload": { "path": path.to_string_lossy() }
    });
    let url = format!("http://127.0.0.1:{port}/api/backend/control/workbench");
    let output = Command::new("curl")
        .args([
            "-sS",
            "-X",
            "POST",
            &url,
            "-H",
            "Content-Type: application/json",
            "-d",
            &body.to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("curl projects.add spawn: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "curl projects.add failed: status={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("projects.add JSON: {e}; body={text}"))?;
    if let Some(result) = v.get("result") {
        return Ok(result.clone());
    }
    Ok(v)
}

/// Business Logic（为什么需要这个函数）:
///     task view DTO 可能是 local 包装或扁平 task 字段。
///
/// Code Logic（这个函数做什么）:
///     从 data.task.id / data.id / data.taskId 提取任务 id。
fn extract_task_id(data: &serde_json::Value) -> Option<String> {
    data.pointer("/task/id")
        .or_else(|| data.get("id"))
        .or_else(|| data.get("taskId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Business Logic（为什么需要这个测试）:
///     backend 离线时 agent CLI 应 exit 5 unavailable，且 JSON envelope 稳定。
///
/// Code Logic（这个测试做什么）:
///     不 start backend，直接 project list --json。
#[test]
fn agent_cli_offline_backend_is_exit_five() {
    if let Err(reason) = ensure_platform_supported() {
        eprintln!("{reason}");
        return;
    }
    let case = SmokeCase::new("agent-cli-offline").expect("case");
    let captured = run_agent_cli(&case, &["--json", "project", "list"], None)
        .expect("run agent cli");
    assert_eq!(
        captured.code,
        Some(5),
        "offline should be exit 5\n{}",
        captured.diagnostic()
    );
    let body: serde_json::Value =
        serde_json::from_str(captured.stdout.lines().find(|l| !l.trim().is_empty()).unwrap_or("{}"))
            .expect("json");
    assert_eq!(body["ok"], false);
    assert_eq!(body["schemaVersion"], 1);
    assert_eq!(body["error"]["outcomeUnknown"], false);
}
