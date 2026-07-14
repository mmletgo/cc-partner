# N5 Transfer Lifecycle Plan Report

- **Status:** DONE_WITH_CONCERNS
- **Branch:** `sdd/n5-transfer`
- **HEAD:** see `git rev-parse HEAD` on `sdd/n5-transfer` after this report lands (T7 docs `871b7f8`, chore `c678622`)
- **Date:** 2026-07-15

## Task summary

| Task | Summary | SHA |
| --- | --- | --- |
| T1 | Persist phase/failure/logical/attempt/protocol ids + clientOperationId | `bb54804` |
| T2 | Idempotent retry/resume claim + source fingerprint + `transfer.resume.v1` | `9a4743b` |
| T3 | Reconcile uncertain / lost final ACK without second finalize | `18652c2` |
| T4 | Open/Reveal prepare (same-device control plane) | `3be24fd` |
| T5 | Recovery APIs / TS schema surface | `5752198` |
| T6 | UI action matrix + complete recovery actions | `0049760` |
| T7 | Protocol/docs + quality matrix + completion gates + plan report | docs `871b7f8`; chore `c678622`; this report commit |

## Completion Contract

- retry/resume idempotent + source/capability validated: **pass** (L2 smoke + unit)
- uncertain outcomes reconcile before retry; lost ACK does not duplicate finalize: **pass** (`lost_final_ack_reconciles_to_completed_without_second_finalize`)
- Open/Reveal same-device desktop GUI Receive+completed only; P2P/mobile unsupported: **pass** (docs + unit/API matrix)
- UI action matrix matches callbacks: **pass** (Transfer unit matrix); L1 e2e covers send/cancel baseline

## Completion gates

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | **pass** |
| `cargo clippy --all-targets --locked -- -D warnings` | **pass** (after pre-existing dead_code silence) |
| `cargo test --locked` | **concern**: default parallel occasionally flakes (inject APPLY_FAIL residual races; browser_proxy body-limit network 502). Serial `--test-threads=1` integration suite **pass** (incl. `transfer_recovery_smoke` 5/5). Isolated re-runs of lib flakes **pass**. |
| `npm run lint` | **pass** (1 pre-existing hooks warning in Workbench controller test) |
| `npm run build` | **pass** |
| `npm test` | **concern**: 6 Settings safe-save concurrent-edit tests timeout consistently (`Settings.test.tsx`, pre-existing / unrelated to transfer). Other suites green (879 pass). |
| `npm run test:e2e` | **pass** (31/31) |
| `node scripts/check-p2p-route-inventory.mjs` | **pass** |
| `node scripts/check-quality-traceability.mjs` | **pass** (30 entries; added `L0-TRANSFER-RECOVERY-001`, `L2-TRANSFER-RECOVERY-001`) |
| `node scripts/check-docs.mjs` | **pass** |

## Durable finalize invariant

- `src-tauri/tests/transfer_recovery_smoke.rs::lost_final_ack_reconciles_to_completed_without_second_finalize`: status-completed after lost complete ACK → local Succeeded; operation query does **not** re-invoke complete; finalize/complete count stays in single convergence window.
- Receiver hash mismatch rejects place (`finalize_transfer` / intent recovery SHA strict match); sender source fingerprint mismatch → `source_changed` (`transfer::sender` unit).

## 1 GiB dual-host resume

- **NOT VERIFIED / deferred L3** — `L3-DUAL-HOST-LAN-001` remains status `NOT VERIFIED`.
- Scenario documented in `docs/development/real-device-certification.md` (mid-stream disconnect + process restart + resume from confirmed offset + SHA-256).
- L0/L2 entries explicitly exclude this surface; must not mark PASS from Mac-only N8 or `transfer_recovery_smoke`.

## Docs updated

- `docs/prd.md` — Transfer recovery lifecycle + action matrix + L3 handoff
- `docs/p2p-protocol.md` — `transfer.resume.v1` capability + lifecycle contract (clientOperationId vs request id, stable protocol id, fingerprint, old-peer fallback, Open/Reveal, lost ACK)
- `src-tauri/CLAUDE.md` / `web/CLAUDE.md` — recovery commands, fingerprint, UI matrix, evidence IDs
- `docs/development/quality-matrix.json` — `L0-TRANSFER-RECOVERY-001`, `L2-TRANSFER-RECOVERY-001`; dual-host stays NOT VERIFIED
- `docs/development/testing.md` + `real-device-certification.md` — anchors + deferred 1 GiB checklist

## Residual risks

1. **Parallel `cargo test` flakiness** on shared inject static / env `CC_PARTNER_DATA_DIR` / local network proxy tests — not transfer-specific; serial or re-run green.
2. **Settings safe-save unit timeouts** (6 tests) block full `npm test` green; unrelated to N5 transfer recovery surface.
3. **1 GiB dual-host resume still unverified** — product claim must stay deferred until real two-host certification.
4. **Old peers without `transfer.resume.v1`** correctly force full retry; operators may misread "continue" absence as regression.
5. **Open/Reveal** depends on same-device control owner + plugin-opener permissions; path missing/permission failures are typed but still UX friction.

## Not done by design

- No push / no PR / no dual review launched.
