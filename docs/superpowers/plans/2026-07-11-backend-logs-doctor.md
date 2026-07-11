# Backend Rotating Logs and Doctor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 detached `cc-partner-backend` 留下受限、轮转、脱敏的本地诊断证据，并提供可人工阅读及机器解析的 `doctor` / `doctor --json` 健康检查。

**Architecture:** backend serve 初始化 stderr 与文件两条 tracing layer；文件 layer 使用白名单字段 formatter 和 5 MiB/3 历史文件的 size rotation writer。Doctor 复用 control/health/dependency helper，在有界 timeout 内构造强类型 snapshot，严格按核心/可选问题判定 healthy/degraded/unhealthy，并把 JSON stdout 与 stderr tracing 分离。

**Tech Stack:** Rust 2021, tracing, tracing-subscriber, tracing-appender, tokio, serde_json, reqwest, platform process/filesystem APIs.

## Global Constraints

- 前置依赖：先完成 cross-platform smoke plan；本计划先完成 rotation/sanitizer 并通过敏感字段测试，再实现 doctor，默认持久化不能早于该门禁。
- 执行阶段先使用 `superpowers:using-git-worktrees` 创建独立 worktree/branch；每次 broad `git add` 前检查 `git status --short`，只提交本计划文件。
- 开始前读取根 `AGENTS.md` 与 `src-tauri/CLAUDE.md`；新增/修改业务函数写中文 Business Logic / Code Logic doc comment。
- 当前日志上限固定 5 MiB，最多保留 3 个历史文件；current file 另算，文件名固定 `backend.log`、`.1`、`.2`、`.3`。
- Unix current/history 文件 mode 为 `0600`，日志目录 `0700`；Windows 使用当前用户应用数据目录权限。
- 文件日志只允许 timestamp、level、request_id、domain、operation、result、elapsed_ms、error_code、sanitized message；stderr 使用同一 sanitizer，只改变排版。
- 禁止写入 Prompt/会话正文、文件内容、请求正文、完整环境变量、token/password/key/Authorization、Claude/Codex 凭据。
- `doctor --json` stdout 只能有一份合法 JSON；说明与 tracing 写 stderr。不枚举环境变量、项目或 Prompt，不做上传/Issue/远程收集。
- home 在 doctor/recent errors 中替换为 `<HOME>`；所有 dependency/network probe 有短 timeout。
- backend 正常 stopped 是信息，不是 warning；状态和退出码严格遵循 0/1/2。
- tracing 测试使用 `tracing::subscriber::with_default` 和独立 writer，禁止并行测试争抢全局 subscriber。

---

## File Structure

- Create `src-tauri/src/backend/logging.rs`: log config/path、rotating writer、strict formatter/sanitizer、guard、recent errors reader。
- Create `src-tauri/src/backend/doctor.rs`: snapshot/check DTO、probes、privacy normalization、status calculation。
- Modify `src-tauri/src/backend/mod.rs`: exports。
- Modify `src-tauri/src/backend/cli.rs`: serve tracing lifecycle、doctor parsing/output/exit codes。
- Modify `src-tauri/src/bin/cc-partner-backend.rs`: 更新入口 doc comment，明确直接转发 CLI 的 0/1/2 exit code。
- Modify `src-tauri/src/config.rs`: 复用 smoke plan 已落地的 `data_dir()`，增加 `backend_log_dir()`/`backend_log_path()`。
- Modify `src-tauri/src/workbench/dependencies.rs`: expose non-mutating bounded dependency probes。
- Modify `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`: add locked `tracing-appender`; rotation remains local for exact semantics。
- Modify `.github/workflows/cross-platform-smoke.yml`: doctor/rotation smoke。
- Modify `src-tauri/CLAUDE.md`, `README.md`: verified operations and privacy policy。

## Shared Interfaces

```rust
pub const BACKEND_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const BACKEND_LOG_HISTORY_FILES: usize = 3;

pub struct BackendLoggingGuard {
    _worker_guard: tracing_appender::non_blocking::WorkerGuard,
}

pub fn init_backend_tracing(
    config: BackendLogConfig,
) -> Result<BackendLoggingGuard, AppError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DoctorStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorVersion {
    pub app: String,
    pub backend: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorPlatform {
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DoctorCheckStatus {
    Ok,
    Warning,
    Error,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    pub status: DoctorCheckStatus,
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorBackendCheck {
    pub state: String,
    pub control_path: String,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub health: DoctorCheck,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorPathChecks {
    pub data: DoctorCheck,
    pub database: DoctorCheck,
    pub log: DoctorCheck,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorDependencies {
    pub git: DoctorCheck,
    pub tmux: DoctorCheck,
    pub wsl: DoctorCheck,
    pub claude_cli: DoctorCheck,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorErrorSummary {
    pub timestamp: String,
    pub code: String,
    pub summary: String,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorSnapshot {
    pub schema_version: u32,
    pub generated_at: String,
    pub status: DoctorStatus,
    pub version: DoctorVersion,
    pub platform: DoctorPlatform,
    pub backend: DoctorBackendCheck,
    pub paths: DoctorPathChecks,
    pub mdns: DoctorCheck,
    pub dependencies: DoctorDependencies,
    pub recent_errors: Vec<DoctorErrorSummary>,
    pub log_path: String,
}
```

Exit contract:

```text
healthy   -> 0
degraded  -> 1
unhealthy -> 2
doctor cannot complete -> 2
```

---

### Task 1: Implement an Exact Size-Rotating Writer

**Files:**
- Create: `src-tauri/src/backend/logging.rs`
- Modify: `src-tauri/src/backend/mod.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`

- [ ] **Step 1: Write failing rotation tests**

Use tiny test limits to prove: append below limit stays current; a record that would exceed the limit rotates before writing; `.1` is newest historical; `.3` is oldest; `.4` never exists; restart reads current length; one record larger than the limit returns `InvalidInput` and no file exceeds the configured maximum.

- [ ] **Step 2: Run the focused failing test**

```bash
cd src-tauri
cargo test --locked backend::logging::tests::rotates_before_crossing_size_limit
```

Expected: module/implementation missing.

- [ ] **Step 3: Implement `RotatingLogWriter`**

Store current path, max bytes, history count, `Option<File>` and current length. Under a mutex, close the current file before rename, delete oldest, rename `.2→.3`, `.1→.2`, current→`.1`, then reopen current. Rotation errors must surface instead of discarding records.

- [ ] **Step 4: Apply permissions on every creation/rotation**

On Unix create directory mode `0700` and files `0600`, then verify metadata. Reopened/renamed files retain or reset `0600`. Windows tests assert files remain writable/readable by the current process.

- [ ] **Step 5: Add a non-blocking guard**

Wrap the writer with `tracing_appender::non_blocking`; return a guard that lives for the full serve lifetime. Test that dropping it after emit flushes the record.

- [ ] **Step 6: Verify and commit**

```bash
cd src-tauri
cargo test --locked backend::logging::tests
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/backend src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat: add bounded backend log rotation"
```

---

### Task 2: Enforce a Sanitized File-Log Schema

**Files:**
- Modify: `src-tauri/src/backend/logging.rs`

- [ ] **Step 1: Write hostile fixture tests**

Emit fields/messages containing Authorization Bearer, token, secret, password, key material, Prompt text, a file-content sentinel, home username/path and a request body. Assert raw and encoded forms never appear in current/history files; allowed request ID/domain/operation/result/duration/error code remain.

- [ ] **Step 2: Implement a strict event visitor/formatter**

Unknown fields are dropped. Known fields are length-bounded. `message` and error text pass through `sanitize_diagnostic_text`, which replaces home with `<HOME>`, redacts secret-key/value patterns, strips header/body-shaped content, normalizes control characters and caps each value at 8 KiB on a valid UTF-8 boundary.

- [ ] **Step 3: Make structured fields the production path**

Expose helpers or a documented field schema for request/operation completion. Update backend HTTP/control high-value events to record only request_id/domain/operation/result/elapsed_ms/error_code and sanitized error summary. Never log request payloads, including debug mode.

- [ ] **Step 4: Add repository-wide sensitive-field regression**

Representative health/control/P2P errors write to fixtures, then tests scan every current/history file. Audit tracing calls under backend/net for banned field names and remove free-form payload logging.

- [ ] **Step 5: Run tests and commit**

```bash
cd src-tauri
cargo test --locked backend::logging::tests::redacts_sensitive_diagnostics
cargo test --locked backend::logging::tests
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/backend/logging.rs src-tauri/src/backend src-tauri/src/net
git commit -m "feat: sanitize backend file diagnostics"
```

---

### Task 3: Initialize File Logging in the Serving Child

**Files:**
- Modify: `src-tauri/src/backend/cli.rs`
- Modify: `src-tauri/src/backend/mod.rs`

- [ ] **Step 1: Add tracing lifecycle tests**

Start a serve runtime in an isolated data dir, emit a known structured event, shut down, then assert the file contains it. Assert detached parent never opens/writes the same log file and the child guard survives until shutdown.

- [ ] **Step 2: Replace stderr-only `init_tracing`**

In `serve`, build one subscriber with human-readable-but-sanitized stderr and the strict JSON file layer. Store `BackendLoggingGuard` through all shutdown awaits. If log directory/file is unavailable, startup fails explicitly.

- [ ] **Step 3: Keep detached stdio behavior unambiguous**

The parent may detach stdio, but the serving child writes its own file. Do not redirect parent and child descriptors to the rotating file simultaneously.

- [ ] **Step 4: Verify and commit**

```bash
cd src-tauri
cargo test --locked backend::cli::tests
cargo test --locked backend::logging::tests
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/backend/cli.rs src-tauri/src/backend/mod.rs
git commit -m "feat: persist detached backend diagnostics"
```

---

### Task 4: Define Doctor Schema and Privacy Normalization

**Files:**
- Create: `src-tauri/src/backend/doctor.rs`
- Modify: `src-tauri/src/backend/mod.rs`

- [ ] **Step 1: Write stable JSON snapshot tests**

Use a fixed clock/platform fixture to assert camelCase schema fields, `schemaVersion=1`, status, version/platform/arch, backend/control/pid/port/health, path checks, mDNS, dependencies, recentErrors and logPath. `serde_json::from_str` must round-trip.

- [ ] **Step 2: Define check severity and overall status**

Each check has `status: ok|warning|error|info`, stable code and sanitized summary. Overall status is computed:

- healthy: core data/database/log paths usable, control self-consistent, no warnings; stopped backend is info.
- degraded: complete doctor with optional dependency missing, mDNS failure, or recoverable stale control.
- unhealthy: core path inaccessible, active control unreachable/unrecoverable, or a core probe cannot complete.

- [ ] **Step 3: Implement privacy helpers**

Normalize home prefixes in every path/text field to `<HOME>`, never include project names, and expose no environment map. Hostile fixtures contain a username, project, Prompt, token and file sentinel and assert absence from serialized JSON.

- [ ] **Step 4: Verify and commit**

```bash
cd src-tauri
cargo test --locked backend::doctor::tests::serializes_stable_snapshot
cargo test --locked backend::doctor::tests::removes_private_values
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/backend/doctor.rs src-tauri/src/backend/mod.rs
git commit -m "feat: define backend doctor snapshot"
```

---

### Task 5: Implement Bounded Doctor Probes

**Files:**
- Modify: `src-tauri/src/backend/doctor.rs`
- Modify: `src-tauri/src/workbench/dependencies.rs`

- [ ] **Step 1: Add fixture tests for core states**

Cover running healthy, stopped healthy, recoverable stale degraded, active/unreachable unhealthy, occupied port conflict, unreadable data/db/log unhealthy, mDNS warning, and missing Git/tmux/WSL/Claude optional dependencies degraded.

- [ ] **Step 2: Implement control/health/path probes**

Reuse `current_status` and health helpers. Test directories with minimal create/open/read checks inside existing paths without deleting data. Every network/process probe uses an explicit timeout and returns a structured check.

- [ ] **Step 3: Implement non-mutating dependency probes**

Reuse command-location/version detection for Git, tmux, WSL and Claude CLI; never install, start WSL, alter PATH or run user configuration. Platform-inapplicable dependencies are info, not warning.

- [ ] **Step 4: Add bounded mDNS summary**

Probe whether discovery can initialize/listen within a short timeout, then stop it. Do not enumerate device/project names. mDNS probe failure is always a degraded warning, never an unhealthy core failure.

- [ ] **Step 5: Read recent errors safely**

Read only tails of current and newest history files, accept only the controlled JSON/error-level schema, re-sanitize, cap count/message length, and ignore malformed lines with a warning check. Never read arbitrary user-selected files.

- [ ] **Step 6: Verify and commit**

```bash
cd src-tauri
cargo test --locked backend::doctor::tests
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/backend/doctor.rs src-tauri/src/workbench/dependencies.rs
git commit -m "feat: collect backend doctor checks"
```

---

### Task 6: Add CLI Parsing, Output Discipline, and Exit Codes

**Files:**
- Modify: `src-tauri/src/backend/cli.rs`
- Modify: `src-tauri/src/bin/cc-partner-backend.rs`

- [ ] **Step 1: Write dispatch/output tests**

Cover `doctor`, `doctor --json`, unknown option, extra args and probe failure. Capture stdout/stderr separately. JSON stdout parses directly and has no tracing prefix; text output includes status/check table/log path without private raw paths.

- [ ] **Step 2: Refactor CLI result to carry exit status**

Map snapshot status to 0/1/2 and construction error to 2 while preserving start/serve/stop/status semantics. Initialize diagnostic tracing on stderr only for the doctor process.

- [ ] **Step 3: Implement human-readable output**

Print one summary and warning/error checks; label stopped backend normal. Recent errors cannot exceed the sanitized JSON detail.

- [ ] **Step 4: Verify real JSON for every exit status**

```bash
cd src-tauri
set +e
cargo run --locked --bin cc-partner-backend -- doctor --json > /tmp/cc-partner-doctor.json
doctor_exit=$?
set -e
jq -e . /tmp/cc-partner-doctor.json
test "$doctor_exit" -ge 0 -a "$doctor_exit" -le 2
```

- [ ] **Step 5: Run tests and commit**

```bash
cd src-tauri
cargo test --locked backend::cli::tests
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/backend/cli.rs src-tauri/src/bin/cc-partner-backend.rs
git commit -m "feat: add backend doctor command"
```

---

### Task 7: Cross-Platform Rotation and Doctor Regression

**Files:**
- Modify: `.github/workflows/cross-platform-smoke.yml`
- Create: `src-tauri/tests/backend_doctor_smoke.rs`
- Modify: `src-tauri/src/backend/logging.rs` platform-specific unit tests

- [ ] **Step 1: Add macOS/Windows doctor smoke**

Run stopped `doctor --json`, parse JSON, accept only expected 0/1, and fail on 2. Start backend, run doctor again, then stop. Preserve stdout JSON, stderr and log directory as failure artifacts.

- [ ] **Step 2: Exercise rotation on both platforms**

Use a small fixture limit to force close/rename/reopen; assert the 3-history ceiling. On Unix assert mode; on Windows assert no open-handle rename failure.

- [ ] **Step 3: Add privacy artifact scan**

Seed a unique secret/home/Prompt/file sentinel only in hostile input, run logs/doctor, recursively scan smoke artifacts, and fail if it appears.

- [ ] **Step 4: Run workflow_dispatch on both platforms**

Confirm JSON remains pure, optional dependency absence yields degraded/1 rather than infrastructure failure, and a core-path fixture yields unhealthy/2.

- [ ] **Step 5: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add .github/workflows/cross-platform-smoke.yml src-tauri/src/backend src-tauri/tests
git commit -m "ci: smoke backend logs and doctor"
```

---

### Task 8: Documentation and Final Verification

**Files:**
- Modify: `src-tauri/CLAUDE.md`
- Modify: `README.md`

- [ ] **Step 1: Document operations and privacy**

Record log path/name/5 MiB/3 history/current-user permissions, approved/forbidden fields, doctor commands/schema/exit codes and no-upload behavior. README contains user-facing commands only; engineering details stay in backend instructions.

- [ ] **Step 2: Run final backend verification**

```bash
cd src-tauri
cargo test --locked backend::logging::tests
cargo test --locked backend::doctor::tests
cargo test --locked backend::cli::tests
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo check --locked --bins
```

- [ ] **Step 3: Scan produced fixtures and source**

Run representative lifecycle/doctor fixtures and scan current/history/JSON outputs for secret/token/Authorization/Prompt/file/home sentinels. Review every backend/net tracing call that passes a free-form payload.

- [ ] **Step 4: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/CLAUDE.md README.md
git commit -m "docs: document backend diagnostics"
```

## Completion Contract

- Detached backend writes bounded, current-user-only, sanitized local logs with exactly 3 historical files maximum.
- File logging drops unknown fields and regression tests prove prohibited content never appears.
- `doctor` and `doctor --json` expose the approved schema without projects/env/credentials and replace home with `<HOME>`.
- healthy/degraded/unhealthy and exits 0/1/2 match the written rules; normally stopped is healthy information.
- Probes are non-mutating and bounded; recent errors come only from controlled sanitized logs.
- macOS/Windows rotation/doctor smoke and Rust fmt/clippy/tests/check pass.
