# Orchestrator Remote Autonomous Verification Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move Orchestrator automation policy into a device-global Settings tab, make LAN remote projects execute on their owning device, and replace manual validation with an automatic Claude verification and repair loop that continues until the verifier passes the task or the user aborts it.

**Architecture:** Keep local Orchestrator tasks authoritative in SQLite on the device that owns the Workbench project. Store automation policy in `AppConfig.orchestrator`. Add P2P Orchestrator endpoints so remote shortcuts forward task operations to the owning device. Add a local pending outbox and remote mirror cache for offline remote projects. Extend the visible Workbench Runner with per-attempt sessions, a terminal completion sentinel, a headless verifier Claude, and a repair loop that reuses the same worktree while creating a new development terminal each failed attempt.

**Tech Stack:** Rust/Tauri 2, axum P2P HTTP routes, reqwest remote client, sqlx SQLite, tokio async tasks, existing Workbench local and remote helpers, React 19, TypeScript, React Router v6, CSS Modules, i18next.

---

## Source Specs

- Product design: `docs/superpowers/specs/2026-07-05-orchestrator-remote-autonomous-verification-loop-design.md`
- Current Orchestrator plan baseline: `docs/superpowers/plans/2026-07-05-orchestrator.md`
- Current project PRD: `docs/prd.md`

This plan implements the confirmed decisions:

1. Automation config is device-global and lives in Settings as its own tab.
2. Remote project execution is owned by the remote cc-partner instance.
3. Offline remote task creation writes a local pending outbox item.
4. Development completion automatically triggers validation.
5. Validation runs configured commands and passes their output plus diff/context to a verifier Claude.
6. Failed verification creates a new development Claude session in the same task worktree.
7. The loop has no fixed retry limit; it stops on verifier pass, user abort, or infrastructure failure.

## Execution Rules

- [ ] Create an isolated worktree before implementation because this change is larger than 100 lines and touches frontend, backend, P2P, and persistence:

```bash
git status --short
git worktree add ../cc-partner-orchestrator-loop -b codex/orchestrator-loop
cd ../cc-partner-orchestrator-loop
```

Expected output: the original worktree status is visible, and the new worktree is on branch `codex/orchestrator-loop`.

- [ ] Read project memory before editing source:

```bash
cat AGENTS.md
cat src-tauri/CLAUDE.md
cat web/CLAUDE.md
```

Expected output: root, Rust, and frontend instructions load successfully.

- [ ] Use subagents for implementation. The minimum split is:

| Workstream | Model route | Scope |
| --- | --- | --- |
| Backend config/scheduler/delivery loop | `gpt-5.5(xhigh)` | Rust Orchestrator core, config, verifier, tests |
| Remote protocol/outbox/mirror | `gpt-5.5(xhigh)` | Rust P2P routes/client, SQLite tables, dispatcher, tests |
| Frontend Settings/Workbench UI | `gpt-5.5(xhigh)` | React Settings tab, Orchestrator panel, i18n, tests |

- [ ] After subagents finish, inspect `git diff` directly instead of reading subagent logs. Verify the implementation against this plan phase by phase.

- [ ] Commit after each completed phase with focused commits. Keep documentation updates in the same commit as the behavior they describe.

## File Map

### Rust Backend

Modify:

- `src-tauri/src/config.rs`
- `src-tauri/src/state.rs`
- `src-tauri/src/lib.rs`
- `src-tauri/src/commands/mod.rs`
- `src-tauri/src/commands/orchestrator.rs`
- `src-tauri/src/orchestrator/models.rs`
- `src-tauri/src/orchestrator/repo.rs`
- `src-tauri/src/orchestrator/state.rs`
- `src-tauri/src/orchestrator/prompt.rs`
- `src-tauri/src/orchestrator/scheduler.rs`
- `src-tauri/src/orchestrator/runner.rs`
- `src-tauri/src/orchestrator/delivery.rs`
- `src-tauri/src/orchestrator/mod.rs`
- `src-tauri/src/workbench/sessions.rs`
- `src-tauri/src/workbench/remote_client.rs`
- `src-tauri/src/workbench/remote_protocol.rs`
- `src-tauri/src/net/http_server.rs`
- `src-tauri/src/net/routes/mod.rs`

Create:

- `src-tauri/src/commands/orchestrator_config.rs`
- `src-tauri/src/orchestrator/config.rs`
- `src-tauri/src/orchestrator/remote_protocol.rs`
- `src-tauri/src/orchestrator/remote_client.rs`
- `src-tauri/src/orchestrator/outbox.rs`
- `src-tauri/src/orchestrator/verifier.rs`
- `src-tauri/src/orchestrator/completion.rs`
- `src-tauri/src/net/routes/orchestrator.rs`

### Frontend

Modify:

- `web/src/api/config.ts`
- `web/src/api/orchestrator.ts`
- `web/src/lib/types.ts`
- `web/src/lib/orchestrator.ts`
- `web/src/pages/Settings/Settings.tsx`
- `web/src/pages/Settings/Settings.module.css`
- `web/src/pages/Settings/settingsState.ts`
- `web/src/pages/Settings/settingsState.test.ts`
- `web/src/pages/Workbench/Workbench.tsx`
- `web/src/pages/Workbench/workbenchAutomationView.test.ts`
- `web/src/pages/Orchestrator/Orchestrator.tsx`
- `web/src/i18n/locales/zh/settings.json`
- `web/src/i18n/locales/en/settings.json`
- `web/src/i18n/locales/zh/orchestrator.json`
- `web/src/i18n/locales/en/orchestrator.json`

Create:

- `web/src/api/orchestratorConfig.ts`
- `web/src/api/orchestratorRemote.test.ts`
- `web/src/lib/orchestratorRemote.ts`
- `web/src/lib/orchestratorRemote.test.ts`
- `web/src/pages/Settings/AutomationSettingsPanel.tsx`
- `web/src/pages/Settings/automationSettingsState.ts`
- `web/src/pages/Settings/automationSettingsState.test.ts`

### Documentation And Memory

Modify when behavior changes:

- `docs/prd.md`
- `src-tauri/CLAUDE.md`
- `web/CLAUDE.md`

Root `AGENTS.md` already describes Orchestrator and Settings at a high level. Keep it unchanged unless implementation adds a new top-level directory or changes the top-level map.

## Data Model

### Device-Global Automation Config

Add a nested config object in `src-tauri/src/config.rs`.

```rust
/// Orchestrator 自动化全局配置。
///
/// Business Logic（为什么需要这个结构）:
///     自动化策略属于本设备运行偏好，不需要按项目分叉；Settings 自动化 tab 需要持久化
///     scheduler 开关、并发上限、验证命令和 full-auto delivery 开关。
///
/// Code Logic（这个结构做什么）:
///     纯 serde 配置载体，落盘在 AppConfig.orchestrator 下；所有字段有默认值，保证旧
///     config.json 缺少 orchestrator 字段时可正常反序列化。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorAutomationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_orchestrator_max_concurrent_tasks")]
    pub max_concurrent_tasks: i64,
    #[serde(default)]
    pub verification_commands: Vec<String>,
    #[serde(default = "default_true")]
    pub auto_commit: bool,
    #[serde(default = "default_true")]
    pub auto_push_task_branch: bool,
    #[serde(default = "default_true")]
    pub auto_merge_to_main: bool,
    #[serde(default = "default_true")]
    pub auto_push_main: bool,
}
```

`AppConfig` adds:

```rust
#[serde(default)]
pub orchestrator: OrchestratorAutomationConfig,
```

Validation rules:

- `enabled`: boolean.
- `max_concurrent_tasks`: integer from 1 to 8.
- `verification_commands`: trim every line, remove empty lines, keep order, reject more than 20 commands, reject a single command over 500 chars.
- Delivery flags: persisted booleans. If any required delivery flag is disabled and a task reaches delivery, the pipeline records evidence and blocks the task with an explicit manual-delivery reason.

Constants that are no longer configurable:

- Branch prefix: keep `agent`.
- Retain worktree on done: `false`.
- Retain worktree on blocked: `true`.
- Retry limit: not used; the verification loop has no fixed max.

The existing `orchestrator_project_config` table remains in SQLite for old records and local inspection, but new runtime code must not read it for scheduling, verification commands, or delivery flags.

### Orchestrator Task Attempt History

Add a table to track every development attempt and its visible terminal session.

```sql
CREATE TABLE IF NOT EXISTS orchestrator_task_attempts (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  worktree_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  prompt TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  completed_at TEXT,
  UNIQUE(task_id, attempt)
);
```

`status` values:

- `running`
- `completed`
- `blocked`

`orchestrator_tasks.session_id` continues to represent the active development session. Attempt history preserves older sessions.

### Remote Outbox

Add a local table in `src-tauri/src/orchestrator/repo.rs`:

```sql
CREATE TABLE IF NOT EXISTS orchestrator_remote_outbox (
  id TEXT PRIMARY KEY,
  device_id TEXT NOT NULL,
  device_name TEXT NOT NULL,
  remote_project_path TEXT NOT NULL,
  remote_project_id TEXT,
  request_json TEXT NOT NULL,
  status TEXT NOT NULL,
  remote_task_id TEXT,
  last_error TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  sent_at TEXT
);
```

`status` values:

- `pending`: waiting for the remote device to become reachable.
- `sending`: dispatcher has claimed the item.
- `mirrored`: remote task was created and `remote_task_id` is stored.
- `failed`: request is invalid or remote rejected it in a non-network way.

### Remote Task Mirror Cache

Add a cache table for the latest remote task payloads:

```sql
CREATE TABLE IF NOT EXISTS orchestrator_remote_task_mirrors (
  id TEXT PRIMARY KEY,
  device_id TEXT NOT NULL,
  device_name TEXT NOT NULL,
  remote_project_id TEXT NOT NULL,
  remote_project_path TEXT NOT NULL,
  remote_task_id TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  last_synced_at TEXT NOT NULL,
  UNIQUE(device_id, remote_task_id)
);
```

The mirror is display-only. Local scheduler, local verifier, and local delivery must never operate on mirror rows.

## Phase 1: Global Automation Config Backend

### Tests First

- [ ] Add config normalization tests in `src-tauri/src/orchestrator/config.rs`.

```rust
#[test]
fn normalizes_verification_commands() {
    let patch = OrchestratorAutomationConfigPatch {
        enabled: Some(true),
        max_concurrent_tasks: Some(3),
        verification_commands: Some(" npm test \n\ncargo check\n".to_string()),
        auto_commit: Some(true),
        auto_push_task_branch: Some(false),
        auto_merge_to_main: Some(true),
        auto_push_main: Some(false),
    };

    let next = apply_orchestrator_config_patch(OrchestratorAutomationConfig::default(), patch)
        .expect("valid patch");

    assert_eq!(next.verification_commands, vec!["npm test", "cargo check"]);
    assert_eq!(next.max_concurrent_tasks, 3);
    assert!(!next.auto_push_task_branch);
}
```

- [ ] Add rejection tests for `max_concurrent_tasks = 0`, `max_concurrent_tasks = 9`, too many commands, and overlong command.

Run:

```bash
cd src-tauri
cargo test orchestrator::config --lib
```

Expected output: new tests fail before implementation, then pass after implementation.

### Implementation

- [ ] Create `src-tauri/src/orchestrator/config.rs`.

Define:

- `OrchestratorAutomationConfigDto`
- `OrchestratorAutomationConfigPatch`
- `default_orchestrator_automation_config()`
- `apply_orchestrator_config_patch(current, patch) -> Result<OrchestratorAutomationConfig, AppError>`
- `normalize_verification_commands(input: &str) -> Result<Vec<String>, AppError>`

- [ ] Modify `src-tauri/src/config.rs`.

Add `OrchestratorAutomationConfig` and `AppConfig.orchestrator`. Update every `AppConfig { ... }` literal in tests and defaults.

- [ ] Create `src-tauri/src/commands/orchestrator_config.rs`.

Expose Tauri commands:

```rust
#[tauri::command]
pub async fn get_orchestrator_config(
    state: tauri::State<'_, AppState>,
) -> Result<OrchestratorAutomationConfigDto, AppError> {
    let cfg = state.config.read().expect("config 读锁中毒").orchestrator.clone();
    Ok(OrchestratorAutomationConfigDto::from(cfg))
}
```

Also implement:

- `get_default_orchestrator_config`
- `update_orchestrator_config`

`update_orchestrator_config` must write `config.save()` after replacing `cfg.orchestrator`.

- [ ] Register commands in `src-tauri/src/commands/mod.rs` and `src-tauri/src/lib.rs`.

- [ ] Remove new runtime dependencies on `get_orchestrator_project_config` from frontend data loading once Phase 2 lands.

### Verification

```bash
cd src-tauri
cargo test orchestrator::config --lib
cargo test commands::orchestrator_config --lib
cargo check
```

Expected output: all commands finish successfully.

### Commit

```bash
git add src-tauri/src/config.rs src-tauri/src/orchestrator/config.rs src-tauri/src/commands/orchestrator_config.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs
git commit -m "feat: add global orchestrator automation config"
```

## Phase 2: Settings Automation Tab

### Tests First

- [ ] Create `web/src/pages/Settings/automationSettingsState.test.ts`.

Cover:

- Parsing textarea lines into command array.
- Joining commands into textarea text.
- Clamping client-side stepper display to 1..8.
- Dirty state after form edits.
- Reset defaults updates form values but does not persist.

Run:

```bash
cd web
npx --yes tsx src/pages/Settings/automationSettingsState.test.ts
```

Expected output: tests fail before helpers exist, then pass after implementation.

### Implementation

- [ ] Create `web/src/api/orchestratorConfig.ts`.

```ts
import { invoke } from '@tauri-apps/api/core';

export interface OrchestratorAutomationConfig {
  enabled: boolean;
  maxConcurrentTasks: number;
  verificationCommands: string[];
  autoCommit: boolean;
  autoPushTaskBranch: boolean;
  autoMergeToMain: boolean;
  autoPushMain: boolean;
}

export interface OrchestratorAutomationConfigPatch {
  enabled?: boolean;
  maxConcurrentTasks?: number;
  verificationCommands?: string;
  autoCommit?: boolean;
  autoPushTaskBranch?: boolean;
  autoMergeToMain?: boolean;
  autoPushMain?: boolean;
}

export const orchestratorConfigApi = {
  get: () => invoke<OrchestratorAutomationConfig>('get_orchestrator_config'),
  getDefaults: () => invoke<OrchestratorAutomationConfig>('get_default_orchestrator_config'),
  update: (patch: OrchestratorAutomationConfigPatch) =>
    invoke<OrchestratorAutomationConfig>('update_orchestrator_config', { patch }),
};
```

- [ ] Create `web/src/pages/Settings/automationSettingsState.ts`.

Export pure helpers:

- `commandsToTextarea(commands: string[]): string`
- `textareaToCommandsText(value: string): string`
- `automationConfigToForm(config)`
- `automationFormToPatch(form)`
- `isAutomationFormDirty(form, initial)`

- [ ] Create `web/src/pages/Settings/AutomationSettingsPanel.tsx`.

Controls:

- Toggle: enabled.
- Number input or stepper: max concurrent tasks, min 1, max 8.
- Textarea: verification commands, one command per line.
- Four checkboxes/toggles: auto commit, push task branch, merge to main, push main.
- Buttons: save, reset defaults.

Hooks must be declared before early returns.

- [ ] Modify `web/src/pages/Settings/Settings.tsx`.

Add `automation` to `SettingsTabId` and `SETTINGS_TABS`. Load config with the existing Settings `Promise.all` pattern. Render `AutomationSettingsPanel` when `activeTab === 'automation'`.

- [ ] Modify `web/src/pages/Settings/Settings.module.css`.

Reuse existing Settings form layout classes where possible. New CSS must use tokens only.

- [ ] Modify i18n files:

`web/src/i18n/locales/zh/settings.json`:

```json
{
  "tabs": {
    "automation": "自动化"
  },
  "automation": {
    "title": "自动化",
    "description": "配置本设备的 Orchestrator 自动执行、验证和交付策略。",
    "enabled": "启用自动领取任务",
    "maxConcurrentTasks": "并发任务上限",
    "verificationCommands": "验证命令",
    "verificationCommandsHint": "每行一条命令，在任务 worktree 中顺序执行。",
    "autoCommit": "自动提交任务分支",
    "autoPushTaskBranch": "自动推送任务分支",
    "autoMergeToMain": "自动合并到主分支",
    "autoPushMain": "自动推送主分支",
    "save": "保存",
    "resetDefaults": "恢复默认"
  }
}
```

Add matching English copy in `web/src/i18n/locales/en/settings.json`.

### Verification

```bash
cd web
npx --yes tsx src/pages/Settings/automationSettingsState.test.ts
npx --yes tsx src/pages/Settings/settingsState.test.ts
npx tsc --noEmit
```

Expected output: tests and TypeScript pass.

### Commit

```bash
git add web/src/api/orchestratorConfig.ts web/src/pages/Settings web/src/i18n/locales/zh/settings.json web/src/i18n/locales/en/settings.json
git commit -m "feat: add automation settings tab"
```

## Phase 3: Scheduler And Delivery Read Global Config

### Tests First

- [ ] Update scheduler tests in `src-tauri/src/orchestrator/scheduler.rs`.

Cover:

- Disabled global automation dispatches zero tasks.
- Enabled global automation claims queued tasks across local projects up to global capacity.
- Remote project rows are skipped by local scheduler.
- Existing project config rows do not affect dispatch.

- [ ] Update delivery tests in `src-tauri/src/orchestrator/delivery.rs`.

Cover:

- Verification commands are read from global config.
- Disabled `auto_commit` blocks at delivery with evidence.
- Disabled `auto_push_task_branch`, `auto_merge_to_main`, and `auto_push_main` each block at their own stage with evidence.

Run:

```bash
cd src-tauri
cargo test orchestrator::scheduler --lib
cargo test orchestrator::delivery --lib
```

Expected output: tests fail while code still reads `orchestrator_project_config`, then pass after refactor.

### Implementation

- [ ] Modify `src-tauri/src/orchestrator/scheduler.rs`.

Replace project-config iteration:

```rust
let config = state.config.read().expect("config 读锁中毒").orchestrator.clone();
if !config.enabled {
    return Ok(0);
}
let capacity = config.max_concurrent_tasks;
let tasks = state
    .orchestrator_repo
    .claim_next_local_queued_tasks_with_global_capacity(capacity)
    .await?;
```

`claim_next_local_queued_tasks_with_global_capacity` must exclude remote project shortcuts. It can do this by joining `workbench_projects` on `project_id` and requiring `kind = 'local'`.

- [ ] Modify `src-tauri/src/orchestrator/repo.rs`.

Add:

- `count_active_local_tasks()`
- `claim_next_local_queued_tasks_with_global_capacity(limit: i64) -> Result<Vec<OrchestratorTaskRow>, AppError>`
- `add_attempt(...)`
- `mark_attempt_completed(...)`

Keep old project config CRUD only for existing commands until frontend no longer calls them. Mark old functions with comments explaining they are legacy display/debug surface.

- [ ] Modify `src-tauri/src/orchestrator/delivery.rs`.

Read config from `DeliveryContext`:

```rust
fn orchestrator_config(&self) -> OrchestratorAutomationConfig;
```

Use the config for validation commands and delivery gates. A disabled delivery gate writes `delivery` evidence with `summary = "blocked"` and transitions the task to `Blocked`.

- [ ] Modify `src-tauri/src/commands/orchestrator.rs`.

Keep manual `complete_orchestrator_agent_run` as an internal/manual fallback. It should call the same automatic completion pipeline used by the terminal sentinel. It must not require project config.

### Verification

```bash
cd src-tauri
cargo test orchestrator::scheduler --lib
cargo test orchestrator::delivery --lib
cargo test commands::orchestrator --lib
cargo check
```

Expected output: all commands pass.

### Commit

```bash
git add src-tauri/src/orchestrator src-tauri/src/commands/orchestrator.rs
git commit -m "refactor: use global config for orchestrator dispatch"
```

## Phase 4: Remote Orchestrator Protocol And Routes

### Tests First

- [ ] Add protocol serialization tests in `src-tauri/src/orchestrator/remote_protocol.rs`.

Cover camelCase field names for create request, list response, evidence response, and config response.

- [ ] Add route handler tests in `src-tauri/src/net/routes/orchestrator.rs` using in-memory state helpers where existing route tests already do so.

Cover:

- Create task with remote local project id.
- List tasks by project id.
- Return evidence for task id.
- Abort task.
- Return device-global automation config.

Run:

```bash
cd src-tauri
cargo test orchestrator::remote_protocol --lib
cargo test net::routes::orchestrator --lib
```

Expected output: protocol tests pass after DTOs are created; route tests pass after axum handlers are wired.

### Implementation

- [ ] Create `src-tauri/src/orchestrator/remote_protocol.rs`.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCreateOrchestratorTaskReq {
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
    pub priority: i64,
    pub queue: bool,
}
```

Also define:

- `RemoteTaskReq { task_id }`
- `RemoteListTasksReq { project_id }`
- `RemoteOrchestratorTaskListResp { tasks }`
- `RemoteOrchestratorEvidenceResp { evidence }`
- `RemoteOrchestratorConfigResp { config }`

- [ ] Create `src-tauri/src/net/routes/orchestrator.rs`.

Handlers:

- `create_task`
- `list_tasks`
- `get_evidence`
- `queue_task`
- `retry_task`
- `abort_task`
- `get_config`

These handlers must call the same repository/command helpers used by local IPC. They must reject remote shortcuts; on the remote device, the project id must refer to a local Workbench project.

- [ ] Wire routes in `src-tauri/src/net/http_server.rs`.

Routes:

- `POST /api/orchestrator/tasks/create`
- `POST /api/orchestrator/tasks/list`
- `POST /api/orchestrator/tasks/evidence`
- `POST /api/orchestrator/tasks/queue`
- `POST /api/orchestrator/tasks/retry`
- `POST /api/orchestrator/tasks/abort`
- `GET /api/orchestrator/config`

Use POST for task operations to match existing Workbench remote JSON patterns.

- [ ] Create `src-tauri/src/orchestrator/remote_client.rs`.

Mirror `RemoteWorkbenchClient` style:

- `create_task(base_url, req)`
- `list_tasks(base_url, project_id)`
- `get_evidence(base_url, task_id)`
- `queue_task(base_url, task_id)`
- `retry_task(base_url, task_id)`
- `abort_task(base_url, task_id)`
- `get_config(base_url)`

Timeouts:

- Short: list/evidence/config.
- Long: create/queue/retry/abort.

- [ ] Add remote client construction where command layer resolves remote projects. Do not duplicate Workbench project opening; reuse `RemoteWorkbenchClient::open_project`.

### Verification

```bash
cd src-tauri
cargo test orchestrator::remote_protocol --lib
cargo test net::routes::orchestrator --lib
cargo check
```

Expected output: all pass.

### Commit

```bash
git add src-tauri/src/orchestrator/remote_protocol.rs src-tauri/src/orchestrator/remote_client.rs src-tauri/src/net/routes/orchestrator.rs src-tauri/src/net/routes/mod.rs src-tauri/src/net/http_server.rs src-tauri/src/workbench/remote_client.rs
git commit -m "feat: add remote orchestrator protocol"
```

## Phase 5: Pending Remote Outbox And Mirror Cache

### Tests First

- [ ] Create outbox repository tests in `src-tauri/src/orchestrator/outbox.rs`.

Cover:

- Insert pending item.
- Claim pending item as sending.
- Network failure returns item to pending and stores last error.
- Remote validation failure marks failed.
- Successful send marks mirrored with remote task id.
- Mirror upsert replaces payload for same `(device_id, remote_task_id)`.

Run:

```bash
cd src-tauri
cargo test orchestrator::outbox --lib
```

Expected output: tests fail until schema and repo helpers exist.

### Implementation

- [ ] Create `src-tauri/src/orchestrator/outbox.rs`.

Functions:

- `create_pending_remote_task(state, remote_shortcut, create_req)`
- `dispatch_remote_outbox_once(state) -> Result<usize, AppError>`
- `start_orchestrator_remote_outbox_dispatcher(app_handle, state) -> CancellationToken`
- `sync_remote_task_mirror_for_project(state, remote_shortcut) -> Result<Vec<RemoteMirrorTask>, AppError>`

- [ ] Modify `src-tauri/src/orchestrator/repo.rs`.

Add schema constants and methods for outbox and mirror tables. `init_schema` must create them.

- [ ] Modify `src-tauri/src/state.rs`.

Add:

```rust
pub orchestrator_outbox_cancel: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
```

- [ ] Modify `src-tauri/src/lib.rs`.

Start the outbox dispatcher after HTTP server/discovery state is initialized. Store the cancellation token in `AppState`.

- [ ] Modify `src-tauri/src/commands/orchestrator.rs`.

Remote-aware command behavior:

- Local project: existing local task CRUD.
- Remote project online: open project on remote, call remote Orchestrator endpoint, upsert mirror, return mirror DTO.
- Remote project offline: insert pending outbox, return pending DTO.

The command layer must return a discriminated DTO, not raw local task rows.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "camelCase")]
pub enum OrchestratorTaskViewDto {
    Local { task: OrchestratorTaskDto },
    Remote { task: OrchestratorTaskDto, device_id: String, device_name: String },
    PendingRemote { item: OrchestratorRemoteOutboxDto },
}
```

### Verification

```bash
cd src-tauri
cargo test orchestrator::outbox --lib
cargo test commands::orchestrator --lib
cargo check
```

Expected output: all pass.

### Commit

```bash
git add src-tauri/src/orchestrator src-tauri/src/commands/orchestrator.rs src-tauri/src/state.rs src-tauri/src/lib.rs
git commit -m "feat: add orchestrator remote outbox"
```

## Phase 6: Frontend Remote-Aware Orchestrator API And UI

### Tests First

- [ ] Create `web/src/lib/orchestratorRemote.test.ts`.

Cover:

- Local, remote, and pending task discriminated union labels.
- Action availability: pending cannot queue/retry/open session; remote can abort when online; done has no retry unless blocked.
- Merge ordering: pending items first by created time, then active tasks, then terminal tasks.

- [ ] Update `web/src/pages/Workbench/workbenchAutomationView.test.ts`.

Cover:

- Remote project displays remote autonomy notice.
- Pending remote task card shows “待发送到远端”.
- Attempt count is rendered for running/verifying tasks.
- Settings entry points to Settings automation tab.

Run:

```bash
cd web
npx --yes tsx src/lib/orchestratorRemote.test.ts
npx --yes tsx src/pages/Workbench/workbenchAutomationView.test.ts
```

Expected output: fail before helpers/UI changes, then pass.

### Implementation

- [ ] Modify `web/src/lib/types.ts`.

Add:

```ts
export type OrchestratorTaskOrigin = 'local' | 'remote' | 'pendingRemote';

export type OrchestratorTaskView =
  | { origin: 'local'; task: OrchestratorTask }
  | { origin: 'remote'; task: OrchestratorTask; deviceId: string; deviceName: string }
  | { origin: 'pendingRemote'; item: OrchestratorRemoteOutboxItem };
```

- [ ] Modify `web/src/api/orchestrator.ts`.

Return `OrchestratorTaskView[]` from list/create calls. Add remote-aware wrappers:

- `listProjectTasks(projectId)`
- `createProjectTask(projectId, input)`
- `queueTask(view)`
- `retryTask(view)`
- `abortTask(view)`
- `getTaskEvidence(view)`

- [ ] Create `web/src/lib/orchestratorRemote.ts`.

Pure helpers:

- `taskViewKey(view)`
- `taskViewStatus(view)`
- `taskViewTitle(view)`
- `canQueueTaskView(view)`
- `canRetryTaskView(view)`
- `canAbortTaskView(view)`
- `automationLocationLabel(view, activeProject)`

- [ ] Modify `web/src/pages/Orchestrator/Orchestrator.tsx` and `web/src/pages/Workbench/Workbench.tsx`.

Use `OrchestratorTaskView` in task lists. Keep all hooks before early returns.

- [ ] Modify `web/src/pages/Orchestrator/Orchestrator.module.css` only if needed.

CSS must use existing tokens and avoid nested cards.

- [ ] Update i18n:

Chinese copy:

- `远端设备执行`
- `待发送到远端`
- `远端离线，任务会在设备上线后自动发送`
- `自动化配置在设置中管理`
- `第 {{attempt}} 轮`

English copy mirrors Chinese meaning.

### Verification

```bash
cd web
npx --yes tsx src/lib/orchestratorRemote.test.ts
npx --yes tsx src/pages/Workbench/workbenchAutomationView.test.ts
npx tsc --noEmit
```

Expected output: all pass.

### Commit

```bash
git add web/src/api/orchestrator.ts web/src/lib web/src/pages/Orchestrator web/src/pages/Workbench web/src/i18n/locales
git commit -m "feat: show remote orchestrator task views"
```

## Phase 7: Runner Attempts And Terminal Completion Sentinel

### Tests First

- [ ] Add prompt tests in `src-tauri/src/orchestrator/prompt.rs`.

Cover:

- Initial prompt includes `ORCHESTRATOR_DEV_DONE`.
- Repair prompt includes previous verifier reason and repair prompt.
- Prompt tells Claude to output the sentinel only after code, tests, and evidence are complete.

- [ ] Add completion parser tests in `src-tauri/src/orchestrator/completion.rs`.

Cover:

- Detect sentinel in a single output chunk.
- Detect sentinel split across chunks.
- Ignore sentinel-like text inside older retained buffer once already consumed.

Run:

```bash
cd src-tauri
cargo test orchestrator::prompt --lib
cargo test orchestrator::completion --lib
```

Expected output: tests fail before implementation, then pass.

### Implementation

- [ ] Modify `src-tauri/src/orchestrator/prompt.rs`.

Add:

- `const DEV_DONE_SENTINEL: &str = "ORCHESTRATOR_DEV_DONE";`
- `build_initial_task_prompt(task, worktree_path)`
- `build_repair_task_prompt(task, worktree_path, verifier_review)`

Both prompts must explicitly require final sentinel output.

- [ ] Modify `src-tauri/src/orchestrator/runner.rs`.

Refactor `prepare_visible_runner` into:

- `prepare_initial_runner(state, app_handle, task)`
- `prepare_repair_runner(state, app_handle, task, review)`
- `prepare_runner_attempt(state, app_handle, task, prompt, attempt)`

Behavior:

- Attempt 1 creates the worktree.
- Attempt > 1 reuses `task.worktree_id`.
- Every attempt creates a new Workbench session.
- `orchestrator_tasks.session_id` is updated to the active session.
- `orchestrator_task_attempts` stores the attempt prompt and session id.
- The command writes `claude\n` then prompt text.

- [ ] Create `src-tauri/src/orchestrator/completion.rs`.

Implement a small per-session detector:

```rust
pub struct DevDoneDetector {
    buffer: String,
    consumed: bool,
}
```

Expose:

- `push_output(&mut self, chunk: &str) -> bool`
- `is_consumed(&self) -> bool`

- [ ] Modify `src-tauri/src/workbench/sessions.rs`.

Add a hook after terminal output is appended/broadcast:

```rust
if orchestrator_completion::maybe_handle_session_output(&state, &session_id, &chunk).await? {
    tracing::info!("Orchestrator development sentinel detected for session {session_id}");
}
```

The hook must be non-blocking for terminal streaming. If the completion pipeline needs async work, spawn a task and return quickly.

- [ ] Add `maybe_handle_session_output` in `src-tauri/src/orchestrator/completion.rs`.

It maps `session_id -> task_id`, detects sentinel once, marks attempt completed, and calls the same completion pipeline that manual `complete_orchestrator_agent_run` uses.

### Verification

```bash
cd src-tauri
cargo test orchestrator::prompt --lib
cargo test orchestrator::completion --lib
cargo test orchestrator::runner --lib
cargo check
```

Expected output: all pass.

### Commit

```bash
git add src-tauri/src/orchestrator src-tauri/src/workbench/sessions.rs
git commit -m "feat: detect orchestrator development completion"
```

## Phase 8: Verifier Claude And Automatic Repair Loop

### Tests First

- [ ] Create verifier parser tests in `src-tauri/src/orchestrator/verifier.rs`.

Cover:

- Parse valid pass JSON.
- Parse valid fail JSON with repair prompt.
- Reject fail JSON without repair prompt.
- Reject malformed JSON.
- Strip Markdown fenced code blocks around JSON.

- [ ] Add state-machine tests in `src-tauri/src/orchestrator/state.rs`.

Cover:

- `Verifying + VerificationFailed -> Preparing`.
- `Verifying + VerificationInfraFailed -> Blocked`.
- `Verifying + VerificationPassed -> Delivering`.

Run:

```bash
cd src-tauri
cargo test orchestrator::verifier --lib
cargo test orchestrator::state --lib
```

Expected output: tests fail before implementation, then pass.

### Implementation

- [ ] Create `src-tauri/src/orchestrator/verifier.rs`.

Core types:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifierReview {
    pub passed: bool,
    pub reason: String,
    pub repair_prompt: String,
    pub risk_notes: Vec<String>,
}
```

Functions:

- `build_verifier_prompt(input: VerifierPromptInput) -> String`
- `parse_verifier_review(output: &str) -> Result<VerifierReview, AppError>`
- `run_verifier_claude(state, task, command_outputs, diff) -> Result<VerifierReview, AppError>`

Use the same Claude CLI path/model conventions already used by existing Claude CLI integrations. The verifier runs headless and does not need a Workbench terminal.

- [ ] Modify `src-tauri/src/orchestrator/delivery.rs`.

Split the current manual completion pipeline into:

- `run_validation_commands(state, task) -> Vec<VerificationCommandOutput>`
- `run_verifier_review(state, task, outputs) -> VerifierReview`
- `continue_after_verifier_review(state, app_handle, task, review) -> Result<OrchestratorTaskDto, AppError>`

Flow:

1. Running task receives sentinel.
2. Task transitions to `Verifying`.
3. Validation commands run in the task worktree.
4. `verificationOutput` evidence is written.
5. Verifier Claude runs.
6. `verificationReview` evidence is written.
7. If `passed`, transition to `Delivering` and run full-auto delivery.
8. If not passed, write `repairPrompt` evidence, increment attempt, transition to `Preparing`, and call `prepare_repair_runner`.

- [ ] Modify `src-tauri/src/orchestrator/models.rs`.

Add evidence kind constants:

- `developmentAttempt`
- `verificationOutput`
- `verificationReview`
- `repairPrompt`
- `remoteOutbox`
- `delivery`

Add DTO fields if needed:

- `attempt`
- `activeSessionId`
- `latestVerificationSummary`

- [ ] Modify `src-tauri/src/orchestrator/repo.rs`.

Add atomic helpers:

- `mark_task_verifying_if_running(task_id)`
- `mark_task_preparing_for_repair(task_id, next_attempt, reason)`
- `mark_task_delivering_if_verifying(task_id)`
- `mark_task_blocked_from_verifying(task_id, reason)`

All helpers must respect user Abort. If task is already `Aborted`, return the current task and do not start a new runner.

### Verification

```bash
cd src-tauri
cargo test orchestrator::verifier --lib
cargo test orchestrator::delivery --lib
cargo test orchestrator::state --lib
cargo check
```

Expected output: all pass.

### Commit

```bash
git add src-tauri/src/orchestrator
git commit -m "feat: add claude verification repair loop"
```

## Phase 9: Evidence, Task Details, And Deep Links

### Tests First

- [ ] Update frontend helper tests.

Cover:

- Evidence kind tone/label for `developmentAttempt`, `verificationReview`, `repairPrompt`, and `remoteOutbox`.
- Attempt status copy for running/verifying/repairing.
- Workbench deep link preserves remote project shortcut identifiers.

Run:

```bash
cd web
npx --yes tsx src/lib/orchestrator.test.ts
npx --yes tsx src/pages/Workbench/workbenchAutomationView.test.ts
```

Expected output: tests fail until labels and task view helpers are updated.

### Implementation

- [ ] Modify `web/src/lib/orchestrator.ts`.

Add helpers:

- `orchestratorEvidenceKindLabel(kind, t)`
- `orchestratorEvidenceKindTone(kind)`
- `orchestratorAttemptLabel(task, t)`
- `orchestratorTaskProgressMessage(view, t)`

- [ ] Modify Orchestrator task detail UI.

Display:

- Current attempt number.
- Active terminal/session link.
- Prior attempts from evidence or attempt DTOs.
- Latest verifier result.
- Repair prompt evidence.
- Remote execution device name.

- [ ] Modify Workbench deep link behavior.

Remote task links must use existing remote id prefix rules already used by Workbench. When remote is offline, the UI shows an offline message and does not clear the selected task.

### Verification

```bash
cd web
npx --yes tsx src/lib/orchestrator.test.ts
npx --yes tsx src/pages/Workbench/workbenchAutomationView.test.ts
npx tsc --noEmit
```

Expected output: all pass.

### Commit

```bash
git add web/src/lib/orchestrator.ts web/src/pages/Orchestrator web/src/pages/Workbench web/src/i18n/locales
git commit -m "feat: show orchestrator verification loop evidence"
```

## Phase 10: Documentation And Project Memory

- [ ] Update `docs/prd.md`.

Required PRD changes:

- Settings has a standalone Automation tab.
- Orchestrator project config is not a user-facing concept.
- Remote projects are executed by remote devices.
- Offline remote task creation uses pending outbox.
- Verification loop uses command output plus verifier Claude.
- Failed verification starts a new development Claude in the same worktree.

- [ ] Update `src-tauri/CLAUDE.md`.

Record:

- `AppConfig.orchestrator` is the only automation policy source.
- `orchestrator_project_config` is legacy storage.
- Remote Orchestrator routes are source-of-truth routes on the owning device.
- Focused Rust test commands for config, scheduler, outbox, verifier, and delivery.

- [ ] Update `web/CLAUDE.md`.

Record:

- Settings automation tab files.
- Orchestrator remote task view union.
- Focused frontend test commands.

### Verification

```bash
rg -n "orchestrator_project_config|项目策略|project config" docs/prd.md src-tauri/CLAUDE.md web/CLAUDE.md
```

Expected output: no user-facing doc says automation config is project-level. Mentions are limited to legacy storage notes in `src-tauri/CLAUDE.md`.

### Commit

```bash
git add docs/prd.md src-tauri/CLAUDE.md web/CLAUDE.md
git commit -m "docs: document global orchestrator automation"
```

## Phase 11: End-To-End Verification

### Local Single-Device Path

- [ ] Start dev mode:

```bash
cd web
./node_modules/.bin/tauri dev
```

Expected:

- App starts.
- Settings shows Automation tab.
- Saving max concurrency and verification commands persists after refresh/restart.

- [ ] Create a local Workbench task with a small harmless change.

Expected:

- Task moves `Queued -> Preparing -> Running`.
- Visible Workbench terminal opens.
- Claude prompt contains sentinel instruction.
- When sentinel appears, task moves to `Verifying`.
- Verification commands run.
- Verifier evidence appears.
- Passing review moves task to delivery.

### Local Simulated Fail-Then-Repair Path

- [ ] Use a verifier stub or controlled test mode that returns fail once and pass second.

Expected:

- First verification writes `verificationReview` with `passed=false`.
- Task returns to `Preparing/Running`.
- Attempt increments.
- New terminal session is created.
- Worktree id remains unchanged.
- Second verifier pass enters delivery.

### Remote Online Path

- [ ] Run two cc-partner instances on the same LAN or two dev profiles with distinct ports and device IDs.

Expected:

- Local device can add a remote Workbench project.
- Creating an Orchestrator task on that remote project calls the remote Orchestrator endpoint.
- Remote device owns task queue, runner, verification, and delivery.
- Local UI displays remote task mirror and remote device name.

### Remote Offline Pending Path

- [ ] Make remote device unavailable, then create a task from local remote shortcut.

Expected:

- Local outbox item is created.
- UI shows pending remote task.
- No local scheduler attempts to run it.
- When remote returns online, dispatcher sends the task.
- Pending item becomes mirrored remote task.

### Focused Automated Verification

Run final focused commands:

```bash
cd src-tauri
cargo test orchestrator:: --lib
cargo test commands::orchestrator --lib
cargo test commands::orchestrator_config --lib
cargo test net::routes::orchestrator --lib
cargo check
```

Expected output: all Rust tests and check pass.

```bash
cd web
npx --yes tsx src/pages/Settings/automationSettingsState.test.ts
npx --yes tsx src/lib/orchestrator.test.ts
npx --yes tsx src/lib/orchestratorRemote.test.ts
npx --yes tsx src/pages/Workbench/workbenchAutomationView.test.ts
npx tsc --noEmit
```

Expected output: all frontend tests and typecheck pass.

## Risk Controls

- Remote execution ownership: every scheduler query must filter local Workbench projects only.
- Outbox idempotency: create remote task request must carry local outbox id as idempotency key, or the dispatcher must mark `sending` with a single active claim and only retry network failures.
- Abort race: sentinel handling, verifier result handling, and repair runner creation must re-read task status before side effects.
- Terminal streaming: sentinel detection must not block PTY output broadcast.
- Verifier parsing: malformed verifier output is an infrastructure failure and blocks the task; verification command failure is normal task feedback and goes to verifier.
- Delivery gates: disabled auto-delivery flags must produce explicit evidence and Blocked status instead of silently skipping Git steps.
- Config scope: frontend must not show per-project automation editors; Workbench only links to Settings automation tab.

## Definition Of Done

- Settings has a standalone Automation tab and saves global config.
- No user-facing Orchestrator path depends on project-level automation config.
- Local scheduler respects global enabled and global concurrency.
- Remote project tasks are created, listed, aborted, retried, and inspected through remote owning device routes.
- Offline remote task creation creates pending outbox items and mirrors remote tasks after successful dispatch.
- Development sessions auto-complete through sentinel detection.
- Validation commands and verifier Claude run automatically after development completion.
- Failed verifier review starts a new Claude development session in the same worktree.
- Evidence records development attempts, validation output, verifier reviews, repair prompts, remote outbox transitions, and delivery.
- Focused Rust and frontend commands listed in Phase 11 pass.
- `docs/prd.md`, `src-tauri/CLAUDE.md`, and `web/CLAUDE.md` describe the final behavior and focused test commands.
