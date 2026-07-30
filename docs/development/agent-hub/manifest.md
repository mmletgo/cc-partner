# Agent Hub Support Manifest & Package Absent Deactivation

> Status: **R5 quality fix (Codex R5)** — the package-Absent deactivation path is
> gated by `evaluate_target_support_flags`, persists a durable deactivation token,
> and materializes a `Pending` row whose uninstall is owned by the activator. The
> activator is responsible for the actual TreeHash precheck before destructive
> writes. This document pins the contract so the next reviewer/operator does not
> re-derive the gate.

## Scope

| Path component | Authority | Notes |
| --- | --- | --- |
| Capability gate | `agent_hub::service::evaluate_target_support_flags` (`pub(crate)`) | Treats `Supported*` (incl. `SupportedAfterRestart` and `ActivationRequired`) as a single Supported-family. `Blocked` and `ReadOnly` both return `false`. |
| Manifest loader | `agent_hub::support::builtin_support_manifest` | Loads the embedded `support-manifest.json` (currently `minTestedVersion: null` until L3 evidence). Failure is **fail-closed**: callers see `None` and must default to no-Supported. |
| Evaluator | `agent_hub::support::evaluate_target_support(manifest, snapshot)` | Folds record reasons + runtime snapshot into `EvaluatedTargetSupport { mode, capabilities, write_allowed, reasons }`. |
| Asset summary top-level | `agent_hub::service::probe_support_map` | Funnels through `builtin_support_manifest + evaluate_target_support + evaluate_target_support_flags` so a `None` manifest labels **no** target as `Supported`. |

## Package Absent deactivation

When a user/project request `DesiredPresence::Absent` on a managed package, the
projection engine must not delete files blindly. The contract is:

1. **Capability gate** — the projection entry point
   (`projection_ops::schedule_one_binding`) calls
   `absent_write_block_reason(target, env)`, which in turn asks
   `evaluate_target_support_flags(&evaluated, TargetCapability::DeactivatePackage)`.
   If the helper returns `false` (capability is `Blocked` or `ReadOnly`),
   `schedule_one_binding` returns `BindingScheduleOutcome::TerminalBlocked`,
   the dispatcher writes a `Materialization { status: Blocked, last_error: ... }`
   row, and **no executable job is enqueued**. This mirrors the
   `RenderInstruction` gate for the Present path.

2. **Durable deactivation token + `Pending` materialization** — for capability-
   approved Absent requests, the durable intent lives in
   `agent_hub_replication.agent_hub_lan_projection_intents` and
   `agent_hub_repo.upsert_materialization(... MaterializationStatus::Pending ...)`.
   The intent is only marked `done` after the activator has reported
   completion (`packages/activator::run_activator_command`) **and** the
   scheduler has re-validated the durable TreeHash (see step 4). This guarantees
   a crash between intent claim and uninstall does not lose the request.

3. **Uninstall semantics owned by activator** — the activator is the only code
   path allowed to issue destructive writes for an `Absent` package. It runs
   under argv-controlled subprocesses (`FakeProcessRunner` for tests,
   `Command::new` for production), never `std::fs::remove_dir_all` on managed
   roots. The activator is responsible for:
   - argv construction per target (Claude / Codex / OpenCode) using the
     canonical managed-root resolver;
   - short-lived process with bounded stdout/stderr (≤256 KiB) and
     deadline-bounded runtime;
   - translation of failure into either a retryable `tracing::warn!` plus
     re-`Pending` or a terminal `MaterializationStatus::Blocked` with
     `last_error: support_blocked:<reason>`.

4. **TreeHash precheck semantics** — before any uninstall the activator must
   compare the current managed directory's computed `TreeHash` against the
   `expected_tree_hash` recorded in the materialization row. The precheck
   fail-closed rules are:
   - **Hash mismatch with a known good baseline** → refuse to uninstall,
     surface as `MaterializationStatus::Blocked` with
     `last_error: tree_hash_drift:<observed>:<expected>`. The user must
     resolve the drift before deactivation can resume.
   - **Missing manifest root** → activator no-ops and re-`Pending`s; the
     durable intent stays queued. A missing root is **not** the same as
     a confirmed clean uninstall and must not be reported as success.
   - **Empty managed root (post-target removal)** → activator may finalise
     as `MaterializationStatus::Synced` and the intent worker may mark the
     LAN intent as `done`. The expected TreeHash must equal the empty
     manifest hash, otherwise the empty case short-circuits to Blocked.
   - **Read-only / Blocked capability** — even with a matching TreeHash
     the uninstall must not run; the gate at step 1 already blocked the
     job. This is a defence-in-depth re-check so a future engine that
     bypasses the gate cannot accidentally drive the activator.

## Service-surface invariants

- `probe_support_map` is a function over the **same** manifest as
  `probe_all_targets_best_effort`. Both report `Supported*` only when
  `evaluate_target_support_flags` says so; a `None` manifest labels every
  target as `unsupported` / `scanOnly`.
- `evaluate_target_support_flags` is `pub(crate)` so future projection
  tests and downstream engines can assert on the helper without
  re-deriving the matches. It is **not** re-exported as part of the
  Agent Hub DTO surface; UI consumers read the higher-level
  `AgentHubProbeDto.support` (string enum `supported` / `scanOnly` /
  `unsupported`).
- The two new service tests (`probe_support_map_null_manifest_marks_no_target_supported`
  and `evaluate_target_support_flags_exposed_for_crate`) pin both
  contracts: any null manifest → no target supported; the helper
  agrees with `EvaluatedTargetSupport::capability` for every
  `TargetCapability` value.

## Residual / out of scope

- L3 (`minTestedVersion: null`) is still unresolved; until the first
  `support-manifest.json` entry gains evidence the manifest reports
  every target as uncertified (ReadOnly / Blocked). The
  `Gate B` / `Gate D` smoke tests are the canonical evidence anchors.
- Multi-target TreeHash precheck for the **deactivate** path is owned by
  the activator, not the projection scheduler. Future L3 evidence may
  require per-target TreeHash for Claude/Codex/OpenCode managed packages.
