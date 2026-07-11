# macOS and Windows Smoke CI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在相关 PR 和每日定时任务中，以真实 macOS/Windows hosted runner 验证 backend CLI 生命周期、stale control、平台路径/进程语义、原生 PTY echo/exit 和最小 Rust 构建。

**Architecture:** 新增两个不依赖 GUI、tmux 或 WSL 的 Rust integration smoke test，并用隔离 data directory/端口确保可重复运行。独立 GitHub Actions workflow 对相关路径 PR 执行两平台矩阵，每日无路径过滤兜底；失败保留日志与测试输出，job summary 明确列出未验证能力。

**Tech Stack:** Rust 2021, cargo integration tests, portable-pty, tempfile, reqwest, GitHub Actions macos-latest/windows-latest.

## Global Constraints

- 前置依赖：先完成 Vitest/CI 与 Workbench controller plans，确保 Wave 1/2 测试入口和平台敏感代码边界稳定；本计划不把 release 构建矩阵当作 smoke 门禁。
- 执行阶段先使用 `superpowers:using-git-worktrees` 创建独立 worktree/branch；每次 broad `git add` 前检查 `git status --short`，只提交本计划文件。
- 开始前读取根 `AGENTS.md`、`src-tauri/CLAUDE.md` 与 `.github` 相关 workflow。
- 第一阶段不宣称覆盖真实 Windows WSL+tmux、macOS 权限弹窗、GUI/WebView 或多机 mDNS。
- 缺少可选环境时输出明确 skip reason 并写入 job summary；不得静默 skip 或使用 `continue-on-error`。
- 每个 job、cargo 命令和子进程都有 timeout；测试结束必须 stop/kill backend 并清理 control state。
- 测试只在隔离目录写数据，不接触 runner 用户真实 `~/.cc-partner`；并行 case 不共享端口/control file。
- PR 路径过滤只减少无关变更成本；每日 schedule 与手动触发始终运行完整 macOS/Windows 矩阵。

---

## File Structure

- Modify `src-tauri/src/config.rs`: 支持 CLI/test 的显式 `CC_PARTNER_DATA_DIR` override，并测试默认路径不变。
- Modify `src-tauri/src/backend/control.rs`: 让 control/status 路径统一使用可注入 data directory，并保留现有平台进程实现。
- Modify `src-tauri/src/backend/cli.rs`: 让 start/serve/status/stop 全部继承隔离 data dir，并保证子进程 cleanup。
- Create `src-tauri/tests/backend_cli_smoke.rs`: binary lifecycle、health、duplicate start、stale control。
- Create `src-tauri/tests/pty_smoke.rs`: native platform shell echo/exit smoke。
- Create `src-tauri/tests/support/mod.rs`: isolated root、timeout、process cleanup、diagnostic retention helpers。
- Create `.github/workflows/cross-platform-smoke.yml`: PR path filter、daily cron、manual run、two-platform matrix。
- Modify `src-tauri/CLAUDE.md`: smoke scope、local commands、explicit exclusions。

## Smoke Matrix

| Check | macOS | Windows | Failure |
| --- | --- | --- | --- |
| backend start→health→status→stop | required | required | block |
| duplicate start | required | required | block |
| stale control recovery | required | required | block |
| platform path/control parsing unit tests | required | required | block |
| native PTY create→echo token→exit | required | required | block |
| Unix process-group lifecycle | required | N/A with explicit reason | block on macOS |
| Windows detached lifecycle | N/A with explicit reason | required | block on Windows |
| `cargo check --locked --bins` | required | required | block |
| WSL/tmux, GUI/WebView, permissions, multi-host mDNS | NOT VERIFIED | NOT VERIFIED | summary only |

---

### Task 1: Isolate Backend Data and Control State

**Files:**
- Modify: `src-tauri/src/config.rs`
- Modify: `src-tauri/src/backend/cli.rs`
- Modify: `src-tauri/src/backend/control.rs`

- [ ] **Step 1: Write failing data-directory tests**

Using a process-wide env lock, assert a valid absolute `CC_PARTNER_DATA_DIR` overrides config/control/database/log paths; blank/relative values are rejected; no override preserves current home path. Tests restore the environment even after panic.

- [ ] **Step 2: Implement one validated path resolver**

```rust
pub fn data_dir() -> Result<PathBuf, AppError>;
```

All backend CLI paths must derive from this function. The override is read by the CLI child after detach, so `start` passes/inherits it unchanged. Reject paths containing NUL or non-absolute values; create directories with current-user permissions.

- [ ] **Step 3: Verify existing lifecycle behavior**

```bash
cd src-tauri
cargo test --locked config::tests
cargo test --locked backend::control::tests
cargo test --locked backend::cli::tests
```

- [ ] **Step 4: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/config.rs src-tauri/src/backend/cli.rs src-tauri/src/backend/control.rs
git commit -m "test: isolate backend runtime directories"
```

---

### Task 2: Add Backend CLI Lifecycle Smoke

**Files:**
- Create: `src-tauri/tests/support/mod.rs`
- Create: `src-tauri/tests/backend_cli_smoke.rs`

- [ ] **Step 1: Create an integration harness**

Use `env!("CARGO_BIN_EXE_cc-partner-backend")`, a unique test root under `CC_PARTNER_SMOKE_ROOT` or `tempfile::TempDir`, an operation timeout, and a Drop cleanup guard. Capture stdout/stderr and retain the case directory when `CC_PARTNER_SMOKE_KEEP=1` or a case fails.

- [ ] **Step 2: Write the start→health→status→stop test**

Run `start`, poll the generated control file with a bounded deadline, request `/api/health`, run `status`, assert pid/port consistency, run `stop`, then assert status becomes stopped and process/control file are gone.

- [ ] **Step 3: Run the failing smoke test**

```bash
cd src-tauri
cargo test --locked --test backend_cli_smoke start_health_status_stop -- --nocapture
```

Expected before harness/config fixes: failure points to real lifecycle isolation or platform behavior, not ignored output.

- [ ] **Step 4: Add duplicate-start and stale-control cases**

Duplicate start must not create a second backend and must report the existing instance. Stale control uses a definitely dead PID and an unused port; `status` classifies stale and a subsequent `start` recovers without touching other cases.

- [ ] **Step 5: Add teardown hardening**

Always attempt CLI stop, then bounded direct process termination only for the recorded test PID, and remove control files. Never kill by broad process name.

- [ ] **Step 6: Verify and commit**

```bash
cd src-tauri
cargo test --locked --test backend_cli_smoke -- --nocapture --test-threads=1
git -C "$(git rev-parse --show-toplevel)" add src-tauri/tests
git commit -m "test: smoke backend cli lifecycle"
```

---

### Task 3: Add Native PTY Echo and Exit Smoke

**Files:**
- Create: `src-tauri/tests/pty_smoke.rs`

- [ ] **Step 1: Write platform shell command helpers with pure tests**

macOS spawns interactive `/bin/sh`; Windows spawns `cmd.exe /D /Q`. Build the platform newline and echo/exit input from a generated alphanumeric token; do not interpolate arbitrary user input.

- [ ] **Step 2: Write the PTY integration case**

Create an 80×24 PTY and spawn the shell above. On macOS write `printf '__CC_PARTNER_<token>__\\n'\nexit\n`; on Windows write `echo __CC_PARTNER_<token>__\r\nexit /b 0\r\n`. Read until the exact marker appears, then wait for zero exit with a bounded timeout. On failure attach raw escaped output to the panic diagnostic.

- [ ] **Step 3: Add platform lifecycle assertions**

On Unix verify the existing process-group helper targets the spawned group and cleanup leaves no child. On Windows verify the detached creation flags/path used by backend lifecycle via existing unit seams. Do not attempt WSL or tmux.

- [ ] **Step 4: Run and commit**

```bash
cd src-tauri
cargo test --locked --test pty_smoke -- --nocapture --test-threads=1
cargo test --locked workbench::sessions::tests
git -C "$(git rev-parse --show-toplevel)" add src-tauri/tests/pty_smoke.rs
git commit -m "test: smoke native pty lifecycle"
```

---

### Task 4: Create the Cross-Platform Workflow

**Files:**
- Create: `.github/workflows/cross-platform-smoke.yml`

- [ ] **Step 1: Define triggers precisely**

Use:

```yaml
on:
  pull_request:
    branches: [master]
    paths:
      - 'src-tauri/**'
      - 'web/src/pages/Workbench/**'
      - 'web/src/hooks/workbench*'
      - 'scripts/**'
      - '.github/workflows/**'
      - 'web/package-lock.json'
  schedule:
    - cron: '23 18 * * *'
  workflow_dispatch:
```

The UTC cron corresponds to one predictable daily run; document UTC rather than relying on local daylight rules.

- [ ] **Step 2: Add a strict two-platform matrix**

Matrix is `[macos-latest, windows-latest]`, `fail-fast: false`, job timeout 30 minutes. Install stable Rust with rustfmt and use `swatinem/rust-cache@v2`. Do not set `continue-on-error` at job or step level.

- [ ] **Step 3: Add build/unit/smoke steps**

Run `cargo fmt --check`, `cargo check --locked --bins`, focused backend control/CLI/session tests, `backend_cli_smoke --test-threads=1`, and `pty_smoke --test-threads=1`. Set `CC_PARTNER_SMOKE_ROOT` to `${{ runner.temp }}/cc-partner-smoke` and `RUST_BACKTRACE=1`.

- [ ] **Step 4: Add unconditional cleanup and diagnostics**

An `if: always()` shell step reads only smoke-owned control files, stops recorded PIDs, and writes process/port/file diagnostics. Upload `${{ runner.temp }}/cc-partner-smoke/**` plus captured cargo output on failure with 7-day retention.

- [ ] **Step 5: Add explicit job summary**

Always append a Markdown table marking native CLI/PTY/platform tests PASS/FAIL from step outcomes and WSL+tmux, GUI/WebView, macOS permissions, multi-host mDNS as `NOT VERIFIED — hosted runner scope`.

- [ ] **Step 6: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add .github/workflows/cross-platform-smoke.yml
git commit -m "ci: add macos and windows smoke matrix"
```

---

### Task 5: Verify Repeatability and Failure Evidence

**Files:**
- Modify: tests/workflow only where evidence shows a real issue

- [ ] **Step 1: Run each smoke twice locally on the available platform**

```bash
cd src-tauri
cargo test --locked --test backend_cli_smoke -- --nocapture --test-threads=1
cargo test --locked --test backend_cli_smoke -- --nocapture --test-threads=1
cargo test --locked --test pty_smoke -- --nocapture --test-threads=1
cargo test --locked --test pty_smoke -- --nocapture --test-threads=1
cargo check --locked --bins
```

Expected: both repetitions pass without stale control, occupied test port or leftover child process.

- [ ] **Step 2: Prove timeout and artifact paths**

Temporarily configure a smoke fixture to withhold health/echo, confirm the case times out within its own bound and preserves diagnostics under the smoke root, then revert the intentional failure.

- [ ] **Step 3: Run workflow_dispatch on both hosted platforms**

Confirm both matrix jobs run regardless of path filters, all required steps block on failure, artifacts are available for an intentional test branch failure, and summary exclusions are visible. Remove the intentional failure before merge.

- [ ] **Step 4: Confirm PR path behavior**

Use a code-only branch under `src-tauri/**` to confirm the workflow triggers. A docs-only PR may skip this workflow, while the daily schedule still runs without path filtering.

- [ ] **Step 5: Commit any evidence-driven corrections**

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/tests src-tauri/src .github/workflows/cross-platform-smoke.yml
git commit -m "test: harden cross platform smoke repeatability"
```

---

### Task 6: Document Verified and Unverified Scope

**Files:**
- Modify: `src-tauri/CLAUDE.md`

- [ ] **Step 1: Record exact local commands and path triggers**

Document the two integration tests, required serial execution for backend lifecycle, isolation environment variables, daily workflow and related PR paths.

- [ ] **Step 2: Record exclusions verbatim**

State hosted smoke does not verify WSL+tmux, GUI/WebView, macOS permission dialogs or multi-host mDNS; release artifacts remain a separate workflow and are not a substitute for these smoke tests.

- [ ] **Step 3: Run final checks**

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked backend::control::tests
cargo test --locked backend::cli::tests
cargo test --locked workbench::sessions::tests
cargo test --locked --test backend_cli_smoke -- --nocapture --test-threads=1
cargo test --locked --test pty_smoke -- --nocapture --test-threads=1
cargo check --locked --bins
```

- [ ] **Step 4: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/CLAUDE.md
git commit -m "docs: define cross platform smoke coverage"
```

## Completion Contract

- Related PRs and daily schedule run strict macOS/Windows jobs without `continue-on-error`.
- Backend lifecycle, duplicate start, stale control, native PTY echo/exit and minimum bins build pass on both hosted platforms.
- Tests are isolated, bounded, repeatable and clean only their own PIDs/control files.
- Failures retain useful logs/test output; job summary explicitly distinguishes verified and unverified behavior.
- No result claims coverage of WSL/tmux, GUI/WebView, native permissions or multi-host mDNS.
