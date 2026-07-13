# Testing & Quality Gates

Concise map of **what to run locally**, **which CI job owns it**, and **what is explicitly out of scope**. Domain-level Vitest patterns live in [`web/CLAUDE.md`](../../web/CLAUDE.md); backend unit/smoke harness details live in [`src-tauri/CLAUDE.md`](../../src-tauri/CLAUDE.md).

## Matrix

| Surface | Local command | CI job | Trigger | Verified scope | Explicit exclusions |
| --- | --- | --- | --- | --- | --- |
| Frontend unit | `cd web && npm test` | `frontend-unit` (`CI`) | PR → `master`; push `master` (docs-only push may skip via `paths-ignore`) | Vitest unit tests under `web/` | No Tauri WebView; no multi-host LAN |
| Frontend E2E | `cd web && npm run test:e2e` | `frontend-e2e` (`CI`) | Same as above | Playwright (Chromium) browser flows | No packaged desktop GUI; no multi-host mDNS |
| Frontend lint + build | `cd web && npm run lint` · `cd web && npm run build` | `quality` (`CI`) | Same as above | ESLint + `tsc -b` + Vite production bundle | Not a substitute for unit/E2E |
| Ubuntu full quality (Rust) | `cd src-tauri && cargo fmt --check` · `cargo clippy --all-targets --locked -- -D warnings` · `cargo test --locked` | `quality` (`CI`) | Same as above | fmt / clippy (deny warnings) / full `cargo test` on **ubuntu-22.04** | Not macOS/Windows process or path smoke |
| macOS / Windows smoke | See [local smoke](#local-cross-platform-smoke) | `smoke (macos-latest\|windows-latest)` (`Cross-Platform Smoke`) | Related PR path filter; daily `schedule` UTC `18:23`; `workflow_dispatch` | Backend CLI lifecycle, doctor `--json`, native PTY, logs rotation/sanitize, focused unit + `cargo check --bins` | **NOT VERIFIED on hosted runners:** WSL + tmux; GUI / WebView; macOS permission dialogs; multi-host mDNS |
| Release installers | Local: `./start.sh build` (dev); formal: tag only | `build` / `publish-release` / `assemble-latest-json` (`Build & Release (Tauri)`) | Push tag `v*` | Platform installers + `.sig` + `latest.json` assembly | **Not** a quality substitute for `CI` or Cross-Platform Smoke |
| Documentation facts | `node scripts/check-docs.mjs` · `node scripts/check-docs.mjs --self-test` | `docs` (`Docs`) | PR/push `master` path filter on `**/*.md`, `scripts/check-docs.mjs`, workflow | Relative links, fence balance, scoped stale claims, README command allowlist | Not a substitute for product CI/smoke; skips `docs/superpowers/**` |

Workflows:

- [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) — name **CI**
- [`.github/workflows/cross-platform-smoke.yml`](../../.github/workflows/cross-platform-smoke.yml) — name **Cross-Platform Smoke**
- [`.github/workflows/release-tauri.yml`](../../.github/workflows/release-tauri.yml) — name **Build & Release (Tauri)**
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

Optional inventory / docs guards (docs guard also runs in `Docs` workflow):

```bash
node scripts/check-p2p-route-inventory.mjs
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
cargo test --locked --test pty_smoke -- --nocapture --test-threads=1
```

Isolate data away from a real home install:

```bash
export CC_PARTNER_SMOKE_ROOT=/tmp/cc-partner-smoke
```

## Hosted smoke: NOT VERIFIED

Do not claim Cross-Platform Smoke (or “CI smoke”) already proves:

| Capability | Status |
| --- | --- |
| WSL + tmux | NOT VERIFIED — hosted runner scope |
| GUI / WebView | NOT VERIFIED — hosted runner scope |
| macOS permission dialogs | NOT VERIFIED — hosted runner scope |
| multi-host mDNS | NOT VERIFIED — hosted runner scope |

Product support for WSL/tmux and desktop GUI is separate from automation coverage. Release packages are produced only by the tag workflow above.

## Separation rules

1. **`CI` (Ubuntu)** gates merge for frontend lint/build/unit/e2e and full Rust quality. Three jobs (`quality`, `frontend-unit`, `frontend-e2e`) run in parallel; no `continue-on-error`.
2. **Cross-Platform Smoke** is a **related-PR + daily** matrix on macOS/Windows for backend lifecycle / PTY / doctor / logs — not a full Ubuntu quality clone, not release packaging.
3. **Release (`v*` tag)** builds installers with prepared sidecar + native Tauri CLI (three-job flow: `build` → `publish-release` → `assemble-latest-json`). It does **not** replace `CI` or smoke.
4. **`Docs`** runs Node-only static fact checks on documentation path changes; it does not install frontend/Rust toolchains.

Backend lifecycle, ports, logs, and `doctor` usage for operators: [`backend-operations.md`](backend-operations.md).
