# Symphony-Inspired Workbench Automation Design

## Context

cc-partner already has Workbench projects, Git worktrees, visible tmux-backed terminal sessions, file/Git panels, project-level Orchestrator tasks, remote task views, mobile automation, verification evidence, and full-auto delivery. The goal is not to replace Workbench with Symphony. The goal is to absorb the useful Symphony ideas into cc-partner's existing model:

- repository/project workflow policy
- separated business state and runner state
- bounded scheduler with explainable runtime state
- isolated per-task worktree execution
- operator-visible evidence and runtime details
- a board-first task management UI inspired by the Symphony demo

This design uses the Symphony SPEC as the reference model while keeping cc-partner's visible Workbench runner as the default execution surface.

## Confirmed Decisions

- Prioritize built-in Workbench tasks now, while reserving tracker fields for later external issue tracker support.
- Split task state into `workflowState`, `runState`, and `attemptPhase`.
- Use fixed default workflow lanes. Do not allow arbitrary lane definitions in this round.
- Use a cc-partner built-in default workflow. A project-root `WORKFLOW.md` is optional and only overrides project-specific automation behavior.
- Automatic delivery is controlled only by cc-partner Settings. It is off by default.
- The desktop automation UI becomes board-first with a right-side task detail drawer.
- Drag and drop is supported only between adjacent lanes.
- Runner provider is abstracted. This round implements `claudeCodeVisible`, backed by a Workbench visible tmux terminal and Claude Code runtime/session association.
- Claude Code session/runtime should be used this round when it can be associated reliably, with graceful fallback to `runtime unknown`.
- Mobile uses a compact grouped-list version of the board instead of desktop horizontal drag and drop.

## Goals

1. Make Workbench automation feel like managing work, not reading a raw lifecycle log.
2. Preserve visible terminal execution and takeover as a first-class cc-partner behavior.
3. Explain why a task is or is not running through project-level runtime state.
4. Make verification and delivery evidence easier to inspect and act on.
5. Create a data model that can later support external tracker tasks and alternate runner providers without another major migration.

## Non-Goals

- Do not implement Linear polling or external tracker synchronization in this round.
- Do not implement Codex app-server as a runner in this round.
- Do not allow arbitrary user-defined workflow lanes.
- Do not make `WORKFLOW.md` control the device-level automatic delivery safety switch.
- Do not replace Workbench terminal/window/pane models.
- Do not move project memory responsibilities from `AGENTS.md` or `CLAUDE.md` into `WORKFLOW.md`.

## Domain Model

### Task State Layers

`workflowState` is the user-facing board lane. It describes business progress:

- `backlog`: task is captured but not ready for scheduling
- `todo`: task is eligible for scheduling
- `inProgress`: task has been claimed or is being worked
- `humanReview`: runner and verifier have completed; user should inspect evidence
- `rework`: task needs another run or user correction
- `merging`: delivery is in progress
- `done`: terminal success state
- `canceled`: terminal stopped state

`runState` describes scheduler/runner state:

- `idle`
- `queued`
- `preparing`
- `running`
- `verifying`
- `retrying`
- `blocked`
- `delivering`

`attemptPhase` describes the current or most recent runner attempt:

- `preparingWorkspace`
- `buildingPrompt`
- `launchingRunner`
- `initializingSession`
- `streaming`
- `finishing`
- `succeeded`
- `failed`
- `timedOut`
- `stalled`
- `canceledByReconciliation`

The board groups only by `workflowState`. `runState` and `attemptPhase` appear on cards, in the status strip, and in the detail drawer.

### Legacy Status Migration

Existing `status` values map as follows:

| Legacy status | workflowState | runState |
| --- | --- | --- |
| `draft` | `backlog` | `idle` |
| `queued` | `todo` | `queued` |
| `preparing` | `inProgress` | `preparing` |
| `running` | `inProgress` | `running` |
| `verifying` | `inProgress` | `verifying` |
| `delivering` | `merging` | `delivering` |
| `done` | `done` | `idle` |
| `blocked` | `rework` | `blocked` |
| `aborted` | `canceled` | `idle` |

The old field can remain as a compatibility projection during migration, but new UI and new scheduling logic should consume the split fields.

### Tracker-Reserved Fields

Tasks should reserve optional fields for future tracker support:

- `source`: `internal` for this round
- `externalId`
- `externalIdentifier`
- `externalUrl`
- `externalState`
- `externalLabels`

These fields do not drive scheduling until an external tracker adapter is implemented.

## Workflow Policy

### Built-In Default Workflow

cc-partner owns a built-in default workflow used for every project when no project override exists. This built-in workflow defines:

- fixed lane set and default lane labels
- default create state: `backlog`
- default active states: `todo`, `rework`
- review state: `humanReview`
- terminal states: `done`, `canceled`
- default runner provider: `claudeCodeVisible`
- default prompt template
- default evidence categories
- default scheduler behavior

### Optional Project `WORKFLOW.md`

Project-root `WORKFLOW.md` is an optional override for project-specific automation behavior. It is repository-owned and versionable, but not required to run automation.

It may override:

- validation commands
- project-specific prompt template
- before/after run hooks
- runner timeout values
- tracker metadata for future use
- project-specific context text

It may not:

- create arbitrary new lanes
- enable automatic delivery
- override Settings safety controls
- replace AGENTS.md/CLAUDE.md as project memory

### Precedence

Runtime policy is resolved in this order:

1. Settings safety and device-level enablement
2. Project `WORKFLOW.md` override
3. cc-partner built-in default workflow

If `WORKFLOW.md` is missing, the UI shows that the built-in workflow is active. If `WORKFLOW.md` exists but fails parsing or validation, new dispatches are blocked, but existing tasks and evidence remain viewable.

## Scheduling And State Progression

### Scheduler Eligibility

Scheduler can claim tasks only when all are true:

- global automation is enabled in Settings
- project workflow is valid
- task `workflowState` is active, initially `todo` or `rework`
- task is not already claimed/running/delivering
- project is online and writable
- global concurrency slots are available
- remote shortcut rules allow the owning device to handle the task

When a task is claimed:

- `workflowState` becomes `inProgress`
- `runState` becomes `queued` then `preparing/running/verifying`
- a task worktree and visible Workbench terminal session are prepared

### Success Path

After runner completion and verifier pass:

- default result is `workflowState=humanReview`, `runState=idle`
- evidence is attached to the task
- the user can open the execution site, inspect evidence, request rework, cancel, or deliver

If Settings automatic delivery is enabled, the backend may continue from `humanReview` to:

- `workflowState=merging`, `runState=delivering`
- then `workflowState=done`, `runState=idle`

Automatic delivery remains disabled by default and is never enabled by `WORKFLOW.md`.

### Failure Path

Runner, hook, verifier, validation, stall, offline, or delivery failures move the task to:

- `workflowState=rework`
- `runState=blocked` or `retrying`
- `blockedReason` and evidence explain the failure

The user can request another run after reviewing or fixing the issue.

### User Actions

Backend actions should be explicit and auditable:

- `moveTaskWorkflowState(projectId, taskId, targetState)`: adjacent-lane movement only
- `startTask(projectId, taskId)`: put a task into the scheduler path
- `requestRework(projectId, taskId, reason)`: move review task to rework with reason
- `deliverReviewedTask(projectId, taskId)`: begin delivery only when Settings allows
- `cancelTask(projectId, taskId)`: move to canceled while preserving worktree/session
- `refreshOrchestratorProject(projectId)`: trigger best-effort dispatch/reconcile

Drag and drop calls `moveTaskWorkflowState` and must be constrained to adjacent lanes. Cross-lane jumps and dangerous side effects use buttons or menus.

## Runner Runtime

### Provider Model

Introduce a runner provider layer. This round implements:

- `claudeCodeVisible`

Reserved future providers:

- `claudeCodeHeadless`
- `codexAppServer`

The UI reads a normalized runtime snapshot and should not depend on provider-specific internals.

### `claudeCodeVisible`

This provider keeps current cc-partner behavior:

1. Create or reuse the task worktree.
2. Create a Workbench tmux terminal session bound to the worktree.
3. Render prompt from built-in workflow plus project override.
4. Write `claude\n<prompt>\n` into the visible terminal.
5. Detect `ORCHESTRATOR_DEV_DONE` sentinel from terminal output.
6. Run validation and verifier.
7. Attach evidence and update task states.

### Claude Code Session Association

This round should use Claude Code's own local runtime/session data where reliable:

- correlate by cwd/worktree path
- correlate by runner start time window
- inspect Claude Code JSONL/session files updated during the attempt
- store session id, transcript path, start time, last activity, and summarized last message/event

Suggested fields:

- `runnerProvider`
- `workbenchSessionId`
- `worktreeId`
- `branchName`
- `claudeSessionId`
- `transcriptPath`
- `runtimeStartedAt`
- `lastActivityAt`
- `lastEvent`
- `lastMessage`
- `runtimeSeconds`
- `usage` fields when reliably available

Association failure must not fail the task. UI should show `runtime unknown` and keep the Workbench execution site available.

## Runtime Snapshot

Expose a project-level runtime snapshot for the automation status strip and operational debugging:

- generated time
- scheduler enabled/disabled
- workflow source: built-in default or project override
- workflow validation status and error
- slots used/available
- running task summaries
- retrying task summaries
- recent scheduler/runner events
- latest error
- remote/offline status

This snapshot is an observability/control surface, not the source of truth for correctness.

## Desktop UI

### Project Automation Entry

Workbench keeps the existing top-level Project Automation entry. When open:

- worktree strip is hidden
- terminal/file layers remain mounted but stop accepting input
- automation board fills the center workspace

### Board

Desktop board lanes:

- Backlog
- Todo
- In Progress
- Human Review
- Rework
- collapsed/secondary lanes for Merging, Done, Canceled

Cards show:

- title
- source badge
- runState/attempt badge
- runtime summary
- blocked reason or verifier result badge
- remote/local origin

### Status Strip

Above the board, show:

- scheduler state
- slots
- workflow source and validity
- local/remote context
- latest tick
- latest error or warning
- refresh control
- Settings link

### Detail Drawer

Selecting a card opens a right-side drawer with:

- title, goal, acceptance criteria
- workflowState, runState, attemptPhase
- execution site: worktree, branch, Workbench terminal link
- Claude runtime: session/transcript/last activity
- evidence stream
- action buttons: start, retry, request rework, deliver, cancel, open execution site

Evidence should be rendered as a readable timeline rather than an undifferentiated raw log list. Verification output, verifier review, repair prompt, delivery stages, and development attempts remain preserved.

### Creation Flow

The create-task dialog keeps AI prompt completion but changes submission actions:

- Create in Backlog
- Create in Todo
- Create and Start

AI completion fills title/goal/acceptance criteria only. The user still confirms.

## Remote And Mobile

### Remote

Remote project automation remains remote-authoritative:

- local device uses task-view APIs
- owning device owns task state and execution
- task view DTO includes split state and runtime snapshot fields
- mirror rows store enough fields for offline display
- pendingRemote outbox items remain non-actionable and do not enter drag/drop or detail selection

### Mobile

Mobile automation uses the same task-view data but a compact UI:

- grouped list by `workflowState`
- no complex horizontal drag/drop
- create dialog supports the same three actions
- details show state, evidence, runtime summary, and open terminal action

## Migration

Migration should be additive and reversible at the code level:

- add new task columns for workflow/run/attempt/provider/runtime fields
- populate fields from legacy `status`
- keep old commands or wrappers temporarily where needed
- update remote protocol DTOs with optional fields to reduce mixed-version breakage
- update PRD and memory docs after implementation

Existing tasks should remain visible after migration. Tasks in old `blocked` state should appear in `Rework` with `runState=blocked`.

## Testing Plan

Backend focused tests:

- legacy status to split state mapping
- adjacent workflow movement validation
- scheduler eligibility and claim behavior
- success path to Human Review when automatic delivery is off
- delivery path only when Settings allows
- `WORKFLOW.md` override parsing and fallback to built-in default
- invalid workflow blocks new dispatch but preserves task viewing
- Claude runtime association success and unknown fallback
- remote task view DTO compatibility

Frontend focused tests:

- board grouping and card badges
- status strip rendering from runtime snapshot
- adjacent drag/drop acceptance and cross-lane rejection
- create dialog three submission actions
- detail drawer state/runtime/evidence display
- action button enablement for each state
- Workbench deep link still opens execution site

Mobile focused tests:

- grouped automation list
- create dialog three actions
- runtime/evidence display

Relevant existing tests to update:

- `web/src/pages/Workbench/workbenchAutomationView.test.ts`
- `web/src/lib/orchestrator.test.ts`
- `web/src/lib/orchestratorRemote.test.ts`
- `web/src/api/orchestrator.test.ts`
- `web/src/mobile/MobileAutomationPanel.test.ts`
- Rust tests in `src-tauri/src/orchestrator/*` and `src-tauri/src/commands/orchestrator.rs`

## Implementation Phases

1. Backend model, migration, state helpers, task-view DTOs, and tests.
2. Workflow resolver: built-in default plus optional project override.
3. Scheduler and action API changes.
4. Desktop board UI, detail drawer, status strip, and create dialog.
5. Claude Code runtime association and runtime snapshot.
6. Remote/mobile compatibility updates.
7. PRD, `web/CLAUDE.md`, and `src-tauri/CLAUDE.md` updates.

## Open Risks

- Claude Code session/jsonl association may be imperfect when multiple sessions share cwd and time windows. The implementation must be conservative and non-blocking.
- Drag/drop can hide side effects if not constrained carefully. Backend validation must reject non-adjacent moves.
- Mixed-version remote devices may lack new fields. DTOs should treat new fields as optional during the transition.
- The Workbench page is already large. Implementation should split board/detail/status helpers instead of adding all logic to `Workbench.tsx`.
