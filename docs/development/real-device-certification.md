# Real-Device Certification (L3)

Honest L3 evidence for packaged GUI, OS permissions, WSL, and multi-host LAN.
This document **must not** claim PASS for surfaces that were not executed on real
hardware. Browser mock (L1) and bound-server / injected-peer automation (L2)
**do not** satisfy L3 rows.

**Product boundary (fixed):** no caller identity auth on business APIs; any
device that can reach the LAN listener may read/write/execute; backend stop is
**loopback + control-file token only**. There is no pairing flow and no
per-device credential gate.

Authoritative machine-readable rows live in
[`quality-matrix.json`](./quality-matrix.json). Hosted-runner exclusions remain
in [`testing.md`](./testing.md).

## Fixed beta profile (infrastructure)

Current executable claim profile (N8):

| Field | Value |
| --- | --- |
| claimMode | `platform-beta` |
| claimProfile | `macos-aarch64-beta` |
| selectedMatrixIds | `macos-aarch64` only |
| requiredExecutions | `L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64`, `L3-MACOS-VOICEOVER-001@macos-aarch64` |
| evidence validity | 90 days from execution |
| release upper bound | prerelease-only Apple Silicon assets; **no** stable `latest.json` |

**Architecture vs aggregate:** an Apple Silicon execution may be PASS while the
canonical quality-matrix row stays `NOT VERIFIED` / aggregate `PARTIAL` because
Intel Mac was never run. The checker consumes matching architecture manifests
only; it does **not** promote aggregate prose to full PASS. Deferred platforms
(Windows, WSL, Ubuntu, Intel Mac, dual-host, iOS, Android, NVDA) remain
`NOT VERIFIED` and do not block `macos-aarch64-beta`.

Infrastructure (must be in `subjectCommit` before freeze):

- `scripts/check-quality-traceability.mjs` — fixed profile dependency closure + self-tests
- `.github/workflows/rc-tauri.yml` — single-matrix RC + non-releasable harness
- `.github/workflows/release-tauri-beta.yml` — beta-only evidence-aware publish gate
- `scripts/prepare-updater-certification-harness.mjs` + `serve-updater-certification.mjs` + `src-tauri/tauri.updater-certification.conf.json`

Stable multi-platform publish remains `.github/workflows/release-tauri.yml` and is
**not** invoked for this beta profile.

## Certification row schema

Each **canonical** matrix row records:

| Field | Meaning |
| --- | --- |
| surface | Capability under test |
| appVersion / version | `tauri.conf.json` version of the binary under test |
| commit | Full git SHA of the tree under test |
| osBuild | OS product/build string of the device |
| status | `PASS` / `FAIL` / `NOT VERIFIED` |
| evidence | Sanitized path to logs/screenshots (no home paths, tokens, or project content) |
| date | UTC date of execution (`YYYY-MM-DD`) |
| expiresAt | `date + 90 days`; after expiry the row is historical only |

Each **architecture execution** manifest (under `docs/development/evidence/<ID>/`)
additionally binds: `artifactMatrixId`, full `subjectCommit`, RC run id, package
filename/SHA, redacted `deviceClass`, checklist, and relative artifact SHAs.
`evidenceCommit` is resolved from the protected evidence ref and is **not**
embedded in the manifest.

Missing device, missing binary, or unrun checklist → **`NOT VERIFIED`**. Do not
fabricate PASS from CI, Playwright, or single-host smoke.

## Frozen Apple Silicon candidate (N8 Task 2 local)

Local freeze-ready candidate prepared on host `arm64` after integrated L0–L2
gates. **No remote tag push, no RC dispatch, no publish** until the operator
explicitly authorizes network actions.

| Field | Value |
| --- | --- |
| appVersion | `0.7.0` |
| subjectCommit | `15f23372b9bc9fce86a4062c3725cbb71d638446` |
| planned subjectTag | `subject-0.7.0-macos-aarch64` (must **not** match stable `v*` push trigger) |
| planned betaTag (later) | `v0.7.0-beta.1` (must differ from subjectTag; prerelease only) |
| planned evidenceRef | `evidence/n8-0.7.0-macos-aarch64` |
| host arch | `arm64` (confirmed `uname -m`) |
| RC workflow run id | **not assigned** — blocked pending authorized tag push + `rc-tauri.yml` dispatch |
| product/checker/workflow mutation | **frozen for this candidate** after `subjectCommit`; only allowlisted evidence paths may diverge |
| L0–L2 gates | **local PASS** on freeze-ready tree (see Task 2 report) |
| L3 executions | **not run** — remain honest `NOT VERIFIED` |

Required operator commands after authorization (do not invent run IDs):

```bash
# From a clone that contains subjectCommit on origin after branch push (if needed):
git tag -a subject-0.7.0-macos-aarch64 15f23372b9bc9fce86a4062c3725cbb71d638446 -m "N8 subject freeze 0.7.0 macos-aarch64"
git push origin refs/tags/subject-0.7.0-macos-aarch64

gh api \
  --method POST \
  -H "Accept: application/vnd.github+json" \
  /repos/<owner>/<repo>/actions/workflows/rc-tauri.yml/dispatches \
  -f ref='subject-0.7.0-macos-aarch64' \
  -f 'inputs[subjectTag]=subject-0.7.0-macos-aarch64' \
  -f 'inputs[subjectCommit]=15f23372b9bc9fce86a4062c3725cbb71d638446'
```

After RC succeeds: verify `head_sha`/`conclusion`, download
`rc-macos-aarch64-production` / `rc-macos-aarch64-harness` /
`rc-macos-aarch64-inventory`, confirm live retention, inventory SHA, and
production contamination scan. Create protected docs-only evidence ref only for
allowlisted paths under `README.md`,
`docs/development/{quality-matrix.json,real-device-certification.md,release-claim.json}`
and `docs/development/evidence/**`. Do **not** publish beta in this task.

## Matrix (current)

**Recorded at:** 2026-07-15  
**Infrastructure baseline:** N8 Task 1 (checker/RC/beta gate/harness)  
**App version baseline:** `0.7.0`  
**subjectCommit (product freeze):** `15f23372b9bc9fce86a4062c3725cbb71d638446`  
**Canonical row commit baseline:** still unexecuted; matrix rows below remain
`NOT VERIFIED` until Tasks 3–4 write architecture executions.

All L3 surfaces below remain **not** executed as packaged real-device
certification until Tasks 3–4. Canonical status is therefore **NOT VERIFIED**.
Architecture executions for `macos-aarch64` are not yet written. Deferred
platforms (Windows, WSL, Ubuntu, Intel Mac, dual-host, iOS, Android, NVDA) stay
`NOT VERIFIED` and do not block this beta profile.

| ID | Surface | appVersion | commit | OS build | status | evidence | date | expiresAt |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| L3-MACOS-GUI-PERMISSIONS-001 | macOS packaged GUI launch; screen/accessibility/input/notification grant-deny-retry; screenshot clipboard; updater via loopback harness | 0.7.0 | 15f2337… | n/a (no host run) | **NOT VERIFIED** | none | 2026-07-15 | 2026-10-13 |
| L3-MACOS-VOICEOVER-001 | macOS VoiceOver: LAN disclosure, Home/Trending, Workbench, Dialog/Drawer, live region, terminal, Attention, Human Review, WORKFLOW | 0.7.0 | 15f2337… | n/a (no host run) | **NOT VERIFIED** | none | 2026-07-15 | 2026-10-13 |
| L3-WINDOWS-GUI-001 | Windows packaged GUI; file transfer path/dialog; native terminal | 0.7.0 | 15f2337… | n/a | **NOT VERIFIED** | none | 2026-07-15 | 2026-10-13 |
| L3-WINDOWS-WSL-001 | Windows WSL + tmux Workbench terminal recovery | 0.7.0 | 15f2337… | n/a | **NOT VERIFIED** | none | 2026-07-15 | 2026-10-13 |
| L3-UBUNTU-GUI-001 | Ubuntu AppImage/deb GUI; terminal + file flows | 0.7.0 | 15f2337… | n/a | **NOT VERIFIED** | none | 2026-07-15 | 2026-10-13 |
| L3-DUAL-HOST-LAN-001 | Two physical hosts same LAN: mDNS; native P2P + mobile credential-free R/W; public peer / XFF / Host / Origin / WS / remote stop rejection; **1GiB transfer: mid-stream disconnect + process restart + resume from confirmed offset + SHA-256 match** | 0.7.0 | 15f2337… | n/a | **NOT VERIFIED** | none | 2026-07-15 | 2026-10-13 |

### What automated layers already cover (not L3)

| Layer | Evidence | Does **not** prove |
| --- | --- | --- |
| L1 Playwright | `web/tests/*` including `lan-boundary.spec.ts` | Tauri command registration, WebView, OS permission dialogs, multi-host mDNS |
| L2 quality_faults | `src-tauri/tests/quality_faults.rs` (batch/busy/peer/malformed DTO) | GUI, WSL, multi-host |
| L2 LAN smoke | `src-tauri/tests/lan_trust_boundary_smoke.rs` | Real public NIC peer, phone QR, two physical hosts, 1GiB resume |
| Hosted Cross-Platform Smoke | macOS/Windows CLI/PTY/doctor/LAN bound matrix + quality_faults | WSL+tmux, GUI/WebView, multi-host mDNS |

## Checklist for future PASS rows

Only mark `PASS` when **all** of the following are true for that row:

1. Packaged binary built from the recorded commit (or installers from the
   matching release tag) was installed on the named OS build.
2. Human operator executed the surface checklist on real hardware (not only
   headless CI).
3. Sanitized evidence exists under a non-sensitive path (redact home, tokens,
   project names, file contents).
4. For `L3-DUAL-HOST-LAN-001`: two **physical** hosts on the same L2/L3
   network completed mDNS discovery, credential-free native + `/mobile`
   read/write, boundary rejections (public peer, XFF spoof, hostile
   Host/Origin, invalid WebSocket Origin, remote stop), **and** the deferred
   **1 GiB resume scenario** below. Injected `ConnectInfo` / `X-Forwarded-For`
   is **not** enough. `L2-TRANSFER-RECOVERY-001` / single-host N8 smoke is
   **not** a substitute and must not flip this row to PASS.
5. `expiresAt` is set to execution date + 90 days.

### Deferred 1 GiB dual-host transfer resume (N5 → L3 handoff)

Keep **`L3-DUAL-HOST-LAN-001` = NOT VERIFIED** until an operator executes all of:

1. Host A sends a **≥1 GiB** file to Host B over real LAN (not loopback-only).
2. Mid-stream **network disconnect** after a confirmed non-zero `resume_offset`
   (receiver tmp partially written; both peers show failed/interrupted with
   resume metadata where supported).
3. Optional: kill/restart the **cc-partner-backend / app process** on either side
   while the partial tmp + protocol transfer id remain durable.
4. User chooses **继续传输** (`resume_transfer` with stable `clientOperationId` +
   reused `protocolTransferId`) when peer advertises `transfer.resume.v1`; if
   peer lacks the capability, only full **重新传输** is offered (no fake resume).
5. Transfer reaches `completed` on both sides; **receiver SHA-256 equals source**;
   Open/Reveal works only on Host B desktop GUI for the received path.
6. Record sanitized evidence path, OS builds of both hosts, app version, and
   full git commit; then set status PASS with `expiresAt = date + 90d`.

After expiry, documentation may only say “historically passed on &lt;date&gt;;
current tree not re-certified.”

## NOT VERIFIED inventory (full)

The following remain **NOT VERIFIED** until a real-device pass is recorded:

1. macOS packaged GUI launch and WebView (Apple Silicon execution planned for N8; Intel unexecuted)
2. macOS screen recording / accessibility / input monitoring / notification dialogs (grant, deny, retry)
3. macOS region screenshot → clipboard round-trip on real display
4. macOS VoiceOver semantic journey on packaged candidate (`L3-MACOS-VOICEOVER-001`)
5. macOS Intel (`macos-x86_64`) GUI/permissions + VoiceOver (deferred; not in current beta)
6. Windows packaged GUI launch and WebView
7. Windows file transfer path picker / receive directory dialogs
8. Windows native terminal (non-WSL) interactive shell in Workbench
9. Windows WSL distribution + tmux Workbench session recovery
10. Windows NVDA accessibility
11. Ubuntu AppImage GUI
12. Ubuntu deb package GUI
13. Ubuntu terminal + file tree flows in packaged app
14. Two physical hosts mDNS discovery (`_cc-partner._tcp`)
15. Two physical hosts native P2P credential-free read/write/actions
16. Two physical hosts mobile `/mobile` credential-free read/write/actions from a second device
17. Real public-peer / non-LAN NIC path rejection on production interfaces
18. Production XFF/Host/Origin/WebSocket boundary on multi-homed hosts (beyond injected L2 evidence)
19. Remote backend stop rejection from a second physical host (valid token still forbidden)
20. 1GiB file transfer: mid-stream disconnect + process restart + resume from confirmed offset + SHA-256 match across two physical hosts (`L3-DUAL-HOST-LAN-001`, deferred from N5; remains **NOT VERIFIED**)
21. Physical iOS Safari / Android Chrome mobile workbench

## How to re-run L2 automation (not L3)

```bash
cd src-tauri
cargo test --locked --test quality_faults -- --nocapture --test-threads=1
cargo test --locked --test lan_trust_boundary_smoke -- --nocapture --test-threads=1
```
