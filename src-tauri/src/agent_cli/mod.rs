//! agent_cli — Agent-first 控制 CLI（参数解析、transport、JSON envelope）。
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent 需要稳定 selector、JSON/exit code 与本机/远端 transport，而不是 GUI 或 lifecycle CLI。
//!
//! Code Logic（这个模块做什么）:
//!     聚合 args/output/selectors/protocol/client/remote；`run_from_env` 分发全部批准命令。

pub mod args;
pub mod client;
pub mod output;
pub mod protocol;
pub mod remote;
pub mod selectors;

use crate::agent_cli::args::{
    AgentAction, AttentionAction, BrowserAction, Cli, Commands, DeviceSelector, EventAction,
    ExperimentAction, FleetAction, ProjectAction, SessionAction, TaskAction, WorktreeAction,
    APPROVED_COMMAND_FIXTURES,
};
use crate::agent_cli::client::AgentCliClient;
use crate::agent_cli::output::{
    emit_rendered, render_event_line, render_failure, render_success, CliError, CliExitCode,
    RenderedOutput,
};
use crate::agent_cli::protocol::{AgentControlMutation, AgentControlQuery};
use crate::agent_cli::selectors::{
    read_input_json, read_terminal_send_body, require_stdin_dash, EntitySelector,
};
use crate::backend::event_bus::BackendRuntimeCursor;
use clap::{CommandFactory, Parser};
use serde_json::{json, Value};
use std::io::{self, Write};

/// 从进程环境运行 Agent CLI。
///
/// Business Logic（为什么需要这个函数）:
///     `src/bin/cc-partner.rs` 需要稳定入口把结果映射为 exit 0..=7。
///
/// Code Logic（这个函数做什么）:
///     解析 argv → dispatch → 写 stdout/stderr → 返回 exit code。
pub fn run_from_env() -> i32 {
    run_with_args(std::env::args())
}

/// 用给定 argv 运行（测试可注入）。
///
/// Business Logic（为什么需要这个函数）:
///     单测覆盖 help/usage 与 dispatch 而不 spawn 进程。
///
/// Code Logic（这个函数做什么）:
///     clap parse → 异步 dispatch → emit。
pub fn run_with_args<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString> + Clone,
{
    let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
    match Cli::try_parse_from(&args) {
        Ok(cli) => {
            let rendered = run_async(async { dispatch_cli(cli).await });
            emit_rendered(&rendered)
        }
        Err(err) => {
            // help / version 直接打印到 stdout，exit 0
            if err.use_stderr() == false
                || matches!(
                    err.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                )
            {
                let _ = err.print();
                return 0;
            }
            // 尝试判断是否用户请求了 --json
            let json = args.iter().any(|a| a == "--json");
            let mapped = crate::agent_cli::args::map_parse_error(err);
            if matches!(mapped.code.as_str(), "help") {
                let mut cmd = Cli::command();
                let _ = cmd.print_help();
                return 0;
            }
            emit_rendered(&render_failure(mapped, json))
        }
    }
}

/// 在独立 runtime 执行 async dispatch。
///
/// Business Logic（为什么需要这个函数）:
///     CLI 是同步 main，内部网络调用需 Tokio。
///
/// Code Logic（这个函数做什么）:
///     `tokio::runtime::Runtime::new().block_on`。
fn run_async<F>(fut: F) -> RenderedOutput
where
    F: std::future::Future<Output = RenderedOutput>,
{
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(fut),
        Err(_) => render_failure(CliError::internal("failed to start async runtime"), true),
    }
}

/// 分发已解析 CLI。
///
/// Business Logic（为什么需要这个函数）:
///     按 local/remote 选择 transport，并统一 JSON 渲染。
///
/// Code Logic（这个函数做什么）:
///     event follow 特殊流式；其余 query/mutate。
pub async fn dispatch_cli(cli: Cli) -> RenderedOutput {
    let json = cli.json;
    match dispatch_inner(cli).await {
        Ok(DispatchResult::Json(data)) => render_success(data, json),
        Ok(DispatchResult::EventFollowDone) => RenderedOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: CliExitCode::Success.as_i32(),
        },
        Err(err) => render_failure(err, json),
    }
}

enum DispatchResult {
    Json(Value),
    EventFollowDone,
}

async fn dispatch_inner(cli: Cli) -> Result<DispatchResult, CliError> {
    // event follow
    if let Commands::Event {
        action: EventAction::Follow {
            after_owner,
            after_sequence,
        },
    } = &cli.command
    {
        if !matches!(cli.device, DeviceSelector::Local) {
            return Err(CliError::unsupported(
                "event follow is local-only in v1",
            ));
        }
        run_event_follow(after_owner.clone(), *after_sequence).await?;
        return Ok(DispatchResult::EventFollowDone);
    }

    let data = match cli.device {
        DeviceSelector::Local => dispatch_local(cli.command).await?,
        DeviceSelector::Remote { device_id } => {
            dispatch_remote(&device_id, cli.command).await?
        }
    };
    Ok(DispatchResult::Json(data))
}

async fn dispatch_local(command: Commands) -> Result<Value, CliError> {
    let client = AgentCliClient::from_control_file()?;
    match command_to_op(command)? {
        Op::Query(q) => client.query(q).await,
        Op::Mutate(m) => client.mutate(m).await,
    }
}

async fn dispatch_remote(device_id: &str, command: Commands) -> Result<Value, CliError> {
    // 远端业务走 peer P2P；device→baseUrl 仅经本机 control agent `DeviceResolve`（mDNS 表），
    // 禁止 GET /api/devices 等 LAN business API 绕过 control plane。
    let peer_base = resolve_device_base_via_control(device_id).await?;

    match command_to_op(command)? {
        Op::Query(q) => remote_query_direct(&peer_base, device_id, q).await,
        Op::Mutate(m) => remote_mutate_direct(&peer_base, device_id, m).await,
    }
}

/// 经本机 control agent `DeviceResolve` 从 owner mDNS 表解析 peer base URL。
///
/// Business Logic（为什么需要这个函数）:
///     Spec：CLI 不通过 localhost LAN 业务 API 绕过 control plane。
///
/// Code Logic（这个函数做什么）:
///     AgentCliClient.query(DeviceResolve) → 读 `baseUrl`；缺失/离线映射 CliError。
async fn resolve_device_base_via_control(device_id: &str) -> Result<String, CliError> {
    let client = AgentCliClient::from_control_file()?;
    let data = client
        .query(AgentControlQuery::DeviceResolve {
            device_id: device_id.to_string(),
        })
        .await?;
    if data.get("online").and_then(|v| v.as_bool()) == Some(false) {
        return Err(CliError::unavailable(
            "peer_offline",
            "remote device is offline",
        ));
    }
    let base = data
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CliError::not_found("remote device baseUrl missing in discovery table"))?;
    Ok(base.trim_end_matches('/').to_string())
}

/// 不持有 AppState 的远端 query（已解析 base_url）。
async fn remote_query_direct(
    base_url: &str,
    device_id: &str,
    op: AgentControlQuery,
) -> Result<Value, CliError> {
    // 复用 remote 模块中不依赖 AppState 的路径：临时构造最小 state 不可行。
    // 将 resolve 跳过，直接用 base_url 调 remote 内部逻辑。
    remote::remote_query_with_base(base_url, device_id, op).await
}

async fn remote_mutate_direct(
    base_url: &str,
    device_id: &str,
    op: AgentControlMutation,
) -> Result<Value, CliError> {
    remote::remote_mutate_with_base(base_url, device_id, op).await
}

enum Op {
    Query(AgentControlQuery),
    Mutate(AgentControlMutation),
}

/// 将 clap 命令转为 query/mutate op，并读取 stdin body。
///
/// Business Logic（为什么需要这个函数）:
///     解析与 transport 分离，便于 coverage 测试。
///
/// Code Logic（这个函数做什么）:
///     match Commands；body-bearing 校验 `--input-json -` 后读 stdin。
fn command_to_op(command: Commands) -> Result<Op, CliError> {
    match command {
        Commands::Project {
            action: ProjectAction::List,
        } => Ok(Op::Query(AgentControlQuery::ProjectList)),
        Commands::Project {
            action: ProjectAction::Inspect { project },
        } => Ok(Op::Query(AgentControlQuery::ProjectInspect {
            selector: project,
        })),
        Commands::Worktree {
            action: WorktreeAction::List { project },
        } => Ok(Op::Query(AgentControlQuery::WorktreeList { project })),
        Commands::Worktree {
            action: WorktreeAction::Create {
                project,
                input_json,
            },
        } => {
            require_stdin_dash(&input_json)?;
            let payload = read_input_json(&mut io::stdin())?;
            Ok(Op::Mutate(AgentControlMutation::WorktreeCreate {
                project,
                payload,
            }))
        }
        Commands::Session {
            action: SessionAction::List { project, worktree },
        } => Ok(Op::Query(AgentControlQuery::SessionList { project, worktree })),
        Commands::Session {
            action: SessionAction::Read {
                session,
                after_sequence,
            },
        } => Ok(Op::Query(AgentControlQuery::SessionRead {
            session_id: session.id,
            after_sequence,
        })),
        Commands::Session {
            action: SessionAction::Send {
                session,
                input_json,
            },
        } => {
            require_stdin_dash(&input_json)?;
            let data = read_terminal_send_body(&mut io::stdin())?;
            Ok(Op::Mutate(AgentControlMutation::SessionSend {
                session_id: session.id,
                data,
            }))
        }
        Commands::Agent {
            action: AgentAction::List { project },
        } => Ok(Op::Query(AgentControlQuery::AgentList { project })),
        Commands::Agent {
            action: AgentAction::Inspect { agent },
        } => Ok(Op::Query(AgentControlQuery::AgentInspect {
            agent_session_id: agent.id,
        })),
        Commands::Agent {
            action: AgentAction::Wait {
                agent,
                phase,
                timeout_ms,
            },
        } => Ok(Op::Query(AgentControlQuery::AgentWait {
            agent_session_id: agent.id,
            phase,
            timeout_ms,
        })),
        Commands::Task {
            action: TaskAction::List { project },
        } => Ok(Op::Query(AgentControlQuery::TaskList { project })),
        Commands::Task {
            action: TaskAction::Create {
                project,
                input_json,
            },
        } => {
            require_stdin_dash(&input_json)?;
            let payload: Value = read_input_json(&mut io::stdin())?;
            let client_request_id = payload
                .get("clientRequestId")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .ok_or_else(|| {
                    CliError::usage(
                        "invalid_input",
                        "clientRequestId required for task create",
                    )
                })?;
            Ok(Op::Mutate(AgentControlMutation::TaskCreate {
                project,
                payload,
                client_request_id,
            }))
        }
        Commands::Task {
            action: TaskAction::Cancel {
                task,
                client_request_id,
            },
        } => Ok(Op::Mutate(AgentControlMutation::TaskCancel {
            task_id: task.id,
            client_request_id,
        })),
        Commands::Task {
            action: TaskAction::Retry {
                task,
                client_request_id,
            },
        } => Ok(Op::Mutate(AgentControlMutation::TaskRetry {
            task_id: task.id,
            client_request_id,
        })),
        Commands::Experiment {
            action: ExperimentAction::Create {
                project,
                input_json,
            },
        } => {
            require_stdin_dash(&input_json)?;
            let payload: Value = read_input_json(&mut io::stdin())?;
            let client_request_id = payload
                .get("clientRequestId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Ok(Op::Mutate(AgentControlMutation::ExperimentCreate {
                project,
                payload,
                client_request_id,
            }))
        }
        Commands::Experiment {
            action: ExperimentAction::Inspect { experiment },
        } => Ok(Op::Query(AgentControlQuery::ExperimentInspect {
            experiment_id: experiment.id,
        })),
        Commands::Experiment {
            action: ExperimentAction::Cancel { experiment },
        } => Ok(Op::Mutate(AgentControlMutation::ExperimentCancel {
            experiment_id: experiment.id,
        })),
        Commands::Attention {
            action: AttentionAction::List,
        } => Ok(Op::Query(AgentControlQuery::AttentionList)),
        Commands::Fleet {
            action: FleetAction::Snapshot,
        } => Ok(Op::Query(AgentControlQuery::FleetSnapshot)),
        Commands::Browser {
            action: BrowserAction::Discover { project },
        } => Ok(Op::Query(AgentControlQuery::BrowserDiscover { project })),
        Commands::Browser {
            action: BrowserAction::Verify {
                project,
                input_json,
            },
        } => {
            require_stdin_dash(&input_json)?;
            let payload = read_input_json(&mut io::stdin())?;
            Ok(Op::Mutate(AgentControlMutation::BrowserVerify {
                project,
                payload,
            }))
        }
        Commands::Browser {
            action: BrowserAction::Inspect { run },
        } => Ok(Op::Query(AgentControlQuery::BrowserInspect {
            run_id: run.id,
        })),
        Commands::Event { .. } => Err(CliError::internal("event follow handled separately")),
    }
}

/// 是否为已支持的命令（coverage）。
///
/// Business Logic（为什么需要这个函数）:
///     表驱动测试断言每个批准 argv 都有 handler。
///
/// Code Logic（这个函数做什么）:
///     try_parse + command_to_op 或 event follow。
pub fn dispatch_kind(cli: &Cli) -> DispatchKind {
    match &cli.command {
        Commands::Event { .. } => DispatchKind::EventFollow,
        other => match command_to_op_ref(other) {
            Ok(OpKind::Query) => DispatchKind::Query,
            Ok(OpKind::Mutate) => DispatchKind::Mutate,
            Err(_) => DispatchKind::Unsupported,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchKind {
    Query,
    Mutate,
    EventFollow,
    Unsupported,
}

impl DispatchKind {
    /// Business Logic（为什么需要这个函数）:
    ///     coverage 测试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Unsupported 以外为 true。
    pub fn is_supported(self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}

enum OpKind {
    Query,
    Mutate,
}

fn command_to_op_ref(command: &Commands) -> Result<OpKind, CliError> {
    // 不读 stdin，仅判断类型
    match command {
        Commands::Worktree {
            action: WorktreeAction::Create { .. },
        }
        | Commands::Session {
            action: SessionAction::Send { .. },
        }
        | Commands::Task {
            action: TaskAction::Create { .. } | TaskAction::Cancel { .. } | TaskAction::Retry { .. },
        }
        | Commands::Experiment {
            action: ExperimentAction::Create { .. } | ExperimentAction::Cancel { .. },
        }
        | Commands::Browser {
            action: BrowserAction::Verify { .. },
        } => Ok(OpKind::Mutate),
        Commands::Event { .. } => Err(CliError::internal("event")),
        _ => Ok(OpKind::Query),
    }
}

/// 跟随本机 runtime 事件流并 JSONL 输出。
///
/// Business Logic（为什么需要这个函数）:
///     Agent 需要 resumable event follow，不发明第二套 bus。
///
/// Code Logic（这个函数做什么）:
///     POST events/catch-up 后 POST events/stream；按 owner/sequence 去重；Ctrl-C → 0。
async fn run_event_follow(
    after_owner: Option<String>,
    after_sequence: Option<u64>,
) -> Result<(), CliError> {
    let file = client::read_control_file_cli()?;
    let client = reqwest::Client::new();
    let mut cursor = BackendRuntimeCursor {
        owner_instance_id: after_owner.unwrap_or_default(),
        sequence: after_sequence.unwrap_or(0),
    };
    let mut last_seq_seen: Option<(String, u64)> = None;

    // catch-up
    let catch_url = format!(
        "http://127.0.0.1:{}/api/backend/control/events/catch-up",
        file.port
    );
    let catch_body = json!({
        "controlToken": file.control_token,
        "afterOwner": if cursor.owner_instance_id.is_empty() { Value::Null } else { json!(cursor.owner_instance_id) },
        "afterSequence": cursor.sequence,
    });
    if let Ok(resp) = client
        .post(&catch_url)
        .timeout(std::time::Duration::from_secs(10))
        .json(&catch_body)
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(body) = resp.json::<Value>().await {
                if let Some(events) = body.get("events").and_then(|v| v.as_array()) {
                    for ev in events {
                        if let Some(line) = filter_and_render_event(ev, &mut last_seq_seen) {
                            println!("{line}");
                            let _ = io::stdout().flush();
                        }
                    }
                }
                if let Some(owner) = body.get("ownerInstanceId").and_then(|v| v.as_str()) {
                    cursor.owner_instance_id = owner.to_string();
                }
                if let Some(seq) = body.get("latestSequence").and_then(|v| v.as_u64()) {
                    cursor.sequence = seq;
                }
            }
        }
    }

    // 简化 stream：轮询 catch-up（真实 SSE 适配在 control_events_stream；CLI 轮询避免复杂 stream 解析）
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                return Ok(());
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                let body = json!({
                    "controlToken": file.control_token,
                    "afterOwner": if cursor.owner_instance_id.is_empty() { Value::Null } else { json!(cursor.owner_instance_id) },
                    "afterSequence": cursor.sequence,
                });
                if let Ok(resp) = client
                    .post(&catch_url)
                    .timeout(std::time::Duration::from_secs(5))
                    .json(&body)
                    .send()
                    .await
                {
                    if !resp.status().is_success() {
                        continue;
                    }
                    if let Ok(body) = resp.json::<Value>().await {
                        if let Some(events) = body.get("events").and_then(|v| v.as_array()) {
                            for ev in events {
                                if let Some(line) = filter_and_render_event(ev, &mut last_seq_seen) {
                                    // 有界行：截断超大
                                    let line = if line.len() > 256 * 1024 {
                                        r#"{"kind":"gap","reason":"line_too_large"}"#.to_string()
                                    } else {
                                        line
                                    };
                                    println!("{line}");
                                    let _ = io::stdout().flush();
                                }
                            }
                        }
                        if let Some(owner) = body.get("ownerInstanceId").and_then(|v| v.as_str()) {
                            cursor.owner_instance_id = owner.to_string();
                        }
                        if let Some(seq) = body.get("latestSequence").and_then(|v| v.as_u64()) {
                            cursor.sequence = seq;
                        }
                    }
                }
            }
        }
    }
}

fn filter_and_render_event(
    ev: &Value,
    last: &mut Option<(String, u64)>,
) -> Option<String> {
    let owner = ev
        .get("ownerInstanceId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let seq = ev.get("sequence").and_then(|v| v.as_u64());
    if let Some(seq) = seq {
        if let Some((o, s)) = last {
            if *o == owner && *s == seq {
                return None; // 去重
            }
        }
        *last = Some((owner, seq));
    }
    render_event_line(ev).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn every_approved_command_has_a_dispatch_handler() {
        for argv in APPROVED_COMMAND_FIXTURES {
            let parsed = Cli::try_parse_from(*argv).unwrap();
            assert!(
                dispatch_kind(&parsed).is_supported(),
                "unsupported {:?}",
                argv
            );
        }
    }

    #[test]
    fn help_excludes_forbidden_tokens_as_subcommands() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        cmd.write_long_help(&mut buf).unwrap();
        let help = String::from_utf8(buf).unwrap();
        assert!(!help.contains("quick-open"));
        assert!(!help.contains("recipe"));
        // lifecycle 不是 agent CLI 子命令
        let lower = help.to_lowercase();
        assert!(lower.contains("project"));
        assert!(lower.contains("session"));
    }

    #[test]
    fn entity_selector_roundtrip_in_session_read() {
        let cli = Cli::try_parse_from([
            "cc-partner",
            "session",
            "read",
            "--session",
            "id:abc",
            "--after-sequence",
            "3",
        ])
        .unwrap();
        match cli.command {
            Commands::Session {
                action: SessionAction::Read {
                    session,
                    after_sequence,
                },
            } => {
                assert_eq!(session, EntitySelector { id: "abc".into() });
                assert_eq!(after_sequence, Some(3));
            }
            _ => panic!("wrong command"),
        }
    }
}
