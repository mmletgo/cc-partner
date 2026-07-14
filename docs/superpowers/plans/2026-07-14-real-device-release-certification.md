# Real-Device Release Certification Implementation Plan

> **For agentic workers:** REQUIRED EXECUTION FLOW: use `superpowers:using-git-worktrees`, then `superpowers:test-driven-development` task-by-task, and run `superpowers:verification-before-completion` before every completion claim. Native subagents may execute independent tasks; do not require unavailable `subagent-driven-development` or `executing-plans` skills. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用发布候选包和真实硬件关闭现有五个 L3 `NOT VERIFIED`，补充移动/无障碍/弱网证据，并把证据有效期与发布措辞绑定。

**Architecture:** 先把 manifest loader/checker、非公开 RC build workflow 和 evidence-aware release workflow 合并，再冻结 `subjectCommit`。RC workflow 从该 commit 生成一次不可变 Actions artifacts；各 L3 在 docs-only evidence ref 独立写 manifest。最终 release 用 subject checkout 的 checker 把 evidence ref 固定为一个 40 位 commit，验证 Actions provenance 后发布同一 RC bytes。`claimMode` + 固定 `claimProfile` 决定 stable/full 或隔离的 platform beta。

**Tech Stack:** packaged Tauri apps, macOS/Windows/Ubuntu physical devices, WSL/tmux, iOS Safari, Android Chrome, VoiceOver/NVDA, Node evidence checker, SHA-256, existing quality matrix.

## Global Constraints

- 必读 `docs/superpowers/specs/2026-07-14-real-device-release-certification-design.md`、`docs/development/quality-matrix.json`、`docs/development/real-device-certification.md` 与 `docs/development/testing.md`。
- 只有 N1–N7 完成且全门禁通过后才冻结候选 commit。
- 不使用 L1 mock、hosted runner 或 loopback 替代 L3 真机。
- 不自动修改防火墙、系统权限或安全策略；用户/执行人手动操作并记录。
- evidence 不含 home path、设备名、项目名、token、Prompt、终端正文或用户文件内容。
- PASS 需要非空真实 artifact；未执行/硬件缺失保持 canonical `NOT VERIFIED`（空格，不新增下划线变体）。
- 证据有效期恰为执行时间后 90 天；影响能力的代码变化使对应证据提前失效。
- 任一产品字节、checker/workflow 或 RC run 变化均使本 candidate 的九行证据整体失效；本轮不按人工影响判断复用旧行。
- `full` 只代表本计划明确列出的九组表面与五个 build matrix，不代表尚未执行的所有 screen reader/移动辅助技术。

---

## File Structure

- Modify: `scripts/check-quality-traceability.mjs` and self-tests。
- Modify: `docs/development/quality-matrix.json`。
- Modify: `docs/development/real-device-certification.md`。
- Create: `docs/development/evidence/L3-MACOS-GUI-PERMISSIONS-001/manifest.json` only after execution。
- Create: `docs/development/evidence/L3-WINDOWS-GUI-001/manifest.json` only after execution。
- Create: `docs/development/evidence/L3-WINDOWS-WSL-001/manifest.json` only after execution。
- Create: `docs/development/evidence/L3-UBUNTU-GUI-001/manifest.json` only after execution。
- Create: `docs/development/evidence/L3-DUAL-HOST-LAN-001/manifest.json` only after execution。
- Create after execution: `docs/development/evidence/L3-IOS-SAFARI-001/manifest.json`。
- Create after execution: `docs/development/evidence/L3-ANDROID-CHROME-001/manifest.json`。
- Create after execution: `docs/development/evidence/L3-MACOS-VOICEOVER-001/manifest.json`。
- Create after execution: `docs/development/evidence/L3-WINDOWS-NVDA-001/manifest.json`。
- Create: `.github/workflows/rc-tauri.yml`; modify `.github/workflows/release-tauri.yml` before candidate freeze。
- Create: `src-tauri/tauri.updater-certification.conf.json` — 只供不可发布 N-1 harness 使用的 loopback updater merge config。
- Create: `scripts/prepare-updater-certification-harness.mjs`, `scripts/serve-updater-certification.mjs` and self-tests。
- Modify: `src-tauri/src/commands/updater.rs` tests — characterize that command behavior is config-driven and has no runtime endpoint override。
- Create in evidence ref: `docs/development/release-claim.json` with `claimMode`, fixed `claimProfile`, `subjectCommit`, `rcWorkflowRunId`, `evidenceRef`, build matrix IDs, claimed/uncertified surfaces and checker-derived required/certified IDs。
- Modify: `docs/testing/mobile-workbench-lan-test-cases.md`, `README.md` and the certification/testing docs named above only when their facts change。

## Shared Evidence Contract

Each manifest contains: stable id, full 40-character `subjectCommit`, exact Tauri `version`, RC workflow run id, aggregate `PASS | FAIL | PARTIAL`, redaction declaration and one or more executions. Each execution has independent `PASS | FAIL` and binds exact `(artifactMatrixId, packageFilename, installedPackageSha256)` tuples to redacted device class, OS build, allowed executor identifier, RFC3339 executed/expires timestamps, checklist and relative artifact SHA. Every package tuple must equal the corresponding RC inventory entry. `evidenceCommit` is resolved from the protected evidence ref, not embedded in the manifest.

### Task 1: Land the Evidence Contract and RC/Release Plumbing Before Freeze

**Files:**
- Modify: `scripts/check-quality-traceability.mjs`
- Modify: `docs/development/quality-matrix.json`
- Create: `.github/workflows/rc-tauri.yml`
- Modify: `.github/workflows/release-tauri.yml`

**Interfaces:** Checker accepts explicit `--subject-commit`, `--subject-tag`, `--rc-run-id`, `--evidence-ref`, `--expected-evidence-commit`, `--claim-mode full|platform-beta` and fixed `--claim-profile`; executed matrix rows point to one real `evidenceManifest`, while never-executed `NOT VERIFIED` rows keep it null. RC workflow builds non-public immutable artifacts at an exact tagged commit. Release workflow only publishes profile-selected certified artifacts.

- [ ] **Step 1: Add failing self-test cases**

Add fixtures that must fail: empty PASS executions/artifacts, missing execution status/`osBuild`/matrix id, invalid aggregate status, aggregate PASS with a missing/failed required execution, aggregate PARTIAL with no PASS or with a FAIL, short SHA, version mismatch, expiry not exactly +90d, missing artifact path, expired PASS, matrix/manifest aggregate mismatch, an evidence package SHA that differs from its RC inventory entry, an evidence package mapped to the wrong matrix/filename, wrong `subjectCommit`/RC run, path escape, symlink, submodule and PASS with unredacted forbidden keys. Add passing fixtures for never-executed canonical `NOT VERIFIED` with `evidenceManifest:null` and one-architecture PARTIAL manifest mapped to `NOT VERIFIED` with a real manifest.

- [ ] **Step 2: Run RED**

Run: `node scripts/check-quality-traceability.mjs --self-test`

Expected: FAIL because current checker accepts at least one invalid fixture.

- [ ] **Step 3: Implement strict manifest validation**

Resolve artifact paths under the evidence directory, reject path escape/symlink, compare version with the RC version, require 40-hex SHA and enforce exact 90-day interval. Load the RC inventory once, index it by `(matrixId,filename)`, and require every execution package SHA to equal that exact inventory entry; duplicate, absent, cross-matrix and same-name/wrong-SHA bindings fail. Execution status accepts `PASS | FAIL`; manifest aggregate accepts `PASS | FAIL | PARTIAL`; matrix status remains canonical `PASS | FAIL | NOT VERIFIED` with PASS→PASS, FAIL→FAIL, PARTIAL→NOT VERIFIED. Validate the existing five L3 IDs plus `L3-IOS-SAFARI-001`, `L3-ANDROID-CHROME-001`, `L3-MACOS-VOICEOVER-001` and `L3-WINDOWS-NVDA-001` independently. Require `commit=subjectCommit` and a matching manifest for executed aggregate PASS/FAIL/PARTIAL; require null manifest/commit/version/date/expiry evidence fields only for never-executed `NOT VERIFIED`.

- [ ] **Step 4: Add machine-readable release claim validation**

Validate `docs/development/release-claim.json` from the supplied evidence ref. Hard-code this closed profile table (unknown names fail):

- `full`: matrices `macos-aarch64`, `macos-x86_64`, `windows-x86_64`, `linux-x86_64`, `linux-aarch64`; all nine IDs; macOS DMG/app.tar.gz/signature, Windows NSIS setup/signature/MSI, Linux AppImage/signature/deb; RPM excluded.
- `macos-aarch64-beta` / `macos-x86_64-beta`: one matching matrix and macOS assets; matching macOS GUI execution plus VoiceOver row; local GUI/permissions/screenshot/hotkey/updater-harness/a11y surfaces only.
- `windows-x86_64-beta`: Windows matrix/assets; Windows GUI + NVDA; no WSL claim. `windows-wsl-x86_64-beta` additionally requires WSL and adds only WSL/tmux.
- `linux-x86_64-beta` / `linux-aarch64-beta`: matching Linux matrix AppImage/signature/deb and matching Ubuntu execution; local GUI/tmux/PTY/backend CLI/updater-harness path only.

No mobile-only or dual-host beta exists. The profile expands to asset filenames/types, prerequisite matrix-specific PASS executions and claimed/uncertified surfaces; handwritten allowlists/prose cannot override it. A fixed per-architecture beta may consume its PASS execution from a PARTIAL/FAIL aggregate manifest, but never an execution for another matrix; any failed/missing selected execution blocks that profile. `full` requires all nine aggregate PASS manifests and five matrices. Resolve `evidenceCommit` from `evidenceRef` rather than requiring it inside a self-referential manifest.

- [ ] **Step 5: Add the immutable RC workflow**

Create a `workflow_dispatch` workflow with exact `ref` and version inputs. Reuse the repository's native three-stage Tauri CLI build/signing path, upload non-public macOS/Windows/Linux candidate artifacts with the platform's maximum supported retention plus an artifact inventory containing `subjectCommit`, version, workflow run id, five matrix IDs, filenames, SHA-256, `artifactExpiresAt` and non-secret signing/notarization summaries. On all five matrices also build a clearly `releasable=false` N-1 certification harness from the same subject source: `prepare-updater-certification-harness.mjs` validates a lower version and merges `tauri.updater-certification.conf.json`, whose only endpoint is fixed loopback `http://127.0.0.1:62190`; its insecure transport allowance exists only in that merge config. `serve-updater-certification.mjs` serves generated signed metadata whose package URL/hash/signature point to the downloaded production RC updater artifact for that matrix. Production build jobs must scan config/bundle strings and fail if the loopback endpoint, certification marker or insecure transport setting appears. Do not create a GitHub Release or tag; harness artifacts are separately named and the release workflow always rejects `releasable=false`.

- [ ] **Step 6: Make final release evidence-aware**

Require `subjectCommit`, protected immutable `subjectTag`, `rcWorkflowRunId`, protected `evidenceRef`, `expectedEvidenceCommit`, `claimMode` and fixed profile for the certified release path. GitHub's workflow-dispatch API accepts a branch/tag ref, not a raw SHA, so both RC/release calls use `ref=subjectTag`; the first job fails before checkout/download if `github.sha != inputs.subjectCommit` or the fetched tag peels elsewhere, making the interpreted workflow YAML subject-owned. Resolve `evidenceRef` once and require exact equality with the 40-hex `expectedEvidenceCommit`; all later checkout/checker/provenance steps use that SHA, never resolve the movable ref again. Verify `merge-base --is-ancestor`, checkout evidence into a separate read-only directory, and allow only `README.md`, `docs/development/{quality-matrix.json,real-device-certification.md,release-claim.json}`, `docs/testing/mobile-workbench-lan-test-cases.md` and regular files below `docs/development/evidence/`; reject every other diff path, submodule, symlink or path escape and never execute evidence scripts/workflows. Pin reusable workflows/actions to audited subject/SHA. Query GitHub Actions API to require the current repository, workflow path/id `rc-tauri.yml`, `workflow_dispatch`, matching `head_sha`, successful conclusion, live artifacts and exact name/count before comparing inventory/SHA. Publish only `releasable=true` files selected by the fixed profile: `full` uses a new stable tag/default `latest.json`; beta uses a new beta tag/channel, `prerelease: true` and no stable metadata. Any existing target tag/release or subjectTag mismatch is fail-closed; force-move/asset overwrite is forbidden. Guard/remove any legacy path that bypasses this gate. See the official [workflow dispatch REST contract](https://docs.github.com/en/rest/actions/workflows#create-a-workflow-dispatch-event).

- [ ] **Step 7: Run checker and docs self-tests**

Run: `node scripts/check-quality-traceability.mjs --self-test && node scripts/check-docs.mjs --self-test`

Expected: PASS.

- [ ] **Step 8: Commit the pre-freeze infrastructure**

```bash
git add scripts/check-quality-traceability.mjs scripts/prepare-updater-certification-harness.mjs scripts/serve-updater-certification.mjs src-tauri/tauri.updater-certification.conf.json src-tauri/src/commands/updater.rs docs/development/quality-matrix.json .github/workflows/rc-tauri.yml .github/workflows/release-tauri.yml
git commit -m "test(certification): enforce real-device evidence"
```

### Task 2: Freeze the Release Candidate and Evidence Workspace

**Files:**
- Modify: `docs/development/real-device-certification.md`

**Interfaces:** Produces one immutable `subjectCommit`/version/RC workflow run used by every manifest, plus a protected docs-only evidence ref rooted at that commit.

- [ ] **Step 1: Select, preflight and commit a unique release version**

Read the current version and all local/remote tags/releases. Because this program adds major user-visible capability, default to the next unused minor version (from the audited 0.6.7 baseline, `0.7.0`); if execution-time history already contains it, choose the next higher unused semver before continuing. Require both `git rev-parse refs/tags/v$VERSION` and the GitHub Releases API to report absence, then run `node scripts/bump-version.mjs "$VERSION"`, inspect the synchronized Tauri/Cargo/web lockfile diff and commit it. Never reuse, delete or force-move an existing version tag/release.

```bash
node scripts/bump-version.mjs "$VERSION"
git add src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock web/package.json web/package-lock.json
git commit -m "chore: bump version to $VERSION"
```

- [ ] **Step 2: Run all automated pre-L3 gates**

```bash
cd web
npm run lint
npm run build
npm test
npm run test:e2e
cd ../src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cd ..
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
```

Expected: all exit 0.

- [ ] **Step 3: Freeze the product subject and immutable dispatch tag**

After Task 1/version commit is merged, record the full `git rev-parse HEAD` as `subjectCommit` and the exact `src-tauri/tauri.conf.json` version. Create `subjectTag=cert-subject-v<version>-<first12sha>` under a repository tag ruleset that rejects update/deletion, push it, then verify the remote tag peels exactly to `subjectCommit`; fail if the tag already exists. Record both values in candidate/claim metadata. From this point, product/checker/workflow changes invalidate the candidate and require a new subject commit/tag; only evidence/documentation commits may follow on the evidence ref.

- [ ] **Step 4: Dispatch and verify the non-public RC build**

Dispatch `.github/workflows/rc-tauri.yml` with API `ref=<subjectTag>` and full `subjectCommit` input; assert its first job observed `github.sha=subjectCommit` and the tag still peels to it. Capture `rcWorkflowRunId`; query Actions metadata to verify repository/workflow/event/head SHA/conclusion and live artifact count; download its inventory, verify every artifact SHA-256/expiry, `releasable` flag, production-bundle certification-marker scan and non-sensitive signing/notarization summary. Candidate packages are never rebuilt locally; updater harness packages are non-releasable test tools, not candidate bytes. Do not publish either set.

- [ ] **Step 5: Create the protected docs-only evidence ref**

Create the protected evidence ref as a descendant of `subjectCommit`. Add CI path enforcement for exactly `README.md`, `docs/development/{quality-matrix.json,real-device-certification.md,release-claim.json}`, `docs/testing/mobile-workbench-lan-test-cases.md` and regular files under `docs/development/evidence/**`; ordinary branch protection alone is insufficient. Product code, other docs, scripts and workflows start a new candidate and are rejected on this ref. All manifests reference the same subject commit, version and RC run. Record the evidence ref in the certification document.

- [ ] **Step 6: Confirm candidate artifacts launch on their target OS**

Install the downloaded RC bytes on their target systems. If a package cannot launch, mark only the owning L3 row FAIL and stop that row; do not rebuild or edit a manifest to PASS.

- [ ] **Step 7: Commit the evidence workspace baseline**

```bash
git add docs/development/real-device-certification.md
git commit -m "chore(release): freeze certification candidate"
```

### Task 3: Execute `L3-MACOS-GUI-PERMISSIONS-001`

**Files:**
- Create after execution: `docs/development/evidence/L3-MACOS-GUI-PERMISSIONS-001/manifest.json`
- Add redacted screenshots/log checksums in the same directory
- Modify: `docs/development/real-device-certification.md`
- Modify: `docs/development/quality-matrix.json`

- [ ] **Step 1: Install and launch the packaged macOS app on a clean permission profile**

On real Apple Silicon and Intel Macs, record exact macOS build, matrix id and installed app checksum. Verify LAN disclosure appears before GUI starts sidecar. If either architecture is unavailable, the row cannot cover full's macOS matrix set.

- [ ] **Step 2: Exercise deny/grant/recheck for four permissions**

Accessibility, Screen Recording, Input Monitoring and Notification: deny first, confirm non-permanent checking state, grant manually, recheck, and verify a generic privacy-safe system notification can display without blocking core work. Notification click callbacks are not part of this row.

- [ ] **Step 3: Verify screenshot, clipboard, hotkey and backend lifecycle**

Capture a region to clipboard; test shortcut conflict/recovery; close GUI once leaving backend running and once stopping it. Install the non-releasable N-1 harness from the same subject, start `serve-updater-certification.mjs` on loopback with generated metadata bound to the exact candidate app.tar.gz/signature inventory entries, then check, download, verify, install and restart into the production candidate. Verify the installed candidate no longer contains/uses the harness endpoint and stable channel was never touched. Record this as “current subject updater path via certification harness”; do not call the harness a prior stable release.

- [ ] **Step 4: Redact artifacts and write manifest**

Write one execution status per architecture. Aggregate PASS only if both architectures complete every checklist; any executed failure makes aggregate FAIL, while one architecture PASS plus the other unexecuted is PARTIAL and maps to matrix `NOT VERIFIED` with evidence. Preserve the passing execution so only its matching fixed beta may consume it.

- [ ] **Step 5: Run checker and commit evidence**

Run: `node scripts/check-quality-traceability.mjs`

```bash
git add docs/development/evidence/L3-MACOS-GUI-PERMISSIONS-001 docs/development/real-device-certification.md docs/development/quality-matrix.json
git commit -m "test(l3): certify packaged macOS GUI"
```

### Task 4: Execute Windows GUI and WSL Certification

**Files:**
- Create after execution: both Windows evidence manifests/directories
- Modify: `docs/development/real-device-certification.md`
- Modify: `docs/development/quality-matrix.json`

- [ ] **Step 1: Execute `L3-WINDOWS-GUI-001`**

Install both packaged NSIS and MSI on clean profiles, verify install/launch/uninstall-or-repair plus local file picker/receive path, region screenshot/clipboard/global shortcut, native PTY fallback, transfer path, sidecar start/status/doctor and GUI-only close. Install the non-releasable N-1 harness from the same subject and use the loopback metadata server to drive the exact production NSIS candidate through check/download/signature/install/restart/version verification. Verify the candidate has the normal stable endpoint/config only. Record exact Windows build, `windows-x86_64` matrix id and non-secret signing identity summary; describe updater evidence as harness-based current-path certification, not historical binary compatibility.

- [ ] **Step 2: Execute `L3-WINDOWS-WSL-001`**

Verify default distribution detection, tmux missing/install/ready states, Windows and `\\wsl$` path conversions, window/pane create/switch/close, app restart restore.

- [ ] **Step 3: Verify single terminal owner under GUI/Mobile**

Open the same session from GUI and `/mobile`; confirm one sidecar registry/attach and consistent output without duplicated tmux window.

- [ ] **Step 4: Write/redact both manifests**

Each row is independent: one may PASS while the other `FAIL`/`NOT VERIFIED`. Do not merge results.

- [ ] **Step 5: Check and commit evidence**

Run: `node scripts/check-quality-traceability.mjs`

```bash
git add docs/development/evidence/L3-WINDOWS-GUI-001 docs/development/evidence/L3-WINDOWS-WSL-001 docs/development/real-device-certification.md docs/development/quality-matrix.json
git commit -m "test(l3): certify Windows GUI and WSL"
```

### Task 5: Execute `L3-UBUNTU-GUI-001`

**Files:**
- Create after execution: Ubuntu evidence manifest/directory
- Modify: `docs/development/real-device-certification.md`
- Modify: `docs/development/quality-matrix.json`

- [ ] **Step 1: Test x86_64 AppImage/deb on supported Ubuntu**

On real x86_64 hardware, verify AppImage/deb install/launch/WebView, tray differences, terminal, file tree/edit, sidecar lifecycle and Doctor; bind results to `linux-x86_64` package hashes. Use the matching non-releasable N-1 harness plus loopback metadata server to verify check/signature/install/restart into the exact production AppImage and confirm the candidate has no certification endpoint.

- [ ] **Step 2: Test arm64 AppImage/deb independently**

On real arm64 hardware, install, launch, remove/reinstall and repeat core terminal/file/sidecar smoke; repeat the matching harness→production AppImage updater path and bind results to `linux-aarch64` package/signature hashes. Record exact Ubuntu build and desktop session for both architectures.

- [ ] **Step 3: Verify tmux/PTY and LAN disclosure**

Test tmux ready and raw PTY fallback; confirm disclosure wording and actual listener/port facts.

- [ ] **Step 4: Write redacted manifest**

Aggregate PASS for full requires every required package form on both architectures. If one architecture PASSes and the other is unexecuted, write aggregate PARTIAL and map the matrix row to `NOT VERIFIED` with the manifest; if any execution FAILs, aggregate is FAIL. A fixed beta may consume only its own matrix-specific PASS execution, never the aggregate prose or another architecture.

- [ ] **Step 5: Check and commit evidence**

Run: `node scripts/check-quality-traceability.mjs`

```bash
git add docs/development/evidence/L3-UBUNTU-GUI-001 docs/development/real-device-certification.md docs/development/quality-matrix.json
git commit -m "test(l3): certify Ubuntu packages"
```

### Task 6: Execute `L3-DUAL-HOST-LAN-001`

**Files:**
- Create after execution: dual-host evidence manifest/directory
- Modify: `docs/testing/mobile-workbench-lan-test-cases.md`
- Modify: `docs/development/real-device-certification.md`
- Modify: `docs/development/quality-matrix.json`

- [ ] **Step 1: Discover two physical hosts with mDNS and port fallback**

Verify normal 62116 and occupied-port +1 behavior, actual access links and no automatic firewall modification.

- [ ] **Step 2: Exercise credential-free business access and socket-boundary rejection**

From a legal LAN socket peer, complete Prompt/SSH/Scratchpad sync, Workbench file/Git/terminal and Orchestrator read/write/execute without credentials. Confirm that arbitrary `X-Forwarded-For`/`Forwarded` values do not change that result. Reject hostile Host/Origin/Content-Type, invalid WebSocket/public socket peers and remote stop; confirm a public socket peer that forges loopback/LAN forwarding headers is still rejected. Do not invent or test business API control tokens.

- [ ] **Step 3: Verify runtime/sync truth**

GUI config update changes sidecar generation; desktop snapshot shows actual scheduler tick; equal second Prompt/SSH/Scratchpad sync pushes zero; partial/disconnect is not reported success.

- [ ] **Step 4: Transfer 1 GiB with disconnect and process restart**

Interrupt mid-transfer, restart the owning backend, resume from the durable checkpoint, verify final SHA-256, one final file and one durable successful finalization outcome with no duplicate rename/content. Do not infer correctness from an in-memory attempt counter.

- [ ] **Step 5: Write/check/commit evidence**

Run: `node scripts/check-quality-traceability.mjs`

```bash
git add docs/development/evidence/L3-DUAL-HOST-LAN-001 docs/development/real-device-certification.md docs/development/quality-matrix.json docs/testing/mobile-workbench-lan-test-cases.md
git commit -m "test(l3): certify dual-host LAN workflows"
```

### Task 7: Execute Four Independent Mobile and Accessibility Rows

**Files:**
- Create after execution: `docs/development/evidence/L3-IOS-SAFARI-001/manifest.json`
- Create after execution: `docs/development/evidence/L3-ANDROID-CHROME-001/manifest.json`
- Create after execution: `docs/development/evidence/L3-MACOS-VOICEOVER-001/manifest.json`
- Create after execution: `docs/development/evidence/L3-WINDOWS-NVDA-001/manifest.json`
- Modify: `docs/testing/mobile-workbench-lan-test-cases.md`
- Modify: `docs/development/real-device-certification.md`
- Modify: `docs/development/quality-matrix.json`

- [ ] **Step 1: Execute `L3-IOS-SAFARI-001`**

On a physical iPhone/Safari, verify 390×844 and 844×390 equivalents, safe-area, soft keyboard, existing top-menu/Drawer navigation, terminal scroll/fullscreen and return-to-panel state. Cover the existing `/mobile` Workbench panels including Attention and Automation; do not claim desktop Home/Transfer mobile flows.

- [ ] **Step 2: Execute `L3-ANDROID-CHROME-001`**

On a physical Android/Chrome device, repeat the layout/state contract and apply controlled 300ms RTT, 1% loss and 10-second disconnect. Verify query timeout/cancel/retry, mutation unknown-outcome reconciliation, current-panel refresh after reconnect and no duplicate action. Also apply the weak-network subset to iOS; each browser keeps its own result.

- [ ] **Step 3: Execute `L3-MACOS-VOICEOVER-001`**

With VoiceOver on packaged macOS, cover LAN disclosure, semantic sidebar groups, Workbench empty-state CTA, Dialog/Drawer focus return, live status, terminal tabs, Attention navigation, Human Review diff and WORKFLOW diagnostics. Record separate executions for Apple Silicon and Intel packages; `full` requires both, while each macOS beta consumes only its matching execution.

- [ ] **Step 4: Execute `L3-WINDOWS-NVDA-001`**

With NVDA on packaged Windows, execute the same semantic/focus/live-region journey. Record it independently from the Windows GUI row.

- [ ] **Step 5: Write four independent redacted manifests**

Each execution is independently PASS/FAIL. Single-matrix browser/NVDA IDs aggregate directly; VoiceOver requires per-macOS-architecture executions and may aggregate PARTIAL when one is unexecuted. Quality rows remain canonical PASS/FAIL/NOT VERIFIED using the aggregate mapping; missing hardware cannot be hidden inside PASS. Every PASS execution requires its own non-empty artifact set and checksums.

- [ ] **Step 6: Check and commit evidence**

Run: `node scripts/check-quality-traceability.mjs`

```bash
git add docs/development/evidence/L3-IOS-SAFARI-001 docs/development/evidence/L3-ANDROID-CHROME-001 docs/development/evidence/L3-MACOS-VOICEOVER-001 docs/development/evidence/L3-WINDOWS-NVDA-001 docs/development/quality-matrix.json docs/development/real-device-certification.md docs/testing/mobile-workbench-lan-test-cases.md
git commit -m "test(l3): certify mobile and accessibility"
```

### Task 8: Enforce Go/No-Go and Calibrate Release Claims

**Files:**
- Create on evidence ref: `docs/development/release-claim.json`
- Modify: `README.md`
- Modify: `docs/development/real-device-certification.md`
- Modify: `docs/development/quality-matrix.json`

- [ ] **Step 1: Write the machine-readable release claim**

Record `claimMode`, checker-owned `claimProfile`, `subjectCommit`, immutable `subjectTag`, `rcWorkflowRunId`, protected `evidenceRef`, selected build matrix IDs and machine-derived required/certified IDs plus `claimedSurfaces`/`uncertifiedSurfaces`. In `full`, required IDs are all five existing/four new rows and selected matrices are all five builds. In `platform-beta`, the fixed profile—not a caller allowlist—selects assets and expands dependency closure; the checker derives disclosure. At minimum, WSL depends on Windows GUI, macOS a11y on the matching macOS GUI architecture, and mobile browser on certified host GUI + dual-host LAN + browser row.

- [ ] **Step 2: Run final evidence and documentation gates**

```bash
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs --subject-commit "$SUBJECT_COMMIT" --subject-tag "$SUBJECT_TAG" --rc-run-id "$RC_WORKFLOW_RUN_ID" --evidence-ref "$EVIDENCE_REF" --claim-mode "$CLAIM_MODE" --claim-profile "$CLAIM_PROFILE"
node scripts/check-docs.mjs --self-test
node scripts/check-docs.mjs
```

Expected: all exit 0; every PASS manifest matches candidate commit/version and artifacts.

- [ ] **Step 3: Perform final go/no-go review**

Any required `FAIL`, `NOT VERIFIED`, expired or subject/RC-mismatched row means no full cross-platform claim. A beta may proceed only through a fixed profile whose dependency closure passes. Record the exact scoped decision in the existing certification document and machine claim, not a new task-summary file.

- [ ] **Step 4: Calibrate user-facing facts**

Update README/release text only to `claimedSurfaces` supported by current evidence; do not say permissions, WSL, multi-host or 1 GiB resume are verified if their row is not PASS. Explicitly list uncertified Ubuntu screen-reader, iOS VoiceOver, Android TalkBack or other surfaces rather than interpreting `full` as universal accessibility.

- [ ] **Step 5: Commit the evidence claim**

```bash
git add README.md docs/development/real-device-certification.md docs/development/quality-matrix.json docs/development/release-claim.json
git commit -m "docs: gate release claims on L3 evidence"
```

- [ ] **Step 6: Produce immutable publish inputs and stop before release**

After Step 5 commit, resolve `evidenceRef` exactly once as 40-hex `expectedEvidenceCommit`, rerun the checker with `--expected-evidence-commit`, and record `subjectCommit`, immutable `subjectTag`, RC run, evidence ref + expected SHA, claim mode/profile and profile-derived artifact inventory as the publish input bundle. Do not dispatch the release here. The final irreversible action uses Actions API `ref=<subjectTag>` plus the full subject/evidence SHA inputs; the workflow first proves `github.sha`/tag peel equal subject and `resolve(evidenceRef)==expectedEvidenceCommit`, then uses only those SHAs to validate ancestry/path allowlist/Actions provenance, download/hash original RC artifacts, point the release tag to `subjectCommit`, and publish profile-selected `releasable=true` bytes. `full` may update stable `latest.json`; beta must use beta tag/channel, `prerelease: true` and no stable updater metadata. It emits an immutable provenance asset/attestation containing release URL, expected evidence commit, RC run and SHA inventory; do not write these values back and pretend the evidence-ref HEAD is self-identical.

```bash
EXPECTED_EVIDENCE_COMMIT="$(git rev-parse "$EVIDENCE_REF^{commit}")"
node scripts/check-quality-traceability.mjs --subject-commit "$SUBJECT_COMMIT" --subject-tag "$SUBJECT_TAG" --rc-run-id "$RC_WORKFLOW_RUN_ID" --evidence-ref "$EVIDENCE_REF" --expected-evidence-commit "$EXPECTED_EVIDENCE_COMMIT" --claim-mode "$CLAIM_MODE" --claim-profile "$CLAIM_PROFILE"
```

## Rollback and Failure Containment

- evidence manifest 与 artifact 一经生成即视为不可变；重测写新执行记录/替换为新候选证据，不手工把 FAIL 改成 PASS。
- checker/release gate 阻断时只能修 owning track 或缩小到固定 beta profile，不能放宽 schema、有效期、依赖闭包或 artifact 要求。
- 任一产品/checker/workflow 修复或 RC artifact/run 变化后创建新 candidate，九行全部重测；禁止只改旧 manifest 的 subject/run 字段。RC artifact 过期/不可下载同样触发此流程。
- 无硬件时保持 `NOT VERIFIED`；认证计划本身不修改产品代码。

## Completion Contract

- checker rejects incomplete/expired/mismatched evidence.
- five existing and four additional stable L3 rows each contain a real manifest or remain honest `NOT VERIFIED`.
- full cross-platform release claims require current PASS evidence.
- evidence is redacted, artifact-backed and valid for exactly 90 days.
- publish inputs select byte-identical certified RC artifacts and a tag target of `subjectCommit`; actual dispatch is the umbrella plan's last step after all final gates.
- full covers all five build matrices; beta is prerelease-only, asset-filtered and cannot update the stable channel.
- release provenance pins the trusted checker subject, evidence commit, Actions run metadata and exact artifact hashes.

## Plan Self-Review

- Spec coverage: checker, candidate freeze, macOS, Windows/WSL, Ubuntu, dual-host, mobile/a11y and go/no-go each map to tasks.
- Placeholder scan: all evidence directories and stable IDs are exact; runtime evidence values are produced by the prescribed commands, not prefilled.
- Type consistency: manifest requirements are identical across tasks.
