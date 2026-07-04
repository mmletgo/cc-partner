# Orchestrator 自动编排器 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a standalone Orchestrator page that manages cc-partner internal tasks and automatically runs Claude Code through visible Workbench tmux terminal windows, then verifies, commits, pushes, merges, and pushes main according to project-scoped policy.

**Architecture:** Add a new Rust `orchestrator` domain with SQLite-backed task/config/event/evidence storage, a project-scoped scheduler, a Workbench-backed visible terminal runner, and a delivery pipeline. Add a new React Orchestrator page that acts as the global task dashboard and deep-links into Workbench for task takeover.

**Tech Stack:** Rust/Tauri 2, sqlx SQLite runtime queries, tokio async tasks, existing Workbench worktree/session/Git helpers, React 19, TypeScript, React Router v6, CSS Modules, i18next.

---

## Scope Check

This feature has four dependent subsystems, not four independent products:

1. Task/config persistence and CRUD.
2. Orchestrator dashboard UI.
3. Visible tmux Runner using existing Workbench worktree/session APIs.
4. Verification and full auto-delivery.

They should be implemented in sequence. Each phase produces working, testable software and can be reviewed independently.

## File Map

### Rust Backend

- Create `src-tauri/src/orchestrator/mod.rs`: module exports and scheduler start/shutdown entry points.
- Create `src-tauri/src/orchestrator/models.rs`: task, config, event, evidence DTOs and enums.
- Create `src-tauri/src/orchestrator/repo.rs`: SQLite schemas and CRUD repository.
- Create `src-tauri/src/orchestrator/state.rs`: pure task state transition helpers.
- Create `src-tauri/src/orchestrator/prompt.rs`: Claude Code task prompt generation.
- Create `src-tauri/src/orchestrator/scheduler.rs`: project-scoped queue dispatch and retry loop.
- Create `src-tauri/src/orchestrator/runner.rs`: worktree/session preparation and terminal prompt injection.
- Create `src-tauri/src/orchestrator/delivery.rs`: verification and delivery pipeline.
- Create `src-tauri/src/commands/orchestrator.rs`: Tauri command boundary.
- Modify `src-tauri/src/lib.rs`: initialize schemas, repos, scheduler runtime, and command registrations.
- Modify `src-tauri/src/state.rs`: add orchestrator repo/runtime fields.
- Modify `src-tauri/src/workbench/mod.rs` or helper modules only when a private helper must be exposed to orchestrator; do not duplicate worktree/session logic.

### Frontend

- Create `web/src/api/orchestrator.ts`: invoke wrapper.
- Create `web/src/lib/orchestrator.ts`: task grouping, action availability, status tone helpers.
- Create `web/src/lib/orchestrator.test.ts`: helper regression tests.
- Create `web/src/pages/Orchestrator/Orchestrator.tsx`: page shell and data flow.
- Create `web/src/pages/Orchestrator/Orchestrator.module.css`: token-based layout.
- Create `web/src/pages/Orchestrator/orchestratorState.test.ts`: UI state helper tests.
- Create `web/src/pages/Orchestrator/index.ts`: page export.
- Modify `web/src/App.tsx`: route `/orchestrator`.
- Modify `web/src/components/layout/AppShell/AppShell.tsx`: sidebar nav item.
- Modify `web/src/lib/icons.tsx`: add `OrchestratorIcon`.
- Modify `web/src/lib/types.ts`: orchestrator DTO types.
- Modify `web/src/i18n/locales/zh/nav.json` and `web/src/i18n/locales/en/nav.json`: nav label.
- Create `web/src/i18n/locales/zh/orchestrator.json` and `web/src/i18n/locales/en/orchestrator.json`: page copy.
- Modify `web/src/i18n/index.ts`: include orchestrator namespace.
- Modify `web/src/pages/Workbench/Workbench.tsx`: parse `projectId`, `worktreeId`, `sessionId` query params and select the requested context.
- Modify `web/CLAUDE.md`: record Orchestrator test commands and routing constraints.
- Modify root `AGENTS.md`: add the top-level Orchestrator page and backend domain to the directory map only if the implementation adds those files.

## Implementation Tasks

### Task 0: Create Isolated Development Worktree

**Files:**
- No source files changed in this task.

- [ ] **Step 1: Create feature worktree**

Run from repo root:

```bash
git status --short
git worktree add ../cc-partner-orchestrator -b codex/orchestrator
cd ../cc-partner-orchestrator
```

Expected: new worktree on branch `codex/orchestrator`. If `git status --short` shows user changes in the original worktree, keep them in the original worktree and do not merge back until explicitly instructed.

- [ ] **Step 2: Confirm project memory**

Run:

```bash
cat AGENTS.md
cat web/CLAUDE.md
cat src-tauri/CLAUDE.md
```

Expected: instructions load successfully. Keep React hooks before early returns and keep colors/spacing in CSS modules token-based.

### Task 1: Backend Models, State Machine, Repo Schema

**Files:**
- Create: `src-tauri/src/orchestrator/mod.rs`
- Create: `src-tauri/src/orchestrator/models.rs`
- Create: `src-tauri/src/orchestrator/state.rs`
- Create: `src-tauri/src/orchestrator/repo.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/state.rs`

- [ ] **Step 1: Write failing state-machine tests**

Create `src-tauri/src/orchestrator/state.rs` with tests first:

```rust
//! Orchestrator task state transitions.

use crate::orchestrator::models::{OrchestratorTaskStatus, TaskStageOutcome};

pub fn next_status(
    current: OrchestratorTaskStatus,
    outcome: TaskStageOutcome,
) -> OrchestratorTaskStatus {
    match (current, outcome) {
        (OrchestratorTaskStatus::Draft, TaskStageOutcome::Queue) => OrchestratorTaskStatus::Queued,
        (OrchestratorTaskStatus::Queued, TaskStageOutcome::StartPreparing) => OrchestratorTaskStatus::Preparing,
        (OrchestratorTaskStatus::Preparing, TaskStageOutcome::RunnerReady) => OrchestratorTaskStatus::Running,
        (OrchestratorTaskStatus::Running, TaskStageOutcome::AgentFinished) => OrchestratorTaskStatus::Verifying,
        (OrchestratorTaskStatus::Verifying, TaskStageOutcome::VerificationPassed) => OrchestratorTaskStatus::Delivering,
        (OrchestratorTaskStatus::Delivering, TaskStageOutcome::DeliveryPassed) => OrchestratorTaskStatus::Done,
        (_, TaskStageOutcome::Block) => OrchestratorTaskStatus::Blocked,
        (_, TaskStageOutcome::Abort) => OrchestratorTaskStatus::Aborted,
        (status, TaskStageOutcome::Noop) => status,
        (status, _) => status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_reaches_done() {
        let status = next_status(OrchestratorTaskStatus::Draft, TaskStageOutcome::Queue);
        assert_eq!(status, OrchestratorTaskStatus::Queued);
        let status = next_status(status, TaskStageOutcome::StartPreparing);
        assert_eq!(status, OrchestratorTaskStatus::Preparing);
        let status = next_status(status, TaskStageOutcome::RunnerReady);
        assert_eq!(status, OrchestratorTaskStatus::Running);
        let status = next_status(status, TaskStageOutcome::AgentFinished);
        assert_eq!(status, OrchestratorTaskStatus::Verifying);
        let status = next_status(status, TaskStageOutcome::VerificationPassed);
        assert_eq!(status, OrchestratorTaskStatus::Delivering);
        let status = next_status(status, TaskStageOutcome::DeliveryPassed);
        assert_eq!(status, OrchestratorTaskStatus::Done);
    }

    #[test]
    fn any_status_can_block() {
        assert_eq!(
            next_status(OrchestratorTaskStatus::Running, TaskStageOutcome::Block),
            OrchestratorTaskStatus::Blocked
        );
        assert_eq!(
            next_status(OrchestratorTaskStatus::Delivering, TaskStageOutcome::Block),
            OrchestratorTaskStatus::Blocked
        );
    }
}
```

- [ ] **Step 2: Create models used by the tests**

Create `src-tauri/src/orchestrator/models.rs`:

```rust
//! Orchestrator DTOs and row models.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrchestratorTaskStatus {
    Draft,
    Queued,
    Preparing,
    Running,
    Verifying,
    Delivering,
    Done,
    Blocked,
    Aborted,
}

impl OrchestratorTaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Queued => "queued",
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Verifying => "verifying",
            Self::Delivering => "delivering",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Aborted => "aborted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStageOutcome {
    Queue,
    StartPreparing,
    RunnerReady,
    AgentFinished,
    VerificationPassed,
    DeliveryPassed,
    Block,
    Abort,
    Noop,
}

#[derive(Debug, Clone)]
pub struct OrchestratorTaskRow {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
    pub status: OrchestratorTaskStatus,
    pub priority: i64,
    pub branch_name: Option<String>,
    pub worktree_id: Option<String>,
    pub session_id: Option<String>,
    pub blocked_reason: Option<String>,
    pub attempt: i64,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorTaskDto {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
    pub status: OrchestratorTaskStatus,
    pub priority: i64,
    pub branch_name: Option<String>,
    pub worktree_id: Option<String>,
    pub session_id: Option<String>,
    pub blocked_reason: Option<String>,
    pub attempt: i64,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}
```

- [ ] **Step 3: Register module and run failing test**

Create `src-tauri/src/orchestrator/mod.rs`:

```rust
pub mod models;
pub mod repo;
pub mod state;
```

Modify `src-tauri/src/lib.rs` near module declarations:

```rust
pub mod orchestrator;
```

Run:

```bash
cd src-tauri
cargo test orchestrator::state --lib
```

Expected: state tests pass after models exist.

- [ ] **Step 4: Add repository schema and CRUD tests**

Create `src-tauri/src/orchestrator/repo.rs` with schemas:

```rust
pub const ORCHESTRATOR_TASK_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS orchestrator_tasks (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  title TEXT NOT NULL,
  goal TEXT NOT NULL,
  acceptance_criteria TEXT NOT NULL,
  status TEXT NOT NULL,
  priority INTEGER NOT NULL DEFAULT 0,
  branch_name TEXT,
  worktree_id TEXT,
  session_id TEXT,
  blocked_reason TEXT,
  attempt INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  started_at TEXT,
  finished_at TEXT
)"#;

pub const ORCHESTRATOR_PROJECT_CONFIG_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS orchestrator_project_config (
  project_id TEXT PRIMARY KEY,
  enabled INTEGER NOT NULL DEFAULT 0,
  max_concurrent_tasks INTEGER NOT NULL DEFAULT 1,
  branch_prefix TEXT NOT NULL DEFAULT 'agent',
  verification_commands_json TEXT NOT NULL DEFAULT '[]',
  auto_commit INTEGER NOT NULL DEFAULT 1,
  auto_push_task_branch INTEGER NOT NULL DEFAULT 1,
  auto_merge_to_main INTEGER NOT NULL DEFAULT 1,
  auto_push_main INTEGER NOT NULL DEFAULT 1,
  retry_limit INTEGER NOT NULL DEFAULT 0,
  retain_worktree_on_done INTEGER NOT NULL DEFAULT 0,
  retain_worktree_on_blocked INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
)"#;

pub const ORCHESTRATOR_EVENT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS orchestrator_task_events (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  message TEXT NOT NULL,
  payload_json TEXT,
  created_at TEXT NOT NULL
)"#;

pub const ORCHESTRATOR_EVIDENCE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS orchestrator_task_evidence (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  title TEXT NOT NULL,
  summary TEXT NOT NULL,
  content TEXT NOT NULL,
  created_at TEXT NOT NULL
)"#;
```

Add `OrchestratorRepo` with `create_task`, `list_tasks`, `get_task`, `update_task_status`. Use the same runtime `sqlx::query` style as `storage/workbench_project_repo.rs`.

Add tests in the same file:

```rust
#[tokio::test]
async fn create_and_list_tasks_by_project() {
    let repo = setup_repo().await;
    let task = task_row("task-1", "project-1", "queued");
    repo.create_task(&task).await.unwrap();

    let listed = repo.list_tasks(Some("project-1")).await.unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "task-1");
    assert_eq!(listed[0].status, OrchestratorTaskStatus::Queued);
}

#[tokio::test]
async fn update_task_status_persists_blocked_reason() {
    let repo = setup_repo().await;
    repo.create_task(&task_row("task-1", "project-1", "running")).await.unwrap();

    repo.update_task_status(
        "task-1",
        OrchestratorTaskStatus::Blocked,
        Some("verification failed"),
    )
    .await
    .unwrap();

    let saved = repo.get_task("task-1").await.unwrap().unwrap();
    assert_eq!(saved.status, OrchestratorTaskStatus::Blocked);
    assert_eq!(saved.blocked_reason.as_deref(), Some("verification failed"));
}
```

- [ ] **Step 5: Wire schema and repo into app state**

Modify `src-tauri/src/lib.rs` `init_db`:

```rust
sqlx::query(orchestrator::repo::ORCHESTRATOR_TASK_SCHEMA)
    .execute(&pool)
    .await?;
sqlx::query(orchestrator::repo::ORCHESTRATOR_PROJECT_CONFIG_SCHEMA)
    .execute(&pool)
    .await?;
sqlx::query(orchestrator::repo::ORCHESTRATOR_EVENT_SCHEMA)
    .execute(&pool)
    .await?;
sqlx::query(orchestrator::repo::ORCHESTRATOR_EVIDENCE_SCHEMA)
    .execute(&pool)
    .await?;
```

Modify `src-tauri/src/state.rs`:

```rust
pub orchestrator_repo: Arc<crate::orchestrator::repo::OrchestratorRepo>,
```

Modify AppState construction in `lib.rs` to initialize:

```rust
orchestrator_repo: Arc::new(orchestrator::repo::OrchestratorRepo::new(db.clone())),
```

- [ ] **Step 6: Verify backend foundation**

Run:

```bash
cd src-tauri
cargo test orchestrator:: --lib
cargo check
```

Expected: orchestrator tests pass and `cargo check` completes.

- [ ] **Step 7: Commit backend foundation**

```bash
git add src-tauri/src/lib.rs src-tauri/src/state.rs src-tauri/src/orchestrator
git commit -m "feat: add orchestrator task storage"
```

### Task 2: Tauri Commands and Frontend API Types

**Files:**
- Create: `src-tauri/src/commands/orchestrator.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Create: `web/src/api/orchestrator.ts`
- Modify: `web/src/lib/types.ts`

- [ ] **Step 1: Add command DTOs and commands**

Create `src-tauri/src/commands/orchestrator.rs`:

```rust
use crate::error::AppError;
use crate::orchestrator::models::{OrchestratorTaskDto, OrchestratorTaskRow, OrchestratorTaskStatus};
use crate::state::AppState;
use chrono::Utc;
use tauri::State;
use uuid::Uuid;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrchestratorTaskRequest {
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
    pub priority: Option<i64>,
}

#[tauri::command]
pub async fn list_orchestrator_tasks(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<OrchestratorTaskDto>, AppError> {
    let rows = state.orchestrator_repo.list_tasks(project_id.as_deref()).await?;
    Ok(rows.into_iter().map(OrchestratorTaskDto::from).collect())
}

#[tauri::command]
pub async fn create_orchestrator_task(
    state: State<'_, AppState>,
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskDto, AppError> {
    if request.project_id.trim().is_empty() {
        return Err(AppError::generic("项目不能为空"));
    }
    if request.title.trim().is_empty() {
        return Err(AppError::generic("任务标题不能为空"));
    }
    if request.goal.trim().is_empty() {
        return Err(AppError::generic("任务目标不能为空"));
    }
    let now = Utc::now().to_rfc3339();
    let row = OrchestratorTaskRow {
        id: Uuid::new_v4().to_string(),
        project_id: request.project_id,
        title: request.title.trim().to_string(),
        goal: request.goal.trim().to_string(),
        acceptance_criteria: request.acceptance_criteria.trim().to_string(),
        status: OrchestratorTaskStatus::Draft,
        priority: request.priority.unwrap_or(0),
        branch_name: None,
        worktree_id: None,
        session_id: None,
        blocked_reason: None,
        attempt: 0,
        created_at: now.clone(),
        updated_at: now,
        started_at: None,
        finished_at: None,
    };
    state.orchestrator_repo.create_task(&row).await?;
    Ok(OrchestratorTaskDto::from(row))
}
```

Implement `From<OrchestratorTaskRow> for OrchestratorTaskDto` in `models.rs`.

- [ ] **Step 2: Register commands**

Modify `src-tauri/src/commands/mod.rs`:

```rust
pub mod orchestrator;
```

Modify `src-tauri/src/lib.rs` `invoke_handler!`:

```rust
commands::orchestrator::list_orchestrator_tasks,
commands::orchestrator::create_orchestrator_task,
```

- [ ] **Step 3: Add TypeScript DTOs**

Modify `web/src/lib/types.ts`:

```ts
export type OrchestratorTaskStatus =
  | 'draft'
  | 'queued'
  | 'preparing'
  | 'running'
  | 'verifying'
  | 'delivering'
  | 'done'
  | 'blocked'
  | 'aborted';

export interface OrchestratorTask {
  id: string;
  projectId: string;
  title: string;
  goal: string;
  acceptanceCriteria: string;
  status: OrchestratorTaskStatus;
  priority: number;
  branchName: string | null;
  worktreeId: string | null;
  sessionId: string | null;
  blockedReason: string | null;
  attempt: number;
  createdAt: string;
  updatedAt: string;
  startedAt: string | null;
  finishedAt: string | null;
}
```

- [ ] **Step 4: Add frontend API wrapper**

Create `web/src/api/orchestrator.ts`:

```ts
import { invoke } from './client';
import type { OrchestratorTask } from '@/lib/types';

export interface CreateOrchestratorTaskRequest {
  projectId: string;
  title: string;
  goal: string;
  acceptanceCriteria: string;
  priority?: number;
}

export const orchestratorApi = {
  listTasks: (projectId?: string | null) =>
    invoke<OrchestratorTask[]>('list_orchestrator_tasks', {
      projectId: projectId ?? null,
    }),
  createTask: (request: CreateOrchestratorTaskRequest) =>
    invoke<OrchestratorTask>('create_orchestrator_task', { request }),
};
```

- [ ] **Step 5: Verify API layer**

Run:

```bash
cd src-tauri
cargo check
cd ../web
npx tsc --noEmit
```

Expected: no Rust or TypeScript errors.

- [ ] **Step 6: Commit command/API layer**

```bash
git add src-tauri/src/commands src-tauri/src/lib.rs web/src/api/orchestrator.ts web/src/lib/types.ts
git commit -m "feat: expose orchestrator task api"
```

### Task 3: Frontend Orchestrator Page Shell

**Files:**
- Create: `web/src/lib/orchestrator.ts`
- Create: `web/src/lib/orchestrator.test.ts`
- Create: `web/src/pages/Orchestrator/Orchestrator.tsx`
- Create: `web/src/pages/Orchestrator/Orchestrator.module.css`
- Create: `web/src/pages/Orchestrator/orchestratorState.test.ts`
- Create: `web/src/pages/Orchestrator/index.ts`
- Modify: `web/src/App.tsx`
- Modify: `web/src/components/layout/AppShell/AppShell.tsx`
- Modify: `web/src/lib/icons.tsx`
- Modify: `web/src/i18n/index.ts`
- Modify: `web/src/i18n/locales/zh/nav.json`
- Modify: `web/src/i18n/locales/en/nav.json`
- Create: `web/src/i18n/locales/zh/orchestrator.json`
- Create: `web/src/i18n/locales/en/orchestrator.json`

- [ ] **Step 1: Write helper tests**

Create `web/src/lib/orchestrator.test.ts`:

```ts
import type { OrchestratorTask } from './types';
import { groupOrchestratorTasks, orchestratorStatusTone } from './orchestrator';

const baseTask: OrchestratorTask = {
  id: 'task-1',
  projectId: 'project-1',
  title: 'Fix screenshot overlay',
  goal: 'Fix the overlay toolbar timing',
  acceptanceCriteria: 'Screenshot includes annotation toolbar result',
  status: 'queued',
  priority: 0,
  branchName: null,
  worktreeId: null,
  sessionId: null,
  blockedReason: null,
  attempt: 0,
  createdAt: '2026-07-05T00:00:00Z',
  updatedAt: '2026-07-05T00:00:00Z',
  startedAt: null,
  finishedAt: null,
};

function testGroupOrchestratorTasks(): void {
  const groups = groupOrchestratorTasks([
    baseTask,
    Object.assign({}, baseTask, { id: 'task-2', status: 'blocked' as const }),
  ]);
  if (groups.queued.length !== 1) throw new Error('expected queued group');
  if (groups.blocked.length !== 1) throw new Error('expected blocked group');
}

function testOrchestratorStatusTone(): void {
  if (orchestratorStatusTone('done') !== 'success') throw new Error('done should be success');
  if (orchestratorStatusTone('blocked') !== 'danger') throw new Error('blocked should be danger');
  if (orchestratorStatusTone('running') !== 'info') throw new Error('running should be info');
}

testGroupOrchestratorTasks();
testOrchestratorStatusTone();
console.log('orchestrator helper tests passed');
```

- [ ] **Step 2: Implement helper**

Create `web/src/lib/orchestrator.ts`:

```ts
import type { OrchestratorTask, OrchestratorTaskStatus } from './types';

export type OrchestratorStatusTone = 'neutral' | 'info' | 'warning' | 'success' | 'danger';

export type OrchestratorTaskGroups = Record<OrchestratorTaskStatus, OrchestratorTask[]>;

export const ORCHESTRATOR_STATUSES: OrchestratorTaskStatus[] = [
  'draft',
  'queued',
  'preparing',
  'running',
  'verifying',
  'delivering',
  'done',
  'blocked',
  'aborted',
];

export function groupOrchestratorTasks(tasks: OrchestratorTask[]): OrchestratorTaskGroups {
  const groups = Object.fromEntries(
    ORCHESTRATOR_STATUSES.map((status) => [status, [] as OrchestratorTask[]]),
  ) as OrchestratorTaskGroups;
  for (const task of tasks) {
    groups[task.status].push(task);
  }
  return groups;
}

export function orchestratorStatusTone(status: OrchestratorTaskStatus): OrchestratorStatusTone {
  switch (status) {
    case 'running':
    case 'preparing':
    case 'verifying':
    case 'delivering':
      return 'info';
    case 'queued':
    case 'draft':
      return 'neutral';
    case 'done':
      return 'success';
    case 'blocked':
    case 'aborted':
      return 'danger';
  }
}
```

- [ ] **Step 3: Create page skeleton**

Create `web/src/pages/Orchestrator/Orchestrator.tsx`:

```tsx
import { useEffect, useMemo, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { orchestratorApi } from '@/api/orchestrator';
import { Button, Card, Input, Pill } from '@/components/primitives';
import { useWorkbenchProjects } from '@/hooks/useWorkbenchProjects';
import type { OrchestratorTask } from '@/lib/types';
import { groupOrchestratorTasks, ORCHESTRATOR_STATUSES, orchestratorStatusTone } from '@/lib/orchestrator';
import styles from './Orchestrator.module.css';

/**
 * Business Logic（为什么需要这个组件）:
 *   用户需要一个全局控制面查看所有项目的自动编排任务，并能为项目创建内置任务队列任务。
 *
 * Code Logic（这个组件做什么）:
 *   加载项目与任务列表，按状态分组展示任务；右侧显示选中任务详情和交付证据占位。
 */
export function Orchestrator(): ReactElement {
  const { t } = useTranslation(['orchestrator', 'common']);
  const { projects, activeProject } = useWorkbenchProjects();
  const [tasks, setTasks] = useState<OrchestratorTask[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [title, setTitle] = useState('');
  const [goal, setGoal] = useState('');
  const [acceptanceCriteria, setAcceptanceCriteria] = useState('');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const groups = useMemo(() => groupOrchestratorTasks(tasks), [tasks]);
  const selectedTask = useMemo(
    () => tasks.find((task) => task.id === selectedTaskId) ?? tasks[0] ?? null,
    [selectedTaskId, tasks],
  );

  useEffect(() => {
    let cancelled = false;
    void orchestratorApi
      .listTasks()
      .then((items) => {
        if (cancelled) return;
        setTasks(items);
        setSelectedTaskId((current) => current ?? items[0]?.id ?? null);
      })
      .catch((reason: unknown) => {
        if (cancelled) return;
        setError(reason instanceof Error ? reason.message : t('orchestrator:errors.loadTasks'));
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  const createTask = async () => {
    if (!activeProject || loading) return;
    setLoading(true);
    setError(null);
    try {
      const task = await orchestratorApi.createTask({
        projectId: activeProject.id,
        title,
        goal,
        acceptanceCriteria,
      });
      setTasks((current) => [task].concat(current));
      setSelectedTaskId(task.id);
      setTitle('');
      setGoal('');
      setAcceptanceCriteria('');
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : t('orchestrator:errors.createTask'));
    } finally {
      setLoading(false);
    }
  };

  return (
    <section className={styles.page}>
      <header className={styles.header}>
        <div>
          <h1>{t('orchestrator:title')}</h1>
          <p>{t('orchestrator:subtitle')}</p>
        </div>
        <Pill tone={activeProject ? 'success' : 'warning'}>{activeProject?.name ?? t('orchestrator:noProject')}</Pill>
      </header>
      {error ? <div className={styles.error}>{error}</div> : null}
      <div className={styles.grid}>
        <aside className={styles.queue}>
          {ORCHESTRATOR_STATUSES.map((status) => (
            <section key={status} className={styles.group}>
              <h2>{t(`orchestrator:status.${status}`)}</h2>
              {groups[status].map((task) => (
                <button
                  key={task.id}
                  className={task.id === selectedTask?.id ? styles.taskActive : styles.task}
                  type="button"
                  onClick={() => setSelectedTaskId(task.id)}
                >
                  <span>{task.title}</span>
                  <Pill tone={orchestratorStatusTone(task.status)}>{t(`orchestrator:status.${task.status}`)}</Pill>
                </button>
              ))}
            </section>
          ))}
        </aside>
        <main className={styles.detail}>
          <Card>
            <Card.Header>
              <h2>{selectedTask?.title ?? t('orchestrator:emptyTitle')}</h2>
            </Card.Header>
            <Card.Body>
              <p>{selectedTask?.goal ?? t('orchestrator:emptyBody')}</p>
            </Card.Body>
          </Card>
          <Card>
            <Card.Header>
              <h2>{t('orchestrator:create.title')}</h2>
            </Card.Header>
            <Card.Body>
              <Input value={title} onChange={(event) => setTitle(event.target.value)} placeholder={t('orchestrator:create.titlePlaceholder')} />
              <Input value={goal} onChange={(event) => setGoal(event.target.value)} placeholder={t('orchestrator:create.goalPlaceholder')} />
              <Input value={acceptanceCriteria} onChange={(event) => setAcceptanceCriteria(event.target.value)} placeholder={t('orchestrator:create.acceptancePlaceholder')} />
            </Card.Body>
            <Card.Footer>
              <Button variant="primary" loading={loading} disabled={!activeProject || !title.trim() || !goal.trim()} onClick={createTask}>
                {t('orchestrator:create.submit')}
              </Button>
            </Card.Footer>
          </Card>
        </main>
        <aside className={styles.evidence}>
          <Card>
            <Card.Header>
              <h2>{t('orchestrator:evidence.title')}</h2>
            </Card.Header>
            <Card.Body>
              <p>{t('orchestrator:evidence.empty')}</p>
            </Card.Body>
          </Card>
        </aside>
      </div>
    </section>
  );
}
```

- [ ] **Step 4: Add token-based CSS**

Create `web/src/pages/Orchestrator/Orchestrator.module.css` using only design tokens:

```css
.page {
  display: flex;
  flex-direction: column;
  gap: var(--space-5);
  min-height: 100%;
  padding: var(--space-6);
  color: var(--fg);
  background: var(--bg);
}

.header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--space-4);
}

.header h1 {
  margin: 0;
  font-size: var(--text-2xl);
  font-weight: var(--weight-semibold);
}

.header p {
  margin: var(--space-2) 0 0;
  color: var(--muted-fg);
}

.grid {
  display: grid;
  grid-template-columns: minmax(220px, 280px) minmax(420px, 1fr) minmax(240px, 320px);
  gap: var(--space-4);
  align-items: start;
}

.queue,
.detail,
.evidence {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
}

.group {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}

.group h2 {
  margin: 0;
  font-size: var(--text-xs);
  font-weight: var(--weight-semibold);
  color: var(--muted-fg);
  text-transform: uppercase;
}

.task,
.taskActive {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-2);
  width: 100%;
  min-height: 40px;
  padding: var(--space-2) var(--space-3);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  color: var(--fg);
  background: var(--surface);
  text-align: left;
  cursor: pointer;
  transition: all var(--motion-fast) var(--ease-standard);
}

.task:hover,
.taskActive {
  border-color: var(--accent);
  background: var(--surface-elevated);
}

.task span,
.taskActive span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.error {
  padding: var(--space-3);
  border: 1px solid var(--danger);
  border-radius: var(--radius-md);
  color: var(--danger);
  background: var(--danger-bg);
}

@media (max-width: 1100px) {
  .grid {
    grid-template-columns: 1fr;
  }
}
```

- [ ] **Step 5: Add route, nav, icon, i18n**

Create `web/src/pages/Orchestrator/index.ts`:

```ts
export { Orchestrator } from './Orchestrator';
```

Modify `web/src/App.tsx`:

```tsx
import { Orchestrator } from './pages/Orchestrator';

// Add inside the existing <Routes> tree:
<Route path="/orchestrator" element={<Orchestrator />} />;
```

Modify `web/src/lib/icons.tsx`:

```tsx
export const OrchestratorIcon = ({ size = 16 }: IconProps) => (
  <svg
    width={size}
    height={size}
    viewBox="0 0 16 16"
    fill="none"
    stroke="currentColor"
    strokeWidth={1.6}
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
    focusable="false"
  >
    <path d="M3 4h4v4H3zM9 2.5h4v4H9zM9 9.5h4v4H9z" />
    <path d="M7 6h2M7 6v5h2" />
  </svg>
);
```

Modify `web/src/components/layout/AppShell/AppShell.tsx` to import `OrchestratorIcon` and add:

```tsx
<NavItem to="/orchestrator" label={t('nav:orchestrator')} icon={<OrchestratorIcon />} />
```

Place it near Workbench-adjacent tools, before Settings.

- [ ] **Step 6: Add i18n files**

Add `orchestrator` key to nav locale files:

```json
"orchestrator": "自动化"
```

and English:

```json
"orchestrator": "Automation"
```

Create page locale files with keys used in the component: `title`, `subtitle`, `noProject`, `emptyTitle`, `emptyBody`, `status.*`, `create.*`, `evidence.*`, `errors.*`.

- [ ] **Step 7: Verify frontend shell**

Run:

```bash
cd web
npx --yes tsx src/lib/orchestrator.test.ts
npx tsc --noEmit
```

Expected: helper tests pass and TypeScript compiles.

- [ ] **Step 8: Commit frontend shell**

```bash
git add web/src/api/orchestrator.ts web/src/lib web/src/pages/Orchestrator web/src/App.tsx web/src/components/layout/AppShell web/src/i18n web/src/lib/icons.tsx
git commit -m "feat: add orchestrator dashboard shell"
```

### Task 4: Project Config and Queue Actions

**Files:**
- Modify: `src-tauri/src/orchestrator/models.rs`
- Modify: `src-tauri/src/orchestrator/repo.rs`
- Modify: `src-tauri/src/commands/orchestrator.rs`
- Modify: `web/src/api/orchestrator.ts`
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/lib/orchestrator.ts`
- Modify: `web/src/pages/Orchestrator/Orchestrator.tsx`
- Modify: `web/src/i18n/locales/{zh,en}/orchestrator.json`

- [ ] **Step 1: Add config model and repo tests**

Add Rust DTO:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorProjectConfigDto {
    pub project_id: String,
    pub enabled: bool,
    pub max_concurrent_tasks: i64,
    pub branch_prefix: String,
    pub verification_commands: Vec<String>,
    pub auto_commit: bool,
    pub auto_push_task_branch: bool,
    pub auto_merge_to_main: bool,
    pub auto_push_main: bool,
    pub retry_limit: i64,
    pub retain_worktree_on_done: bool,
    pub retain_worktree_on_blocked: bool,
    pub created_at: String,
    pub updated_at: String,
}
```

Add repo tests:

```rust
#[tokio::test]
async fn config_defaults_to_full_auto_but_disabled() {
    let repo = setup_repo().await;
    let config = repo.get_or_create_project_config("project-1").await.unwrap();
    assert!(!config.enabled);
    assert_eq!(config.max_concurrent_tasks, 1);
    assert!(config.auto_commit);
    assert!(config.auto_push_task_branch);
    assert!(config.auto_merge_to_main);
    assert!(config.auto_push_main);
    assert!(config.retain_worktree_on_blocked);
}
```

- [ ] **Step 2: Add queue command**

Add command:

```rust
#[tauri::command]
pub async fn queue_orchestrator_task(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<OrchestratorTaskDto, AppError> {
    let task = state
        .orchestrator_repo
        .set_task_status(&task_id, OrchestratorTaskStatus::Queued, None)
        .await?;
    Ok(OrchestratorTaskDto::from(task))
}
```

Register it in `lib.rs`.

- [ ] **Step 3: Add frontend queue API and action helper**

Add API:

```ts
queueTask: (taskId: string) =>
  invoke<OrchestratorTask>('queue_orchestrator_task', { taskId }),
```

Add helper:

```ts
export function canQueueOrchestratorTask(task: OrchestratorTask | null): boolean {
  return task?.status === 'draft';
}
```

Add test:

```ts
if (!canQueueOrchestratorTask(Object.assign({}, baseTask, { status: 'draft' as const }))) {
  throw new Error('draft task should be queueable');
}
if (canQueueOrchestratorTask(Object.assign({}, baseTask, { status: 'running' as const }))) {
  throw new Error('running task should not be queueable');
}
```

- [ ] **Step 4: Show project config panel and queue button**

In `Orchestrator.tsx`, add a queue button for selected draft tasks and a right-side policy card that shows enabled, max concurrency, verification commands, and all full-auto switches.

- [ ] **Step 5: Verify task queue behavior**

Run:

```bash
cd src-tauri
cargo test orchestrator::repo --lib
cargo check
cd ../web
npx --yes tsx src/lib/orchestrator.test.ts
npx tsc --noEmit
```

Expected: repo/config tests pass, queue command compiles, frontend helper tests pass.

- [ ] **Step 6: Commit queue/config**

```bash
git add src-tauri/src/orchestrator src-tauri/src/commands/orchestrator.rs src-tauri/src/lib.rs web/src
git commit -m "feat: add orchestrator project policy"
```

### Task 5: Scheduler and Visible Workbench Runner

**Files:**
- Create: `src-tauri/src/orchestrator/scheduler.rs`
- Create: `src-tauri/src/orchestrator/runner.rs`
- Create: `src-tauri/src/orchestrator/prompt.rs`
- Modify: `src-tauri/src/orchestrator/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/commands/orchestrator.rs`

- [ ] **Step 1: Add prompt tests**

Create `src-tauri/src/orchestrator/prompt.rs`:

```rust
use crate::orchestrator::models::OrchestratorTaskRow;

pub fn build_task_prompt(task: &OrchestratorTaskRow, project_path: &str) -> String {
    format!(
        "请在当前项目中完成 Orchestrator 任务。\\n\\n任务标题：{}\\n\\n任务目标：\\n{}\\n\\n验收标准：\\n{}\\n\\n项目路径：{}\\n\\n要求：遵守项目 AGENTS.md/CLAUDE.md；完成后说明已完成、验证方式和风险。不要自行清理 worktree。\\n",
        task.title, task.goal, task.acceptance_criteria, project_path
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::models::OrchestratorTaskStatus;

    #[test]
    fn prompt_contains_goal_and_acceptance() {
        let task = OrchestratorTaskRow {
            id: "task-1".into(),
            project_id: "project-1".into(),
            title: "Fix bug".into(),
            goal: "修复截图保存失败".into(),
            acceptance_criteria: "截图可复制到剪贴板".into(),
            status: OrchestratorTaskStatus::Queued,
            priority: 0,
            branch_name: None,
            worktree_id: None,
            session_id: None,
            blocked_reason: None,
            attempt: 0,
            created_at: "2026-07-05T00:00:00Z".into(),
            updated_at: "2026-07-05T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
        };
        let prompt = build_task_prompt(&task, "/repo");
        assert!(prompt.contains("修复截图保存失败"));
        assert!(prompt.contains("截图可复制到剪贴板"));
        assert!(prompt.contains("/repo"));
    }
}
```

- [ ] **Step 2: Implement runner with Workbench helpers**

In `runner.rs`, create `prepare_visible_runner(state, app_handle, task)` that:

1. Creates a branch name `agent/<task-id-short>-<slug-title>`.
2. Calls existing local workbench worktree creation helper.
3. Calls existing local workbench session creation helper with the new worktree id.
4. Writes `claude\n` to the session.
5. Writes the generated task prompt to the session without an extra destructive command.
6. Stores `branch_name`, `worktree_id`, `session_id`, and status `Running`.

Expose only the minimum existing Workbench helpers needed; do not duplicate tmux or Git code.

- [ ] **Step 3: Add scheduler loop**

In `scheduler.rs`, define a runtime:

```rust
#[derive(Clone)]
pub struct OrchestratorRuntime {
    cancel: tokio_util::sync::CancellationToken,
}
```

Add `start_orchestrator_scheduler(app_handle, state)` that ticks every 10 seconds:

- Load enabled project configs.
- For each project, count `Preparing | Running | Verifying | Delivering`.
- If below `max_concurrent_tasks`, claim one Queued task.
- Move it to Preparing.
- Call `prepare_visible_runner`.
- On error, move to Blocked and record event.

- [ ] **Step 4: Wire runtime into AppState**

Add to `AppState`:

```rust
pub orchestrator_cancel: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
```

Start scheduler in `lib.rs` setup after `app.manage(state.clone())`, following health/cloud scheduler patterns.

Cancel it in `RunEvent::Exit`.

- [ ] **Step 5: Add manual dispatch command for tests and UI**

Add command:

```rust
#[tauri::command]
pub async fn dispatch_orchestrator_once(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<serde_json::Value, AppError> {
    let dispatched = crate::orchestrator::scheduler::dispatch_once(&state, app_handle).await?;
    Ok(serde_json::json!({ "dispatched": dispatched }))
}
```

- [ ] **Step 6: Verify scheduler compiles**

Run:

```bash
cd src-tauri
cargo test orchestrator::prompt --lib
cargo check
```

Expected: prompt test passes and scheduler/runner compile.

- [ ] **Step 7: Commit visible runner**

```bash
git add src-tauri/src/orchestrator src-tauri/src/state.rs src-tauri/src/lib.rs src-tauri/src/commands/orchestrator.rs
git commit -m "feat: run orchestrator tasks in workbench terminal"
```

### Task 6: Evidence, Verification, and Blocked UI

**Files:**
- Create: `src-tauri/src/orchestrator/delivery.rs`
- Modify: `src-tauri/src/orchestrator/repo.rs`
- Modify: `src-tauri/src/orchestrator/models.rs`
- Modify: `src-tauri/src/orchestrator/scheduler.rs`
- Modify: `src-tauri/src/commands/orchestrator.rs`
- Modify: `web/src/api/orchestrator.ts`
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/pages/Orchestrator/Orchestrator.tsx`
- Modify: `web/src/i18n/locales/{zh,en}/orchestrator.json`

- [ ] **Step 1: Add evidence model and repo tests**

Add `OrchestratorEvidenceDto` and repo methods `add_evidence`, `list_evidence(task_id)`.

Test:

```rust
#[tokio::test]
async fn evidence_is_listed_by_task() {
    let repo = setup_repo().await;
    repo.add_evidence("task-1", "verificationOutput", "npm test", "passed", "output").await.unwrap();
    let items = repo.list_evidence("task-1").await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, "verificationOutput");
}
```

- [ ] **Step 2: Implement verification command runner**

In `delivery.rs`, implement:

```rust
pub async fn run_verification_commands(
    cwd: &std::path::Path,
    commands: &[String],
) -> Result<String, AppError>
```

Execute each command through the user's shell, with cwd set to task worktree. Return combined stdout/stderr on success. On non-zero exit, return an `AppError` containing command and output.

- [ ] **Step 3: Add verification pipeline step**

Scheduler detects tasks that are ready for verification by explicit command state transition, not by guessing terminal text in the first implementation. Add command:

```rust
complete_orchestrator_agent_run(task_id)
```

This moves Running → Verifying and runs verification. The UI exposes this as “Claude Code 已完成，开始验证” until automatic terminal completion detection is added.

- [ ] **Step 4: Show evidence and blocked controls**

Orchestrator page loads `list_orchestrator_task_evidence(taskId)` for selected task. Right panel shows evidence items. Blocked tasks show:

- blocked reason.
- open Workbench.
- retry.
- abort.

- [ ] **Step 5: Verify evidence flow**

Run:

```bash
cd src-tauri
cargo test orchestrator::repo --lib
cargo test orchestrator::delivery --lib
cargo check
cd ../web
npx tsc --noEmit
```

Expected: evidence and verification tests pass.

- [ ] **Step 6: Commit verification/evidence**

```bash
git add src-tauri/src/orchestrator src-tauri/src/commands/orchestrator.rs web/src
git commit -m "feat: record orchestrator verification evidence"
```

### Task 7: Full Auto Delivery

**Files:**
- Modify: `src-tauri/src/orchestrator/delivery.rs`
- Modify: `src-tauri/src/orchestrator/scheduler.rs`
- Modify: `src-tauri/src/workbench/git.rs`
- Modify: `src-tauri/src/commands/orchestrator.rs`
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/pages/Orchestrator/Orchestrator.tsx`

- [ ] **Step 1: Add delivery tests using temporary Git repo**

In `delivery.rs`, add tests that create a bare origin, clone it, create a task branch, commit, push branch, merge main, and push main.

Test names and required assertions:

```rust
#[tokio::test]
async fn full_delivery_pushes_task_branch_and_main() {
    // Create a temporary bare origin and local clone.
    // Create one orchestrator task branch with a committed file change.
    // Run deliver_task with all full-auto flags enabled.
    // Assert the task branch exists on origin.
    // Assert origin/main contains the committed file change.
    // Assert task status is Done and evidence contains commit, push branch, merge, and push main stages.
}

#[tokio::test]
async fn delivery_blocks_when_main_worktree_is_dirty() {
    // Create a temporary bare origin and local clone.
    // Add an uncommitted change in the main worktree before delivery.
    // Run deliver_task with all full-auto flags enabled.
    // Assert the task status is Blocked.
    // Assert blocked_reason contains "main worktree is dirty".
    // Assert origin/main does not receive the task branch change.
}
```

Use `tempfile::tempdir()` and shell `git` commands like existing `workbench/git.rs` tests.

- [ ] **Step 2: Implement delivery pipeline**

Implement:

```rust
pub async fn deliver_task(
    state: &AppState,
    task_id: &str,
) -> Result<DeliverySummary, AppError>
```

Pipeline:

1. Load task and config.
2. Confirm `auto_commit`, `auto_push_task_branch`, `auto_merge_to_main`, `auto_push_main` are true.
3. Commit task worktree via existing Workbench commit helper.
4. Push task worktree via existing Workbench push helper.
5. Merge worktree via existing Workbench merge helper.
6. Push main branch from main worktree with a new focused helper that runs `git push` against the main branch upstream/origin.
7. Add evidence for each successful stage.

- [ ] **Step 3: Block on partial delivery failure**

If branch push succeeds but merge fails, save evidence for branch push and set task `Blocked` with `blocked_reason = "merge main failed: <message>"`.

If merge succeeds but push main fails, save evidence for merge and set task `Blocked` with `blocked_reason = "main merged locally but push main failed: <message>"`.

- [ ] **Step 4: Show delivery evidence in UI**

Right panel lists:

- commit hash.
- branch push result.
- merge main result.
- push main result.

Use `Pill` tones; no hardcoded colors.

- [ ] **Step 5: Verify delivery**

Run:

```bash
cd src-tauri
cargo test orchestrator::delivery --lib
cargo test workbench::git --lib
cargo check
cd ../web
npx tsc --noEmit
```

Expected: full delivery tests pass, partial failure tests pass, TypeScript compiles.

- [ ] **Step 6: Commit auto delivery**

```bash
git add src-tauri/src/orchestrator src-tauri/src/workbench src-tauri/src/commands/orchestrator.rs web/src
git commit -m "feat: deliver orchestrator tasks automatically"
```

### Task 8: Workbench Deep Link Integration

**Files:**
- Modify: `web/src/pages/Workbench/Workbench.tsx`
- Create: `web/src/pages/Workbench/workbenchDeepLink.ts`
- Create: `web/src/pages/Workbench/workbenchDeepLink.test.ts`
- Modify: `web/src/pages/Orchestrator/Orchestrator.tsx`

- [ ] **Step 1: Write deep link helper tests**

Create `web/src/pages/Workbench/workbenchDeepLink.test.ts`:

```ts
import { parseWorkbenchDeepLink } from './workbenchDeepLink';

function testParseWorkbenchDeepLink(): void {
  const parsed = parseWorkbenchDeepLink('?projectId=p1&worktreeId=w1&sessionId=s1');
  if (parsed.projectId !== 'p1') throw new Error('expected project id');
  if (parsed.worktreeId !== 'w1') throw new Error('expected worktree id');
  if (parsed.sessionId !== 's1') throw new Error('expected session id');
}

function testParseEmptyDeepLink(): void {
  const parsed = parseWorkbenchDeepLink('');
  if (parsed.projectId !== null) throw new Error('expected no project id');
  if (parsed.worktreeId !== null) throw new Error('expected no worktree id');
  if (parsed.sessionId !== null) throw new Error('expected no session id');
}

testParseWorkbenchDeepLink();
testParseEmptyDeepLink();
console.log('workbench deep link tests passed');
```

- [ ] **Step 2: Implement deep link helper**

Create `web/src/pages/Workbench/workbenchDeepLink.ts`:

```ts
export interface WorkbenchDeepLink {
  projectId: string | null;
  worktreeId: string | null;
  sessionId: string | null;
}

export function parseWorkbenchDeepLink(search: string): WorkbenchDeepLink {
  const params = new URLSearchParams(search);
  return {
    projectId: params.get('projectId'),
    worktreeId: params.get('worktreeId'),
    sessionId: params.get('sessionId'),
  };
}
```

- [ ] **Step 3: Apply deep link in Workbench**

In `Workbench.tsx`, call `useLocation()` and `parseWorkbenchDeepLink(location.search)`.

Behavior:

- If `projectId` exists and differs from active project, use `useWorkbenchProjects().selectProject` after locating the project in `projects`.
- After worktrees load, if `worktreeId` exists and is present, set `activeWorktreeId`.
- After sessions load, if `sessionId` exists and is present, set `activeSessionId` and call focus.
- Do not early return before hooks.

- [ ] **Step 4: Add Orchestrator open Workbench action**

In `Orchestrator.tsx`, add:

```tsx
const openWorkbenchUrl = selectedTask
  ? `/workbench?projectId=${encodeURIComponent(selectedTask.projectId)}&worktreeId=${encodeURIComponent(selectedTask.worktreeId ?? '')}&sessionId=${encodeURIComponent(selectedTask.sessionId ?? '')}`
  : '/workbench';
```

Render a Button that navigates to that URL when task has worktree/session.

- [ ] **Step 5: Verify deep link**

Run:

```bash
cd web
npx --yes tsx src/pages/Workbench/workbenchDeepLink.test.ts
npx tsc --noEmit
```

Expected: tests pass and Workbench compiles.

- [ ] **Step 6: Commit deep links**

```bash
git add web/src/pages/Workbench web/src/pages/Orchestrator
git commit -m "feat: link orchestrator tasks to workbench"
```

### Task 9: Documentation, Project Memory, and Final Verification

**Files:**
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `AGENTS.md`
- Modify: `docs/prd.md`

- [ ] **Step 1: Update project memory**

Update `AGENTS.md` root directory map:

```markdown
├── web/src/pages/Orchestrator/   # 自动编排器全局任务看板
├── src-tauri/src/orchestrator/   # 自动编排器任务、调度、Runner 和交付逻辑
```

Keep the root memory concise. Do not add an implementation changelog.

- [ ] **Step 2: Update frontend memory**

Update `web/CLAUDE.md` with:

- Orchestrator route `/orchestrator`.
- Orchestrator test commands:

```bash
npx --yes tsx src/lib/orchestrator.test.ts
npx --yes tsx src/pages/Workbench/workbenchDeepLink.test.ts
npx tsc --noEmit
```

- Constraint: Orchestrator owns task dashboard; Workbench owns project现场 and terminal takeover.

- [ ] **Step 3: Update backend memory**

Update `src-tauri/CLAUDE.md` with:

- `src-tauri/src/orchestrator/` module responsibilities.
- Scheduler lifecycle.
- Reuse requirement for Workbench worktree/session/Git helpers.
- Rust verification commands:

```bash
cargo test orchestrator:: --lib
cargo check
```

- [ ] **Step 4: Update PRD**

Update `docs/prd.md` with a concise Orchestrator feature section:

- internal task queue.
- project-scoped policy.
- visible tmux Runner.
- full auto delivery.
- blocked state and evidence chain.

Do not add a changelog entry.

- [ ] **Step 5: Run focused verification**

Run:

```bash
cd src-tauri
cargo test orchestrator:: --lib
cargo check
cd ../web
npx --yes tsx src/lib/orchestrator.test.ts
npx --yes tsx src/pages/Workbench/workbenchDeepLink.test.ts
npx tsc --noEmit
```

Expected: all commands pass.

- [ ] **Step 6: Run broader build verification**

Run:

```bash
cd web
npm run build
```

Expected: TypeScript and Vite build pass.

- [ ] **Step 7: Commit docs and final verification fixes**

```bash
git add AGENTS.md web/CLAUDE.md src-tauri/CLAUDE.md docs/prd.md
git commit -m "docs: document orchestrator automation"
```

## Final Review Checklist

- [ ] Orchestrator route exists and appears in sidebar.
- [ ] User can create a task for the active project.
- [ ] User can queue a task.
- [ ] Project policy displays full-auto delivery switches clearly.
- [ ] Scheduler creates isolated `agent/<task-id>` worktree.
- [ ] Scheduler creates a visible Workbench terminal window.
- [ ] Task prompt includes goal and acceptance criteria.
- [ ] Verification output is captured as evidence.
- [ ] Delivery records commit, branch push, merge main, and push main evidence.
- [ ] Partial delivery failure enters Blocked with a precise reason.
- [ ] Orchestrator can open Workbench at task project/worktree/session.
- [ ] No React hooks are placed after early returns.
- [ ] CSS uses design tokens and no hardcoded colors.
- [ ] `cargo test orchestrator:: --lib` passes.
- [ ] `cargo check` passes.
- [ ] Frontend helper tests pass.
- [ ] `npx tsc --noEmit` passes.
- [ ] `npm run build` passes.
