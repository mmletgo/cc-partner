# Agent Hub Support Manifest, Package Absent Deactivation & User Instruction V2

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
| Manifest loader | `agent_hub::support::builtin_support_manifest` | Loads the embedded `support-manifest.json`. **2026-08-08 phase-1 pin:** Claude `2.1.0`–`2.1.207` and Codex `0.145.0` expose Supported* write caps (except `liveReload`); OpenCode remains `min/current=null` + write blocked until installed and certified. Failure is **fail-closed**. |
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

## User-level instruction V2: inspect is useful before write certification

`agent_hub::user_instructions` owns the V2 workspace for the stable user
scope `agent-hub-scope-user`. Its first deliverable is a truthful inventory,
not an optimistic multi-CLI writer. The authoritative read entry point is
`inspect_user_instruction_workspace` (with the injected-environment variant
used by tests).

### Read contract

- Inspect reads the Hub asset/revision/binding/materialization records and the
  three CLI adapters' source chains. It returns canonical revision metadata,
  the real target path, ordered native/override/fallback/shadowed sources,
  ownership, projection state, activation requirement, and per-operation
  capability/evidence. It must not create a binding, materialization,
  ownership record, or target-side file.
- `inventorySnapshotHash` is the freshness fence for a later preview. It is
  derived from the scope, canonical head, target config/version/path,
  management and capability facts, plus source role/presence/hash/ownership;
  it intentionally excludes refresh time and unbounded instruction text. A
  caller must send the current hash and base revision back to preview; an
  obsolete value is rejected rather than silently rebased.
- Canonical editor content is bounded for the loopback control response. A
  truncated value is explicitly marked `contentTruncated`; it is not safe to
  overwrite, delete, or use as a full diff base. Source hashing is likewise
  bounded, so a giant external file remains inspectable without turning the
  control plane into an unbounded content transport.

### Preview / apply safety contract

- Preview may persist a short-lived, owner-local plan, but it performs no
  canonical or target filesystem mutation. Every plan is tied to its owner,
  base revision, inventory hash, expected source hash, bounded diff, and
  expiry. A plan is opaque to the client: apply receives only its token and a
  stable `clientRequestId`.
- Apply claims that token atomically and replays the same request result. It
  rechecks expiry, owner, revision, inventory, source hash, ownership, empty
  target-only render, and diff truncation before reporting each target result.
  A stale plan is a per-target `stalePreview`, never an implicit refresh or
  write.
- **Current release state is scan-only for target writes/removes.** While the
  embedded support manifest has no L3-certified target write/remove evidence,
  any create/update/delete change ends as
  `USER_INSTRUCTION_TARGET_SCAN_ONLY`. The UI may show source facts, compare
  and preview intent, but cannot treat a successful inspect or preview as
  proof that a CLI file was changed. No client-side fallback is allowed for a
  missing V2 mutation command.

### Evidence boundary

The L0 and browser-mock entry points exercise inventory/decode/draft/preview
and stale-plan behavior. They do **not** certify a real CLI path write or
remove. Real user-level write/remove evidence remains the existing
`L3-AGENT-HUB-CLAUDE-001`, `L3-AGENT-HUB-CODEX-001`, and
`L3-AGENT-HUB-OPENCODE-001` rows, plus the V2 aggregate row
`L3-AGENT-HUB-USER-INSTRUCTION-WRITE-001`; all are **NOT VERIFIED** until an
exact version, commit, platform, date, and expiry are recorded.

## Residual / out of scope

- L3 (`minTestedVersion: null`) is still unresolved; until the first
  `support-manifest.json` entry gains evidence the manifest reports
  every target as uncertified (ReadOnly / Blocked). The
  `Gate B` / `Gate D` smoke tests are the canonical evidence anchors.
- User instruction V2 does not weaken that gate. A fixture, browser mock, or
  source scan cannot be promoted into a target write/remove certification.
- Multi-target TreeHash precheck for the **deactivate** path is owned by
  the activator, not the projection scheduler. Future L3 evidence may
  require per-target TreeHash for Claude/Codex/OpenCode managed packages.
