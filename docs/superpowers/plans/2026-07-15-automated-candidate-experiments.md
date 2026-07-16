# Automated Candidate Experiments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让同一任务的2–8个candidate在同一owning device自动运行、验证、比较，并只允许唯一winner进入交付。

**Architecture:** Experiment是组级聚合，candidate继续复用普通task/worktree/attempt/evidence。数据库partial unique index、winner CAS、delivery defense check和现有per-task delivery lock共同保证唯一交付；正常high-confidence路径不产生人工Diff审查。

**Tech Stack:** Rust Orchestrator/SQLite, existing scheduler/verifier/delivery/outbox, Agent Adapters, A2 projection contracts, Browser Verification evidence, React/Mobile, axum P2P.

## Global Constraints

- Experiment显式opt-in；所有candidate固定在同一project owning device。
- candidate数量2–8；`maxParallel`不得超过candidate数量或设备global cap。
- candidate task永不直接进入普通Human Review/delivery。
- loser永不commit/push/merge；不展示用户Diff或批注。
- 只有零合格、并列、judge error或confidence非high时产生一个组级Attention。
- remote create使用一条组级原子幂等请求，不拼N条task outbox。
- P2P business API继续无调用者身份鉴权；experiment capability只表示协议支持。
- capability固定`orchestrator.experiments.v1`；旧peer不fallback普通tasks。
- 降级前必须quiesce active groups，不能宣称旧版本可安全运行candidate tasks。

---

## File Structure

- Create: `src-tauri/src/orchestrator/experiments/{mod.rs,models.rs,create.rs,reducer.rs,judge.rs,delivery.rs,outbox.rs,remote_protocol.rs,remote_client.rs}`。
- Create: `src-tauri/src/orchestrator/repo/experiments.rs`。
- Modify: `src-tauri/src/orchestrator/{mod.rs,claim.rs,scheduler.rs,delivery.rs,state.rs}`、`src-tauri/src/orchestrator/repo/{mod.rs,schema.rs,tasks.rs,tests.rs}`、`src-tauri/src/commands/orchestrator/{mod.rs,actions.rs,tests.rs}`、`src-tauri/src/attention/{models.rs,orchestrator_source.rs}`、`src-tauri/src/net/routes/orchestrator.rs`、`src-tauri/src/net/{http_server.rs,protocol.rs,discovery.rs}`。
- Create: `src-tauri/src/commands/orchestrator/experiments.rs`。
- Create: `web/src/pages/Orchestrator/views/OrchestratorExperimentPanel.tsx` and tests/styles.
- Create: `web/src/mobile/components/MobileAutomationExperiments.tsx`。
- Modify: `web/src/lib/types/orchestrator.ts`、`web/src/lib/schemas/{orchestrator.ts,orchestrator.test.ts}`、`web/src/api/{orchestrator.ts,orchestrator.test.ts}`、`web/src/pages/Orchestrator/controllers/useOrchestratorController.ts`、`web/src/i18n/locales/{zh/orchestrator.json,en/orchestrator.json}`、`web/tests/{workbench.spec.ts,mobile-workbench.spec.ts}`。

## Task Dependency Graph

```text
T1 → T2 → T3 → T4 → T5 → T6 → T7 → T8
```

### Task 1: Add Experiment Schema, Models and Repository

**Files:**
- Create: `src-tauri/src/orchestrator/experiments/{mod.rs,models.rs}`
- Create: `src-tauri/src/orchestrator/repo/experiments.rs`
- Modify: `src-tauri/src/orchestrator/{mod.rs,models.rs}`
- Modify: `src-tauri/src/orchestrator/repo/{mod.rs,schema.rs,helpers.rs,tests.rs}`
- Modify: `src-tauri/migrations/0001_init.sql`

**Interfaces:**
- Produces: experiment/candidate/evidence/create-request tables and typed status/outcome/confidence.

- [ ] **Step 1: Write additive schema and unique-winner tests**

```rust
#[tokio::test]
async fn database_rejects_two_winners_for_one_experiment() {
    let repo = experiment_repo_fixture().await;
    let exp = repo.insert_fixture(2).await;
    repo.set_outcome(&exp.id, "task-1", CandidateOutcome::Winner).await.unwrap();
    assert!(repo.set_outcome(&exp.id, "task-2", CandidateOutcome::Winner).await.is_err());
}
```

Add old DB upgrade, unknown status fail-closed and stable enum serialization cases.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::repo::experiments --lib`

Expected: FAIL because tables/repo are absent.

- [ ] **Step 3: Implement additive schema**

Create `orchestrator_experiments`, `orchestrator_experiment_candidates`, `orchestrator_experiment_evidence`, `orchestrator_experiment_create_requests`; add partial unique index on candidate `outcome='winner'`. Add task `experiment_id/delivery_suppressed` columns with defaults preserving ordinary tasks.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked orchestrator::repo::experiments --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/experiments src-tauri/src/orchestrator/repo src-tauri/src/orchestrator/mod.rs src-tauri/src/orchestrator/models.rs src-tauri/migrations/0001_init.sql
git commit -m "feat(orchestrator): add candidate experiment storage"
```

### Task 2: Create Experiment and Candidate Tasks Atomically

**Files:**
- Create: `src-tauri/src/orchestrator/experiments/create.rs`
- Create: `src-tauri/src/commands/orchestrator/experiments.rs`
- Modify: `src-tauri/src/commands/orchestrator/mod.rs`
- Modify: `src-tauri/src/orchestrator/repo/experiments.rs`

**Interfaces:**
- Produces: `CreateExperimentRequest`, `create_experiment_idempotently`.

- [ ] **Step 1: Write rollback/idempotency tests**

```rust
#[tokio::test]
async fn candidate_insert_failure_rolls_back_entire_experiment() {
    let fixture = create_fixture().fail_after_candidate(2);
    assert!(fixture.create(request_with_candidates(3)).await.is_err());
    assert_eq!(fixture.count_experiments().await, 0);
    assert_eq!(fixture.count_tasks().await, 0);
}
```

Add 1/9 candidate rejection, same request+fingerprint reuse and request-key/different-payload conflict.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::experiments::create --lib`

Expected: FAIL because create service is absent.

- [ ] **Step 3: Implement one-transaction group creation**

Normalize/fingerprint request; create group, 2–8 ordinary task rows with `source=experiment`, candidate links, initial group evidence and idempotency row. Provider override is stored on each task; request never accepts device/baseUrl.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked orchestrator::experiments::create --lib && cargo test --locked commands::orchestrator::experiments --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/experiments/create.rs src-tauri/src/commands/orchestrator/experiments.rs src-tauri/src/commands/orchestrator/mod.rs src-tauri/src/orchestrator/repo/experiments.rs
git commit -m "feat(orchestrator): create candidate experiments atomically"
```

### Task 3: Enforce Group Capacity and Fair Claim Order

**Files:**
- Modify: `src-tauri/src/orchestrator/claim.rs`
- Modify: `src-tauri/src/orchestrator/repo/tasks.rs`
- Modify: `src-tauri/src/orchestrator/scheduler.rs`

**Interfaces:**
- Produces: `fair_order_claim_candidates` and group cap check inside claim CAS.

- [ ] **Step 1: Write double-cap and fairness tests**

```rust
#[tokio::test]
async fn one_experiment_cannot_consume_more_than_group_cap() {
    let fixture = scheduler_fixture().global_cap(4).experiment("e1", 4, 1).await;
    fixture.dispatch_once().await;
    assert_eq!(fixture.running_candidates("e1").await, 1);
}
```

Add two-group round robin and ordinary-task non-starvation cases.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::claim --lib && cargo test --locked orchestrator::scheduler --lib`

Expected: current scheduler can claim multiple same-group tasks.

- [ ] **Step 3: Implement fair ordering and transactional group count**

Add `experiment_id` to claim candidate. Stable-order ordinary tasks, then round-robin experiment groups one candidate per group per pass. Claim transaction recomputes global and group active counts after each successful CAS.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked orchestrator::claim --lib && cargo test --locked orchestrator::scheduler --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/claim.rs src-tauri/src/orchestrator/repo/tasks.rs src-tauri/src/orchestrator/scheduler.rs
git commit -m "feat(orchestrator): schedule experiment candidates fairly"
```

### Task 4: Reduce Candidate Verification Without Per-task Delivery

**Files:**
- Create: `src-tauri/src/orchestrator/experiments/reducer.rs`
- Modify: `src-tauri/src/commands/orchestrator/{actions.rs,common.rs}`
- Modify: `src-tauri/src/orchestrator/repo/{tasks.rs,experiments.rs}`
- Modify: `src-tauri/src/attention/orchestrator_source.rs`

**Interfaces:**
- Produces: `record_candidate_review`, `reduce_experiment` and child Attention suppression.

- [ ] **Step 1: Write candidate-ready/no-delivery test**

```rust
#[tokio::test]
async fn passed_candidate_stops_at_candidate_ready() {
    let fixture = experiment_fixture(2).await;
    fixture.verify_passed("task-1").await;
    assert_eq!(fixture.candidate_outcome("task-1").await, CandidateOutcome::CandidateReady);
    assert_eq!(fixture.delivery_side_effects(), 0);
    assert_eq!(fixture.child_attention_count("task-1").await, 0);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::experiments::reducer --lib && cargo test --locked attention::orchestrator_source --lib`

Expected: current passed task enters ordinary review/delivery path.

- [ ] **Step 3: Insert experiment branch before ordinary review/delivery**

Passed→CandidateReady; recoverable failure follows existing repair within max turns; terminal failure→Failed. Reduce group only after durable child transition. Exclude all experiment child tasks from per-task HumanReview/Blocked Attention.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked orchestrator::experiments::reducer --lib && cargo test --locked commands::orchestrator::actions --lib && cargo test --locked attention::orchestrator_source --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/experiments/reducer.rs src-tauri/src/commands/orchestrator/actions.rs src-tauri/src/commands/orchestrator/common.rs src-tauri/src/orchestrator/repo/tasks.rs src-tauri/src/orchestrator/repo/experiments.rs src-tauri/src/attention/orchestrator_source.rs
git commit -m "feat(orchestrator): reduce candidate verification"
```

### Task 5: Add Comparative Judge and Group Evidence

**Files:**
- Create: `src-tauri/src/orchestrator/experiments/judge.rs`
- Modify: `src-tauri/src/orchestrator/experiments/reducer.rs`
- Modify: `src-tauri/src/orchestrator/verifier.rs`

**Interfaces:**
- Produces: `ComparativeVerdict`, `evaluate_experiment` and `selectionReview` evidence.

- [ ] **Step 1: Write deterministic and invalid-judge tests**

```rust
#[tokio::test]
async fn one_ready_candidate_is_high_confidence_without_judge_call() {
    let fixture = judge_fixture().ready("task-1").failed("task-2");
    let verdict = fixture.evaluate().await.unwrap();
    assert_eq!(verdict.winner_task_id.as_deref(), Some("task-1"));
    assert_eq!(verdict.confidence, ComparativeConfidence::High);
    assert_eq!(fixture.judge_calls(), 0);
}
```

Add winner-not-in-ready-set, malformed JSON, tie, medium/low and zero-ready cases.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::experiments::judge --lib`

Expected: FAIL because judge is absent.

- [ ] **Step 3: Implement bounded comparative input**

Input only goal/acceptance/provider, validation/review summaries, risk notes, machine diff digest/stat and Browser evidence summary. Never create a user Diff API or store full patch in group evidence. Invalid output enters NeedsDecision, never guesses.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked orchestrator::experiments::judge --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/experiments/judge.rs src-tauri/src/orchestrator/experiments/reducer.rs src-tauri/src/orchestrator/verifier.rs
git commit -m "feat(orchestrator): compare experiment candidates"
```

### Task 6: Enforce One Winner and One Delivery

**Files:**
- Create: `src-tauri/src/orchestrator/experiments/delivery.rs`
- Modify: `src-tauri/src/orchestrator/repo/experiments.rs`
- Modify: `src-tauri/src/orchestrator/delivery.rs`
- Modify: `src-tauri/src/commands/orchestrator/common.rs`

**Interfaces:**
- Produces: `select_experiment_winner`, `start_experiment_winner_delivery`.

- [ ] **Step 1: Write concurrent winner barrier tests**

```rust
#[tokio::test]
async fn concurrent_reducers_can_select_only_one_winner() {
    let fixture = delivery_fixture_two_ready().await;
    let (a, b) = tokio::join!(fixture.select("task-1"), fixture.select("task-2"));
    assert_eq!([a.unwrap().selected, b.unwrap().selected].into_iter().filter(|v| *v).count(), 1);
    assert_eq!(fixture.delivery_commit_count().await, 0);
}
```

Add loser direct-delivery zero side effect and duplicate winner delivery one Git side effect.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::experiments::delivery --lib`

Expected: FAIL because defense checks are absent.

- [ ] **Step 3: Implement four-layer uniqueness**

CAS experiment from Comparing with null winner, set one Winner and all others Loser/Cancelled. `deliver_task` checks experiment winner/group Delivering before Git operations. High+full-auto starts delivery; otherwise WinnerReady and one “采用推荐” action. Reuse existing delivery lock.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked orchestrator::experiments::delivery --lib && cargo test --locked orchestrator::delivery --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/experiments/delivery.rs src-tauri/src/orchestrator/repo/experiments.rs src-tauri/src/orchestrator/delivery.rs src-tauri/src/commands/orchestrator/common.rs
git commit -m "feat(orchestrator): deliver one experiment winner"
```

### Task 7: Add Owner Protocol and Group-level Remote Outbox

**Files:**
- Create: `src-tauri/src/orchestrator/experiments/{remote_protocol.rs,remote_client.rs,outbox.rs}`
- Modify: `src-tauri/src/orchestrator/repo/{schema.rs,remote.rs}`
- Modify: `src-tauri/src/commands/orchestrator/experiments.rs`
- Modify: `src-tauri/src/net/routes/orchestrator.rs`, `http_server.rs`, `protocol.rs`, `discovery.rs`
- Modify: `docs/p2p-protocol.md`, `scripts/check-p2p-route-inventory.mjs`

**Interfaces:**
- Produces: create/list/detail/approve-winner/cancel routes and one group outbox/mirror.

- [ ] **Step 1: Write lost-response atomicity tests**

```rust
#[tokio::test]
async fn retry_after_lost_response_reuses_same_remote_experiment() {
    let fixture = remote_experiment_fixture().drop_first_response();
    let first = fixture.create_offline(request()).await.unwrap();
    fixture.dispatch_twice().await;
    assert_eq!(fixture.owner_experiment_count().await, 1);
    assert_eq!(fixture.owner_candidate_count().await, 3);
    assert_eq!(fixture.mirror(first.id).await.status, "mirrored");
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::experiments::outbox --lib && cargo test --locked net::routes::orchestrator --lib`

Expected: FAIL because routes/outbox are absent.

- [ ] **Step 3: Implement group transport**

Add one `orchestrator_remote_experiment_outbox` row per request and group mirror. Owner route only accepts local project. Network failure→pending, protocol failure→failed, success transactionally marks mirrored+upserts group/candidates. Old owner unsupported never creates ordinary tasks.

- [ ] **Step 4: Verify protocol**

Run: `cd src-tauri && cargo test --locked orchestrator::experiments::outbox --lib && cargo test --locked net::routes::orchestrator --lib && cargo test --locked net::protocol --lib && cd .. && node scripts/check-p2p-route-inventory.mjs`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/experiments src-tauri/src/orchestrator/repo src-tauri/src/commands/orchestrator/experiments.rs src-tauri/src/net/routes/orchestrator.rs src-tauri/src/net/http_server.rs src-tauri/src/net/protocol.rs src-tauri/src/net/discovery.rs docs/p2p-protocol.md scripts/check-p2p-route-inventory.mjs
git commit -m "feat(p2p): relay candidate experiments atomically"
```

### Task 8: Add Group UI, Attention and Downgrade Guard

**Files:**
- Create: `src-tauri/src/attention/experiment_source.rs`
- Modify: `src-tauri/src/attention/{mod.rs,models.rs,aggregator.rs}`
- Modify: `src-tauri/src/operational_notifications/source.rs`
- Modify: `src-tauri/src/commands/orchestrator/experiments.rs`
- Modify: `web/src/lib/types/orchestrator.ts`
- Modify: `web/src/lib/schemas/{orchestrator.ts,orchestrator.test.ts}`
- Modify: `web/src/api/{orchestrator.ts,orchestrator.test.ts}`
- Modify: `web/src/pages/Orchestrator/controllers/useOrchestratorController.ts`
- Create: `web/src/pages/Orchestrator/views/{OrchestratorExperimentPanel.tsx,OrchestratorExperimentPanel.module.css,OrchestratorExperimentPanel.test.tsx}`
- Create: `web/src/mobile/components/MobileAutomationExperiments.tsx`
- Modify: `web/src/i18n/locales/{zh/orchestrator.json,en/orchestrator.json}`、`web/tests/{workbench.spec.ts,mobile-workbench.spec.ts}`、`docs/prd.md`、`docs/development/{quality-matrix.json,backend-operations.md}`。

**Interfaces:**
- Produces: one `ExperimentNeedsDecision` Attention and local-only `prepare-experiment-downgrade`.

- [ ] **Step 1: Write one-decision/no-diff UI tests**

```tsx
it('shows one recommended action and no diff review controls', () => {
  render(<OrchestratorExperimentPanel experiment={awaitingApprovalExperiment()} />)
  expect(screen.getAllByRole('button', { name: '采用推荐' })).toHaveLength(1)
  expect(screen.queryByText(/Changes|Diff|批注/)).not.toBeInTheDocument()
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- OrchestratorExperimentPanel MobileAutomationExperiments`

Expected: FAIL because views are absent.

- [ ] **Step 3: Implement group-only UX and downgrade quiesce**

Show group progress, candidate state, recommendation reason/risk only. NeedsDecision has accept recommended/select another already-passed candidate/cancel, no patch. Attention only navigates. Downgrade helper refuses Delivering then cancels nonterminal groups/losers while preserving evidence/worktrees.

- [ ] **Step 4: Run full gates**

Run: `cd src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked orchestrator::experiments && cargo test --locked attention:: && cd ../web && npm run check:i18n && npm test -- OrchestratorExperiment MobileAutomationExperiments Attention && npm run build && npm run test:e2e -- workbench.spec.ts mobile-workbench.spec.ts && cd .. && node scripts/check-quality-traceability.mjs && node scripts/check-docs.mjs`

Expected: all exit 0.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/attention src-tauri/src/operational_notifications/source.rs src-tauri/src/commands/orchestrator/experiments.rs web/src/lib/types/orchestrator.ts web/src/lib/schemas/orchestrator.ts web/src/api/orchestrator.ts web/src/pages/Orchestrator web/src/mobile web/src/i18n web/tests docs/prd.md docs/development/quality-matrix.json docs/development/backend-operations.md
git commit -m "feat(orchestrator): surface automated experiments"
```

## Completion Contract

- Group create is atomic/idempotent locally and remotely.
- Candidate tasks cannot enter ordinary delivery; database and delivery code enforce one winner.
- High-confidence full-auto completes without manual diff review.
- Ambiguity creates one group-level decision and no child Attention.

## Plan Self-Review

- Spec coverage: schema, create, capacity, reducer, judge, delivery, remote and UI/rollback each map to a task.
- Placeholder scan: no unresolved implementation placeholders.
- Type consistency: `ExperimentStatus`, `CandidateOutcome`, `ComparativeConfidence` and winner IDs are stable across tasks.
