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

## Certification row schema

Each row records:

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

Missing device, missing binary, or unrun checklist → **`NOT VERIFIED`**. Do not
fabricate PASS from CI, Playwright, or single-host smoke.

## Matrix (current)

**Recorded at:** 2026-07-14  
**Branch:** `sdd/s6-quality-architecture-governance`  
**App version baseline:** `0.6.7`  
**Commit baseline (branch tip when matrix was authored; certification commit SHA
is reported after `test: certify cross surface quality matrix` lands):**
`987ec3c32b18f83c231e4b7921c18dc850cd1409`

All L3 surfaces below were **not** executed as packaged real-device
certification in this Task 10 session. Status is therefore **NOT VERIFIED**.

| ID | Surface | appVersion | commit | OS build | status | evidence | date | expiresAt |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| L3-MACOS-GUI-PERMISSIONS-001 | macOS packaged GUI launch; screen/accessibility/input/notification grant-deny-retry; screenshot clipboard | 0.6.7 | 987ec3c… | n/a (no host run) | **NOT VERIFIED** | none | 2026-07-14 | 2026-10-12 |
| L3-WINDOWS-GUI-001 | Windows packaged GUI; file transfer path/dialog; native terminal | 0.6.7 | 987ec3c… | n/a | **NOT VERIFIED** | none | 2026-07-14 | 2026-10-12 |
| L3-WINDOWS-WSL-001 | Windows WSL + tmux Workbench terminal recovery | 0.6.7 | 987ec3c… | n/a | **NOT VERIFIED** | none | 2026-07-14 | 2026-10-12 |
| L3-UBUNTU-GUI-001 | Ubuntu AppImage/deb GUI; terminal + file flows | 0.6.7 | 987ec3c… | n/a | **NOT VERIFIED** | none | 2026-07-14 | 2026-10-12 |
| L3-DUAL-HOST-LAN-001 | Two physical hosts same LAN: mDNS; native P2P + mobile credential-free R/W; public peer / XFF / Host / Origin / WS / remote stop rejection; 1GiB transfer + resume | 0.6.7 | 987ec3c… | n/a | **NOT VERIFIED** | none | 2026-07-14 | 2026-10-12 |

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
   Host/Origin, invalid WebSocket Origin, remote stop), and a ≥1GiB transfer
   with resume. Injected `ConnectInfo` / `X-Forwarded-For` is **not** enough.
5. `expiresAt` is set to execution date + 90 days.

After expiry, documentation may only say “historically passed on &lt;date&gt;;
current tree not re-certified.”

## NOT VERIFIED inventory (full)

The following remain **NOT VERIFIED** until a real-device pass is recorded:

1. macOS packaged GUI launch and WebView
2. macOS screen recording / accessibility / input monitoring / notification dialogs (grant, deny, retry)
3. macOS region screenshot → clipboard round-trip on real display
4. Windows packaged GUI launch and WebView
5. Windows file transfer path picker / receive directory dialogs
6. Windows native terminal (non-WSL) interactive shell in Workbench
7. Windows WSL distribution + tmux Workbench session recovery
8. Ubuntu AppImage GUI
9. Ubuntu deb package GUI
10. Ubuntu terminal + file tree flows in packaged app
11. Two physical hosts mDNS discovery (`_cc-partner._tcp`)
12. Two physical hosts native P2P credential-free read/write/actions
13. Two physical hosts mobile `/mobile` credential-free read/write/actions from a second device
14. Real public-peer / non-LAN NIC path rejection on production interfaces
15. Production XFF/Host/Origin/WebSocket boundary on multi-homed hosts (beyond injected L2 evidence)
16. Remote backend stop rejection from a second physical host (valid token still forbidden)
17. 1GiB file transfer and resume across two physical hosts

## How to re-run L2 automation (not L3)

```bash
cd src-tauri
cargo test --locked --test quality_faults -- --nocapture --test-threads=1
cargo test --locked --test lan_trust_boundary_smoke -- --nocapture --test-threads=1
```
