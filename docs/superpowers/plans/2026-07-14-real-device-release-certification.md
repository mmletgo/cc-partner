# Real-Device Release Certification Implementation Plan

> **For agentic workers:** REQUIRED EXECUTION FLOW: use `superpowers:using-git-worktrees`, then `superpowers:test-driven-development` for checker/workflow changes, and run `superpowers:verification-before-completion` before every completion claim. Real-device results may be recorded only after manual execution on the named device. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 只用当前 Apple Silicon Mac 认证并可选发布 `macos-aarch64-beta`；Windows、Ubuntu 与其他无硬件表面保持 `NOT VERIFIED`，不阻塞本机 beta，也不产生 stable/full 宣称。

**Architecture:** 固定 `claimMode=platform-beta`、`claimProfile=macos-aarch64-beta`。先把 profile-scoped evidence checker、单矩阵 RC、隔离 updater harness 和 beta-only release gate 合并，再冻结 `subjectCommit`。RC 只生成 `macos-aarch64` production bytes 与非发布 harness；本机分别执行 GUI/permissions 和 VoiceOver。证据写入 docs-only ref，release gate 固定 `expectedEvidenceCommit` 并发布同一 RC bytes。

**Tech Stack:** packaged Tauri macOS arm64 app, VoiceOver, Node evidence checker, GitHub Actions macOS runner, SHA-256, existing quality matrix.

## Global Constraints

- 执行开始时运行 `uname -m`；必须为 `arm64`。若设备架构变化，停止并修订 profile，不得把 Rosetta 当 Intel 真机。
- 当前 required executions 只有 `L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64` 与 `L3-MACOS-VOICEOVER-001@macos-aarch64`。
- Windows、WSL、Ubuntu、Intel Mac、dual-host、iOS、Android、NVDA 保持 `NOT VERIFIED`；本计划不创建其占位 evidence、构建 job 或发布资产。
- aggregate macOS row 可因 Intel 未执行保持 `PARTIAL`/canonical `NOT VERIFIED`；checker 必须消费匹配架构 execution，而不是把它提升为 full PASS。
- beta 只能使用新 beta tag/channel 与 `prerelease: true`；不得生成/覆盖 stable tag、stable release 或 `latest.json`。
- 任一产品/checker/workflow 修复或 RC run/bytes 变化都创建新 candidate，并重跑当前两项必需 execution。
- 证据必须脱敏且 90 天有效；无真实执行时不得写 PASS。
- 发布是可选的最后不可逆动作；如果用户只要求认证，Task 6 停在 frozen publish bundle。

## File Structure

- Modify/Create: `scripts/check-quality-traceability.mjs` and self-tests — 固定 `macos-aarch64-beta` dependency closure。
- Create: `scripts/prepare-updater-certification-harness.mjs`, `scripts/serve-updater-certification.mjs`, `src-tauri/tauri.updater-certification.conf.json` — 本机 loopback updater harness。
- Create: `.github/workflows/rc-tauri.yml`, `.github/workflows/release-tauri-beta.yml` — 单矩阵 RC 与 beta-only evidence gate；保留现有 stable `release-tauri.yml`，本计划不调用它。
- Modify: `docs/development/{testing.md,quality-matrix.json,real-device-certification.md}`。
- Create after execution: `docs/development/evidence/L3-MACOS-GUI-PERMISSIONS-001/**` and `L3-MACOS-VOICEOVER-001/**`。
- Create after execution: `docs/development/release-claim.json`。
- Modify after evidence: `README.md` — 只写 scoped macOS beta 事实。

## Shared Interfaces

```json
{
  "claimMode": "platform-beta",
  "claimProfile": "macos-aarch64-beta",
  "selectedMatrixIds": ["macos-aarch64"],
  "requiredExecutions": [
    "L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64",
    "L3-MACOS-VOICEOVER-001@macos-aarch64"
  ]
}
```

Each execution binds stable id, full 40-character `subjectCommit`, exact Tauri version, RC workflow run id, `artifactMatrixId`, package filename/SHA, redacted device class, exact macOS build, executor identifier, RFC3339 executed/expires timestamps, checklist result and relative artifact SHA. `evidenceCommit` is resolved from the protected evidence ref and is not embedded in the manifest.

### Task 1: Land the macOS arm64 Beta Certification Infrastructure

**Files:** Follow the File Structure above except real evidence directories.

- [ ] **Step 1: Write failing checker/profile tests**

Cover accepted Apple Silicon GUI + VoiceOver executions and rejection of: missing dependency, wrong architecture, aggregate prose without execution PASS, expired/mismatched package SHA, arbitrary required-ID allowlist, `releasable=false` asset, Windows/Linux/Intel selected matrix, stable metadata or non-beta release mode.

- [ ] **Step 2: Run RED**

Run: `node scripts/check-quality-traceability.mjs --self-test`

Expected: FAIL because the scoped execution/profile contract does not exist.

- [ ] **Step 3: Implement schema/checker and documentation contract**

Add architecture-level executions without changing canonical row truth: one Apple Silicon PASS plus unexecuted Intel remains aggregate PARTIAL and quality-matrix `NOT VERIFIED`, while fixed `macos-aarch64-beta` may consume only the matching PASS execution. Required IDs/matrices/surfaces are checker-owned. Update testing/certification docs and the required stable-ID inventory only as needed by this current profile; deferred rows remain unchanged.

- [ ] **Step 4: Implement single-matrix RC and isolated updater harness**

`.github/workflows/rc-tauri.yml` accepts exact protected `subjectTag` + 40-hex `subjectCommit`, proves the tag peels to the commit, then builds only `macos-aarch64`. Upload production DMG/app.tar.gz/`.sig` as `releasable=true` plus an inventory with SHA/expiry/signing summary. Build the lower-version harness from the same subject with a merge config whose only endpoint is `http://127.0.0.1:62190`; mark it `releasable=false`. Scan production config/bundles and fail if the loopback endpoint, insecure transport allowance or certification marker appears.

- [ ] **Step 5: Implement beta-only evidence-aware release gate**

Create a separate `release-tauri-beta.yml` requiring subject/tag/RC/evidence ref/expected evidence SHA and exact fixed profile. Verify Actions provenance, live artifact inventory/SHA and evidence-ref path allowlist. Publish only `macos-aarch64` production assets to a new beta tag/release with `prerelease: true`; reject existing tags/releases, force moves, asset overwrite, any Windows/Linux/Intel asset and any stable `latest.json` mutation. Do not repurpose or invoke the existing stable `release-tauri.yml` in this milestone.

- [ ] **Step 6: Verify and commit infrastructure**

```bash
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-docs.mjs --self-test
node scripts/check-docs.mjs
cd src-tauri && cargo test --locked
```

Expected: all exit 0.

```bash
git add scripts/check-quality-traceability.mjs scripts/prepare-updater-certification-harness.mjs scripts/serve-updater-certification.mjs src-tauri/tauri.updater-certification.conf.json .github/workflows/rc-tauri.yml .github/workflows/release-tauri-beta.yml docs/development/testing.md docs/development/quality-matrix.json docs/development/real-device-certification.md
git commit -m "feat(release): add macOS arm64 beta certification gate"
```

### Task 2: Freeze One Apple Silicon Candidate

**Files:** synchronized version files, existing broad product/methodology docs before freeze, then evidence workspace baseline.

- [ ] **Step 1: Confirm host and unique version/tag**

Run `uname -m` and require `arm64`. Check local/remote tags and releases; choose an unused semver and beta subject tag. Never reuse or move an existing tag/release.

- [ ] **Step 2: Bump version and run all integrated L0–L2 gates**

Use `node scripts/bump-version.mjs <version>`, commit synchronized version files, then run frontend, Rust, P2P inventory, quality and docs gates from the umbrella plan. Fixes remain pre-freeze.

- [ ] **Step 3: Freeze subject and protected tag**

Record exact HEAD as `subjectCommit`; create/push an immutable protected `subjectTag`, verify remote peel equality, and prohibit later product/checker/workflow mutation for this candidate.

- [ ] **Step 4: Dispatch and verify the single-matrix non-public RC**

Dispatch `rc-tauri.yml` with API `ref=<subjectTag>` and `subjectCommit`. Assert repository/workflow/event/head SHA/conclusion, exact `macos-aarch64` production/harness artifact count, live retention, inventory SHA and production certification-marker scan. Do not publish.

- [ ] **Step 5: Create protected docs-only evidence ref**

Allow only `README.md`, `docs/development/{quality-matrix.json,real-device-certification.md,release-claim.json}` and regular files below `docs/development/evidence/**`. Resolve all future evidence from this descendant ref; do not execute scripts from it.

- [ ] **Step 6: Commit evidence workspace baseline**

```bash
git add docs/development/real-device-certification.md
git commit -m "chore(release): freeze macOS arm64 beta candidate"
```

### Task 3: Execute Apple Silicon GUI and Permissions

**Files:** `docs/development/evidence/L3-MACOS-GUI-PERMISSIONS-001/**`, certification document and quality matrix.

- [ ] **Step 1: Install exact packaged candidate on a clean permission profile**

Record macOS build, `macos-aarch64` matrix id, DMG/app checksum, signing/notarization summary. Verify LAN disclosure precedes GUI-started listener and confirmation returns the actual address.

- [ ] **Step 2: Exercise four permission lifecycles**

For Accessibility, Screen Recording, Input Monitoring and Notification: deny first, confirm the UI exits checking and explains recovery, grant manually, recheck and verify the related capability. Do not automate System Settings or store sensitive screenshots.

- [ ] **Step 3: Verify screenshot, hotkey and backend lifecycle**

Capture a region to clipboard; test shortcut conflict/recovery; close GUI once leaving backend running and once stopping it; verify status/doctor remain truthful.

- [ ] **Step 4: Verify updater through the non-releasable harness**

Install matching N-1 harness, serve loopback metadata bound to the exact production app.tar.gz/`.sig`, then check, download, verify, install and restart into the production candidate. Confirm the installed candidate no longer contains/uses the harness endpoint and stable channel was never touched.

- [ ] **Step 5: Write redacted architecture execution and check evidence**

Write `macos-aarch64` execution PASS/FAIL. With Intel unexecuted, aggregate may be PARTIAL and canonical row remains `NOT VERIFIED`; never relabel it full PASS. Every PASS includes non-empty artifact checksums.

Run: `node scripts/check-quality-traceability.mjs`

```bash
git add docs/development/evidence/L3-MACOS-GUI-PERMISSIONS-001 docs/development/real-device-certification.md docs/development/quality-matrix.json
git commit -m "test(l3): certify Apple Silicon macOS GUI"
```

### Task 4: Execute Apple Silicon VoiceOver

**Files:** `docs/development/evidence/L3-MACOS-VOICEOVER-001/**`, certification document and quality matrix.

- [ ] **Step 1: Bind VoiceOver to the same candidate**

Verify subject, RC run, `macos-aarch64` package filename/SHA and GUI execution PASS before starting. A different package cannot reuse this evidence.

- [ ] **Step 2: Execute the semantic/focus journey**

Cover LAN disclosure, grouped navigation, Trending default Home, Workbench “继续工作” launch surface and zero-project CTA, Dialog/Drawer focus return, live status, terminal tabs, Attention navigation, Human Review diff and WORKFLOW diagnostics.

- [ ] **Step 3: Write redacted execution and check evidence**

Record every required action as PASS/FAIL with non-empty artifact checksums. Intel VoiceOver remains unexecuted; aggregate may be PARTIAL/canonical `NOT VERIFIED`, while the matching beta consumes only Apple Silicon PASS.

Run: `node scripts/check-quality-traceability.mjs`

```bash
git add docs/development/evidence/L3-MACOS-VOICEOVER-001 docs/development/real-device-certification.md docs/development/quality-matrix.json
git commit -m "test(l3): certify Apple Silicon VoiceOver"
```

### Task 5: Make the Scoped Go/No-Go Decision

**Files:** `docs/development/release-claim.json`, `README.md`, certification document and quality matrix.

- [ ] **Step 1: Write fixed machine-readable claim**

Set `claimMode=platform-beta`, `claimProfile=macos-aarch64-beta`, selected matrix `macos-aarch64`, and let the checker derive the two required executions. List Windows、WSL、Ubuntu、Intel Mac、dual-host、iOS、Android、NVDA and full/stable release in `uncertifiedSurfaces`.

- [ ] **Step 2: Run evidence and docs gates**

```bash
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs --subject-commit "$SUBJECT_COMMIT" --subject-tag "$SUBJECT_TAG" --rc-run-id "$RC_WORKFLOW_RUN_ID" --evidence-ref "$EVIDENCE_REF" --claim-mode platform-beta --claim-profile macos-aarch64-beta
node scripts/check-docs.mjs --self-test
node scripts/check-docs.mjs
```

Expected: GO only when both matching executions and artifact provenance pass. Deferred `NOT VERIFIED` rows are reported but do not fail this profile.

- [ ] **Step 3: Calibrate user-facing facts and commit**

README may say only the exact Apple Silicon macOS beta surfaces proven by current evidence. It must not say all macOS architectures, Windows, Ubuntu, mobile, dual-host, stable updater or full accessibility are certified.

```bash
git add README.md docs/development/real-device-certification.md docs/development/quality-matrix.json docs/development/release-claim.json
git commit -m "docs: scope release claim to Apple Silicon beta"
```

- [ ] **Step 4: Freeze publish inputs**

Resolve `evidenceRef` once as 40-hex `expectedEvidenceCommit`, rerun the checker with that SHA, and freeze subject/tag/RC/evidence/profile/asset inventory. Do not dispatch release in this task.

### Task 6: Optionally Publish the Beta as the Final Action

- [ ] **Step 1: Re-run final non-mutating provenance gates**

Require exact frozen SHAs, live RC artifacts, new target beta tag/release, no stable metadata, no repository mutation since Task 5.

- [ ] **Step 2: Stop if the user requested certification only**

Return the frozen publish bundle and GO/NO-GO result. Publication requires explicit execution scope from the umbrella/user and remains the last irreversible action.

- [ ] **Step 3: Dispatch beta-only release when authorized**

Dispatch `.github/workflows/release-tauri-beta.yml` with `ref=<subjectTag>` and exact frozen inputs. The workflow must publish only `macos-aarch64` releasable RC bytes plus provenance to a new prerelease; any stable `latest.json`, extra platform asset, existing target or SHA mismatch is fatal. Do not invoke `release-tauri.yml` and do not mutate code/docs/evidence after release.

## Deferred Certification Backlog

These are separate future plans, not unchecked tasks required by this plan:

- Intel Mac: `macos-x86_64-beta` GUI/permissions + VoiceOver.
- Windows: GUI/NSIS/MSI/updater/native PTY, WSL/tmux, NVDA.
- Ubuntu: x86_64/arm64 AppImage/deb/updater/tmux/backend CLI.
- Dual-host LAN: two physical hosts, mDNS/port fallback, socket boundary, 1 GiB durable resume.
- Mobile: physical iOS Safari and Android Chrome, safe-area/keyboard/weak-network.
- Full/stable: all fixed matrices and required rows before stable assets/`latest.json` are even eligible.

## Rollback and Failure Containment

- Evidence and RC artifacts are immutable; retest uses a new execution/candidate, never manual FAIL→PASS edits.
- Checker/release failure may block or keep the fixed beta unpublished; it cannot be bypassed by dropping GUI/VoiceOver dependencies.
- Product/checker/workflow change or RC expiry creates a new candidate and requires rerunning the two current executions.
- Missing deferred hardware remains `NOT VERIFIED`; no product code is changed merely to make a matrix row look complete.

## Completion Contract

- checker rejects incomplete/expired/mismatched Apple Silicon GUI or VoiceOver evidence.
- current Mac executions are artifact-backed and valid for exactly 90 days, or the beta is honestly NO-GO.
- Windows、Ubuntu and every other deferred surface remain explicit `NOT VERIFIED` without blocking `macos-aarch64-beta`.
- any publication is prerelease-only, Apple Silicon asset-filtered and cannot update the stable channel.
- no stable/full/cross-platform claim is produced.

## Plan Self-Review

- Spec coverage: infrastructure, candidate freeze, macOS GUI/permissions, VoiceOver, scoped go/no-go and optional beta publish each map to one task.
- Placeholder scan: runtime SHAs/build identifiers are generated by prescribed commands; no fake evidence value is prefilled.
- Type consistency: profile, matrix and execution IDs are identical in checker, evidence and release claim.
