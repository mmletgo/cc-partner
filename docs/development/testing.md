# Testing & Quality Gates

Concise map of **what to run locally**, **which CI job owns it**, and **what is explicitly out of scope**. Domain-level Vitest patterns live in [`web/AGENTS.md`](../../web/AGENTS.md); backend unit/smoke harness details live in [`src-tauri/AGENTS.md`](../../src-tauri/AGENTS.md).

**Authoritative machine-readable coverage** lives in [`quality-matrix.json`](quality-matrix.json). Stable evidence IDs (`E2E-*` / `L0-*` / `L2-*` / `L3-*`) map surface → tests → command → CI job → platforms → exclusions. Validate with:

```bash
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs
```

Docs may only reference registered `E2E-` / `L2-` / `L3-` IDs (`node scripts/check-docs.mjs` checks existence; it does **not** re-validate the full JSON matrix).

## Evidence layers

| Level | Meaning | What it may claim | What it must not claim |
| --- | --- | --- | --- |
| **L0** | Unit / contract (Vitest, pure Rust unit) | Decoder fail-closed, pure policy, schema kinds | Real IPC host, WebView, multi-host LAN |
| **L1** | Deterministic browser mock (Playwright Chromium + backendHarness) | UI journey, optimistic rollback, offline/stale guards, simulated boundary reject | Real Tauri command registration, system permission dialogs, real file dialog, multi-host mDNS / phone QR |
| **L2** | Backend / integration / hosted smoke | CLI lifecycle, PTY, doctor, LAN socket peer + Host/Origin guards (incl. injected peer labels), fault injection seams | Packaged GUI, macOS permission sheets, WSL+tmux on hosted runners, real public NIC peer |
| **L3** | Real-device certification | Packaged GUI, OS permissions, dual-host LAN, 1 GiB transfer — **only when executed** | Any PASS without commit/version/date/expiresAt; substituting L1/L2 |

### Required L1 product journeys (CI `frontend-e2e`)

| ID | Spec | Surface |
| --- | --- | --- |
| `E2E-TRANSFER-001` | `web/tests/transfer.spec.ts` | Transfer send/progress/cancel (recovery matrix covered by unit L0; not dual-host) |
| `E2E-SCRATCH-001` | `web/tests/scratchpad.spec.ts` | Scratchpad unmount flush / save reject |
| `E2E-PROMPTS-001` | `web/tests/prompts.spec.ts` | Prompt create/update/delete rollback |
| `E2E-PERM-001` | `web/tests/permissions.spec.ts` | Permission check fail/retry |
| `E2E-SETTINGS-001` | `web/tests/settings.spec.ts` | Settings partial load + save dirty |
| `E2E-WORKBENCH-001` | `web/tests/workbench.spec.ts` | Workbench stale/offline/files |
| `E2E-WORKBENCH-WINDOW-001` | `web/tests/workbench-window.spec.ts` | Satellite Rail open/occupancy (mock; not multi-monitor GUI) |
| `E2E-MOBILE-001` | `web/tests/mobile-workbench.spec.ts` | Mobile 390×844 navigation/HTTP write |
| `E2E-LAN-001` | `web/tests/lan-boundary.spec.ts` | L1 credential-free + simulated boundary reject |
| `E2E-AGENT-HUB-A-001` | `web/tests/agent-hub.spec.ts` | Agent Hub Gate A: status card / preview+enable / target matrix / conflict deep link / `/claude-md` redirect (mock only) |
| `E2E-AGENT-HUB-B-001` | `web/tests/agent-hub.spec.ts` | Agent Hub Gate B: scope/kind filters / alias / adoption preview / detached restore-remove / delete everywhere (mock only) |
| `E2E-AGENT-HUB-C-001` | `web/tests/agent-hub.spec.ts` | Agent Hub Gate C: LAN source-push selection/progress / unsupported peer / Git inspect+confirm / credential disclosure / stale preview / mapping / Attention deep link (mock only) |
| `E2E-AGENT-HUB-D-001` | `web/tests/agent-hub.spec.ts` | Agent Hub Gate D: Plugin drawer matrix / ownership delete preview / residual statuses / OpenCode catalog fail-closed + effectively-available runner selection / bridge preview deep link / Attention agentHubProjectionBlocked navigation (mock only) |
| `E2E-AGENT-HUB-SHELL-001` | `web/tests/agent-hub-interaction.spec.ts` | Interaction redesign: Agent/scope/tabs shell + context URL (mock only) |
| `E2E-AGENT-HUB-INSTR-3PANE-001` | `web/tests/agent-hub-interaction.spec.ts` | Instruction three-pane lane layouts (common/adapted/exclusive) + analyze original + AI revise calls Claude then saves canonical (mock only) |
| `E2E-AGENT-HUB-DISCOVER-MANAGED-001` | `web/tests/agent-hub-interaction.spec.ts` | Portable inventory: no primary Adopt CTA (mock only) |
| `E2E-AGENT-HUB-ADAPT-FULL-001` | `web/tests/agent-hub-interaction.spec.ts` | Cross-agent full mode forced preview gate (stub generator; mock only) |
| `L2-AGENT-HUB-B-001` | `src-tauri/tests/agent_hub_gate_b_smoke.rs` | Portable discovery / targetOnly isolation / unmanaged config / adoption recovery / credential redaction |
| `L2-AGENT-HUB-ENSURE-MANAGED-001` | `src-tauri/src/agent_hub/portable_inventory/ensure_managed.rs` | ensure_managed ledger promote without adopt; failure isolation |
| `L2-AGENT-HUB-PORTABLE-PARITY-001` | `src-tauri/tests/agent_hub_portable_inventory_smoke.rs` | Isolated HOME/data_dir 3×4 inventory + enable/disable/uninstall preview→apply→rescan, MCP secret privacy, unopted no-write, action request replay |
| `L2-AGENT-HUB-PORTABLE-PULL-001` | `src-tauri/tests/agent_hub_portable_pull_smoke.rs` | Same-agent portable pull: metadata inventory, capability routes, same-target fail-before-transfer, chunk offset resume, install-mode wire tokens |
| `L2-AGENT-HUB-C-001` | `src-tauri/tests/agent_hub_replication_smoke.rs` | Two-owner-style LAN prepare/chunk-resume/commit / idempotency / credential bytes + log redaction / projection-fail-after-commit |
| `L2-AGENT-HUB-C-GIT-001` | `src-tauri/tests/agent_hub_replication_smoke.rs` | Git device-lane expand → third-env inspect/preview/confirm; map one project leave another unmapped |
| `L2-AGENT-HUB-D-PLUGIN-001` | `src-tauri/tests/agent_hub_gate_d_runtime_smoke.rs` | Mixed Plugin projection full/partial/sourceOnly + activationRequired via natural ActivationPlan merge + Snapshot residual CAS + package delete preserve shared/standalone |
| `L2-AGENT-HUB-D-RUNTIME-001` | `src-tauri/tests/agent_hub_gate_d_runtime_smoke.rs` | OpenCode bridge hash/preview/verify + OSC strip + preflight fail-closed + Fresh resume CAS fail-closed order proof (Fresh-arm window; library-level; not real TUI) |
| `L3-AGENT-HUB-B-CLI-001` | `src-tauri/tests/agent_hub_cli_contract.rs` | Real CLI exact-version support pins (manual / ignored; default NOT VERIFIED) |
| `L3-AGENT-HUB-C-LAN-001` | dual-host manual | Real multi-host mDNS agent-hub.v1 source-push + Git confirm import — **NOT VERIFIED** |
| `L3-AGENT-HUB-D-OPENCODE-001` | real OpenCode TUI manual | Exact pinned OpenCode visible session/completion via runtime bridge — **NOT VERIFIED** |

Additional L1 extras (also registered): `E2E-ATTENTION-001`, `E2E-CORE-INTEGRITY-001`, `E2E-FRONTEND-FOUNDATION-001`, `E2E-SCREENSHOT-OVERLAY-001`, `E2E-AGENT-LEDGER-001`.

### L0 / L2 / L3 anchors

| ID | Level | Evidence |
| --- | --- | --- |
| `L0-RUNTIME-SCHEMA-001` | L0 | `web/src/lib/runtimeSchema.test.ts` + `web/src/lib/schemas/*` |
| `L0-TRANSFER-RECOVERY-001` | L0 | Transfer recovery schema/API/UI matrix + Rust model/repo/sender unit (`phase`/`clientOperationId`/action guards) |
| `L0-WORKBENCH-BROWSER-GATE-001` | L0 | Workbench preflight `workspace_view != 'browser'` skips `browserTarget` Select action but still keeps normalized URL in `plan.browser_target_url`; Rust `preflight_{skips_browser_when_view_is_terminal_with_url,restores_browser_when_view_is_browser_with_url,skips_browser_for_files_and_automation_views,has_skip_excludes_browser_skipped_for_non_browser_view}` + Web `workspaceRestore.test.ts` `does not invoke restoreBrowserTarget when plan omits browser Select action` |
| `L2-QUALITY-FAULTS-001` | L2 | `src-tauri/tests/quality_faults.rs` (batch rollback / busy bound / idempotent peer / malformed transfer DTO) — also on Cross-Platform Smoke |
| `L2-TRANSFER-RECOVERY-001` | L2 | `src-tauri/tests/transfer_recovery_smoke.rs` (idempotent resume/retry claim, resume capability, lost-ACK reconcile without second finalize) |
| `L2-LAN-TRUST-BOUNDARY-001` | L2 | `src-tauri/tests/lan_trust_boundary_smoke.rs` |
| `L2-BACKEND-CLI-SMOKE-001` | L2 | `src-tauri/tests/backend_cli_smoke.rs` |
| `L2-BACKEND-DOCTOR-SMOKE-001` | L2 | `src-tauri/tests/backend_doctor_smoke.rs` |
| `L2-AGENT-CLI-SMOKE-001` | L2 | `src-tauri/tests/agent_cli_smoke.rs` + `cargo test --locked agent_cli --lib` |
| `L2-AGENT-HUB-GATE-A-001` | L2 | `src-tauri/tests/agent_hub_gate_a_smoke.rs` — isolated HOME/data-dir Gate A process smoke (opt-in zero-write before enable, nested projection after opt-in, conflict Attention). **Does not** certify real multi-CLI product installs |
| `L2-AGENT-HUB-PORTABLE-PARITY-001` | L2 | `src-tauri/tests/agent_hub_portable_inventory_smoke.rs` — isolated HOME/data_dir portable inventory + local actions (3×4 scan, FakeProcessRunner enable/disable/uninstall, MCP credential fact present/hash only, unopted project no-write, `clientRequestId` action replay). **Does not** certify real product CLI installs |
| `L2-AGENT-HUB-ENSURE-MANAGED-001` | L2 | `cargo test --locked ensure_managed` — discover-as-managed ledger promote without adopt; per-item failure isolation. **Does not** certify real product CLI installs |
| `L2-AGENT-HUB-PORTABLE-PULL-001` | L2 | `src-tauri/tests/agent_hub_portable_pull_smoke.rs` — library-level dual isolated data_dir + frozen loopback peer for `agent-hub.portable-pull.v1` inventory/selection/objects, metadata-only DTO, cross-target fail-before-transfer, 8 MiB chunk resume, install-mode wire tokens. **Does not** certify dual-host mDNS or full dest apply via devices table |
| `L2-AGENT-HUB-C-001` | L2 | `src-tauri/tests/agent_hub_replication_smoke.rs` — isolated dual-data_dir source-push contracts (chunk resume, idempotency, credential plaintext in CAS + absent from logs, projection failure does not roll back). **Does not** certify dual-host mDNS |
| `L2-AGENT-HUB-C-GIT-001` | L2 | Same smoke file Git path: export lane archive → third env inspect/confirm; unmapped projects importable without auto path/opt-in. **Never** auto Git import |
| `L2-AGENT-HUB-D-PLUGIN-001` | L2 | `src-tauri/tests/agent_hub_gate_d_runtime_smoke.rs` — mixed Plugin portable + targetOnly Hook + residual projection, natural ActivationPlan.activation_required merge (not force-flag-only), ownership-aware delete. **Does not** certify real CLI marketplace installs |
| `L2-AGENT-HUB-D-RUNTIME-001` | L2 | Same smoke file runtime path: bridge hash pin, OSC never enters visible terminal, preflight fail-closed, Fresh resume CAS fail-closed order proof (CAS inside Fresh arm before plan/write). **Does not** launch real OpenCode TUI |
| `L2-PTY-SMOKE-001` | L2 | `src-tauri/tests/pty_smoke.rs` |
| `L2-TRANSACTIONAL-RUNTIME-001` | L2 | `src-tauri/tests/transactional_runtime_smoke.rs` |
| `L3-MACOS-GUI-PERMISSIONS-001` | L3 | Packaged macOS GUI + permission grant/deny/retry + screenshot clipboard — **NOT VERIFIED** (canonical aggregate; architecture executions under `macos-aarch64-beta` only) ([real-device-certification.md](real-device-certification.md)) |
| `L3-MACOS-VOICEOVER-001` | L3 | Packaged macOS VoiceOver semantic/focus journey on same Apple Silicon candidate — **NOT VERIFIED** (depends on matching GUI execution for beta profile) |
| `L3-WINDOWS-GUI-001` | L3 | Packaged Windows GUI / transfer dialog / native terminal — **NOT VERIFIED** |
| `L3-WINDOWS-WSL-001` | L3 | Windows WSL + tmux Workbench recovery — **NOT VERIFIED** (separate from native terminal) |
| `L3-UBUNTU-GUI-001` | L3 | AppImage/deb GUI + terminal/files — **NOT VERIFIED** |
| `L3-DUAL-HOST-LAN-001` | L3 | Two physical hosts: mDNS, credential-free native/mobile R/W, boundary reject, remote stop reject, **1GiB mid-transfer disconnect + process restart + resume + SHA-256** — **NOT VERIFIED** (deferred; L2 smoke is not a substitute) |

Honest L3 row schema (version, commit, OS build, status, evidence, date, 90-day expiry) and the full NOT VERIFIED inventory: [`real-device-certification.md`](real-device-certification.md). Do **not** invent caller identity auth. LAN product semantics remain credential-free for legal loopback/LAN peers; Host/Origin/Content-Type and socket peer class are deployment boundaries, not identity.

### Architecture-level PASS vs aggregate NOT VERIFIED

Fixed claim profile **`macos-aarch64-beta`** (`claimMode=platform-beta`) may GO when:

- `L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64` execution is artifact-backed PASS (≤90 days)
- `L3-MACOS-VOICEOVER-001@macos-aarch64` execution is PASS on the **same** subject/RC/package SHA

even if the **canonical** quality-matrix rows for those IDs remain `NOT VERIFIED` / aggregate `PARTIAL` because Intel Mac (`macos-x86_64`) was never executed. Checker owns required IDs/matrices; callers cannot pass arbitrary allowlists. Windows / WSL / Ubuntu / dual-host / mobile / NVDA stay explicit `NOT VERIFIED` and **do not** block this beta profile.

Validate profile contract:

```bash
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs --claim-mode platform-beta --claim-profile macos-aarch64-beta \
  --subject-commit <40hex> --subject-tag <tag> --rc-run-id <id> --evidence-ref <ref>
```

RC / beta publish workflows (do **not** repurpose stable `release-tauri.yml` for beta):

- [`.github/workflows/rc-tauri.yml`](../../.github/workflows/rc-tauri.yml) — single matrix `macos-aarch64` RC + non-releasable updater harness
- [`.github/workflows/release-tauri-beta.yml`](../../.github/workflows/release-tauri-beta.yml) — prerelease-only Apple Silicon assets; no `latest.json`

## Workflow matrix

| Surface | Local command | CI job | Trigger | Verified scope | Explicit exclusions |
| --- | --- | --- | --- | --- | --- |
| Frontend unit | `cd web && npm test` | `frontend-unit` (`CI`) | PR → `master`; push `master` (docs-only push may skip via `paths-ignore`) | Vitest unit tests under `web/` (incl. `L0-RUNTIME-SCHEMA-001`) | No Tauri WebView; no multi-host LAN |
| Frontend E2E | `cd web && npm run test:e2e` | `frontend-e2e` (`CI`) | Same as above | Playwright (Chromium) browser flows (`E2E-*` L1 IDs) | No packaged desktop GUI; no multi-host mDNS |
| Frontend lint + build + static gates | `cd web && npm run lint` · `npm run build` · token/bundle/module gates · `node scripts/check-quality-traceability.mjs` | `quality` (`CI`) | Same as above | ESLint + `tsc -b` + Vite production bundle + CSS token / bundle / module / quality-matrix gates | Not a substitute for unit/E2E or L3 |
| Ubuntu full quality (Rust) | `cd src-tauri && cargo fmt --check` · `cargo clippy --all-targets --locked -- -D warnings` · `cargo test --locked` | `quality` (`CI`) | Same as above | fmt / clippy (deny warnings) / full `cargo test` on **ubuntu-22.04** (incl. `L2-QUALITY-FAULTS-001`) | Not macOS/Windows process or path smoke |
| macOS / Windows smoke | See [local smoke](#local-cross-platform-smoke) | `smoke (macos-latest\|windows-latest)` (`Cross-Platform Smoke`) | Related PR path filter; daily `schedule` UTC `18:23`; `workflow_dispatch` | Backend CLI lifecycle, doctor `--json`, native PTY, **LAN trust boundary smoke**, **quality_faults L2**, transactional runtime, logs rotation/sanitize, focused unit + `cargo check --bins` | **NOT VERIFIED on hosted runners:** WSL + tmux; GUI / WebView; macOS permission dialogs; multi-host mDNS / phone QR / multi-device P2P; real public-peer NIC path; 1GiB dual-host transfer |
| Release installers (stable) | Local: `./start.sh build` (dev); formal: tag only | `build` / `publish-release` / `assemble-latest-json` (`Build & Release (Tauri)`) | Push tag `v*` | Multi-platform installers + `.sig` + `latest.json` assembly | **Not** a quality substitute for `CI` or Cross-Platform Smoke; **not** the beta path |
| RC candidate (macOS arm64 beta infra) | Dispatch only | `validate-subject` / `build-macos-aarch64` (`RC Tauri (macOS arm64)`) | `workflow_dispatch` with subjectTag + subjectCommit | Immutable `macos-aarch64` production + non-releasable updater harness inventory | No Windows/Linux/Intel jobs; no publish; no `latest.json` |
| Beta prerelease (macOS arm64 only) | Dispatch only after GO | `gate` (`Release Tauri Beta (macOS arm64)`) | `workflow_dispatch` with frozen subject/RC/evidence/profile/betaTag | Apple Silicon prerelease assets only | No stable tag; no `latest.json`; no harness publish; no Windows/Linux/Intel assets |
| Documentation facts + matrix ID refs | `node scripts/check-docs.mjs` · `node scripts/check-docs.mjs --self-test` · `node scripts/check-quality-traceability.mjs` | `docs` (`Docs`) | PR/push `master` path filter on Markdown, quality-matrix, docs/traceability checkers, workflow | Relative links, fence balance, scoped stale claims, README command allowlist, evidence ID existence | Not a substitute for product CI/smoke; skips `docs/superpowers/**` |

Workflows:

- [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) — name **CI**
- [`.github/workflows/cross-platform-smoke.yml`](../../.github/workflows/cross-platform-smoke.yml) — name **Cross-Platform Smoke**
- [`.github/workflows/release-tauri.yml`](../../.github/workflows/release-tauri.yml) — name **Build & Release (Tauri)** (stable multi-platform; **not** used for macos-aarch64-beta)
- [`.github/workflows/rc-tauri.yml`](../../.github/workflows/rc-tauri.yml) — name **RC Tauri (macOS arm64)**
- [`.github/workflows/release-tauri-beta.yml`](../../.github/workflows/release-tauri-beta.yml) — name **Release Tauri Beta (macOS arm64)**
- [`.github/workflows/docs.yml`](../../.github/workflows/docs.yml) — name **Docs**

## Local frontend

Install with lockfile only (CI form):

```bash
cd web && npm ci
```

```bash
cd web && npm run lint
cd web && npm run build
cd web && npm test
cd web && npm run test:e2e
# or serial unit + e2e
cd web && npm run test:all
```

### Agent Hub Gate A verification

Focused Gate A commands (instruction foundation):

```bash
# Rust unit / domain
cd src-tauri && cargo test --locked agent_hub

# Process smoke (isolated data dir; serialize)
cd src-tauri && cargo test --locked --test agent_hub_gate_a_smoke -- --test-threads=1

# L1 UI journey (backendHarness mocks) — also covers Gate B UI cases in the same file
cd web && npm run test:e2e -- agent-hub.spec.ts
```

**NOT VERIFIED by the above alone:** real Claude / Codex / OpenCode product installs and path writes on macos/windows/ubuntu; LAN Hub replication / multi-host Agent Hub state; packaged desktop GUI. Those stay L3 `NOT VERIFIED` rows in `quality-matrix.json` until real-device certification lands.

### Agent Hub user-level instruction V2 (scan-only) verification

The V2 workspace intentionally starts with a user-visible inventory rather
than claiming write support. `inspect_user_instruction_workspace` returns each
CLI's effective source chain, real target path, management/projection state,
activation requirement, capability evidence, canonical revision, and
`inventorySnapshotHash`. Inspection is zero target-side writes.

The preview/apply contract is also fail-closed: preview checks the canonical
revision and inventory hash, gives a bounded per-target diff, and creates no
target file. Apply rechecks the stored plan and uses `clientRequestId` replay,
but, while the support manifest has no L3 write/remove evidence, every
target-side create/update/delete is returned as
`USER_INSTRUCTION_TARGET_SCAN_ONLY`. A browser mock or unit test must not
claim that a Claude, Codex, or OpenCode instruction file was written.

Focused L0 contracts:

```bash
cd src-tauri && cargo test --locked agent_hub::user_instructions --lib
cd web && npm test -- useUserInstructionManager
```

- `L0-AGENT-HUB-USER-INSTRUCTION-BACKEND-001` covers bounded content/diff
  behavior, Codex base-path selection, and read-only capability preservation.
  `L0-AGENT-HUB-USER-INSTRUCTION-UI-001` covers the frontend rule that
  drafts/selections remain local until preview; stale apply preserves the
  draft.
- `E2E-AGENT-HUB-USER-INSTRUCTION-001` is the L1 browser-mock journey in
  `web/tests/agent-hub.spec.ts`: the page renders scan-only facts and does not
  silently invoke a V2 write command when the backend has not certified it.

Real filesystem create/update/delete and remove across exact supported CLI
versions are `L3-AGENT-HUB-USER-INSTRUCTION-WRITE-001` and remain **NOT
VERIFIED**. It is additive to the per-CLI
`L3-AGENT-HUB-CLAUDE-001` / `L3-AGENT-HUB-CODEX-001` /
`L3-AGENT-HUB-OPENCODE-001` rows; no L0/L1/L2 result can flip any of them.

### Agent Hub Gate B verification (portable assets)

Focused Gate B commands (shared Skill/Command/Agent/MCP discovery, managed packages, legacy adoption, config patch isolation). Do **not** claim real CLI write support unless the exact L3 version pin was exercised:

```bash
cd src-tauri
cargo fmt --check
# clippy may be heavy; still preferred
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked agent_hub::assets
cargo test --locked agent_hub::targets
cargo test --locked agent_hub::packages
# L2 process smoke — isolated HOME/data; FakeProcessRunner + support bypass
cargo test --locked --test agent_hub_gate_b_smoke -- --nocapture --test-threads=1

cd ../web
npm run lint
npm run check:css-tokens
npm run check:i18n
npm test -- AgentHub agentHub AgentAssetRow localeParity
npm run build
npm run test:e2e -- agent-hub.spec.ts

cd ..
node scripts/check-agent-hub-support-manifest.mjs --gate-b
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
```

Evidence IDs:
- `L2-AGENT-HUB-B-001` — `src-tauri/tests/agent_hub_gate_b_smoke.rs` (PASS when smoke suite green)
- `L2-AGENT-HUB-PORTABLE-PARITY-001` — `src-tauri/tests/agent_hub_portable_inventory_smoke.rs` (PASS when inventory/action smoke green; FakeProcessRunner only)
- `L2-AGENT-HUB-PORTABLE-PULL-001` — `src-tauri/tests/agent_hub_portable_pull_smoke.rs` (PASS when pull smoke green; library + frozen peer, not dual-host mDNS)
- `E2E-AGENT-HUB-B-001` — `web/tests/agent-hub.spec.ts` Gate B describe (PASS when e2e green; else NOT VERIFIED if Playwright env missing)
- `L2-AGENT-HUB-C-001` / `L2-AGENT-HUB-C-GIT-001` — `src-tauri/tests/agent_hub_replication_smoke.rs` (PASS when smoke suite green; dual-host mDNS remains L3)
- `E2E-AGENT-HUB-C-001` — `web/tests/agent-hub.spec.ts` Gate C describe (PASS when e2e green; mock-only, not real multi-host)
- `L2-AGENT-HUB-D-PLUGIN-001` / `L2-AGENT-HUB-D-RUNTIME-001` — `src-tauri/tests/agent_hub_gate_d_runtime_smoke.rs` (PASS when smoke suite green; real OpenCode TUI remains L3)
- `E2E-AGENT-HUB-D-001` — `web/tests/agent-hub.spec.ts` Gate D describe (PASS when e2e green; mock-only, not real TUI/runtime)
- `L3-AGENT-HUB-B-CLI-001` — `src-tauri/tests/agent_hub_cli_contract.rs` ignored real-CLI harness; remains **NOT VERIFIED** without pinned local CLI
- `L3-AGENT-HUB-C-LAN-001` — dual-host agent-hub.v1 + Git confirm; remains **NOT VERIFIED** until real-device certification
- `L3-AGENT-HUB-D-OPENCODE-001` / `L3-AGENT-HUB-OPENCODE-RUNTIME-001` — exact OpenCode visible TUI + runtime bridge evidence; remain **NOT VERIFIED** until real pinned CLI + provider credentials are exercised

### Agent Hub portable asset management parity (inventory / actions / pull)

Backend parity for same-agent portable asset management (B1–B8). Focused commands:

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked --lib agent_hub -- --test-threads=1
cargo test --locked --test agent_hub_portable_inventory_smoke -- --nocapture --test-threads=1
cargo test --locked --test agent_hub_portable_pull_smoke -- --nocapture --test-threads=1

cd ..
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs --self-test
node scripts/check-docs.mjs
```

Evidence IDs:
- `L2-AGENT-HUB-PORTABLE-PARITY-001` — inventory + local action smoke (`agent_hub_portable_inventory_smoke`)
- `L2-AGENT-HUB-PORTABLE-PULL-001` — pull contract smoke (`agent_hub_portable_pull_smoke`)
- Protocol: capability `agent-hub.portable-pull.v1` + routes in `docs/p2p-protocol.md`
- L3 product CLI / dual-host mDNS / packaged GUI remain **NOT VERIFIED** (do not flip from L2)

### Agent Hub Gate D verification (Plugin + OpenCode runtime)

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked agent_hub::plugins
cargo test --locked orchestrator::agent_adapter
cargo test --locked orchestrator::agent_runtime_bridge
cargo test --locked --test agent_hub_gate_d_runtime_smoke -- --nocapture --test-threads=1
# optional: replication still green after Gate D
cargo test --locked --test agent_hub_replication_smoke -- --nocapture --test-threads=1

cd ../web
npm run lint
npm run check:css-tokens
npm run check:i18n
npm test -- AgentHub PluginComponentsDrawer AutomationSettings localeParity
npm run build
npm run test:e2e -- agent-hub.spec.ts

cd ..
node scripts/check-agent-hub-support-manifest.mjs --gate-d
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
```

Official behavior references (documentation only; not CI substitutes):

- https://opencode.ai/docs/cli/
- https://opencode.ai/docs/plugins/
- https://developers.openai.com/codex/cli/reference/
- https://code.claude.com/docs/en/cli-reference

Optional real CLI contract (manual; exact version only):

```bash
cd src-tauri
CC_PARTNER_L3_TARGET=claude cargo test --locked --test agent_hub_cli_contract -- --ignored --nocapture --test-threads=1
CC_PARTNER_L3_TARGET=opencode cargo test --locked --test agent_hub_cli_contract -- --ignored --nocapture --test-threads=1
```

Playwright browsers (match CI; avoid floating installers):

```bash
cd web && npm exec -- playwright install --with-deps chromium
```

Do **not** document or use `npx --yes` / ad-hoc single-file runners for CI-shaped checks.

### Agent Hub program-wide certification (Gates A–D integration)

Run on the integrated tree after Gate D merge (Program Task 5). Prefer serial process/smoke tests; record honest FAIL/partials rather than greenwashing.

```bash
# Rust — prefer --lib for unit filters; serialize smoke/integration
cd src-tauri
# Tauri resource glob needs at least one file under resources/browser-runtime/
# (gitignored; src-tauri/build.rs writes .platform-unavailable if the dir is missing/empty)
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked --lib agent_hub -- --test-threads=1
cargo test --locked --lib orchestrator::agent_adapter
cargo test --locked --test agent_hub_replication_smoke -- --nocapture --test-threads=1
cargo test --locked --test agent_hub_gate_d_runtime_smoke -- --test-threads=1
cargo test --locked --test agent_hub_gate_a_smoke -- --test-threads=1
cargo test --locked --test agent_hub_gate_b_smoke -- --test-threads=1

# Frontend
cd ../web
npm run check:css-tokens   # alias: check:tokens
npm run check:i18n
npm test -- AgentHub attention typeBarrel localeParity
npm run build
npm run test:e2e -- agent-hub.spec.ts
# npm run lint — may carry pre-existing react-hooks / React Compiler debt; do not false-green
# npm run check:bundle — default strict baseline ratchet; local drift hosts may use
#   CC_PARTNER_BUNDLE_RATCHET=final-only (CI remains strict)

# Docs / protocol
cd ..
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs --self-test
node scripts/check-docs.mjs
node scripts/check-agent-hub-support-manifest.mjs --gate-d
git diff --check
```

**Program Task 5 run (integration `sdd/agent-hub-2026-07-29` @ `8d1fa0d7` post Codex R6 (worktree branch `sdd/agent-hub-program-t5`), 2026-07-30):**

| Command | Result |
| --- | --- |
| `cargo fmt --check` | PASS |
| `cargo clippy --all-targets --locked -- -D warnings` | PASS (after local gitignored browser-runtime placeholder) |
| `cargo test --locked --lib agent_hub -- --test-threads=1` | PASS 386 (post Codex R6) |
| same with default/parallel threads | **FAIL flake** (2 importer tests; process-global import fault injection races) — **serial is the certified mode** |
| `cargo test --locked --lib orchestrator::agent_adapter` | PASS 24 |
| `agent_hub_replication_smoke` serial | PASS 2 (`L2-AGENT-HUB-C-001` / `L2-AGENT-HUB-C-GIT-001`) |
| `agent_hub_gate_d_runtime_smoke` serial | PASS 3 (`L2-AGENT-HUB-D-PLUGIN-001` / `L2-AGENT-HUB-D-RUNTIME-001` + honesty NOT VERIFIED guard) |
| `agent_hub_gate_a_smoke` / `agent_hub_gate_b_smoke` serial | PASS 7 / PASS 5 |
| `check:css-tokens` / `check:i18n` | PASS |
| `npm test -- AgentHub attention typeBarrel localeParity` | PASS 109 (20 files); benign jsdom `document is not defined` MutationObserver noise after teardown |
| `npm run build` | PASS |
| `npm run test:e2e -- agent-hub.spec.ts` | PASS 8 (`E2E-AGENT-HUB-A-001` / `E2E-AGENT-HUB-B-001` / `E2E-AGENT-HUB-C-001` / `E2E-AGENT-HUB-D-001`) |
| `npm run lint` | **FAIL** 96 errors / 14 warnings — concentrated in Agent Hub controller/tests (`react-hooks/preserve-manual-memoization` + exhaustive-deps); not claimed green |
| `npm run check:bundle` strict | **FAIL** baseline ratchet overages; `CC_PARTNER_BUNDLE_RATCHET=final-only` PASS hard ceilings only (local drift; CI strict) |
| P2P inventory / quality-traceability / docs / support-manifest `--gate-d` / `git diff --check` | PASS |

**Still NOT VERIFIED (do not claim from this program run):**

- `L3-AGENT-HUB-D-OPENCODE-001` / `L3-AGENT-HUB-OPENCODE-RUNTIME-001` — real pinned OpenCode TUI + provider credentials
- `L3-AGENT-HUB-B-CLI-001` — real Claude/Codex/OpenCode exact-version product writes (`agent_hub_cli_contract` ignored harness)
- `L3-AGENT-HUB-C-LAN-001` — dual-host mDNS `agent-hub.v1` source-push + Git confirm
- `L3-AGENT-HUB-CLAUDE-001` / `L3-AGENT-HUB-CODEX-001` / `L3-AGENT-HUB-OPENCODE-001` (install-path family) — real multi-CLI product installs
- Packaged desktop GUI / multi-platform Agent Hub matrix; marketplace activation side effects; N+2 legacy route/table deletion (migration evidence still locked)

## Local Ubuntu-style quality (Rust + frontend)

```bash
cd web && npm ci && npm run lint && npm run build && npm test
cd src-tauri && cargo fmt --check
cd src-tauri && cargo clippy --all-targets --locked -- -D warnings
cd src-tauri && cargo test --locked
```

Optional inventory / docs / coverage guards (docs + matrix also run in `Docs` workflow; matrix also in `CI` quality):

```bash
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
node scripts/check-docs.mjs --self-test
```

## Local cross-platform smoke

Same crates the smoke workflow runs; **serialize** process/port-sensitive tests:

```bash
cd src-tauri
cargo fmt --check
cargo check --locked --bins
cargo test --locked backend::control::tests -- --nocapture
cargo test --locked backend::cli::tests -- --nocapture
cargo test --locked backend::logging::tests -- --nocapture
cargo test --locked backend::doctor::tests -- --nocapture
cargo test --locked workbench::sessions::tests -- --nocapture
cargo test --locked --test backend_cli_smoke -- --nocapture --test-threads=1
cargo test --locked --test backend_doctor_smoke -- --nocapture --test-threads=1
cargo test --locked agent_cli --lib
cargo test --locked --test agent_cli_smoke -- --nocapture --test-threads=1
cargo test --locked --test pty_smoke -- --nocapture --test-threads=1
cargo test --locked --test lan_trust_boundary_smoke -- --nocapture --test-threads=1
cargo test --locked --test quality_faults -- --nocapture --test-threads=1
cargo test --locked --test transactional_runtime_smoke -- --nocapture --test-threads=1
```

Isolate data away from a real home install:

```bash
export CC_PARTNER_SMOKE_ROOT=/tmp/cc-partner-smoke
```

## LAN trust boundary evidence matrix (S1 fixed semantics)

Product semantics: no identity auth on business APIs; loopback/LAN fully allowed without credentials; socket from `ConnectInfo` only (ignore Forwarded/XFF); Host/Origin/Content-Type + resource limits; stop = loopback + control-file token; browser L1 Playwright owned by S6.

| Scenario | Evidence surface | Status |
| --- | --- | --- |
| Credential-free loopback business read (`GET /api/health`) | `tests/lan_trust_boundary_smoke.rs` bound server | **Verified** (automation) |
| Credential-free same-origin mobile write | bound server `POST /api/mobile/workbench/files/save-text` | **Verified** (automation) |
| Native no-Origin interoperability | bound server `POST /api/sync/pull` without Origin | **Verified** (automation) |
| Denied/public classifier + forwarded spoof | oneshot + `INJECTED_PEER_EVIDENCE` label | **Verified as injected evidence only** — not a real public NIC peer |
| Injected LAN peer business allow | oneshot ConnectInfo `192.168.x` | **Verified as injected evidence only** |
| Valid Host/port, hostile Host, wrong port | bound server Host matrix | **Verified** (automation) |
| Cross-origin / ordinary `Origin: null` | bound server | **Verified** (automation) |
| Simple Content-Type ordinary write reject | bound server `text/plain` | **Verified** (automation) |
| Preview proxy `Origin: null` deferred into handler | bound server proxy path | **Verified** (global guard); **session registry + WS upgrade** = unit-level `workbench::browser_proxy` / `net::lan_guard` |
| Invalid/unknown preview null-origin rejection | unit `opaque_origin_matrix` | **Verified** (unit); not full AppState preview harness in smoke |
| Stop loopback + valid/invalid token | bound server `/api/backend/control/stop` | **Verified** (automation) |
| Stop non-loopback + valid token | oneshot LAN ConnectInfo + `INJECTED_PEER_EVIDENCE` | **Verified as injected evidence only** |
| Global 32 MiB body limit envelope | oneshot production-like stack | **Verified** (automation) |
| Transfer chunk 960 KiB route limit | oneshot chunk route | **Verified** (automation) |
| File text 5 MiB / preview proxy 32 MiB stream limits | unit suites (`file_content` / `browser_proxy`) | **Verified** (unit); not re-run as multi-GB HTTP in smoke |
| Multi-host mDNS discovery | manual / multi-machine | **NOT VERIFIED** |
| Phone QR / Mobile browser on second device | manual | **NOT VERIFIED** |
| Real public-peer path on production NIC | multi-homed / external host | **NOT VERIFIED** — do **not** treat `X-Forwarded-For` injection as production evidence |
| Browser L1 Playwright journey | S6 `web/tests/lan-boundary.spec.ts` | **NOT VERIFIED in S1** (owned by S6; do not duplicate here) |

Local focused re-run:

```bash
cd src-tauri
cargo test --locked --test lan_trust_boundary_smoke -- --nocapture --test-threads=1
cargo test --locked net::lan_guard --lib
cargo test --locked net::http_server --lib
```

## Backend scale & observability (S5)

Local correctness and protocol inventory for bounded Orchestrator claim + paged CC History:

```bash
# Protocol inventory + docs facts (route count +3 for manifest-page/items/push-batch)
node scripts/check-p2p-route-inventory.mjs
node scripts/check-docs.mjs
node scripts/check-docs.mjs --self-test

cd src-tauri
cargo test --locked net::protocol --lib
cargo test --locked net::routes::cc_history --lib
cargo test --locked --test backend_scale -- --nocapture --test-threads=1
# Optional load gate (ignored; prints desensitized JSON only — no IDs/content/path).
# CC_PARTNER_BENCH_POOL=1|2 selects one pool; unset runs both (3 samples each).
# CC_PARTNER_BENCH_POOL=1 cargo test --release --locked --test backend_scale backend_scale_benchmark -- --ignored --nocapture --test-threads=1
# CC_PARTNER_BENCH_POOL=2 cargo test --release --locked --test backend_scale backend_scale_benchmark -- --ignored --nocapture --test-threads=1
```

Non-ignored `scale_safety_*` gates (bounded fixture): request limits, resolver outside SQLite tx, no duplicate CAS claims, partial-batch rollback, `SQLITE_BUSY == 0`.

| Surface | Status |
| --- | --- |
| Paged routes + capability `cc-history.paged-sync.v1` atomic | Verified by inventory + protocol unit tests |
| Mixed-version new↔new / new↔legacy / legacy↔new | Verified (`backend_scale` + mixed_version_harness) |
| Claim IO outside SQLite transaction / CAS no duplicate | Verified (`backend_scale` orchestrator_claim_* + `scale_safety_*`) |
| Production SQLite pool expansion to 2 | **NOT VERIFIED / keep `max_connections(1)`** — Task 8 `backend_scale_benchmark` (release, 6 JSON samples: pool 1×3 + pool 2×3) **did not authorize expansion**. §4.2 gates: (1) pool=1 acquire wait p95 was 27/21/23ms — **not** >50ms for three consecutive runs; (2) pool=2 p95 29/29/33ms did **not** cut wait ≥30% (worse/flat vs baseline); (3) correctness suite green; (4) pool=2 SQLITE_BUSY 0/0/2 **not** ≤ pool=1 baseline 0/0/0; (5) no max≥3. Production remains `max_connections(1)`. |
| Metrics telemetry upload | **Out of product scope** — process-local + sanitized tracing only |

## Hosted smoke: NOT VERIFIED


Do not claim Cross-Platform Smoke (or “CI smoke”) already proves:

| Capability | Status |
| --- | --- |
| WSL + tmux | NOT VERIFIED — hosted runner scope |
| GUI / WebView | NOT VERIFIED — hosted runner scope |
| macOS permission dialogs | NOT VERIFIED — hosted runner scope |
| multi-host mDNS / phone QR / multi-device P2P | NOT VERIFIED — hosted runner scope |
| real public-peer path on production NIC | NOT VERIFIED — injected ConnectInfo/XFF is not production evidence |

Product support for WSL/tmux and desktop GUI is separate from automation coverage. Release packages are produced only by the tag workflow above.

## Separation rules

1. **`CI` (Ubuntu)** gates merge for frontend lint/build/unit/e2e and full Rust quality. Three jobs (`quality`, `frontend-unit`, `frontend-e2e`) run in parallel; no `continue-on-error`.
2. **Cross-Platform Smoke** is a **related-PR + daily** matrix on macOS/Windows for backend lifecycle / PTY / doctor / logs / LAN trust boundary smoke — not a full Ubuntu quality clone, not release packaging.
3. **Release (`v*` tag)** builds installers with prepared sidecar + native Tauri CLI (three-job flow: `build` → `publish-release` → `assemble-latest-json`). It does **not** replace `CI` or smoke.
4. **`Docs`** runs Node-only static fact checks on documentation path changes; it does not install frontend/Rust toolchains.

Backend lifecycle, ports, logs, and `doctor` usage for operators: [`backend-operations.md`](backend-operations.md).


## Transactional runtime smoke

```bash
cd src-tauri
cargo test --locked config_store:: --lib
cargo test --locked config_runtime:: --lib
cargo test --locked cloud_sync::runtime:: --lib
cargo test --locked updater::runtime:: --lib
cargo test --locked health::validation:: --lib
cargo test --locked --test transactional_runtime_smoke -- --nocapture --test-threads=1
```

Cross-Platform Smoke workflow 在 macOS/Windows 上额外跑 `transactional_runtime_smoke`。

**NOT VERIFIED**：真实磁盘满、GUI 全局快捷键冲突、真实 updater 安装/重启。

## Browser verification (A5)

- Unit/FakeEngine: `cd src-tauri && cargo test --locked browser_verification --lib`
- Smoke: `cd src-tauri && cargo test --locked --test browser_verification_smoke -- --nocapture --test-threads=1`
- Prepare managed runtime: `node scripts/prepare-browser-runtime.mjs --self-test` then `--platform current`
- L3 managed Chromium packaging/screenshot across release targets: **NOT VERIFIED** (`L3-BROWSER-VERIFICATION-001`)
- Sandbox audit: iframe must not gain `allow-same-origin`; verification API must not accept `targetUrl`

### Agent Metadata Ledger (A9)

- L0/L2: `cd src-tauri && cargo test --locked workbench::agent_ledger --lib`；privacy `cargo test --locked --test agent_ledger_privacy`
- Frontend unit: `cd web && npm test -- agentLedger AgentLedgerDrawer WorkbenchFleetView`
- L1 E2E (browser mock): `cd web && npm run test:e2e -- agent-metadata-ledger.spec.ts`
- P2P inventory: `node scripts/check-p2p-route-inventory.mjs`（含 `POST /api/workbench/agent-ledger/summary`）
- Cross-device multi-host Fleet ledger join remains **NOT VERIFIED** until L3 certification.
