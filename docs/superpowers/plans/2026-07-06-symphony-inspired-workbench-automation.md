# Symphony-Inspired Workbench Automation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Upgrade Workbench project automation into a Symphony-inspired board with split workflow/runtime state, built-in workflow defaults plus optional project overrides, visible Claude Code runner runtime, remote/mobile compatibility, and Settings-controlled delivery.

**Architecture:** The backend becomes the source of truth for `workflowState`, `runState`, `attemptPhase`, runtime snapshots, and validated state transitions. Desktop and mobile clients render the same remote-aware task-view DTOs, with desktop using a board plus drawer and mobile using grouped lists. `claudeCodeVisible` remains the only implemented runner provider in this round, with provider fields and runtime snapshots shaped so additional providers can be added without UI rewrites.

**Tech Stack:** Rust/Tauri 2, sqlx SQLite, serde/serde_yaml, React 19, TypeScript, CSS Modules, existing Workbench tmux terminal APIs, existing Orchestrator task-view remote protocol.

---

## File Structure

Backend files:

- Modify `src-tauri/src/orchestrator/models.rs`: add split state enums, DTO fields, runtime snapshot DTOs, and legacy status mapping helpers.
- Modify `src-tauri/src/orchestrator/repo.rs`: extend schema, migration guards, row mapping, state transition methods, runtime snapshot queries, and tests.
- Create `src-tauri/src/orchestrator/workflow.rs`: built-in workflow defaults, optional project `WORKFLOW.md` front matter parsing, prompt rendering, validation command resolution.
- Modify `src-tauri/src/orchestrator/mod.rs`: export `workflow`.
- Modify `src-tauri/src/orchestrator/scheduler.rs`: claim by `workflow_state/run_state`, produce explainable dispatch decisions.
- Modify `src-tauri/src/orchestrator/runner.rs`: set split states and attempt phases, pass workflow-rendered prompt, record runner provider fields.
- Create `src-tauri/src/orchestrator/claude_runtime.rs`: best-effort Claude Code JSONL/session association and runtime summary extraction.
- Modify `src-tauri/src/orchestrator/completion.rs`: update completion path to split states.
- Modify `src-tauri/src/orchestrator/delivery.rs`: keep delivery gated by Settings; transition through `humanReview/merging/done`.
- Modify `src-tauri/src/orchestrator/outbox.rs`, `remote_protocol.rs`, `remote_client.rs`: add optional split state/runtime fields for remote views and mirrors.
- Modify `src-tauri/src/commands/orchestrator.rs`: add remote-aware action commands and runtime snapshot command; adapt task views.
- Modify `src-tauri/src/commands/orchestrator_config.rs` and `src-tauri/src/orchestrator/config.rs`: expose Settings delivery mode with default off if current booleans need a clearer UI contract.
- Modify `src-tauri/src/lib.rs`: register new commands.

Frontend files:

- Modify `web/src/lib/types.ts`: add split state types, runtime snapshot types, task fields, create action mode.
- Modify `web/src/api/orchestrator.ts`: add new commands and request builders.
- Modify `web/src/lib/orchestrator.ts`: status helpers, lane metadata, action enablement, migration fallback.
- Modify `web/src/lib/orchestratorRemote.ts`: split task views and pending remote behavior.
- Create `web/src/pages/Orchestrator/orchestratorBoard.ts`: pure board grouping, lane movement validation, and card helper functions.
- Create `web/src/pages/Orchestrator/OrchestratorBoard.tsx`: desktop board rendering and adjacent drag/drop.
- Create `web/src/pages/Orchestrator/OrchestratorStatusStrip.tsx`: runtime snapshot strip.
- Create `web/src/pages/Orchestrator/OrchestratorTaskDrawer.tsx`: task detail drawer and evidence timeline.
- Create `web/src/pages/Orchestrator/OrchestratorCreateDialog.tsx`: three-action create dialog.
- Modify `web/src/pages/Orchestrator/Orchestrator.tsx`: compose board/status/drawer/dialog and keep embedded mode.
- Modify `web/src/pages/Orchestrator/Orchestrator.module.css`: board/drawer/status styles using design tokens.
- Modify `web/src/pages/Workbench/Workbench.tsx`: keep automation layer mount, wire status refresh and execution-site open behavior.
- Modify `web/src/mobile/components/MobileAutomationPanel.tsx`: grouped list, runtime/evidence summary, three create actions.
- Modify `web/src/i18n/locales/zh/orchestrator.json` and `web/src/i18n/locales/en/orchestrator.json`: new board/runtime/drawer strings.

Docs and tests:

- Modify `docs/prd.md`: describe split state, built-in workflow override, board UI, Settings delivery gate.
- Modify `web/CLAUDE.md`: frontend Orchestrator/Workbench automation behavior and test commands.
- Modify `src-tauri/CLAUDE.md`: backend Orchestrator model, workflow resolver, runtime snapshot, test commands.
- Update tests listed in each task.

## Execution Notes

- This implementation is larger than 100 lines and should be executed in a separate `codex/` branch or worktree.
- If the main worktree has uncommitted user changes at execution time, create a git worktree and do not merge back without user instruction.
- Use subagents for implementation slices with disjoint write sets:
  - Backend model/workflow/scheduler.
  - Runner runtime association.
  - Desktop UI.
  - Mobile/remote/docs polish.
- Do not read subagent stdout for coding tasks; review `git diff` after workers finish.
- Keep commits frequent and task-scoped.

---

### Task 1: Backend Split State Model And Migration

**Files:**
- Modify: `src-tauri/src/orchestrator/models.rs`
- Modify: `src-tauri/src/orchestrator/repo.rs`
- Test: `src-tauri/src/orchestrator/models.rs`
- Test: `src-tauri/src/orchestrator/repo.rs`

- [ ] **Step 1: Add failing enum/mapping tests**

Add this test module content to `src-tauri/src/orchestrator/models.rs` tests:

```rust
#[cfg(test)]
mod split_state_tests {
    use super::*;

    #[test]
    fn legacy_status_maps_to_split_states() {
        let cases = [
            (
                OrchestratorTaskStatus::Draft,
                OrchestratorWorkflowState::Backlog,
                OrchestratorRunState::Idle,
            ),
            (
                OrchestratorTaskStatus::Queued,
                OrchestratorWorkflowState::Todo,
                OrchestratorRunState::Queued,
            ),
            (
                OrchestratorTaskStatus::Preparing,
                OrchestratorWorkflowState::InProgress,
                OrchestratorRunState::Preparing,
            ),
            (
                OrchestratorTaskStatus::Running,
                OrchestratorWorkflowState::InProgress,
                OrchestratorRunState::Running,
            ),
            (
                OrchestratorTaskStatus::Verifying,
                OrchestratorWorkflowState::InProgress,
                OrchestratorRunState::Verifying,
            ),
            (
                OrchestratorTaskStatus::Delivering,
                OrchestratorWorkflowState::Merging,
                OrchestratorRunState::Delivering,
            ),
            (
                OrchestratorTaskStatus::Done,
                OrchestratorWorkflowState::Done,
                OrchestratorRunState::Idle,
            ),
            (
                OrchestratorTaskStatus::Blocked,
                OrchestratorWorkflowState::Rework,
                OrchestratorRunState::Blocked,
            ),
            (
                OrchestratorTaskStatus::Aborted,
                OrchestratorWorkflowState::Canceled,
                OrchestratorRunState::Idle,
            ),
        ];

        for (legacy, workflow, run) in cases {
            let mapped = SplitTaskState::from_legacy_status(legacy);
            assert_eq!(mapped.workflow_state, workflow);
            assert_eq!(mapped.run_state, run);
        }
    }
}
```

- [ ] **Step 2: Run model test and verify it fails**

Run:

```bash
cd src-tauri
cargo test orchestrator::models::split_state_tests::legacy_status_maps_to_split_states --lib
```

Expected: fail because `OrchestratorWorkflowState`, `OrchestratorRunState`, and `SplitTaskState` do not exist.

- [ ] **Step 3: Add split state enums and DTO fields**

In `src-tauri/src/orchestrator/models.rs`, add:

```rust
/// Orchestrator 任务业务泳道状态。
///
/// Business Logic（为什么需要这个枚举）:
///     Workbench 自动化看板需要表达任务在业务流程中的位置，不能再把用户可见泳道和
///     Runner 技术阶段混在同一个 status 字段里。
///
/// Code Logic（这个枚举做什么）:
///     定义固定默认泳道集合，并用 serde camelCase 与前端 DTO 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrchestratorWorkflowState {
    Backlog,
    Todo,
    InProgress,
    HumanReview,
    Rework,
    Merging,
    Done,
    Canceled,
}

impl OrchestratorWorkflowState {
    /// Business Logic（为什么需要这个函数）:
    ///     SQLite 存储使用稳定短字符串，前端和远端协议依赖这些值长期不变。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回与 serde camelCase 相同语义的数据库字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            OrchestratorWorkflowState::Backlog => "backlog",
            OrchestratorWorkflowState::Todo => "todo",
            OrchestratorWorkflowState::InProgress => "inProgress",
            OrchestratorWorkflowState::HumanReview => "humanReview",
            OrchestratorWorkflowState::Rework => "rework",
            OrchestratorWorkflowState::Merging => "merging",
            OrchestratorWorkflowState::Done => "done",
            OrchestratorWorkflowState::Canceled => "canceled",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     迁移和 repo 读取需要把数据库短字符串恢复成强类型状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对已知字符串返回枚举，否则返回 AppError 让调用方暴露数据异常。
    pub fn from_str(value: &str) -> Result<Self, AppError> {
        match value {
            "backlog" => Ok(OrchestratorWorkflowState::Backlog),
            "todo" => Ok(OrchestratorWorkflowState::Todo),
            "inProgress" => Ok(OrchestratorWorkflowState::InProgress),
            "humanReview" => Ok(OrchestratorWorkflowState::HumanReview),
            "rework" => Ok(OrchestratorWorkflowState::Rework),
            "merging" => Ok(OrchestratorWorkflowState::Merging),
            "done" => Ok(OrchestratorWorkflowState::Done),
            "canceled" => Ok(OrchestratorWorkflowState::Canceled),
            other => Err(AppError::generic(format!("未知 workflow_state: {other}"))),
        }
    }
}

/// Orchestrator Runner 调度状态。
///
/// Business Logic（为什么需要这个枚举）:
///     用户需要知道任务为什么正在运行、排队、验证或阻塞；这些是执行态，不应决定看板泳道。
///
/// Code Logic（这个枚举做什么）:
///     定义调度器和 Runner 写入的稳定状态集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrchestratorRunState {
    Idle,
    Queued,
    Preparing,
    Running,
    Verifying,
    Retrying,
    Blocked,
    Delivering,
}

impl OrchestratorRunState {
    /// Business Logic（为什么需要这个函数）:
    ///     运行态需要以稳定短字符串落库和跨设备传输。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回数据库字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            OrchestratorRunState::Idle => "idle",
            OrchestratorRunState::Queued => "queued",
            OrchestratorRunState::Preparing => "preparing",
            OrchestratorRunState::Running => "running",
            OrchestratorRunState::Verifying => "verifying",
            OrchestratorRunState::Retrying => "retrying",
            OrchestratorRunState::Blocked => "blocked",
            OrchestratorRunState::Delivering => "delivering",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Repo 层读取 SQLite 字符串时需要强类型校验。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对已知字符串返回枚举，否则返回业务错误。
    pub fn from_str(value: &str) -> Result<Self, AppError> {
        match value {
            "idle" => Ok(OrchestratorRunState::Idle),
            "queued" => Ok(OrchestratorRunState::Queued),
            "preparing" => Ok(OrchestratorRunState::Preparing),
            "running" => Ok(OrchestratorRunState::Running),
            "verifying" => Ok(OrchestratorRunState::Verifying),
            "retrying" => Ok(OrchestratorRunState::Retrying),
            "blocked" => Ok(OrchestratorRunState::Blocked),
            "delivering" => Ok(OrchestratorRunState::Delivering),
            other => Err(AppError::generic(format!("未知 run_state: {other}"))),
        }
    }
}

/// Orchestrator 单次执行尝试阶段。
///
/// Business Logic（为什么需要这个枚举）:
///     任务详情和运行状态条需要解释当前 attempt 卡在准备、启动、streaming、验证还是失败。
///
/// Code Logic（这个枚举做什么）:
///     提供可选 attempt phase；空值表示任务尚未进入 runner attempt。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrchestratorAttemptPhase {
    PreparingWorkspace,
    BuildingPrompt,
    LaunchingRunner,
    InitializingSession,
    Streaming,
    Finishing,
    Succeeded,
    Failed,
    TimedOut,
    Stalled,
    CanceledByReconciliation,
}

/// 旧 status 到新状态的映射结果。
///
/// Business Logic（为什么需要这个结构体）:
///     升级已有数据库时不能丢失任务可见性；需要稳定映射旧状态到新模型。
///
/// Code Logic（这个结构体做什么）:
///     承载 workflow_state/run_state 两个字段，供迁移和兼容 DTO 使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitTaskState {
    pub workflow_state: OrchestratorWorkflowState,
    pub run_state: OrchestratorRunState,
}

impl SplitTaskState {
    /// Business Logic（为什么需要这个函数）:
    ///     已有任务只保存 legacy status，升级后仍必须出现在正确看板泳道。
    ///
    /// Code Logic（这个函数做什么）:
    ///     把旧生命周期状态映射到新的业务泳道和运行态。
    pub fn from_legacy_status(status: OrchestratorTaskStatus) -> Self {
        match status {
            OrchestratorTaskStatus::Draft => Self {
                workflow_state: OrchestratorWorkflowState::Backlog,
                run_state: OrchestratorRunState::Idle,
            },
            OrchestratorTaskStatus::Queued => Self {
                workflow_state: OrchestratorWorkflowState::Todo,
                run_state: OrchestratorRunState::Queued,
            },
            OrchestratorTaskStatus::Preparing => Self {
                workflow_state: OrchestratorWorkflowState::InProgress,
                run_state: OrchestratorRunState::Preparing,
            },
            OrchestratorTaskStatus::Running => Self {
                workflow_state: OrchestratorWorkflowState::InProgress,
                run_state: OrchestratorRunState::Running,
            },
            OrchestratorTaskStatus::Verifying => Self {
                workflow_state: OrchestratorWorkflowState::InProgress,
                run_state: OrchestratorRunState::Verifying,
            },
            OrchestratorTaskStatus::Delivering => Self {
                workflow_state: OrchestratorWorkflowState::Merging,
                run_state: OrchestratorRunState::Delivering,
            },
            OrchestratorTaskStatus::Done => Self {
                workflow_state: OrchestratorWorkflowState::Done,
                run_state: OrchestratorRunState::Idle,
            },
            OrchestratorTaskStatus::Blocked => Self {
                workflow_state: OrchestratorWorkflowState::Rework,
                run_state: OrchestratorRunState::Blocked,
            },
            OrchestratorTaskStatus::Aborted => Self {
                workflow_state: OrchestratorWorkflowState::Canceled,
                run_state: OrchestratorRunState::Idle,
            },
        }
    }
}
```

Add fields to `OrchestratorTaskRow` and `OrchestratorTaskDto`:

```rust
pub workflow_state: OrchestratorWorkflowState,
pub run_state: OrchestratorRunState,
pub attempt_phase: Option<OrchestratorAttemptPhase>,
pub source: String,
pub external_id: Option<String>,
pub external_identifier: Option<String>,
pub external_url: Option<String>,
pub runner_provider: Option<String>,
pub claude_session_id: Option<String>,
pub transcript_path: Option<String>,
pub runtime_started_at: Option<String>,
pub last_activity_at: Option<String>,
pub last_runtime_event: Option<String>,
pub last_runtime_message: Option<String>,
```

Update `From<OrchestratorTaskRow> for OrchestratorTaskDto` to copy all fields.

- [ ] **Step 4: Run model test and verify it passes**

Run:

```bash
cd src-tauri
cargo test orchestrator::models::split_state_tests::legacy_status_maps_to_split_states --lib
```

Expected: pass.

- [ ] **Step 5: Add schema migration tests**

In `src-tauri/src/orchestrator/repo.rs` tests, add a test that initializes schema and checks columns:

```rust
#[tokio::test]
async fn init_schema_adds_split_state_columns() {
    let repo = test_repo().await;
    repo.init_schema().await.expect("schema 初始化成功");

    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('orchestrator_tasks')")
        .fetch_all(repo.pool())
        .await
        .expect("读取列信息成功");

    for expected in [
        "workflow_state",
        "run_state",
        "attempt_phase",
        "source",
        "external_id",
        "external_identifier",
        "external_url",
        "runner_provider",
        "claude_session_id",
        "transcript_path",
        "runtime_started_at",
        "last_activity_at",
        "last_runtime_event",
        "last_runtime_message",
    ] {
        assert!(columns.iter().any(|column| column == expected), "missing column {expected}");
    }
}
```

- [ ] **Step 6: Run schema test and verify it fails**

Run:

```bash
cd src-tauri
cargo test orchestrator::repo::tests::init_schema_adds_split_state_columns --lib
```

Expected: fail because columns are missing.

- [ ] **Step 7: Extend `OrchestratorRepo::init_schema`**

In `src-tauri/src/orchestrator/repo.rs`, after existing task schema creation, add guarded `ALTER TABLE` calls using the repo's existing column-exists helper pattern. Add columns with non-null defaults where possible:

```rust
self.ensure_column(
    "orchestrator_tasks",
    "workflow_state",
    "TEXT NOT NULL DEFAULT 'backlog'",
).await?;
self.ensure_column(
    "orchestrator_tasks",
    "run_state",
    "TEXT NOT NULL DEFAULT 'idle'",
).await?;
self.ensure_column("orchestrator_tasks", "attempt_phase", "TEXT").await?;
self.ensure_column(
    "orchestrator_tasks",
    "source",
    "TEXT NOT NULL DEFAULT 'internal'",
).await?;
self.ensure_column("orchestrator_tasks", "external_id", "TEXT").await?;
self.ensure_column("orchestrator_tasks", "external_identifier", "TEXT").await?;
self.ensure_column("orchestrator_tasks", "external_url", "TEXT").await?;
self.ensure_column("orchestrator_tasks", "runner_provider", "TEXT").await?;
self.ensure_column("orchestrator_tasks", "claude_session_id", "TEXT").await?;
self.ensure_column("orchestrator_tasks", "transcript_path", "TEXT").await?;
self.ensure_column("orchestrator_tasks", "runtime_started_at", "TEXT").await?;
self.ensure_column("orchestrator_tasks", "last_activity_at", "TEXT").await?;
self.ensure_column("orchestrator_tasks", "last_runtime_event", "TEXT").await?;
self.ensure_column("orchestrator_tasks", "last_runtime_message", "TEXT").await?;
self.backfill_split_state_from_legacy_status().await?;
```

Implement `backfill_split_state_from_legacy_status` by selecting rows where `workflow_state='backlog' AND run_state='idle'` and applying `SplitTaskState::from_legacy_status(status)`. Use a single `UPDATE orchestrator_tasks SET workflow_state=?, run_state=? WHERE id=?`.

- [ ] **Step 8: Update repo row mapping and inserts**

Update `create_task`, `create_remote_task_idempotent`, `list_tasks`, `get_task`, and task row scanners to include new columns. For new internal tasks, default:

```rust
workflow_state: OrchestratorWorkflowState::Backlog,
run_state: OrchestratorRunState::Idle,
attempt_phase: None,
source: "internal".to_string(),
runner_provider: Some("claudeCodeVisible".to_string()),
```

- [ ] **Step 9: Run focused backend tests**

Run:

```bash
cd src-tauri
cargo test orchestrator::models::split_state_tests orchestrator::repo::tests::init_schema_adds_split_state_columns --lib
```

Expected: pass.

- [ ] **Step 10: Commit Task 1**

Run:

```bash
git add src-tauri/src/orchestrator/models.rs src-tauri/src/orchestrator/repo.rs
git commit -m "feat(orchestrator): add split task states"
```

---

### Task 2: Built-In Workflow Resolver And Optional `WORKFLOW.md`

**Files:**
- Create: `src-tauri/src/orchestrator/workflow.rs`
- Modify: `src-tauri/src/orchestrator/mod.rs`
- Modify: `src-tauri/src/orchestrator/runner.rs`
- Test: `src-tauri/src/orchestrator/workflow.rs`

- [ ] **Step 1: Write workflow resolver tests**

Create `src-tauri/src/orchestrator/workflow.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn built_in_workflow_is_valid_without_project_file() {
        let dir = tempdir().expect("创建临时目录成功");
        let resolved = resolve_project_workflow(dir.path()).expect("内置 workflow 可用");
        assert_eq!(resolved.source, WorkflowSource::BuiltInDefault);
        assert_eq!(resolved.default_create_state, OrchestratorWorkflowState::Backlog);
        assert!(resolved.active_states.contains(&OrchestratorWorkflowState::Todo));
        assert!(resolved.active_states.contains(&OrchestratorWorkflowState::Rework));
    }

    #[test]
    fn project_workflow_overrides_validation_commands_and_prompt() {
        let dir = tempdir().expect("创建临时目录成功");
        std::fs::write(
            dir.path().join("WORKFLOW.md"),
            "---\nvalidation:\n  commands:\n    - cd web && npx tsc --noEmit\nrunner:\n  stall_timeout_ms: 120000\n---\nCustom prompt for {{ task.title }}",
        )
        .expect("写入 WORKFLOW.md 成功");

        let resolved = resolve_project_workflow(dir.path()).expect("项目 workflow 解析成功");
        assert_eq!(resolved.source, WorkflowSource::ProjectOverride);
        assert_eq!(resolved.validation_commands, vec!["cd web && npx tsc --noEmit"]);
        assert_eq!(resolved.runner.stall_timeout_ms, 120000);
        assert_eq!(resolved.prompt_template, "Custom prompt for {{ task.title }}");
    }

    #[test]
    fn invalid_front_matter_returns_validation_error() {
        let dir = tempdir().expect("创建临时目录成功");
        std::fs::write(dir.path().join("WORKFLOW.md"), "---\n[\n---\nBody")
            .expect("写入 WORKFLOW.md 成功");
        let error = resolve_project_workflow(dir.path()).expect_err("非法 yaml 必须报错");
        assert!(error.to_string().contains("WORKFLOW.md"));
    }

    #[test]
    fn render_prompt_rejects_unknown_variables() {
        let workflow = ResolvedWorkflow::built_in_default();
        let task = PromptTaskContext {
            title: "Add badges".to_string(),
            goal: "Show badge counts".to_string(),
            acceptance_criteria: "Counts update correctly".to_string(),
        };
        let error = render_prompt("Hello {{ task.missing }}", &task, 1)
            .expect_err("未知变量必须报错");
        assert!(error.to_string().contains("未知模板变量"));
        let rendered = workflow
            .render_task_prompt(&task, 1)
            .expect("内置模板渲染成功");
        assert!(rendered.contains("Add badges"));
        assert!(rendered.contains("ORCHESTRATOR_DEV_DONE"));
    }
}
```

- [ ] **Step 2: Run workflow tests and verify they fail**

Run:

```bash
cd src-tauri
cargo test orchestrator::workflow::tests --lib
```

Expected: fail because module does not compile yet.

- [ ] **Step 3: Implement workflow resolver**

Add to `src-tauri/src/orchestrator/workflow.rs`:

```rust
use crate::error::AppError;
use crate::orchestrator::models::OrchestratorWorkflowState;
use serde::Deserialize;
use std::path::Path;

const WORKFLOW_FILE_NAME: &str = "WORKFLOW.md";
const DEFAULT_PROMPT_TEMPLATE: &str = r#"你正在处理 cc-partner Workbench 项目自动化任务。

任务：
{{ task.title }}

目标：
{{ task.goal }}

验收标准：
{{ task.acceptance_criteria }}

完成代码、测试和必要证据后，请最后单独输出 ORCHESTRATOR_DEV_DONE。
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowSource {
    BuiltInDefault,
    ProjectOverride,
}

#[derive(Debug, Clone)]
pub struct WorkflowRunnerConfig {
    pub provider: String,
    pub max_turns: i64,
    pub stall_timeout_ms: i64,
}

#[derive(Debug, Clone)]
pub struct ResolvedWorkflow {
    pub source: WorkflowSource,
    pub default_create_state: OrchestratorWorkflowState,
    pub active_states: Vec<OrchestratorWorkflowState>,
    pub review_state: OrchestratorWorkflowState,
    pub terminal_states: Vec<OrchestratorWorkflowState>,
    pub runner: WorkflowRunnerConfig,
    pub validation_commands: Vec<String>,
    pub prompt_template: String,
}

#[derive(Debug, Clone)]
pub struct PromptTaskContext {
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
}

#[derive(Debug, Deserialize, Default)]
struct WorkflowFrontMatter {
    workflow: Option<WorkflowSection>,
    runner: Option<RunnerSection>,
    validation: Option<ValidationSection>,
}

#[derive(Debug, Deserialize, Default)]
struct WorkflowSection {
    default_create_state: Option<String>,
    active_states: Option<Vec<String>>,
    review_state: Option<String>,
    terminal_states: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct RunnerSection {
    provider: Option<String>,
    max_turns: Option<i64>,
    stall_timeout_ms: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
struct ValidationSection {
    commands: Option<Vec<String>>,
}

impl ResolvedWorkflow {
    pub fn built_in_default() -> Self {
        ResolvedWorkflow {
            source: WorkflowSource::BuiltInDefault,
            default_create_state: OrchestratorWorkflowState::Backlog,
            active_states: vec![
                OrchestratorWorkflowState::Todo,
                OrchestratorWorkflowState::Rework,
            ],
            review_state: OrchestratorWorkflowState::HumanReview,
            terminal_states: vec![
                OrchestratorWorkflowState::Done,
                OrchestratorWorkflowState::Canceled,
            ],
            runner: WorkflowRunnerConfig {
                provider: "claudeCodeVisible".to_string(),
                max_turns: 1,
                stall_timeout_ms: 300_000,
            },
            validation_commands: Vec::new(),
            prompt_template: DEFAULT_PROMPT_TEMPLATE.trim().to_string(),
        }
    }

    pub fn render_task_prompt(
        &self,
        task: &PromptTaskContext,
        attempt: i64,
    ) -> Result<String, AppError> {
        render_prompt(&self.prompt_template, task, attempt)
    }
}

pub fn resolve_project_workflow(project_path: &Path) -> Result<ResolvedWorkflow, AppError> {
    let path = project_path.join(WORKFLOW_FILE_NAME);
    if !path.exists() {
        return Ok(ResolvedWorkflow::built_in_default());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|err| AppError::generic(format!("读取 WORKFLOW.md 失败: {err}")))?;
    let (front_matter, body) = parse_workflow_document(&content)?;
    let mut resolved = ResolvedWorkflow::built_in_default();
    resolved.source = WorkflowSource::ProjectOverride;
    apply_front_matter(&mut resolved, front_matter)?;
    if !body.trim().is_empty() {
        resolved.prompt_template = body.trim().to_string();
    }
    Ok(resolved)
}

fn parse_workflow_document(content: &str) -> Result<(WorkflowFrontMatter, String), AppError> {
    if !content.starts_with("---") {
        return Ok((WorkflowFrontMatter::default(), content.trim().to_string()));
    }
    let mut lines = content.lines();
    let _opening = lines.next();
    let mut yaml = Vec::new();
    let mut body = Vec::new();
    let mut in_yaml = true;
    for line in lines {
        if in_yaml && line.trim() == "---" {
            in_yaml = false;
            continue;
        }
        if in_yaml {
            yaml.push(line);
        } else {
            body.push(line);
        }
    }
    if in_yaml {
        return Err(AppError::generic("WORKFLOW.md front matter 缺少结束 ---"));
    }
    let front_matter = serde_yaml::from_str::<WorkflowFrontMatter>(&yaml.join("\n"))
        .map_err(|err| AppError::generic(format!("解析 WORKFLOW.md front matter 失败: {err}")))?;
    Ok((front_matter, body.join("\n").trim().to_string()))
}

fn apply_front_matter(
    resolved: &mut ResolvedWorkflow,
    front_matter: WorkflowFrontMatter,
) -> Result<(), AppError> {
    if let Some(workflow) = front_matter.workflow {
        if let Some(value) = workflow.default_create_state {
            resolved.default_create_state = workflow_state_from_config(&value)?;
        }
        if let Some(values) = workflow.active_states {
            resolved.active_states = parse_state_list(values)?;
        }
        if let Some(value) = workflow.review_state {
            resolved.review_state = workflow_state_from_config(&value)?;
        }
        if let Some(values) = workflow.terminal_states {
            resolved.terminal_states = parse_state_list(values)?;
        }
    }
    if let Some(runner) = front_matter.runner {
        if let Some(provider) = runner.provider {
            if provider.trim() != "claudeCodeVisible" {
                return Err(AppError::generic(format!(
                    "当前仅支持 runner.provider=claudeCodeVisible，收到 {provider}"
                )));
            }
            resolved.runner.provider = provider;
        }
        if let Some(max_turns) = runner.max_turns {
            if !(1..=20).contains(&max_turns) {
                return Err(AppError::generic("runner.max_turns 必须在 1..=20"));
            }
            resolved.runner.max_turns = max_turns;
        }
        if let Some(stall_timeout_ms) = runner.stall_timeout_ms {
            if stall_timeout_ms < 30_000 {
                return Err(AppError::generic("runner.stall_timeout_ms 不能小于 30000"));
            }
            resolved.runner.stall_timeout_ms = stall_timeout_ms;
        }
    }
    if let Some(validation) = front_matter.validation {
        if let Some(commands) = validation.commands {
            resolved.validation_commands = commands
                .into_iter()
                .map(|command| command.trim().to_string())
                .filter(|command| !command.is_empty())
                .collect();
        }
    }
    Ok(())
}

fn parse_state_list(values: Vec<String>) -> Result<Vec<OrchestratorWorkflowState>, AppError> {
    values
        .into_iter()
        .map(|value| workflow_state_from_config(&value))
        .collect()
}

fn workflow_state_from_config(value: &str) -> Result<OrchestratorWorkflowState, AppError> {
    match value.trim() {
        "backlog" => Ok(OrchestratorWorkflowState::Backlog),
        "todo" => Ok(OrchestratorWorkflowState::Todo),
        "inProgress" | "in_progress" => Ok(OrchestratorWorkflowState::InProgress),
        "humanReview" | "human_review" => Ok(OrchestratorWorkflowState::HumanReview),
        "rework" => Ok(OrchestratorWorkflowState::Rework),
        "merging" => Ok(OrchestratorWorkflowState::Merging),
        "done" => Ok(OrchestratorWorkflowState::Done),
        "canceled" | "cancelled" => Ok(OrchestratorWorkflowState::Canceled),
        other => Err(AppError::generic(format!("不支持的 workflow state: {other}"))),
    }
}

pub fn render_prompt(
    template: &str,
    task: &PromptTaskContext,
    attempt: i64,
) -> Result<String, AppError> {
    let mut output = template.to_string();
    for (key, value) in [
        ("task.title", task.title.as_str()),
        ("task.goal", task.goal.as_str()),
        ("task.acceptance_criteria", task.acceptance_criteria.as_str()),
    ] {
        output = output.replace(&format!("{{{{ {key} }}}}"), value);
        output = output.replace(&format!("{{{{{key}}}}}"), value);
    }
    output = output.replace("{{ attempt }}", &attempt.to_string());
    output = output.replace("{{attempt}}", &attempt.to_string());
    if let Some(start) = output.find("{{") {
        let tail = &output[start..];
        let end = tail.find("}}").map(|index| index + 2).unwrap_or(tail.len());
        return Err(AppError::generic(format!(
            "未知模板变量: {}",
            &tail[..end]
        )));
    }
    Ok(output)
}
```

- [ ] **Step 4: Export workflow module**

In `src-tauri/src/orchestrator/mod.rs`, add:

```rust
pub mod workflow;
```

- [ ] **Step 5: Run workflow tests and verify they pass**

Run:

```bash
cd src-tauri
cargo test orchestrator::workflow::tests --lib
```

Expected: pass.

- [ ] **Step 6: Commit Task 2**

Run:

```bash
git add src-tauri/src/orchestrator/workflow.rs src-tauri/src/orchestrator/mod.rs
git commit -m "feat(orchestrator): resolve project workflow policy"
```

---

### Task 3: Scheduler, Actions, And Runtime Snapshot API

**Files:**
- Modify: `src-tauri/src/orchestrator/repo.rs`
- Modify: `src-tauri/src/orchestrator/scheduler.rs`
- Modify: `src-tauri/src/commands/orchestrator.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `src-tauri/src/orchestrator/scheduler.rs`
- Test: `src-tauri/src/commands/orchestrator.rs`

- [ ] **Step 1: Add state movement tests**

In `src-tauri/src/orchestrator/repo.rs` tests, add:

```rust
#[tokio::test]
async fn move_workflow_state_allows_only_adjacent_lanes() {
    let repo = test_repo().await;
    repo.init_schema().await.expect("schema 初始化成功");
    let task = insert_test_task(&repo, OrchestratorWorkflowState::Backlog, OrchestratorRunState::Idle)
        .await;

    let moved = repo
        .move_task_workflow_state(&task.id, OrchestratorWorkflowState::Todo)
        .await
        .expect("相邻移动成功");
    assert_eq!(moved.workflow_state, OrchestratorWorkflowState::Todo);

    let error = repo
        .move_task_workflow_state(&task.id, OrchestratorWorkflowState::HumanReview)
        .await
        .expect_err("跨泳道移动必须失败");
    assert!(error.to_string().contains("只能移动到相邻泳道"));
}
```

Define `insert_test_task` near existing repo test helpers. It should insert an `OrchestratorTaskRow` with provided split states and `source="internal"`.

- [ ] **Step 2: Run movement test and verify it fails**

Run:

```bash
cd src-tauri
cargo test orchestrator::repo::tests::move_workflow_state_allows_only_adjacent_lanes --lib
```

Expected: fail because `move_task_workflow_state` does not exist.

- [ ] **Step 3: Implement adjacent-lane transition**

In `src-tauri/src/orchestrator/repo.rs`, add:

```rust
const WORKFLOW_LANE_ORDER: [OrchestratorWorkflowState; 8] = [
    OrchestratorWorkflowState::Backlog,
    OrchestratorWorkflowState::Todo,
    OrchestratorWorkflowState::InProgress,
    OrchestratorWorkflowState::HumanReview,
    OrchestratorWorkflowState::Rework,
    OrchestratorWorkflowState::Merging,
    OrchestratorWorkflowState::Done,
    OrchestratorWorkflowState::Canceled,
];

fn workflow_lane_index(state: OrchestratorWorkflowState) -> Option<usize> {
    WORKFLOW_LANE_ORDER.iter().position(|candidate| *candidate == state)
}
```

Add method:

```rust
/// Business Logic（为什么需要这个函数）:
///     桌面看板允许拖拽，但为了避免隐式触发危险副作用，只允许相邻泳道迁移。
///
/// Code Logic（这个函数做什么）:
///     读取当前任务，校验目标泳道与当前泳道相邻且任务不处于运行态，然后原子更新 workflow_state。
pub async fn move_task_workflow_state(
    &self,
    task_id: &str,
    target: OrchestratorWorkflowState,
) -> Result<OrchestratorTaskRow, AppError> {
    let task = self.get_task(task_id).await?;
    if matches!(
        task.run_state,
        OrchestratorRunState::Running
            | OrchestratorRunState::Preparing
            | OrchestratorRunState::Verifying
            | OrchestratorRunState::Delivering
    ) {
        return Err(AppError::generic("运行中的任务不能通过拖拽移动"));
    }
    let current_index = workflow_lane_index(task.workflow_state)
        .ok_or_else(|| AppError::generic("当前泳道不可移动"))?;
    let target_index =
        workflow_lane_index(target).ok_or_else(|| AppError::generic("目标泳道不可移动"))?;
    if current_index.abs_diff(target_index) != 1 {
        return Err(AppError::generic("任务只能移动到相邻泳道"));
    }
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE orchestrator_tasks SET workflow_state=?, updated_at=? WHERE id=?",
    )
    .bind(target.as_str())
    .bind(now)
    .bind(task_id)
    .execute(&self.pool)
    .await?;
    self.get_task(task_id).await
}
```

- [ ] **Step 4: Add scheduler claim tests for active workflow states**

In `src-tauri/src/orchestrator/scheduler.rs` tests, add:

```rust
#[tokio::test]
async fn scheduler_claims_todo_and_rework_only() {
    let (state, _temp) = test_state_with_orchestrator_enabled(2).await;
    insert_project_task(
        &state,
        "backlog-task",
        OrchestratorWorkflowState::Backlog,
        OrchestratorRunState::Idle,
    )
    .await;
    insert_project_task(
        &state,
        "todo-task",
        OrchestratorWorkflowState::Todo,
        OrchestratorRunState::Idle,
    )
    .await;
    insert_project_task(
        &state,
        "rework-task",
        OrchestratorWorkflowState::Rework,
        OrchestratorRunState::Blocked,
    )
    .await;

    let claimed = claim_tasks_for_dispatch(state.orchestrator_repo.as_ref(), &state.config.read().unwrap().orchestrator)
        .await
        .expect("claim 成功");
    let ids: Vec<String> = claimed.into_iter().map(|task| task.id).collect();
    assert!(ids.contains(&"todo-task".to_string()));
    assert!(ids.contains(&"rework-task".to_string()));
    assert!(!ids.contains(&"backlog-task".to_string()));
}
```

- [ ] **Step 5: Update repo claim query**

Change `claim_next_local_queued_tasks_with_global_capacity` or add `claim_next_local_active_tasks_with_global_capacity` so it selects:

```sql
workflow_state IN ('todo', 'rework')
AND run_state IN ('idle', 'blocked')
```

The update should set:

```sql
workflow_state='inProgress',
run_state='preparing',
attempt_phase='preparingWorkspace'
```

Keep global capacity behavior and remote-project skip behavior.

- [ ] **Step 6: Add command request/response DTOs**

In `src-tauri/src/commands/orchestrator.rs`, add:

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveOrchestratorTaskWorkflowStateRequest {
    pub project_id: String,
    pub task_id: String,
    pub target_state: OrchestratorWorkflowState,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorRuntimeSnapshotDto {
    pub project_id: String,
    pub generated_at: String,
    pub scheduler_enabled: bool,
    pub workflow_source: String,
    pub workflow_valid: bool,
    pub workflow_error: Option<String>,
    pub slots_used: i64,
    pub slots_available: i64,
    pub latest_error: Option<String>,
}
```

Add commands:

```rust
#[tauri::command]
pub async fn move_orchestrator_task_workflow_state(
    state: State<'_, AppState>,
    request: MoveOrchestratorTaskWorkflowStateRequest,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let task = state
        .orchestrator_repo
        .move_task_workflow_state(&request.task_id, request.target_state)
        .await?;
    Ok(local_task_view(task.to_dto()))
}

#[tauri::command]
pub async fn get_orchestrator_runtime_snapshot(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<OrchestratorRuntimeSnapshotDto, AppError> {
    build_runtime_snapshot(&state, &project_id).await
}
```

Implement `build_runtime_snapshot` using current Settings config, workflow resolver for the active project path, and repo counts of active `run_state`.

- [ ] **Step 7: Register commands**

In `src-tauri/src/lib.rs`, add new commands to `invoke_handler!`:

```rust
commands::orchestrator::move_orchestrator_task_workflow_state,
commands::orchestrator::get_orchestrator_runtime_snapshot,
```

- [ ] **Step 8: Run scheduler/action tests**

Run:

```bash
cd src-tauri
cargo test orchestrator::repo::tests::move_workflow_state_allows_only_adjacent_lanes orchestrator::scheduler::tests::scheduler_claims_todo_and_rework_only --lib
cargo check
```

Expected: tests pass and `cargo check` exits 0.

- [ ] **Step 9: Commit Task 3**

Run:

```bash
git add src-tauri/src/orchestrator/repo.rs src-tauri/src/orchestrator/scheduler.rs src-tauri/src/commands/orchestrator.rs src-tauri/src/lib.rs
git commit -m "feat(orchestrator): add workflow actions and runtime snapshot"
```

---

### Task 4: Runner Split-State Updates And Claude Runtime Association

**Files:**
- Create: `src-tauri/src/orchestrator/claude_runtime.rs`
- Modify: `src-tauri/src/orchestrator/mod.rs`
- Modify: `src-tauri/src/orchestrator/runner.rs`
- Modify: `src-tauri/src/orchestrator/completion.rs`
- Modify: `src-tauri/src/commands/orchestrator.rs`
- Test: `src-tauri/src/orchestrator/claude_runtime.rs`
- Test: `src-tauri/src/orchestrator/runner.rs`

- [ ] **Step 1: Write Claude runtime association tests**

Create `src-tauri/src/orchestrator/claude_runtime.rs` with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn associates_latest_jsonl_for_matching_cwd() {
        let home = tempdir().expect("创建 home 成功");
        let project = tempdir().expect("创建项目目录成功");
        let encoded_project = encode_claude_project_path(project.path());
        let session_dir = home.path().join(".claude/projects").join(encoded_project);
        std::fs::create_dir_all(&session_dir).expect("创建 session 目录成功");
        let jsonl_path = session_dir.join("session-a.jsonl");
        std::fs::write(
            &jsonl_path,
            format!(
                r#"{{"type":"user","cwd":"{}","sessionId":"session-a","timestamp":"2026-07-06T01:00:00Z","message":{{"role":"user","content":"hello"}}}}
{{"type":"assistant","cwd":"{}","sessionId":"session-a","timestamp":"2026-07-06T01:00:05Z","message":{{"role":"assistant","content":"Working on tests"}}}}"#,
                project.path().display(),
                project.path().display()
            ),
        )
        .expect("写入 jsonl 成功");

        let summary = associate_claude_runtime(home.path(), project.path())
            .expect("关联执行成功")
            .expect("找到 runtime");
        assert_eq!(summary.claude_session_id.as_deref(), Some("session-a"));
        assert_eq!(summary.transcript_path, jsonl_path.to_string_lossy());
        assert_eq!(summary.last_runtime_message.as_deref(), Some("Working on tests"));
    }

    #[test]
    fn returns_none_when_no_matching_cwd_exists() {
        let home = tempdir().expect("创建 home 成功");
        let project = tempdir().expect("创建项目目录成功");
        let summary = associate_claude_runtime(home.path(), project.path())
            .expect("关联执行成功");
        assert!(summary.is_none());
    }
}
```

- [ ] **Step 2: Run runtime tests and verify they fail**

Run:

```bash
cd src-tauri
cargo test orchestrator::claude_runtime::tests --lib
```

Expected: fail because module is not exported and functions do not exist.

- [ ] **Step 3: Implement Claude runtime module**

Add to `src-tauri/src/orchestrator/claude_runtime.rs`:

```rust
use crate::error::AppError;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct ClaudeRuntimeSummary {
    pub claude_session_id: Option<String>,
    pub transcript_path: String,
    pub last_activity_at: Option<String>,
    pub last_runtime_event: Option<String>,
    pub last_runtime_message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeJsonLine {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    message: Option<serde_json::Value>,
}

pub fn associate_claude_runtime(
    home_dir: &Path,
    worktree_path: &Path,
) -> Result<Option<ClaudeRuntimeSummary>, AppError> {
    let root = home_dir.join(".claude/projects");
    if !root.exists() {
        return Ok(None);
    }
    let mut candidates = Vec::new();
    collect_jsonl_files(&root, &mut candidates)?;
    candidates.sort();
    candidates.reverse();
    for path in candidates {
        if let Some(summary) = summarize_matching_jsonl(&path, worktree_path)? {
            return Ok(Some(summary));
        }
    }
    Ok(None)
}

fn collect_jsonl_files(dir: &Path, output: &mut Vec<PathBuf>) -> Result<(), AppError> {
    for entry in std::fs::read_dir(dir)
        .map_err(|err| AppError::generic(format!("读取 Claude session 目录失败: {err}")))?
    {
        let entry = entry.map_err(|err| AppError::generic(format!("读取目录项失败: {err}")))?;
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, output)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            output.push(path);
        }
    }
    Ok(())
}

fn summarize_matching_jsonl(
    path: &Path,
    worktree_path: &Path,
) -> Result<Option<ClaudeRuntimeSummary>, AppError> {
    let content = std::fs::read_to_string(path)
        .map_err(|err| AppError::generic(format!("读取 Claude transcript 失败: {err}")))?;
    let worktree = worktree_path.to_string_lossy();
    let mut summary = ClaudeRuntimeSummary {
        transcript_path: path.to_string_lossy().to_string(),
        ..ClaudeRuntimeSummary::default()
    };
    let mut matched = false;
    for line in content.lines() {
        let Ok(parsed) = serde_json::from_str::<ClaudeJsonLine>(line) else {
            continue;
        };
        if parsed.cwd.as_deref() != Some(worktree.as_ref()) {
            continue;
        }
        matched = true;
        if parsed.session_id.is_some() {
            summary.claude_session_id = parsed.session_id;
        }
        if parsed.timestamp.is_some() {
            summary.last_activity_at = parsed.timestamp;
        }
        if parsed.r#type.is_some() {
            summary.last_runtime_event = parsed.r#type;
        }
        if let Some(message) = parsed.message {
            if let Some(content) = message.get("content").and_then(|value| value.as_str()) {
                summary.last_runtime_message = Some(content.to_string());
            }
        }
    }
    Ok(matched.then_some(summary))
}

pub fn encode_claude_project_path(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}
```

- [ ] **Step 4: Export module**

In `src-tauri/src/orchestrator/mod.rs`, add:

```rust
pub mod claude_runtime;
```

- [ ] **Step 5: Update runner state writes**

In `src-tauri/src/orchestrator/runner.rs`:

- When preparing workspace starts, update `attempt_phase='preparingWorkspace'`.
- When building prompt, update `attempt_phase='buildingPrompt'`.
- When writing terminal input succeeds, update `attempt_phase='streaming'`, `run_state='running'`, `runner_provider='claudeCodeVisible'`.
- After session creation, call `associate_claude_runtime` with `dirs::home_dir()` and worktree path. If it returns `Some`, persist `claude_session_id`, `transcript_path`, `last_activity_at`, `last_runtime_event`, `last_runtime_message`.
- If association returns `None`, leave runtime fields null.

Add repo helper:

```rust
pub async fn update_task_runtime_summary(
    &self,
    task_id: &str,
    summary: &ClaudeRuntimeSummary,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE orchestrator_tasks
         SET claude_session_id=?, transcript_path=?, last_activity_at=?, last_runtime_event=?,
             last_runtime_message=?, updated_at=?
         WHERE id=?",
    )
    .bind(&summary.claude_session_id)
    .bind(&summary.transcript_path)
    .bind(&summary.last_activity_at)
    .bind(&summary.last_runtime_event)
    .bind(&summary.last_runtime_message)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(task_id)
    .execute(&self.pool)
    .await?;
    Ok(())
}
```

- [ ] **Step 6: Update completion success path**

In `src-tauri/src/commands/orchestrator.rs`, after verifier pass and automatic delivery off, update task to:

```rust
workflow_state = OrchestratorWorkflowState::HumanReview;
run_state = OrchestratorRunState::Idle;
attempt_phase = Some(OrchestratorAttemptPhase::Succeeded);
```

When verifier fails, update task to:

```rust
workflow_state = OrchestratorWorkflowState::Rework;
run_state = OrchestratorRunState::Blocked;
attempt_phase = Some(OrchestratorAttemptPhase::Failed);
```

- [ ] **Step 7: Run focused runtime/runner tests**

Run:

```bash
cd src-tauri
cargo test orchestrator::claude_runtime::tests --lib
cargo test orchestrator::runner:: --lib
cargo check
```

Expected: tests pass and `cargo check` exits 0.

- [ ] **Step 8: Commit Task 4**

Run:

```bash
git add src-tauri/src/orchestrator/claude_runtime.rs src-tauri/src/orchestrator/mod.rs src-tauri/src/orchestrator/runner.rs src-tauri/src/orchestrator/repo.rs src-tauri/src/commands/orchestrator.rs
git commit -m "feat(orchestrator): track visible Claude runtime"
```

---

### Task 5: Frontend Types, API, And Pure Board Helpers

**Files:**
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/api/orchestrator.ts`
- Modify: `web/src/lib/orchestrator.ts`
- Modify: `web/src/lib/orchestratorRemote.ts`
- Create: `web/src/pages/Orchestrator/orchestratorBoard.ts`
- Test: `web/src/api/orchestrator.test.ts`
- Test: `web/src/lib/orchestrator.test.ts`
- Test: `web/src/lib/orchestratorRemote.test.ts`
- Test: `web/src/pages/Orchestrator/orchestratorBoard.test.ts`

- [ ] **Step 1: Add pure board helper tests**

Create `web/src/pages/Orchestrator/orchestratorBoard.test.ts`:

```ts
import {
  canMoveToLane,
  groupTasksByWorkflowState,
  ORCHESTRATOR_BOARD_LANES,
} from './orchestratorBoard';
import type { OrchestratorTaskView } from '@/lib/types';

function task(id: string, workflowState: string, runState = 'idle'): OrchestratorTaskView {
  return {
    origin: 'local',
    task: {
      id,
      projectId: 'project-1',
      title: id,
      goal: 'goal',
      acceptanceCriteria: 'accept',
      status: 'draft',
      workflowState,
      runState,
      attemptPhase: null,
      priority: 0,
      branchName: null,
      worktreeId: null,
      sessionId: null,
      blockedReason: null,
      attempt: 0,
      createdAt: '2026-07-06T00:00:00Z',
      updatedAt: '2026-07-06T00:00:00Z',
      startedAt: null,
      finishedAt: null,
      source: 'internal',
      externalId: null,
      externalIdentifier: null,
      externalUrl: null,
      runnerProvider: null,
      claudeSessionId: null,
      transcriptPath: null,
      runtimeStartedAt: null,
      lastActivityAt: null,
      lastRuntimeEvent: null,
      lastRuntimeMessage: null,
    },
  };
}

const views = [
  task('a', 'backlog'),
  task('b', 'todo'),
  task('c', 'humanReview'),
];

const grouped = groupTasksByWorkflowState(views);

if (ORCHESTRATOR_BOARD_LANES[0] !== 'backlog') {
  throw new Error('backlog should be the first lane');
}
if (grouped.backlog.length !== 1 || grouped.todo.length !== 1 || grouped.humanReview.length !== 1) {
  throw new Error('tasks should group by workflowState');
}
if (!canMoveToLane('backlog', 'todo', 'idle')) {
  throw new Error('adjacent move should be allowed');
}
if (canMoveToLane('backlog', 'humanReview', 'idle')) {
  throw new Error('cross-lane move should be rejected');
}
if (canMoveToLane('inProgress', 'humanReview', 'running')) {
  throw new Error('running task should not move by drag');
}
```

- [ ] **Step 2: Run helper test and verify it fails**

Run:

```bash
cd web
npx --yes tsx src/pages/Orchestrator/orchestratorBoard.test.ts
```

Expected: fail because helper file does not exist.

- [ ] **Step 3: Update frontend types**

In `web/src/lib/types.ts`, add:

```ts
export type OrchestratorWorkflowState =
  | 'backlog'
  | 'todo'
  | 'inProgress'
  | 'humanReview'
  | 'rework'
  | 'merging'
  | 'done'
  | 'canceled';

export type OrchestratorRunState =
  | 'idle'
  | 'queued'
  | 'preparing'
  | 'running'
  | 'verifying'
  | 'retrying'
  | 'blocked'
  | 'delivering';

export type OrchestratorAttemptPhase =
  | 'preparingWorkspace'
  | 'buildingPrompt'
  | 'launchingRunner'
  | 'initializingSession'
  | 'streaming'
  | 'finishing'
  | 'succeeded'
  | 'failed'
  | 'timedOut'
  | 'stalled'
  | 'canceledByReconciliation';

export interface OrchestratorRuntimeSnapshot {
  projectId: string;
  generatedAt: string;
  schedulerEnabled: boolean;
  workflowSource: 'builtInDefault' | 'projectOverride' | string;
  workflowValid: boolean;
  workflowError: string | null;
  slotsUsed: number;
  slotsAvailable: number;
  latestError: string | null;
}
```

Extend `OrchestratorTask` with:

```ts
workflowState: OrchestratorWorkflowState;
runState: OrchestratorRunState;
attemptPhase: OrchestratorAttemptPhase | null;
source: 'internal' | 'tracker' | string;
externalId: string | null;
externalIdentifier: string | null;
externalUrl: string | null;
runnerProvider: string | null;
claudeSessionId: string | null;
transcriptPath: string | null;
runtimeStartedAt: string | null;
lastActivityAt: string | null;
lastRuntimeEvent: string | null;
lastRuntimeMessage: string | null;
```

- [ ] **Step 4: Implement board helpers**

Create `web/src/pages/Orchestrator/orchestratorBoard.ts`:

```ts
import type {
  OrchestratorRunState,
  OrchestratorTaskView,
  OrchestratorWorkflowState,
} from '@/lib/types';

export const ORCHESTRATOR_BOARD_LANES: readonly OrchestratorWorkflowState[] = [
  'backlog',
  'todo',
  'inProgress',
  'humanReview',
  'rework',
  'merging',
  'done',
  'canceled',
];

export type OrchestratorBoardGroups = Record<OrchestratorWorkflowState, OrchestratorTaskView[]>;

/**
 * Business Logic（为什么需要）:
 *   项目自动化看板按业务泳道展示任务，而 pendingRemote outbox 不参与泳道分组。
 *
 * Code Logic（做什么）:
 *   初始化所有固定泳道，然后把 local/remote 真实任务按 task.workflowState 归组。
 */
export function groupTasksByWorkflowState(
  views: OrchestratorTaskView[],
): OrchestratorBoardGroups {
  const groups = ORCHESTRATOR_BOARD_LANES.reduce((acc, lane) => {
    acc[lane] = [];
    return acc;
  }, {} as OrchestratorBoardGroups);

  for (const view of views) {
    if (view.origin === 'pendingRemote') continue;
    groups[view.task.workflowState].push(view);
  }

  return groups;
}

/**
 * Business Logic（为什么需要）:
 *   用户可以通过拖拽调整任务泳道，但只能移动到相邻泳道以避免隐式触发复杂副作用。
 *
 * Code Logic（做什么）:
 *   根据固定泳道顺序和 runState 判断是否允许拖拽。
 */
export function canMoveToLane(
  from: OrchestratorWorkflowState,
  to: OrchestratorWorkflowState,
  runState: OrchestratorRunState,
): boolean {
  if (['preparing', 'running', 'verifying', 'delivering'].includes(runState)) return false;
  const fromIndex = ORCHESTRATOR_BOARD_LANES.indexOf(from);
  const toIndex = ORCHESTRATOR_BOARD_LANES.indexOf(to);
  if (fromIndex === -1 || toIndex === -1) return false;
  return Math.abs(fromIndex - toIndex) === 1;
}
```

- [ ] **Step 5: Update API command builders**

In `web/src/api/orchestrator.ts`, add:

```ts
export interface MoveOrchestratorTaskWorkflowStateRequest {
  projectId: string;
  taskId: string;
  targetState: OrchestratorWorkflowState;
}

export function buildMoveOrchestratorTaskWorkflowStateInvokeArgs(
  request: MoveOrchestratorTaskWorkflowStateRequest,
): Record<string, unknown> {
  return { request };
}
```

Add API methods:

```ts
moveWorkflowState: (request: MoveOrchestratorTaskWorkflowStateRequest) =>
  invoke<OrchestratorTaskView>(
    'move_orchestrator_task_workflow_state',
    buildMoveOrchestratorTaskWorkflowStateInvokeArgs(request),
  ),

getRuntimeSnapshot: (projectId: string) =>
  invoke<OrchestratorRuntimeSnapshot>('get_orchestrator_runtime_snapshot', {
    projectId: projectId.trim(),
  }),
```

- [ ] **Step 6: Update orchestrator helper tests**

Add tests to `web/src/lib/orchestrator.test.ts` asserting new lane tones and legacy fallback:

```ts
import { orchestratorWorkflowStateTone } from './orchestrator';

if (orchestratorWorkflowStateTone('humanReview') !== 'warn') {
  throw new Error('humanReview should use warn tone');
}
if (orchestratorWorkflowStateTone('done') !== 'success') {
  throw new Error('done should use success tone');
}
```

Implement `orchestratorWorkflowStateTone` in `web/src/lib/orchestrator.ts`.

- [ ] **Step 7: Run frontend pure/API tests**

Run:

```bash
cd web
npx --yes tsx src/pages/Orchestrator/orchestratorBoard.test.ts
npx --yes tsx src/lib/orchestrator.test.ts
npx --yes tsx src/lib/orchestratorRemote.test.ts
npx --yes tsx src/api/orchestrator.test.ts
npx tsc --noEmit
```

Expected: all pass.

- [ ] **Step 8: Commit Task 5**

Run:

```bash
git add web/src/lib/types.ts web/src/api/orchestrator.ts web/src/lib/orchestrator.ts web/src/lib/orchestratorRemote.ts web/src/pages/Orchestrator/orchestratorBoard.ts web/src/pages/Orchestrator/orchestratorBoard.test.ts web/src/api/orchestrator.test.ts web/src/lib/orchestrator.test.ts web/src/lib/orchestratorRemote.test.ts
git commit -m "feat(web): add orchestrator board data helpers"
```

---

### Task 6: Desktop Board, Status Strip, Drawer, And Create Dialog

**Files:**
- Create: `web/src/pages/Orchestrator/OrchestratorBoard.tsx`
- Create: `web/src/pages/Orchestrator/OrchestratorStatusStrip.tsx`
- Create: `web/src/pages/Orchestrator/OrchestratorTaskDrawer.tsx`
- Create: `web/src/pages/Orchestrator/OrchestratorCreateDialog.tsx`
- Modify: `web/src/pages/Orchestrator/Orchestrator.tsx`
- Modify: `web/src/pages/Orchestrator/Orchestrator.module.css`
- Modify: `web/src/i18n/locales/zh/orchestrator.json`
- Modify: `web/src/i18n/locales/en/orchestrator.json`
- Test: `web/src/pages/Workbench/workbenchAutomationView.test.ts`

- [ ] **Step 1: Add Workbench automation view test expectations**

Update `web/src/pages/Workbench/workbenchAutomationView.test.ts` to assert:

```ts
import { ORCHESTRATOR_BOARD_LANES } from '@/pages/Orchestrator/orchestratorBoard';

if (!ORCHESTRATOR_BOARD_LANES.includes('humanReview')) {
  throw new Error('automation board should include Human Review lane');
}
```

Add a pure test for create action labels if this test already uses render helpers:

```ts
const expectedCreateActions = ['backlog', 'todo', 'start'] as const;
if (expectedCreateActions.length !== 3) {
  throw new Error('create dialog should expose three actions');
}
```

- [ ] **Step 2: Create status strip component**

Create `web/src/pages/Orchestrator/OrchestratorStatusStrip.tsx`:

```tsx
import { Pill } from '@/components/primitives';
import type { OrchestratorRuntimeSnapshot } from '@/lib/types';
import type { TFunction } from 'i18next';
import styles from './Orchestrator.module.css';

interface OrchestratorStatusStripProps {
  snapshot: OrchestratorRuntimeSnapshot | null;
  loading: boolean;
  onRefresh: () => void;
  t: TFunction<'orchestrator'>;
}

/**
 * Business Logic（为什么需要）:
 *   用户需要在看板顶部看到 scheduler、slots 和 workflow 是否健康，解释任务为什么没有启动。
 *
 * Code Logic（做什么）:
 *   渲染 runtime snapshot 的紧凑状态条；缺快照时显示加载/未知态。
 */
export function OrchestratorStatusStrip(props: OrchestratorStatusStripProps): JSX.Element {
  const { snapshot, loading, onRefresh, t } = props;
  return (
    <section className={styles.statusStrip} aria-label={t('statusStrip.ariaLabel')}>
      <Pill tone={snapshot?.schedulerEnabled ? 'success' : 'warn'} dot>
        {snapshot?.schedulerEnabled ? t('statusStrip.schedulerOn') : t('statusStrip.schedulerOff')}
      </Pill>
      <Pill tone="neutral">
        {snapshot
          ? t('statusStrip.slots', {
              used: snapshot.slotsUsed,
              available: snapshot.slotsAvailable,
            })
          : t('statusStrip.slotsUnknown')}
      </Pill>
      <Pill tone={snapshot?.workflowValid === false ? 'danger' : 'accent'}>
        {snapshot?.workflowSource === 'projectOverride'
          ? t('statusStrip.workflowProject')
          : t('statusStrip.workflowBuiltIn')}
      </Pill>
      {snapshot?.latestError ? <span className={styles.statusError}>{snapshot.latestError}</span> : null}
      <button className={styles.inlineLinkButton} type="button" disabled={loading} onClick={onRefresh}>
        {loading ? t('statusStrip.refreshing') : t('statusStrip.refresh')}
      </button>
    </section>
  );
}
```

- [ ] **Step 3: Create board component**

Create `web/src/pages/Orchestrator/OrchestratorBoard.tsx` with props:

```tsx
interface OrchestratorBoardProps {
  views: OrchestratorTaskView[];
  selectedTaskId: string | null;
  movingTaskId: string | null;
  onSelectTask: (taskId: string) => void;
  onMoveTask: (taskId: string, targetState: OrchestratorWorkflowState) => void;
  t: TFunction<'orchestrator'>;
}
```

Use `draggable` task cards. On drag start, set `dataTransfer.setData('text/plain', task.id)`. On drop, call `canMoveToLane(task.workflowState, lane, task.runState)` before `onMoveTask`.

Render fixed lanes from `ORCHESTRATOR_BOARD_LANES`. Keep `merging/done/canceled` visually compact with CSS class `secondaryLane`.

- [ ] **Step 4: Create task drawer component**

Create `web/src/pages/Orchestrator/OrchestratorTaskDrawer.tsx` with props:

```tsx
interface OrchestratorTaskDrawerProps {
  view: OrchestratorTaskView | null;
  evidenceItems: OrchestratorEvidence[];
  evidenceLoading: boolean;
  onOpenWorkbench: () => void;
  onStart: () => void;
  onRetry: () => void;
  onDeliver: () => void;
  onCancel: () => void;
  t: TFunction<'orchestrator'>;
}
```

Render:

- title/goal/acceptance
- workflow/run/attempt pills
- worktree/session link
- Claude runtime fields when present
- evidence timeline list with existing evidence tone helpers
- action buttons with existing `Button` primitive

- [ ] **Step 5: Create three-action dialog**

Create `web/src/pages/Orchestrator/OrchestratorCreateDialog.tsx`:

```tsx
export type OrchestratorCreateAction = 'backlog' | 'todo' | 'start';
```

The submit buttons call `onSubmit(form, action)` for:

- `backlog`
- `todo`
- `start`

Keep AI completion behavior from the old dialog by passing `onCompletePrompt`.

- [ ] **Step 6: Refactor `OrchestratorPanel` composition**

In `web/src/pages/Orchestrator/Orchestrator.tsx`:

- keep state loading logic
- replace old grid render with `OrchestratorStatusStrip`, `OrchestratorBoard`, `OrchestratorTaskDrawer`, `OrchestratorCreateDialog`
- call `orchestratorApi.getRuntimeSnapshot(activeProjectId)` alongside task view loading
- call `orchestratorApi.moveWorkflowState({projectId, taskId, targetState})` on board drop
- for create action:
  - `backlog`: create task with `initialWorkflowState='backlog'`
  - `todo`: create task with `initialWorkflowState='todo'`
  - `start`: create task with `initialWorkflowState='todo'` and queue/start after creation

If backend create request does not yet accept these fields, add them in Task 3 command DTO before this step.

- [ ] **Step 7: Add CSS module classes**

In `web/src/pages/Orchestrator/Orchestrator.module.css`, add classes:

```css
.statusStrip { display: flex; align-items: center; gap: var(--space-2); padding: var(--space-3); border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--surface); }
.statusError { color: var(--danger); font-size: var(--text-sm); }
.board { display: grid; grid-template-columns: repeat(5, minmax(180px, 1fr)) minmax(150px, 0.7fr); gap: var(--space-3); min-height: 480px; }
.lane { min-width: 0; border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--surface); padding: var(--space-3); }
.secondaryLane { opacity: 0.86; }
.laneHeader { display: flex; align-items: center; justify-content: space-between; gap: var(--space-2); margin-bottom: var(--space-3); }
.taskCard { width: 100%; text-align: left; border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--surface-raised); padding: var(--space-3); transition: all var(--motion-fast) var(--ease-standard); }
.taskCardActive { border-color: var(--accent); box-shadow: var(--shadow-sm); }
.drawer { border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--surface); padding: var(--space-4); }
.evidenceTimeline { display: grid; gap: var(--space-3); }
```

Use existing token names; adjust names only if tokens differ after checking `web/src/styles/tokens.css`.

- [ ] **Step 8: Add i18n keys**

Add to both `zh/orchestrator.json` and `en/orchestrator.json` keys for:

- `statusStrip.ariaLabel`
- `statusStrip.schedulerOn`
- `statusStrip.schedulerOff`
- `statusStrip.slots`
- `statusStrip.slotsUnknown`
- `statusStrip.workflowProject`
- `statusStrip.workflowBuiltIn`
- `statusStrip.refresh`
- `statusStrip.refreshing`
- `board.lanes.backlog`
- `board.lanes.todo`
- `board.lanes.inProgress`
- `board.lanes.humanReview`
- `board.lanes.rework`
- `board.lanes.merging`
- `board.lanes.done`
- `board.lanes.canceled`
- `create.submitBacklog`
- `create.submitTodo`
- `create.submitStart`
- `drawer.runtimeUnknown`
- `drawer.openTranscript`

- [ ] **Step 9: Run frontend tests/typecheck**

Run:

```bash
cd web
npx --yes tsx src/pages/Workbench/workbenchAutomationView.test.ts
npx --yes tsx src/pages/Orchestrator/orchestratorBoard.test.ts
npx --yes tsx src/lib/orchestrator.test.ts
npx tsc --noEmit
```

Expected: all pass.

- [ ] **Step 10: Commit Task 6**

Run:

```bash
git add web/src/pages/Orchestrator web/src/pages/Workbench/workbenchAutomationView.test.ts web/src/i18n/locales/zh/orchestrator.json web/src/i18n/locales/en/orchestrator.json
git commit -m "feat(web): add automation board experience"
```

---

### Task 7: Remote And Mobile Compatibility

**Files:**
- Modify: `src-tauri/src/orchestrator/remote_protocol.rs`
- Modify: `src-tauri/src/orchestrator/outbox.rs`
- Modify: `src-tauri/src/commands/orchestrator.rs`
- Modify: `web/src/mobile/components/MobileAutomationPanel.tsx`
- Test: `web/src/mobile/MobileAutomationPanel.test.ts`
- Test: `web/src/lib/orchestratorRemote.test.ts`

- [ ] **Step 1: Extend remote DTOs with optional fields**

In `src-tauri/src/orchestrator/remote_protocol.rs`, ensure remote task DTO includes optional fields with `#[serde(default)]` where deserializing:

```rust
pub workflow_state: Option<OrchestratorWorkflowState>,
pub run_state: Option<OrchestratorRunState>,
pub attempt_phase: Option<OrchestratorAttemptPhase>,
pub runner_provider: Option<String>,
pub claude_session_id: Option<String>,
pub transcript_path: Option<String>,
pub last_activity_at: Option<String>,
pub last_runtime_event: Option<String>,
pub last_runtime_message: Option<String>,
```

When mapping old remote tasks without fields, apply `SplitTaskState::from_legacy_status(task.status)`.

- [ ] **Step 2: Update outbox/mirror persistence**

In `src-tauri/src/orchestrator/outbox.rs`, add the new fields to mirror payload JSON. Keep pending remote outbox UI limited to item status/device/error; do not synthesize workflow cards for pending items.

- [ ] **Step 3: Update mobile grouped list test**

In `web/src/mobile/MobileAutomationPanel.test.ts`, add:

```ts
import { groupTasksByWorkflowState } from '@/pages/Orchestrator/orchestratorBoard';

const grouped = groupTasksByWorkflowState([
  {
    origin: 'local',
    task: {
      id: 'mobile-task',
      projectId: 'project-1',
      title: 'Mobile task',
      goal: 'goal',
      acceptanceCriteria: 'accept',
      status: 'draft',
      workflowState: 'todo',
      runState: 'idle',
      attemptPhase: null,
      priority: 0,
      branchName: null,
      worktreeId: null,
      sessionId: null,
      blockedReason: null,
      attempt: 0,
      createdAt: '2026-07-06T00:00:00Z',
      updatedAt: '2026-07-06T00:00:00Z',
      startedAt: null,
      finishedAt: null,
      source: 'internal',
      externalId: null,
      externalIdentifier: null,
      externalUrl: null,
      runnerProvider: null,
      claudeSessionId: null,
      transcriptPath: null,
      runtimeStartedAt: null,
      lastActivityAt: null,
      lastRuntimeEvent: null,
      lastRuntimeMessage: null,
    },
  },
]);

if (grouped.todo.length !== 1) {
  throw new Error('mobile automation should group by workflowState');
}
```

- [ ] **Step 4: Update MobileAutomationPanel rendering**

In `web/src/mobile/components/MobileAutomationPanel.tsx`:

- use `groupTasksByWorkflowState`
- render collapsible sections for workflow states with task counts
- show `runState` and `lastRuntimeMessage` on task rows
- update create dialog to expose three actions
- keep no drag/drop on mobile

- [ ] **Step 5: Run remote/mobile tests**

Run:

```bash
cd web
npx --yes tsx src/mobile/MobileAutomationPanel.test.ts
npx --yes tsx src/lib/orchestratorRemote.test.ts
npx tsc --noEmit
cd ../src-tauri
cargo test orchestrator::remote_protocol orchestrator::outbox --lib
```

Expected: all pass.

- [ ] **Step 6: Commit Task 7**

Run:

```bash
git add src-tauri/src/orchestrator/remote_protocol.rs src-tauri/src/orchestrator/outbox.rs src-tauri/src/commands/orchestrator.rs web/src/mobile/components/MobileAutomationPanel.tsx web/src/mobile/MobileAutomationPanel.test.ts web/src/lib/orchestratorRemote.test.ts
git commit -m "feat(orchestrator): keep remote and mobile automation compatible"
```

---

### Task 8: Settings Delivery Gate And Documentation

**Files:**
- Modify: `src-tauri/src/orchestrator/config.rs`
- Modify: `src-tauri/src/commands/orchestrator_config.rs`
- Modify: `web/src/pages/Settings/automationSettingsState.ts`
- Modify: `web/src/pages/Settings/AutomationSettingsPanel.tsx`
- Modify: `web/src/pages/Settings/automationSettingsState.test.ts`
- Modify: `docs/prd.md`
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`

- [ ] **Step 1: Add Settings delivery gate tests**

In `web/src/pages/Settings/automationSettingsState.test.ts`, add:

```ts
import { automationConfigToForm, automationFormToPatch } from './automationSettingsState';

const config = {
  enabled: false,
  maxConcurrentTasks: 1,
  verificationCommands: [],
  autoCommit: false,
  autoPushTaskBranch: false,
  autoMergeToMain: false,
  autoPushMain: false,
  autoDeliveryEnabled: false,
};

const form = automationConfigToForm(config);
if (form.autoDeliveryEnabled !== false) {
  throw new Error('automatic delivery should default to disabled');
}
const patch = automationFormToPatch({ ...form, autoDeliveryEnabled: true });
if (patch.autoDeliveryEnabled !== true) {
  throw new Error('autoDeliveryEnabled should round-trip into patch');
}
```

- [ ] **Step 2: Run Settings test and verify it fails**

Run:

```bash
cd web
npx --yes tsx src/pages/Settings/automationSettingsState.test.ts
```

Expected: fail because `autoDeliveryEnabled` is not modeled.

- [ ] **Step 3: Add backend config field**

In `src-tauri/src/orchestrator/config.rs`, add `auto_delivery_enabled: bool` to config DTO/patch, default false. Keep the old four delivery stage booleans only as stage-specific controls if they already exist. Delivery can run only if:

```rust
config.auto_delivery_enabled
    && config.auto_commit
    && config.auto_push_task_branch
    && config.auto_merge_to_main
    && config.auto_push_main
```

- [ ] **Step 4: Update Settings UI helper and panel**

In `web/src/pages/Settings/automationSettingsState.ts`, include `autoDeliveryEnabled` in form conversion and patch generation.

In `AutomationSettingsPanel.tsx`, add a checkbox/toggle labeled:

- zh: `验证通过后自动交付`
- en: `Automatically deliver after review passes`

Description:

- zh: `默认关闭。关闭时任务会停在 Human Review，需手动交付。`
- en: `Off by default. When disabled, tasks stop in Human Review until delivered manually.`

- [ ] **Step 5: Update delivery gate**

In `src-tauri/src/orchestrator/delivery.rs` and completion path, ensure automatic delivery is entered only when `auto_delivery_enabled=true`. If false after verifier pass, set `workflow_state='humanReview'` and do not call `deliver_task`.

- [ ] **Step 6: Update PRD and memory files**

Modify `docs/prd.md` sections 2.15 and 2.16 to include:

- board-first Workbench automation
- split `workflowState/runState/attemptPhase`
- built-in workflow default plus optional `WORKFLOW.md`
- Settings-only automatic delivery, default off
- Claude Code visible runtime association

Modify `web/CLAUDE.md` Orchestrator frontend section with:

- board components
- status strip
- detail drawer
- drag/drop adjacent rule
- test command:

```bash
npx --yes tsx src/pages/Orchestrator/orchestratorBoard.test.ts && npx --yes tsx src/pages/Workbench/workbenchAutomationView.test.ts && npx --yes tsx src/lib/orchestrator.test.ts && npx --yes tsx src/lib/orchestratorRemote.test.ts && npx --yes tsx src/api/orchestrator.test.ts
```

Modify `src-tauri/CLAUDE.md` Orchestrator section with:

- split state model
- workflow resolver
- Settings delivery gate
- Claude runtime association
- focused Rust test command:

```bash
cargo test orchestrator::models::split_state_tests orchestrator::workflow::tests orchestrator::claude_runtime::tests --lib
```

- [ ] **Step 7: Run Settings/docs-adjacent validation**

Run:

```bash
cd web
npx --yes tsx src/pages/Settings/automationSettingsState.test.ts
npx tsc --noEmit
cd ../src-tauri
cargo test orchestrator::config orchestrator::delivery --lib
```

Expected: tests pass.

- [ ] **Step 8: Commit Task 8**

Run:

```bash
git add src-tauri/src/orchestrator/config.rs src-tauri/src/commands/orchestrator_config.rs src-tauri/src/orchestrator/delivery.rs web/src/pages/Settings/automationSettingsState.ts web/src/pages/Settings/AutomationSettingsPanel.tsx web/src/pages/Settings/automationSettingsState.test.ts docs/prd.md web/CLAUDE.md src-tauri/CLAUDE.md
git commit -m "feat(orchestrator): gate automatic delivery in settings"
```

---

### Task 9: Final Integration Verification

**Files:**
- Review all changed files from Tasks 1-8.

- [ ] **Step 1: Run frontend focused suite**

Run:

```bash
cd web
npx --yes tsx src/pages/Orchestrator/orchestratorBoard.test.ts
npx --yes tsx src/pages/Workbench/workbenchAutomationView.test.ts
npx --yes tsx src/lib/orchestrator.test.ts
npx --yes tsx src/lib/orchestratorRemote.test.ts
npx --yes tsx src/api/orchestrator.test.ts
npx --yes tsx src/mobile/MobileAutomationPanel.test.ts
npx --yes tsx src/pages/Settings/automationSettingsState.test.ts
npx tsc --noEmit
```

Expected: all commands exit 0.

- [ ] **Step 2: Run backend focused suite**

Run:

```bash
cd src-tauri
cargo test orchestrator::models::split_state_tests orchestrator::workflow::tests orchestrator::claude_runtime::tests --lib
cargo test orchestrator::scheduler orchestrator::runner orchestrator::delivery orchestrator::remote_protocol orchestrator::outbox --lib
cargo check
```

Expected: all commands exit 0.

- [ ] **Step 3: Inspect git diff for scope**

Run:

```bash
git diff --stat HEAD
git diff --check
```

Expected: diff only includes Orchestrator/Workbench automation files, Settings delivery gate, tests, PRD, and memory files. `git diff --check` exits 0.

- [ ] **Step 4: Manual desktop smoke test**

Run the app:

```bash
./web/node_modules/.bin/tauri dev
```

Smoke path:

1. Open a local Workbench project.
2. Open Project Automation.
3. Confirm status strip renders.
4. Create a task in Backlog.
5. Drag Backlog → Todo.
6. Try dragging Todo → Human Review and confirm it is rejected.
7. Create a task with Create and Start.
8. Confirm task enters In Progress and execution site link opens terminal.

Expected: no console/runtime errors and no overlapping UI.

- [ ] **Step 5: Commit final fixes**

If verification required fixes, inspect the changed file list first:

```bash
git status --short
```

Stage only the files changed by the verification fixes. For the expected files in this plan, use the relevant subset of:

```bash
git add src-tauri/src/orchestrator src-tauri/src/commands/orchestrator.rs src-tauri/src/commands/orchestrator_config.rs src-tauri/src/lib.rs web/src/pages/Orchestrator web/src/pages/Workbench/workbenchAutomationView.test.ts web/src/lib/types.ts web/src/lib/orchestrator.ts web/src/lib/orchestratorRemote.ts web/src/api/orchestrator.ts web/src/mobile/components/MobileAutomationPanel.tsx web/src/mobile/MobileAutomationPanel.test.ts web/src/pages/Settings/automationSettingsState.ts web/src/pages/Settings/automationSettingsState.test.ts web/src/pages/Settings/AutomationSettingsPanel.tsx web/src/i18n/locales/zh/orchestrator.json web/src/i18n/locales/en/orchestrator.json docs/prd.md web/CLAUDE.md src-tauri/CLAUDE.md
git commit -m "fix(orchestrator): polish automation integration"
```

If no fixes were required, do not create an empty commit.
