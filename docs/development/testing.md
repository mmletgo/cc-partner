# Testing & Quality Gates

Concise map of **what to run locally**, **which CI job owns it**, and **what is explicitly out of scope**. Domain-level Vitest patterns live in [`web/CLAUDE.md`](../../web/CLAUDE.md); backend unit/smoke harness details live in [`src-tauri/CLAUDE.md`](../../src-tauri/CLAUDE.md).

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
| `E2E-MOBILE-001` | `web/tests/mobile-workbench.spec.ts` | Mobile 390×844 navigation/HTTP write |
| `E2E-LAN-001` | `web/tests/lan-boundary.spec.ts` | L1 credential-free + simulated boundary reject |
| `E2E-AGENT-HUB-A-001` | `web/tests/agent-hub.spec.ts` | Agent Hub Gate A: status card / preview+enable / target matrix / conflict deep link / `/claude-md` redirect (mock only) |

Additional L1 extras (also registered): `E2E-ATTENTION-001`, `E2E-CORE-INTEGRITY-001`, `E2E-FRONTEND-FOUNDATION-001`, `E2E-SCREENSHOT-OVERLAY-001`, `E2E-AGENT-LEDGER-001`.

### L0 / L2 / L3 anchors

| ID | Level | Evidence |
| --- | --- | --- |
| `L0-RUNTIME-SCHEMA-001` | L0 | `web/src/lib/runtimeSchema.test.ts` + `web/src/lib/schemas/*` |
| `L0-TRANSFER-RECOVERY-001` | L0 | Transfer recovery schema/API/UI matrix + Rust model/repo/sender unit (`phase`/`clientOperationId`/action guards) |
| `L2-QUALITY-FAULTS-001` | L2 | `src-tauri/tests/quality_faults.rs` (batch rollback / busy bound / idempotent peer / malformed transfer DTO) — also on Cross-Platform Smoke |
| `L2-TRANSFER-RECOVERY-001` | L2 | `src-tauri/tests/transfer_recovery_smoke.rs` (idempotent resume/retry claim, resume capability, lost-ACK reconcile without second finalize) |
| `L2-LAN-TRUST-BOUNDARY-001` | L2 | `src-tauri/tests/lan_trust_boundary_smoke.rs` |
| `L2-BACKEND-CLI-SMOKE-001` | L2 | `src-tauri/tests/backend_cli_smoke.rs` |
| `L2-BACKEND-DOCTOR-SMOKE-001` | L2 | `src-tauri/tests/backend_doctor_smoke.rs` |
| `L2-AGENT-CLI-SMOKE-001` | L2 | `src-tauri/tests/agent_cli_smoke.rs` + `cargo test --locked agent_cli --lib` |
| `L2-AGENT-HUB-GATE-A-001` | L2 | `src-tauri/tests/agent_hub_gate_a_smoke.rs` — isolated HOME/data-dir Gate A process smoke (opt-in zero-write before enable, nested projection after opt-in, conflict Attention). **Does not** certify real multi-CLI product installs |
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

Focused Gate A commands (instruction foundation only; do **not** claim full multi-CLI skill/MCP/plugin sync):

```bash
# Rust unit / domain
cd src-tauri && cargo test --locked agent_hub

# Process smoke (isolated data dir; serialize)
cd src-tauri && cargo test --locked --test agent_hub_gate_a_smoke -- --test-threads=1

# L1 UI journey (backendHarness mocks)
cd web && npm run test:e2e -- agent-hub.spec.ts
```

**NOT VERIFIED by the above:** real Claude / Codex / OpenCode product installs and path writes on macos/windows/ubuntu; Skill / MCP / Plugin surfaces; LAN Hub replication / multi-host Agent Hub state; packaged desktop GUI. Those stay L3 `NOT VERIFIED` rows in `quality-matrix.json` until real-device certification lands.

Playwright browsers (match CI; avoid floating installers):

```bash
cd web && npm exec -- playwright install --with-deps chromium
```

Do **not** document or use `npx --yes` / ad-hoc single-file runners for CI-shaped checks.

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
