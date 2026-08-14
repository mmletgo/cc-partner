//! workbench/agent_runtime/claude_status — 普通 Claude 终端结构化状态对账
//!
//! Business Logic（为什么需要这个模块）:
//!     普通 Workbench 终端没有 Orchestrator 注入的 Hook 身份；auto-title 只能建立首次
//!     Agent 绑定，无法持续回答“正在工作 / 等待用户 / 已退出”。Claude Code 会在用户级
//!     sessions 目录维护不含对话正文的结构化状态文件，本模块把它作为可靠运行态信号。
//!
//! Code Logic（这个模块做什么）:
//!     有界扫描 Claude session JSON；仅保留当前普通 claudeCodeVisible active 行需要的
//!     native session；把 busy/idle/进程退出映射为 Working/NeedsInput/Disconnected，
//!     生成 CAS mutation 交回统一 reducer worker 应用。不会读取 transcript 或终端字节流。

use super::{AgentRuntimeMutation, AgentRuntimeReducer, AgentSessionPhase, AgentSessionRuntime};
use crate::error::AppError;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// 普通 Claude Code provider 的稳定 id。
const CLAUDE_VISIBLE_PROVIDER_ID: &str = "claudeCodeVisible";
/// 单个结构化状态文件上限；正常文件只有几百字节。
const MAX_STATUS_FILE_BYTES: u64 = 64 * 1024;
/// 单轮累计读取预算；超出时本轮只应用已精确命中的状态，不依据缺失终结 session。
const MAX_STATUS_SCAN_BYTES: u64 = 8 * 1024 * 1024;
/// 单轮最多检查的状态文件数，避免异常目录拖垮 owner worker。
const MAX_STATUS_FILES: usize = 4_096;
/// 退出/缺失需要连续完整观察的轮数，跨过 provider 原子替换文件的短暂空窗。
const STOP_CONFIRMATION_SCANS: u8 = 2;

/// Claude Code 结构化 session 状态的最小读取模型。
///
/// Business Logic（为什么需要这个类型）:
///     运行态只需要 pid/sessionId/status/更新时间；cwd、标题、Prompt 等字段不得进入投影。
///
/// Code Logic（这个类型做什么）:
///     宽松反序列化 Claude sessions JSON 的六个非敏感字段；未知字段自动忽略。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeSessionStatusFile {
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    updated_at: u64,
    #[serde(default)]
    status_updated_at: u64,
}

/// 一条与 active runtime native id 精确匹配的结构化观察。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClaudeStatusObservation {
    phase: AgentSessionPhase,
    source_updated_at: u64,
}

/// 单轮有界扫描结果。
///
/// Business Logic（为什么需要这个类型）:
///     “目录不可用 / 文件正写到一半”不能被误判成 Agent 已退出；只有连续完整扫描才能依据
///     native session 缺失确认退出。
///
/// Code Logic（这个类型做什么）:
///     available 表示至少一个候选目录可读，complete 表示未遇到截断或解析/IO 错误，
///     observations 按 native session id 保存最新状态。
#[derive(Debug, Default)]
struct ClaudeStatusScan {
    available: bool,
    complete: bool,
    observations: HashMap<String, ClaudeStatusObservation>,
}

/// 解析 Claude 状态 token。
///
/// Business Logic（为什么需要这个函数）:
///     存活的 busy 表示 Agent 正在处理；存活的 idle 表示 CLI 停在输入提示符，正等待用户。
///     未知 token 必须 fail-closed，不能猜成某个 UI 状态。
///
/// Code Logic（这个函数做什么）:
///     busy → Working，idle → NeedsInput，其它返回 None。
fn phase_for_live_claude_status(status: &str) -> Option<AgentSessionPhase> {
    match status.trim() {
        "busy" => Some(AgentSessionPhase::Working),
        "idle" => Some(AgentSessionPhase::NeedsInput),
        _ => None,
    }
}

/// 解析 Claude Code 可能使用的结构化 session 目录。
///
/// Business Logic（为什么需要这个函数）:
///     Claude 支持 CLAUDE_CONFIG_DIR；普通桌面启动又常走默认 ~/.claude。两处均扫描可覆盖
///     GUI 环境与 shell 环境不完全一致的常见情况。
///
/// Code Logic（这个函数做什么）:
///     依次加入非空 CLAUDE_CONFIG_DIR/sessions 与 home/.claude/sessions，并去重。
fn claude_status_dirs() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(config_dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        let path = PathBuf::from(config_dir);
        if !path.as_os_str().is_empty() {
            roots.push(path.join("sessions"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        let default = home.join(".claude").join("sessions");
        if !roots.iter().any(|existing| existing == &default) {
            roots.push(default);
        }
    }
    roots
}

/// 有界读取并解析一份状态文件。
///
/// Business Logic（为什么需要这个函数）:
///     状态目录是外部 CLI 写入边界；异常大文件与写入中的半截 JSON 不得阻塞后台 worker。
///
/// Code Logic（这个函数做什么）:
///     metadata 先验限长，再 take(max+1) 读取；超限/IO/JSON 错误返回 None。
fn read_status_file(path: &Path) -> Option<ClaudeSessionStatusFile> {
    let metadata = path.metadata().ok()?;
    if metadata.len() > MAX_STATUS_FILE_BYTES {
        return None;
    }
    let file = File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_STATUS_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_STATUS_FILE_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// 同步扫描状态目录，并只收集当前 active runtime 需要的 native ids。
///
/// Business Logic（为什么需要这个函数）:
///     用户目录可能积累很多 provider 文件；对账不得枚举正文或为无关 session 执行进程探测。
///
/// Code Logic（这个函数做什么）:
///     有界枚举常规 .json 文件，宽松解析后按 wanted 过滤；匹配记录用 process_alive 判断
///     Disconnected，否则按 busy/idle 映射；同 native 取 statusUpdatedAt/updatedAt 较新者。
fn scan_claude_status_dirs(
    dirs: &[PathBuf],
    wanted_native_ids: &HashSet<String>,
    process_alive: &dyn Fn(u32) -> bool,
) -> ClaudeStatusScan {
    let mut scan = ClaudeStatusScan {
        complete: true,
        ..ClaudeStatusScan::default()
    };
    let mut files_seen = 0usize;
    let mut bytes_seen = 0u64;

    'directories: for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => {
                scan.available = true;
                entries
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                scan.complete = false;
                continue;
            }
        };
        for entry in entries {
            if files_seen >= MAX_STATUS_FILES {
                scan.complete = false;
                break 'directories;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    scan.complete = false;
                    continue;
                }
            };
            let is_file = match entry.file_type() {
                Ok(file_type) => file_type.is_file(),
                Err(_) => {
                    scan.complete = false;
                    continue;
                }
            };
            let path = entry.path();
            if !is_file || path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(file_pid) = path
                .file_stem()
                .and_then(|value| value.to_str())
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|pid| *pid > 0)
            else {
                // Claude 的 interactive 状态文件固定以 pid 命名；daemon/其它 JSON 不属于此协议。
                continue;
            };
            files_seen = files_seen.saturating_add(1);
            let file_len = match entry.metadata() {
                Ok(metadata) => metadata.len(),
                Err(_) => {
                    scan.complete = false;
                    continue;
                }
            };
            if file_len > MAX_STATUS_FILE_BYTES
                || bytes_seen.saturating_add(file_len) > MAX_STATUS_SCAN_BYTES
            {
                scan.complete = false;
                if bytes_seen.saturating_add(file_len) > MAX_STATUS_SCAN_BYTES {
                    break 'directories;
                }
                continue;
            }
            bytes_seen = bytes_seen.saturating_add(file_len);
            let Some(record) = read_status_file(&path) else {
                scan.complete = false;
                continue;
            };
            let native = record.session_id.trim();
            if native.is_empty() || !wanted_native_ids.contains(native) {
                continue;
            }
            if record.kind.trim() != "interactive" {
                // 同一目录未来可能包含 daemon/worker；即使 session id 异常碰撞也不能串写。
                continue;
            }
            let Some(pid) = record.pid.filter(|pid| *pid > 0) else {
                // 缺 pid 是未知/演进 schema，不可把默认 0 当成“进程已退出”。
                scan.complete = false;
                continue;
            };
            if pid != file_pid {
                scan.complete = false;
                continue;
            }
            let phase = if process_alive(pid) {
                let Some(phase) = phase_for_live_claude_status(&record.status) else {
                    scan.complete = false;
                    continue;
                };
                phase
            } else {
                AgentSessionPhase::Disconnected
            };
            let source_updated_at = record.status_updated_at.max(record.updated_at);
            let replace = scan
                .observations
                .get(native)
                .map(|current| source_updated_at >= current.source_updated_at)
                .unwrap_or(true);
            if replace {
                scan.observations.insert(
                    native.to_string(),
                    ClaudeStatusObservation {
                        phase,
                        source_updated_at,
                    },
                );
            }
        }
    }
    scan
}

/// 判断 runtime 行是否归普通 Claude 状态对账所有。
///
/// Business Logic（为什么需要这个函数）:
///     Orchestrator/Hook 的 phase 有更强权威来源，状态目录不得覆盖编排任务；其它 provider 也不能
///     因 native id 偶然相同被串写。
///
/// Code Logic（这个函数做什么）:
///     仅接受 active、无 orchestrator_task_id、provider=claudeCodeVisible、native 非空的行。
fn interactive_claude_native_id(row: &AgentSessionRuntime) -> Option<&str> {
    if !row.is_active
        || row.orchestrator_task_id.is_some()
        || row.provider_id != CLAUDE_VISIBLE_PROVIDER_ID
    {
        return None;
    }
    row.native_session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// 根据扫描结果为普通 Claude rows 规划 CAS mutations。
///
/// Business Logic（为什么需要这个函数）:
///     只有 phase 真正变化才升版；目录短暂不可读不能清空运行态。退出/缺失需要连续完整扫描
///     确认，避免 provider 原子替换文件时的一轮空窗误终结，同时也能在 owner 重启后清理旧行。
///
/// Code Logic（这个函数做什么）:
///     exact live 命中时清零 miss 并采用扫描 phase；dead 或完整扫描缺失时累加 miss，达到
///     STOP_CONFIRMATION_SCANS 才用 Disconnected；相同 phase 跳过；其余生成 version+1 mutation。
fn plan_claude_status_mutations(
    rows: &[AgentSessionRuntime],
    scan: &ClaudeStatusScan,
    unavailable_counts: &mut HashMap<String, u8>,
    occurred_at: &str,
) -> Vec<AgentRuntimeMutation> {
    let current_native_ids: HashSet<String> = rows
        .iter()
        .filter_map(interactive_claude_native_id)
        .map(str::to_string)
        .collect();
    unavailable_counts.retain(|native, _| current_native_ids.contains(native));

    let mut mutations = Vec::new();
    for row in rows {
        let Some(native) = interactive_claude_native_id(row) else {
            continue;
        };
        let phase = match scan.observations.get(native) {
            Some(observation) if observation.phase != AgentSessionPhase::Disconnected => {
                unavailable_counts.remove(native);
                observation.phase
            }
            Some(_) => {
                let misses = unavailable_counts.entry(native.to_string()).or_default();
                *misses = misses.saturating_add(1);
                if *misses < STOP_CONFIRMATION_SCANS {
                    continue;
                }
                AgentSessionPhase::Disconnected
            }
            None if scan.available && scan.complete => {
                let misses = unavailable_counts.entry(native.to_string()).or_default();
                *misses = misses.saturating_add(1);
                if *misses < STOP_CONFIRMATION_SCANS {
                    continue;
                }
                AgentSessionPhase::Disconnected
            }
            None => continue,
        };
        if phase == row.phase {
            continue;
        }
        mutations.push(AgentRuntimeMutation {
            agent_session_id: row.id.clone(),
            terminal_session_id: row.terminal_session_id.clone(),
            expected_version: row.version,
            event_version: row.version.saturating_add(1),
            phase,
            native_session_id: None,
            outcome_code: if phase == AgentSessionPhase::Disconnected {
                Some("provider_session_exited".to_string())
            } else {
                None
            },
            occurred_at: occurred_at.to_string(),
        });
    }
    mutations
}

/// 从默认 Claude 结构化状态目录收集普通终端 runtime mutations。
///
/// Business Logic（为什么需要这个函数）:
///     owner worker 需要周期性把 provider 真值投影到 SQLite/Event Bus，驱动项目卡与 tab 的
///     等待/已停止数字；扫描本身不得阻塞 async worker。
///
/// Code Logic（这个函数做什么）:
///     读取 active rows → 过滤普通 Claude → spawn_blocking 有界扫描 → 规划 version+1 mutations。
pub(super) async fn collect_claude_status_mutations(
    reducer: &AgentRuntimeReducer,
    unavailable_counts: &mut HashMap<String, u8>,
) -> Result<Vec<AgentRuntimeMutation>, AppError> {
    let rows = reducer.repo().list_active(None, 10_000).await?;
    let wanted: HashSet<String> = rows
        .iter()
        .filter_map(interactive_claude_native_id)
        .map(str::to_string)
        .collect();
    unavailable_counts.retain(|native, _| wanted.contains(native));
    if wanted.is_empty() {
        return Ok(Vec::new());
    }

    let dirs = claude_status_dirs();
    if dirs.is_empty() {
        return Ok(Vec::new());
    }
    let scan = tokio::task::spawn_blocking(move || {
        scan_claude_status_dirs(&dirs, &wanted, &crate::backend::control::process_is_alive)
    })
    .await
    .map_err(|error| AppError::generic(format!("Claude session 状态扫描任务失败: {error}")))?;
    let occurred_at = chrono::Utc::now().to_rfc3339();
    Ok(plan_claude_status_mutations(
        &rows,
        &scan,
        unavailable_counts,
        &occurred_at,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造普通 runtime row 测试夹具。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     mutation 规划测试需要精确控制 provider、编排归属、native 与 version。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 active Idle 的 claudeCodeVisible 行，调用方可覆盖字段。
    fn runtime_row(id: &str, native: &str) -> AgentSessionRuntime {
        AgentSessionRuntime {
            id: id.to_string(),
            project_id: "project-1".to_string(),
            worktree_id: None,
            terminal_session_id: format!("terminal-{id}"),
            orchestrator_task_id: None,
            orchestrator_attempt: None,
            provider_id: CLAUDE_VISIBLE_PROVIDER_ID.to_string(),
            native_session_id: Some(native.to_string()),
            phase: AgentSessionPhase::Idle,
            version: 1,
            started_at: "2026-08-14T00:00:00Z".to_string(),
            last_activity_at: "2026-08-14T00:00:00Z".to_string(),
            ended_at: None,
            outcome_code: None,
            resumed_from_agent_session_id: None,
            is_active: true,
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     provider 的 busy/idle/进程退出必须稳定映射到 UI 依赖的三种 phase，且不读正文。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时目录写三份最小 JSON，注入 pid liveness，断言三种 observation。
    #[test]
    fn scan_maps_busy_idle_and_dead_processes() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("11.json"),
            r#"{"pid":11,"sessionId":"native-busy","kind":"interactive","status":"busy","updatedAt":1,"statusUpdatedAt":2}"#,
        )
        .unwrap();
        std::fs::write(
            temp.path().join("12.json"),
            r#"{"pid":12,"sessionId":"native-idle","kind":"interactive","status":"idle","updatedAt":3,"statusUpdatedAt":4}"#,
        )
        .unwrap();
        std::fs::write(
            temp.path().join("13.json"),
            r#"{"pid":13,"sessionId":"native-dead","kind":"interactive","status":"busy","updatedAt":5,"statusUpdatedAt":6}"#,
        )
        .unwrap();
        let wanted = HashSet::from([
            "native-busy".to_string(),
            "native-idle".to_string(),
            "native-dead".to_string(),
        ]);

        let scan = scan_claude_status_dirs(&[temp.path().to_path_buf()], &wanted, &|pid| {
            pid == 11 || pid == 12
        });

        assert!(scan.available);
        assert!(scan.complete);
        assert_eq!(
            scan.observations.get("native-busy").map(|item| item.phase),
            Some(AgentSessionPhase::Working)
        );
        assert_eq!(
            scan.observations.get("native-idle").map(|item| item.phase),
            Some(AgentSessionPhase::NeedsInput)
        );
        assert_eq!(
            scan.observations.get("native-dead").map(|item| item.phase),
            Some(AgentSessionPhase::Disconnected)
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     缺 pid、pid 与文件名不一致、daemon kind 都不是可确认的 interactive 进程退出，
    ///     不能被默认值误报成 Disconnected。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写三份异常/无关记录，断言均无 observation，且无关 daemon 不参与协议。
    #[test]
    fn scan_rejects_invalid_pid_and_non_interactive_records() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("21.json"),
            r#"{"sessionId":"native-missing-pid","kind":"interactive","status":"busy"}"#,
        )
        .unwrap();
        std::fs::write(
            temp.path().join("22.json"),
            r#"{"pid":23,"sessionId":"native-mismatch","kind":"interactive","status":"idle"}"#,
        )
        .unwrap();
        std::fs::write(
            temp.path().join("24.json"),
            r#"{"pid":24,"sessionId":"native-daemon","kind":"daemon","status":"busy"}"#,
        )
        .unwrap();
        let wanted = HashSet::from([
            "native-missing-pid".to_string(),
            "native-mismatch".to_string(),
            "native-daemon".to_string(),
        ]);

        let scan = scan_claude_status_dirs(&[temp.path().to_path_buf()], &wanted, &|_| true);

        assert!(scan.available);
        assert!(!scan.complete);
        assert!(scan.observations.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     普通 Claude 行应随结构化状态升版；Orchestrator 与其它 provider 必须保持原权威来源。
    ///
    /// Code Logic（这个测试做什么）:
    ///     注入 busy/idle observation，并包含编排行、Codex 行与此前观察后消失的 Claude 行；
    ///     断言只生成 Working/NeedsInput/Disconnected 三条 mutation。
    #[test]
    fn plan_updates_only_interactive_claude_rows() {
        let busy = runtime_row("busy", "native-busy");
        let idle = runtime_row("idle", "native-idle");
        let missing = runtime_row("missing", "native-missing");
        let mut orchestrator = runtime_row("orchestrator", "native-orchestrator");
        orchestrator.orchestrator_task_id = Some("task-1".to_string());
        let mut codex = runtime_row("codex", "native-codex");
        codex.provider_id = "codexVisible".to_string();
        let scan = ClaudeStatusScan {
            available: true,
            complete: true,
            observations: HashMap::from([
                (
                    "native-busy".to_string(),
                    ClaudeStatusObservation {
                        phase: AgentSessionPhase::Working,
                        source_updated_at: 1,
                    },
                ),
                (
                    "native-idle".to_string(),
                    ClaudeStatusObservation {
                        phase: AgentSessionPhase::NeedsInput,
                        source_updated_at: 2,
                    },
                ),
            ]),
        };
        let mut unavailable_counts = HashMap::from([("native-missing".to_string(), 1)]);

        let mutations = plan_claude_status_mutations(
            &[busy, idle, missing, orchestrator, codex],
            &scan,
            &mut unavailable_counts,
            "2026-08-14T00:01:00Z",
        );

        assert_eq!(mutations.len(), 3);
        assert_eq!(mutations[0].phase, AgentSessionPhase::Working);
        assert_eq!(mutations[1].phase, AgentSessionPhase::NeedsInput);
        assert_eq!(mutations[2].phase, AgentSessionPhase::Disconnected);
        assert_eq!(
            mutations[2].outcome_code.as_deref(),
            Some("provider_session_exited")
        );
        assert!(mutations.iter().all(|mutation| mutation.event_version == 2));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     外部状态文件短暂半写或目录不可读时，不能把正在工作的 Agent 误报为已停止。
    ///
    /// Code Logic（这个测试做什么）:
    ///     已有一次 miss 计数但 scan.complete=false，断言不生成 Disconnected mutation。
    #[test]
    fn incomplete_scan_does_not_disconnect_missing_session() {
        let row = runtime_row("working", "native-working");
        let scan = ClaudeStatusScan {
            available: true,
            complete: false,
            observations: HashMap::new(),
        };
        let mut unavailable_counts = HashMap::from([("native-working".to_string(), 1)]);

        let mutations = plan_claude_status_mutations(
            &[row],
            &scan,
            &mut unavailable_counts,
            "2026-08-14T00:01:00Z",
        );

        assert!(mutations.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     owner 重启后内存没有“曾观察”集合，仍需在稳定缺失后清理数据库里的幽灵 active；
    ///     同时单轮文件替换空窗不能误终结。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对同一完整空扫描连续规划两次，断言第一轮无 mutation，第二轮才 Disconnected。
    #[test]
    fn missing_session_requires_two_complete_scans_before_disconnect() {
        let row = runtime_row("missing", "native-missing");
        let scan = ClaudeStatusScan {
            available: true,
            complete: true,
            observations: HashMap::new(),
        };
        let mut unavailable_counts = HashMap::new();

        let first = plan_claude_status_mutations(
            std::slice::from_ref(&row),
            &scan,
            &mut unavailable_counts,
            "2026-08-14T00:01:00Z",
        );
        let second = plan_claude_status_mutations(
            &[row],
            &scan,
            &mut unavailable_counts,
            "2026-08-14T00:01:01Z",
        );

        assert!(first.is_empty());
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].phase, AgentSessionPhase::Disconnected);
    }
}
