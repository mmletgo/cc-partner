# Backend Transactional Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 Cloud Sync、配置、快捷键、Updater 与 Health 命令在并发、取消、磁盘故障和非法输入下保持可恢复的一致状态。

**Architecture:** 新增 `ConfigRuntime` 串行 clone/validate/durable replace/swap，`CloudSyncRuntime` 覆盖正式 Git 工作区全流程单飞，`UpdateRuntime` 用单锁 generation 状态机拒绝 stale callback。快捷键使用“注册新值—注销旧值—持久化—失败补偿”，Health 在命令边界使用纯 validator 与 checked arithmetic。

**Tech Stack:** Rust 2021, Tauri 2, tokio, serde_json, filesystem sync/atomic replace, tauri-plugin-global-shortcut, tauri-plugin-updater, Git CLI, React 19, TypeScript, Vitest.

## Global Constraints

- 开始前读取根 `AGENTS.md`、`src-tauri/CLAUDE.md`、`web/CLAUDE.md`；代码 UTF-8，新增/修改业务函数写中文 Business Logic / Code Logic 注释。
- 执行时先用 `superpowers:using-git-worktrees` 建独立 worktree；每个 Task 单独 review/commit，只 stage 本任务文件。
- 配置提交顺序固定为 `clone → mutate → validate → temp write → flush/fsync → re-read → atomic replace → directory fsync → memory swap`；错误返回时内存与旧文件保持一致。
- Unix config/secrets 目录保持 `0700`、文件 `0600`；Windows replace 使用 replace-existing + write-through 语义，不能先删除目标形成空窗。
- 所有配置 writer 经过同一 `tokio::sync::Mutex`，禁止 clone/swap lost update；任何 std `RwLockGuard` 都不能跨 await。
- Cloud Sync gate 覆盖 ensure/clone/fetch/reset/import/export/commit/push 全流程；scheduler 忙时跳过，手动与 CLAUDE.md push 最多等 5 分钟。
- Updater metadata/bytes/task/cancel/status/phase/generation 在一个 mutex state 中；callback 只有 generation 和 phase 同时匹配才可写。
- 安装失败保留 `Arc<[u8]>` 与 metadata，允许重试；cancel 先原子改 phase/取 handle，再锁外 cancel/abort。
- Health 固定范围：work 60..=28800、break 30..=7200、retain 1..=3650、water 300..=86400、snooze 1..=1440；DND 两端同时空或严格 `HH:MM`。
- 时间乘加减统一 `checked_mul/add/sub`；非法输入返回 Validation 且不得改变 config/runtime/数据库。
- 本计划不修改 SQLite schema；若实现中出现数据库 DDL，停止并拆出独立兼容/回滚设计。
- 不引入 Redux/Zustand，不更换 Tauri updater/Git CLI，不持久化更新 bytes 或 Cloud Sync 队列。

---

## Task Dependency Graph

最大并行 waves：`T1 → T2 → T3`；`T1 → T4`；`T1 → T7`；`T5 → T6`；`(T3 | T4 | T6 | T7) → T8 → T9`。T1 atomic store 是全部配置写入前置；Updater T5/T6 可与配置/Cloud Sync 分支并行；T8 汇总故障/并发 smoke，T9 校准文档和全量门禁。

## File Structure

- Create `src-tauri/src/config_store.rs`: `ConfigStore`、atomic filesystem implementation、fault injection adapter。
- Create `src-tauri/src/config_runtime.rs`: writer gate 与 transactional update helper。
- Modify `src-tauri/src/config.rs`: `validate()`、load/migration 改用 atomic store。
- Modify `src-tauri/src/state.rs`, `backend/runtime.rs`, `lib.rs`: 初始化/共享 `ConfigRuntime`、`CloudSyncRuntime`、`UpdateRuntime`。
- Modify `src-tauri/src/commands/{config,cloud_sync,github_trending,health,orchestrator_config}.rs`: 迁移配置 writer。
- Modify `src-tauri/src/hotkey.rs`: 可测试的 shortcut backend 与补偿事务。
- Create `src-tauri/src/cloud_sync/runtime.rs`; modify `cloud_sync/{mod,engine,scheduler}.rs`, `commands/claude_md.rs`: 全流程单飞。
- Create `src-tauri/src/updater/runtime.rs`, `src-tauri/src/updater/mod.rs`; modify `commands/updater.rs`: 单锁 generation 状态机。
- Create `src-tauri/src/health/validation.rs`; modify `commands/health.rs`, `health/mod.rs`: validator/checked arithmetic。
- Modify `web/src/lib/types.ts`, `web/src/api/config.ts`, `web/src/pages/Settings/Settings.tsx`, related tests: checking/installing/retry 与 Health validation UX。
- Create `src-tauri/tests/transactional_runtime_smoke.rs`; modify cross-platform smoke workflow and docs。

## Shared Interfaces

```rust
pub trait ConfigStore: Send + Sync {
    fn load(&self) -> Result<AppConfig, AppError>;
    fn save_atomic(&self, candidate: &AppConfig) -> Result<(), AppError>;
}

pub struct ConfigRuntime {
    pub value: Arc<RwLock<AppConfig>>,
    update_lock: tokio::sync::Mutex<()>,
    store: Arc<dyn ConfigStore>,
}

pub async fn update_config_transactionally<T>(
    runtime: &ConfigRuntime,
    mutate: impl FnOnce(&mut AppConfig) -> Result<T, AppError>,
) -> Result<(AppConfig, T), AppError>;
```

```rust
pub enum CloudSyncTrigger { Manual, Scheduler, ClaudeMdPush }
pub enum CloudSyncBusyPolicy { Wait { timeout: Duration }, ReturnBusy }

pub async fn run_cloud_sync_exclusive<T, F, Fut>(
    runtime: &CloudSyncRuntime,
    trigger: CloudSyncTrigger,
    policy: CloudSyncBusyPolicy,
    operation: F,
) -> Result<Option<T>, AppError>
where F: FnOnce() -> Fut, Fut: Future<Output = Result<T, AppError>>;
```

```rust
pub enum UpdatePhase {
    Idle, Checking, Available, Downloading, Downloaded, Installing, Failed, Cancelled,
}

pub struct UpdateRuntimeState {
    pub generation: u64,
    pub phase: UpdatePhase,
    pub pending: Option<tauri_plugin_updater::Update>,
    pub bytes: Option<Arc<[u8]>>,
    pub cancel: Option<CancellationToken>,
    pub task: Option<JoinHandle<()>>,
    pub status: UpdateDownloadStatus,
}
```

### Task 1: Build the Durable Atomic Config Store

**Files:**
- Create: `src-tauri/src/config_store.rs`
- Create: `src-tauri/src/config_runtime.rs`
- Modify: `src-tauri/src/config.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`

**Interfaces:**
- Produces: `FsConfigStore`, `ConfigIo`, `ConfigRuntime`, `update_config_transactionally`, `AppConfig::validate`。
- Consumes: existing `data_dir/config_file_path`, serde defaults and data-dir isolation migration。

- [ ] **Step 1: Write atomicity and validation tests first**

Add tests for successful UTF-8 round-trip, concurrent writers preserving both non-conflicting patches, temp-file uniqueness, re-read mismatch, and faults at create/write/flush/file-sync/rename/directory-sync. Every injected error asserts old JSON parses, memory is unchanged and a later healthy save succeeds. Validate malformed db isolation, shortcuts, Cloud interval, Health and Orchestrator ranges.

- [ ] **Step 2: Run the red tests**

```bash
cd src-tauri
cargo test --locked config_store::tests --lib && cargo test --locked config_runtime::tests --lib && cargo test --locked config::tests::validate --lib
```

Expected: FAIL because store/runtime/validate do not exist.

- [ ] **Step 3: Implement exact durable replace semantics**

Write same-directory `.config.json.<uuid>.tmp`, set permissions, serialize UTF-8 JSON, flush + `sync_all`, re-read and deserialize/compare, then atomically replace. Unix uses `rename` and parent-directory `sync_all`; Windows uses `ReplaceFileW` for existing target or `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)` for first creation. Remove only this invocation's temp file on failure; clean stale matching temp files older than 24 hours at startup.

- [ ] **Step 4: Implement writer serialization and swap-after-durability**

Hold only the async `update_lock` across the operation. Clone from `Arc<RwLock<AppConfig>>`, release read guard, mutate/validate/save, then take the write lock only for final swap. `AppConfig::load()` migrations and first-install creation use `FsConfigStore::save_atomic` rather than `fs::write`.

- [ ] **Step 5: Verify and commit**

```bash
cd src-tauri
cargo test --locked config_store::tests --lib && cargo test --locked config_runtime::tests --lib && cargo test --locked config::tests --lib
git add src-tauri/src/config_store.rs src-tauri/src/config_runtime.rs src-tauri/src/config.rs src-tauri/src/lib.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat: persist config atomically"
```

### Task 2: Migrate Every Configuration Writer

**Files:**
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/commands/config.rs`
- Modify: `src-tauri/src/commands/cloud_sync.rs`
- Modify: `src-tauri/src/commands/github_trending.rs`
- Modify: `src-tauri/src/commands/health.rs`
- Modify: `src-tauri/src/commands/orchestrator_config.rs`

**Interfaces:**
- Produces: `AppState.config_runtime`; all writer commands return committed DTO from candidate。
- Consumes: T1 `update_config_transactionally`。

- [ ] **Step 1: Add command rollback and lost-update tests**

For each writer inject `ConfigStore::save_atomic` failure and assert command returns Err, `get_*_config` returns old value, and disk remains old. Start two blocked updates to different fields, release them together, and assert final config contains both changes in serialized writer order.

- [ ] **Step 2: Confirm existing commands fail the new tests**

```bash
cd src-tauri
cargo test --locked commands::config::tests::save_failure_rolls_back --lib && cargo test --locked commands::cloud_sync::tests::concurrent_config_updates_do_not_lose_fields --lib && cargo test --locked commands::health::tests::save_failure_rolls_back --lib
```

Expected: FAIL because commands mutate `state.config` before `save()`.

- [ ] **Step 3: Replace all six direct `cfg.save()` writer paths**

Use one closure per command to normalize and mutate candidate; return DTO from the committed `AppConfig` returned by helper. Keep `state.config` as a read-compatible Arc sharing `config_runtime.value` during migration; do not create a second divergent value.

- [ ] **Step 4: Prove there are no production direct writers**

```bash
cd src-tauri
rg -n "\.save\(\)\?" src/commands
```

Expected: no output. `AppConfig::save` may remain a deprecated test wrapper only if it delegates to `FsConfigStore::save_atomic`.

- [ ] **Step 5: Verify and commit**

```bash
cd src-tauri
cargo test --locked commands::config --lib && cargo test --locked commands::cloud_sync --lib && cargo test --locked commands::github_trending --lib && cargo test --locked commands::health --lib && cargo test --locked commands::orchestrator_config --lib
git add src-tauri/src/state.rs src-tauri/src/backend/runtime.rs src-tauri/src/commands
git commit -m "refactor: serialize config updates"
```

### Task 3: Make Screenshot Hotkey Replacement Compensatable

**Files:**
- Modify: `src-tauri/src/hotkey.rs`
- Modify: `src-tauri/src/commands/config.rs`

**Interfaces:**
- Produces: `GlobalShortcutBackend`, `replace_screenshot_hotkey`, rollback error `hotkey.rollback_failed`。
- Consumes: T1/T2 config transaction and existing `screenshot_handler`。

- [ ] **Step 1: Write a fake shortcut backend and transition matrix tests**

Cover parse failure, new-register conflict, old-unregister failure, config-store failure after OS replacement, old re-register failure during compensation, unchanged value, and success. Assert the exact registered set plus disk/memory value after each case; no test uses the process-global real plugin.

- [ ] **Step 2: Run the focused red tests**

```bash
cd src-tauri
cargo test --locked hotkey::tests::replace --lib && cargo test --locked commands::config::tests::hotkey --lib
```

Expected: FAIL because production uses `unregister_all` and ignores boolean failure.

- [ ] **Step 3: Implement register-new before unregister-old and explicit compensation**

Parse both values first. Register new while old remains; if successful unregister only old, never `unregister_all`. Persist through ConfigRuntime. On persistence failure, re-register old then unregister new. If compensation fails, keep disk/memory old and return `hotkey.rollback_failed` with restart guidance; never report success.

- [ ] **Step 4: Verify startup and hot-update paths**

```bash
cd src-tauri
cargo test --locked hotkey::tests --lib && cargo test --locked commands::config::tests --lib
cargo check --locked --all-targets
```

Expected: PASS; setup still registers the configured shortcut once.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/hotkey.rs src-tauri/src/commands/config.rs
git commit -m "fix: rollback failed hotkey updates"
```

### Task 4: Serialize Every Cloud Sync Worktree Writer

**Files:**
- Create: `src-tauri/src/cloud_sync/runtime.rs`
- Modify: `src-tauri/src/cloud_sync/mod.rs`
- Modify: `src-tauri/src/cloud_sync/engine.rs`
- Modify: `src-tauri/src/cloud_sync/scheduler.rs`
- Modify: `src-tauri/src/commands/cloud_sync.rs`
- Modify: `src-tauri/src/commands/claude_md.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/runtime.rs`

**Interfaces:**
- Produces: `CloudSyncRuntime`, `run_cloud_sync_exclusive`, status fields `runningTrigger/startedAt/lastResult/skippedBusy`。
- Consumes: existing `trigger_cloud_sync`, `push_claude_md_to_cloud`, scheduler tick。

- [ ] **Step 1: Write barrier-based concurrency tests**

Inject an operation closure that records entry/exit. Prove two Wait callers never overlap, scheduler ReturnBusy returns `Ok(None)` without running, Wait times out at an injected short duration, panic/cancellation releases gate, and config is re-read only after acquisition. Add an engine test running manual sync and CLAUDE.md push against one fake Git workdir with no interleaved reset/write.

- [ ] **Step 2: Run red tests**

```bash
cd src-tauri
cargo test --locked cloud_sync::runtime::tests --lib && cargo test --locked cloud_sync::engine::tests::writers_do_not_overlap --lib
```

Expected: FAIL because no gate exists.

- [ ] **Step 3: Wrap the complete worktree lifecycle**

Manual and CLAUDE.md push use `Wait { timeout: 300s }`; scheduler uses `ReturnBusy`. Acquire before `detect_git/ensure_repo` and release only after final push/error. After acquisition re-read repo URL/branch. Before reusing an existing workdir run bounded `git status --porcelain`/`.git` integrity checks; a dirty sync-generated worktree is reset through the existing remote reconciliation path, not silently reported successful.

- [ ] **Step 4: Verify trigger behavior**

```bash
cd src-tauri
cargo test --locked cloud_sync:: --lib
cargo test --locked commands::cloud_sync --lib && cargo test --locked commands::claude_md --lib
```

Expected: PASS; scheduler busy path increments `skippedBusy` once and queues no future.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/cloud_sync src-tauri/src/commands/cloud_sync.rs src-tauri/src/commands/claude_md.rs src-tauri/src/state.rs src-tauri/src/backend/runtime.rs
git commit -m "fix: serialize cloud sync worktree access"
```

### Task 5: Build the Single-Lock Updater Generation State Machine

**Files:**
- Create: `src-tauri/src/updater/mod.rs`
- Create: `src-tauri/src/updater/runtime.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `UpdateRuntime::{begin_check,finish_check,begin_download,record_progress,finish_download,cancel,begin_install,finish_install}`。
- Consumes: existing `UpdateDownloadStatus`, owned `Update`, `CancellationToken`, `JoinHandle`。

- [ ] **Step 1: Write pure transition tests**

Cover every legal phase edge; illegal check during downloading/installing; generation increment only at begin-check; old-generation progress/completion ignored; cancel takes handle/token once; new check clears old bytes; install failure returns to Downloaded and retains `Arc<[u8]>`; install success reaches terminal restart-requested result.

- [ ] **Step 2: Run the focused red tests**

```bash
cd src-tauri
cargo test --locked updater::runtime::tests --lib
```

Expected: FAIL because updater runtime does not exist.

- [ ] **Step 3: Implement short critical sections**

Keep one `std::sync::Mutex<UpdateRuntimeState>` because transitions are synchronous and callbacks cannot await. No network/install call occurs under lock. Return `(generation, cloned update/token/bytes)` leases to commands; completion methods compare generation and required phase before mutation.

- [ ] **Step 4: Replace AppState's five updater fields atomically**

Add `pub update_runtime: Arc<UpdateRuntime>` and remove `update_status/update_pending/update_bytes/update_download_task/update_cancel_token` in the same compile-green task. Update test AppState builders discovered by `rg "update_pending|update_status" src-tauri/src`.

- [ ] **Step 5: Verify and commit**

```bash
cd src-tauri
cargo test --locked updater::runtime::tests --lib
cargo check --locked --all-targets
git add src-tauri/src/updater src-tauri/src/state.rs src-tauri/src/backend/runtime.rs src-tauri/src/lib.rs
git commit -m "refactor: unify updater runtime state"
```

### Task 6: Rewire Updater Commands, Cancellation and Install Retry

**Files:**
- Modify: `src-tauri/src/commands/updater.rs`
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/api/config.ts`
- Modify: `web/src/pages/Settings/Settings.tsx`
- Modify: `web/src/pages/Settings/settingsState.ts`
- Modify: `web/src/pages/Settings/settingsState.test.ts`

**Interfaces:**
- Produces: IPC status values `idle|checking|downloading|completed|installing|failed|cancelled`; retryable install UI。
- Consumes: T5 runtime leases/generation checks and existing `update:download-progress` event。

- [ ] **Step 1: Add command race tests and frontend state tests**

Use fake updater driver/barriers to prove: old download completes after a new check without overwrite; cancel wins over a late callback; duplicate download conflicts; install failure retains bytes and second install succeeds; recheck during installing conflicts; progress is clamped. Frontend tests assert buttons/labels for checking/installing/install-failed retry.

- [ ] **Step 2: Run red tests**

```bash
cd src-tauri && cargo test --locked commands::updater::tests --lib
cd ../web && npm test -- settingsState
```

Expected: FAIL against split locks and old status union.

- [ ] **Step 3: Rewire commands without holding runtime lock across await**

`check_update` begins generation then awaits plugin check and finishes only if lease is current. Download callback calls `record_progress(generation, ...)`; completion moves bytes into `Arc<[u8]>`. Cancel changes phase/takes resources first, then cancels/aborts. Install clones bytes/update, marks Installing, runs `spawn_blocking`, and on error returns to Downloaded with error while retaining data.

- [ ] **Step 4: Update UI for explicit phases and retry**

Disable check/download while checking/installing; show install retry only when phase is completed with non-empty install error. Do not fake progress during install. Keep existing DTO field names and error rendering.

- [ ] **Step 5: Verify and commit**

```bash
cd src-tauri && cargo test --locked commands::updater::tests --lib && cargo test --locked updater::runtime::tests --lib
cd ../web && npm test -- settingsState && npm run lint && npm run build
git add ../src-tauri/src/commands/updater.rs src/lib/types.ts src/api/config.ts src/pages/Settings
git commit -m "fix: isolate updater generations and retry install"
```

### Task 7: Validate Health Inputs with Checked Arithmetic

**Files:**
- Create: `src-tauri/src/health/validation.rs`
- Modify: `src-tauri/src/health/mod.rs`
- Modify: `src-tauri/src/commands/health.rs`
- Modify: `web/src/pages/Settings/HealthPanel.tsx`
- Modify: `web/src/pages/Settings/HealthPanel.test.ts`

**Interfaces:**
- Produces: `validate_health_config`, `checked_future_timestamp`, `checked_water_snooze_origin`, `validate_dnd_pair`。
- Consumes: exact numeric ranges and DND semantics from Global Constraints。

- [ ] **Step 1: Write boundary and no-side-effect tests**

For every numeric field test min-1/min/max/max+1 plus `i64::MIN/MAX`; DND tests cover missing half, `7:00`, `24:00`, `23:60`, valid cross-midnight and equal all-day bounds. Command tests assert invalid update/snooze leaves config, snooze, water state and DB unchanged. Daemon test feeds an invalid legacy config and asserts no panic/no reminder side effect.

- [ ] **Step 2: Run red tests**

```bash
cd src-tauri
cargo test --locked health::validation::tests --lib && cargo test --locked commands::health::tests::rejects_invalid --lib
```

Expected: FAIL because current code accepts values and uses unchecked arithmetic.

- [ ] **Step 3: Implement validators and replace arithmetic**

Parse `HH:MM` by exact two-digit components. Use `checked_mul(60)`, `checked_add`, `checked_sub`, and `retain_days.checked_mul(86400)`. Validate before taking runtime locks or calling ConfigRuntime. For invalid disk-loaded Health config, daemon logs stable field code and skips reminder/cleanup for that tick.

- [ ] **Step 4: Align frontend constraints without relying on them for safety**

Set input min/max and render backend field errors in `HealthPanel`; retain backend as authority. Equal DND values are labeled “全天免打扰”.

- [ ] **Step 5: Verify and commit**

```bash
cd src-tauri && cargo test --locked health:: --lib && cargo test --locked commands::health --lib
cd ../web && npm test -- HealthPanel && npm run build
git add ../src-tauri/src/health ../src-tauri/src/commands/health.rs src/pages/Settings/HealthPanel.tsx src/pages/Settings/HealthPanel.test.ts
git commit -m "fix: validate health runtime inputs"
```

### Task 8: Add Transactional Fault and Concurrency Smoke Coverage

**Files:**
- Create: `src-tauri/tests/transactional_runtime_smoke.rs`
- Modify: `.github/workflows/cross-platform-smoke.yml`
- Modify: `src-tauri/tests/support/mod.rs`

**Interfaces:**
- Produces: isolated macOS/Windows smoke evidence for config recovery and runtime races。
- Consumes: T1–T7 test hooks; no database migration。

- [ ] **Step 1: Implement isolated smoke cases**

Under a unique `CC_PARTNER_DATA_DIR`, write valid config, inject atomic-store stage failures through the test adapter, restart/load and assert valid old/new JSON only. Run two Cloud Sync fake writers and assert max concurrency 1. Run updater stale-generation/cancel/install-retry driver. Feed Health extreme IPC payloads and assert validation/no state change. On Unix assert 0700/0600; on Windows assert replace keeps file readable/writable.

- [ ] **Step 2: Run locally serially**

```bash
cd src-tauri
cargo test --locked --test transactional_runtime_smoke -- --nocapture --test-threads=1
```

Expected: PASS with bounded timeout and per-case cleanup.

- [ ] **Step 3: Add to Cross-Platform Smoke without overstating coverage**

Run on `macos-latest` and `windows-latest`, upload only sanitized stage/result diagnostics on failure, and add NOT VERIFIED entries for actual disk-full hardware behavior, GUI global-shortcut conflicts and real updater installation/restart.

- [ ] **Step 4: Run regression gates**

```bash
cd src-tauri
cargo test --locked config_store:: --lib && cargo test --locked config_runtime:: --lib && cargo test --locked cloud_sync::runtime:: --lib && cargo test --locked updater::runtime:: --lib && cargo test --locked health::validation:: --lib
cargo test --locked --test transactional_runtime_smoke -- --nocapture --test-threads=1
```

Expected: PASS; no test is ignored and no case touches real `~/.cc-partner`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/tests/transactional_runtime_smoke.rs src-tauri/tests/support/mod.rs .github/workflows/cross-platform-smoke.yml
git commit -m "test: exercise transactional backend recovery"
```

### Task 9: Calibrate Persistent Behavior Documentation and Run Final Gates

**Files:**
- Modify: `docs/prd.md`
- Modify: `docs/development/backend-operations.md`
- Modify: `docs/development/testing.md`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `web/CLAUDE.md`

**Interfaces:**
- Produces: authoritative recovery, validation and NOT VERIFIED documentation。
- Consumes: complete implementation and test evidence from T1–T8。

- [ ] **Step 1: Update only persistent contracts**

Document Cloud Sync busy policies, config durable commit/recovery, hotkey rollback, updater checking/installing/install retry, Health ranges/DND, fault-injection commands, and explicit “no SQLite schema change/rollback script required”. Remove old text claiming updater uses five independent AppState locks or hotkey uses `unregister_all`.

- [ ] **Step 2: Scan for stale implementation claims**

```bash
rg -n "update_pending|update_bytes|update_download_task|update_cancel_token|unregister_all|fs::write.*config" src-tauri/CLAUDE.md docs web/CLAUDE.md
```

Expected: no stale production-contract claim; historical text is either removed or explicitly marked historical.

- [ ] **Step 3: Run final Rust gates**

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked config_store:: --lib
cargo test --locked config_runtime:: --lib
cargo test --locked cloud_sync:: --lib
cargo test --locked commands::updater --lib
cargo test --locked updater:: --lib
cargo test --locked health:: --lib
cargo test --locked commands::health --lib
cargo test --locked --test transactional_runtime_smoke -- --nocapture --test-threads=1
```

Expected: all commands exit 0.

- [ ] **Step 4: Run final frontend gates**

```bash
cd web
npm run lint
npm run build
npm test -- settingsState HealthPanel
```

Expected: all commands exit 0; TypeScript status union matches Rust serde values.

- [ ] **Step 5: Commit**

```bash
git add docs/prd.md docs/development/backend-operations.md docs/development/testing.md src-tauri/CLAUDE.md web/CLAUDE.md
git commit -m "docs: record transactional runtime guarantees"
```
