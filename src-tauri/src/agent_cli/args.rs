//! agent_cli/args.rs — clap 命令树与 device/selector 参数解析。
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent 需要固定、可脚本化的命令面；禁止 active/current/recent/name 与 fuzzy 选择。
//!
//! Code Logic（这个模块做什么）:
//!     用 clap derive 定义资源/动作；自定义 parser 解析 `local`/`id:`/`path:`/`branch:`。

use crate::agent_cli::output::CliError;
use crate::agent_cli::selectors::{
    parse_entity_selector, parse_project_selector, parse_worktree_selector, EntitySelector,
    ProjectSelector, WorktreeSelector,
};
use clap::{Parser, Subcommand};

/// 设备选择器：仅 local 或显式 id。
///
/// Business Logic（为什么需要这个枚举）:
///     禁止自动挑选 remote peer；远端必须 `id:<deviceId>`。
///
/// Code Logic（这个枚举做什么）:
///     Local 默认；Remote 持有 device_id 字符串（不含 `id:` 前缀）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceSelector {
    Local,
    Remote { device_id: String },
}

impl DeviceSelector {
    /// Business Logic（为什么需要这个函数）:
    ///     clap 从 `--device` 字符串解析，拒绝 auto/current。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `local` / 省略语义由 clap default；`id:<uuid>` 提取 id；其它 → Usage。
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.eq_ignore_ascii_case("local") {
            return Ok(Self::Local);
        }
        if let Some(rest) = trimmed.strip_prefix("id:") {
            let id = rest.trim();
            if id.is_empty() {
                return Err("device id after id: must be non-empty".into());
            }
            if id.eq_ignore_ascii_case("local")
                || id.eq_ignore_ascii_case("auto")
                || id.eq_ignore_ascii_case("current")
            {
                return Err("device id must be an explicit peer device id".into());
            }
            return Ok(Self::Remote {
                device_id: id.to_string(),
            });
        }
        Err("device must be local or id:<deviceId>".into())
    }
}

impl std::str::FromStr for DeviceSelector {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// 顶层 CLI。
///
/// Business Logic（为什么需要这个结构）:
///     所有 Agent 命令共用 `--device` / `--json` 全局开关。
///
/// Code Logic（这个结构做什么）:
///     clap Parser：device 默认 local；子命令为资源树。
#[derive(Debug, Parser)]
#[command(
    name = "cc-partner",
    about = "cc-partner Agent control CLI (stable selectors + JSON envelopes)",
    long_about = "Agent-first control plane over local control API or explicit remote device.\n\
Does not manage backend lifecycle commands. Bodies only via --input-json -."
)]
pub struct Cli {
    /// Target device: local or id:<deviceId>
    #[arg(long, default_value = "local", value_parser = parse_device_arg, global = true)]
    pub device: DeviceSelector,

    /// Emit a single JSON envelope on stdout
    #[arg(long, global = true, default_value_t = false)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

/// 解析 `--device` clap value_parser。
///
/// Business Logic（为什么需要这个函数）:
///     clap 需要 `Fn(&str) -> Result<T, String>`。
///
/// Code Logic（这个函数做什么）:
///     委托 `DeviceSelector::parse`。
fn parse_device_arg(raw: &str) -> Result<DeviceSelector, String> {
    DeviceSelector::parse(raw)
}

/// 一级资源命令。
#[derive(Debug, Subcommand)]
pub enum Commands {
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    Worktree {
        #[command(subcommand)]
        action: WorktreeAction,
    },
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
    Experiment {
        #[command(subcommand)]
        action: ExperimentAction,
    },
    Attention {
        #[command(subcommand)]
        action: AttentionAction,
    },
    Fleet {
        #[command(subcommand)]
        action: FleetAction,
    },
    Browser {
        #[command(subcommand)]
        action: BrowserAction,
    },
    Event {
        #[command(subcommand)]
        action: EventAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectAction {
    /// List projects on the selected device
    List,
    /// Inspect a project by id: or path:
    Inspect {
        #[arg(long, value_parser = parse_project_arg)]
        project: ProjectSelector,
    },
}

#[derive(Debug, Subcommand)]
pub enum WorktreeAction {
    List {
        #[arg(long, value_parser = parse_project_arg)]
        project: ProjectSelector,
    },
    Create {
        #[arg(long, value_parser = parse_project_arg)]
        project: ProjectSelector,
        /// Must be `-` (read body from stdin)
        #[arg(long)]
        input_json: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum SessionAction {
    List {
        #[arg(long, value_parser = parse_project_arg)]
        project: ProjectSelector,
        #[arg(long, value_parser = parse_worktree_arg)]
        worktree: Option<WorktreeSelector>,
    },
    Read {
        #[arg(long, value_parser = parse_entity_arg)]
        session: EntitySelector,
        #[arg(long)]
        after_sequence: Option<u64>,
    },
    Send {
        #[arg(long, value_parser = parse_entity_arg)]
        session: EntitySelector,
        #[arg(long)]
        input_json: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum AgentAction {
    List {
        #[arg(long, value_parser = parse_project_arg)]
        project: ProjectSelector,
    },
    Inspect {
        #[arg(long, value_parser = parse_entity_arg)]
        agent: EntitySelector,
    },
    Wait {
        #[arg(long, value_parser = parse_entity_arg)]
        agent: EntitySelector,
        #[arg(long)]
        phase: String,
        #[arg(long)]
        timeout_ms: u64,
    },
}

#[derive(Debug, Subcommand)]
pub enum TaskAction {
    List {
        #[arg(long, value_parser = parse_project_arg)]
        project: ProjectSelector,
    },
    Create {
        #[arg(long, value_parser = parse_project_arg)]
        project: ProjectSelector,
        #[arg(long)]
        input_json: String,
    },
    Cancel {
        #[arg(long, value_parser = parse_entity_arg)]
        task: EntitySelector,
        #[arg(long)]
        client_request_id: String,
    },
    Retry {
        #[arg(long, value_parser = parse_entity_arg)]
        task: EntitySelector,
        #[arg(long)]
        client_request_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ExperimentAction {
    Create {
        #[arg(long, value_parser = parse_project_arg)]
        project: ProjectSelector,
        #[arg(long)]
        input_json: String,
    },
    Inspect {
        #[arg(long, value_parser = parse_entity_arg)]
        experiment: EntitySelector,
    },
    Cancel {
        #[arg(long, value_parser = parse_entity_arg)]
        experiment: EntitySelector,
    },
}

#[derive(Debug, Subcommand)]
pub enum AttentionAction {
    List,
}

#[derive(Debug, Subcommand)]
pub enum FleetAction {
    Snapshot,
}

#[derive(Debug, Subcommand)]
pub enum BrowserAction {
    Discover {
        #[arg(long, value_parser = parse_project_arg)]
        project: ProjectSelector,
    },
    Verify {
        #[arg(long, value_parser = parse_project_arg)]
        project: ProjectSelector,
        #[arg(long)]
        input_json: String,
    },
    Inspect {
        #[arg(long, value_parser = parse_entity_arg)]
        run: EntitySelector,
    },
}

#[derive(Debug, Subcommand)]
pub enum EventAction {
    /// Stream JSONL runtime events
    Follow {
        #[arg(long)]
        after_owner: Option<String>,
        #[arg(long)]
        after_sequence: Option<u64>,
    },
}

/// 解析 project selector。
fn parse_project_arg(raw: &str) -> Result<ProjectSelector, String> {
    parse_project_selector(raw).map_err(|e| e.message)
}

/// 解析 worktree selector。
fn parse_worktree_arg(raw: &str) -> Result<WorktreeSelector, String> {
    parse_worktree_selector(raw).map_err(|e| e.message)
}

/// 解析 entity `id:` selector。
fn parse_entity_arg(raw: &str) -> Result<EntitySelector, String> {
    parse_entity_selector(raw).map_err(|e| e.message)
}

/// 将 clap 解析错误映射为 CliError（exit 2）。
///
/// Business Logic（为什么需要这个函数）:
///     未知命令/缺参数必须 exit 2，且 `--json` 时 stdout 给 envelope。
///
/// Code Logic（这个函数做什么）:
///     包装 usage 错误；不把完整 clap 帮助塞进 error.message（有界）。
pub fn map_parse_error(err: clap::Error) -> CliError {
    let kind = err.kind();
    let code = match kind {
        clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
            // help/version 由调用方特殊处理
            return CliError::usage("help", "help or version requested");
        }
        _ => "usage",
    };
    CliError::usage(code, "invalid arguments or missing required action")
}

/// 已批准命令 argv 夹具（coverage 测试用）。
pub const APPROVED_COMMAND_FIXTURES: &[&[&str]] = &[
    &["cc-partner", "project", "list"],
    &["cc-partner", "project", "inspect", "--project", "id:p1"],
    &["cc-partner", "worktree", "list", "--project", "id:p1"],
    &[
        "cc-partner",
        "worktree",
        "create",
        "--project",
        "id:p1",
        "--input-json",
        "-",
    ],
    &["cc-partner", "session", "list", "--project", "id:p1"],
    &[
        "cc-partner",
        "session",
        "read",
        "--session",
        "id:s1",
        "--after-sequence",
        "0",
    ],
    &[
        "cc-partner",
        "session",
        "send",
        "--session",
        "id:s1",
        "--input-json",
        "-",
    ],
    &["cc-partner", "agent", "list", "--project", "id:p1"],
    &["cc-partner", "agent", "inspect", "--agent", "id:a1"],
    &[
        "cc-partner",
        "agent",
        "wait",
        "--agent",
        "id:a1",
        "--phase",
        "idle",
        "--timeout-ms",
        "1000",
    ],
    &["cc-partner", "task", "list", "--project", "id:p1"],
    &[
        "cc-partner",
        "task",
        "create",
        "--project",
        "id:p1",
        "--input-json",
        "-",
    ],
    &[
        "cc-partner",
        "task",
        "cancel",
        "--task",
        "id:t1",
        "--client-request-id",
        "req-1",
    ],
    &[
        "cc-partner",
        "task",
        "retry",
        "--task",
        "id:t1",
        "--client-request-id",
        "req-1",
    ],
    &[
        "cc-partner",
        "experiment",
        "create",
        "--project",
        "id:p1",
        "--input-json",
        "-",
    ],
    &[
        "cc-partner",
        "experiment",
        "inspect",
        "--experiment",
        "id:e1",
    ],
    &[
        "cc-partner",
        "experiment",
        "cancel",
        "--experiment",
        "id:e1",
    ],
    &["cc-partner", "attention", "list"],
    &["cc-partner", "fleet", "snapshot"],
    &["cc-partner", "browser", "discover", "--project", "id:p1"],
    &[
        "cc-partner",
        "browser",
        "verify",
        "--project",
        "id:p1",
        "--input-json",
        "-",
    ],
    &["cc-partner", "browser", "inspect", "--run", "id:r1"],
    &["cc-partner", "event", "follow"],
    &[
        "cc-partner",
        "event",
        "follow",
        "--after-owner",
        "o1",
        "--after-sequence",
        "2",
    ],
];

/// 禁止出现在 help 中的字样。
pub const FORBIDDEN_HELP_TOKENS: &[&str] = &[
    "quick-open",
    "recipe",
    "current",
    "recent",
    "fuzzy",
    "start",
    "serve",
    "stop",
    "status",
    "doctor",
];

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use clap::Parser;

    #[test]
    fn parses_local_default_and_remote_device() {
        let cli = Cli::try_parse_from(["cc-partner", "project", "list"]).unwrap();
        assert_eq!(cli.device, DeviceSelector::Local);
        assert!(!cli.json);

        let cli = Cli::try_parse_from([
            "cc-partner",
            "--device",
            "id:dev-1",
            "--json",
            "attention",
            "list",
        ])
        .unwrap();
        assert_eq!(
            cli.device,
            DeviceSelector::Remote {
                device_id: "dev-1".into()
            }
        );
        assert!(cli.json);
    }

    #[test]
    fn rejects_invalid_device() {
        let err =
            Cli::try_parse_from(["cc-partner", "--device", "auto", "project", "list"]).unwrap_err();
        assert!(err.to_string().contains("local") || err.to_string().contains("device"));
    }

    #[test]
    fn missing_action_is_usage_error() {
        let err = Cli::try_parse_from(["cc-partner", "project"]).unwrap_err();
        let mapped = map_parse_error(err);
        assert_eq!(mapped.exit, crate::agent_cli::output::CliExitCode::Usage);
    }

    #[test]
    fn every_approved_command_parses() {
        for argv in APPROVED_COMMAND_FIXTURES {
            let parsed = Cli::try_parse_from(*argv);
            assert!(
                parsed.is_ok(),
                "failed to parse {:?}: {:?}",
                argv,
                parsed.err().map(|e| e.to_string())
            );
        }
    }

    #[test]
    fn help_lists_resources_without_backend_lifecycle() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        cmd.write_long_help(&mut buf).unwrap();
        let help = String::from_utf8(buf).unwrap().to_lowercase();
        for token in [
            "project",
            "worktree",
            "session",
            "agent",
            "task",
            "experiment",
            "attention",
            "fleet",
            "browser",
            "event",
        ] {
            assert!(help.contains(token), "missing {token}");
        }
        // lifecycle 不应作为顶级子命令
        assert!(!help.contains("\n  start"));
        assert!(!help.contains("\n  serve"));
        assert!(!help.contains("\n  doctor"));
        assert!(!help.contains("quick-open"));
        assert!(!help.contains("recipe"));
    }

    #[test]
    fn device_selector_parse_rejects_empty_id() {
        assert!(DeviceSelector::parse("id:").is_err());
        assert!(DeviceSelector::parse("name:foo").is_err());
        assert_eq!(
            DeviceSelector::parse("local").unwrap(),
            DeviceSelector::Local
        );
    }
}
