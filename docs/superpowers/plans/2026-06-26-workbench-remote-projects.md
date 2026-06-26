# Workbench Remote Projects Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow Workbench to browse LAN devices, select a remote project folder directly, and operate that project through the same Workbench UI while all files, Git, worktree, terminal, and Prompt optimization work runs on the remote device.

**Architecture:** Keep the frontend Workbench UI mostly shared. Add a Rust gateway layer that routes existing Workbench commands to local execution for `kind=local` projects and to remote HTTP calls for `kind=remote` projects. Remote devices expose Workbench HTTP routes over the existing axum P2P server, and terminal output is bridged back to local Tauri events.

**Tech Stack:** Rust, Tauri 2 commands, axum, reqwest, SQLite/sqlx, tmux/portable-pty, React 19, TypeScript, Vite, CSS Modules, i18next.

---

## File Structure

### Rust

- Create `src-tauri/src/workbench/remote_ids.rs`
  - Owns remote ID prefixing/parsing for project/worktree/session IDs.
- Create `src-tauri/src/workbench/remote_directory.rs`
  - Owns remote directory roots, directory listing, path info, and Git repo detection for the add-project picker.
- Create `src-tauri/src/workbench/remote_client.rs`
  - Owns reqwest calls to `/api/workbench/...` on a discovered peer.
- Create `src-tauri/src/workbench/remote_events.rs`
  - Owns event stream connection to remote devices and re-emits mapped Tauri events locally.
- Create `src-tauri/src/net/routes/workbench.rs`
  - Owns HTTP routes that remote peers call.
- Modify `src-tauri/src/net/routes/mod.rs`
  - Export the new `workbench` route module.
- Modify `src-tauri/src/net/http_server.rs`
  - Register Workbench remote routes and the event stream route.
- Modify `src-tauri/src/workbench/mod.rs`
  - Export new Workbench modules.
- Modify `src-tauri/src/workbench/models.rs`
  - Add remote directory picker DTOs and optional remote fields if needed.
- Modify `src-tauri/src/commands/workbench.rs`
  - Add gateway checks for remote projects/worktrees/sessions.
  - Add commands for remote directory roots/list/info/open-project.
- Modify `src-tauri/src/commands/prompt_optimizer.rs`
  - Route Workbench Prompt optimization to the remote device when the target session/project is remote.
- Modify `src-tauri/src/state.rs`
  - Add shared runtime for remote Workbench event stream lifecycle if needed.
- Modify `src-tauri/src/lib.rs`
  - Register new Tauri commands and initialize remote event runtime.
- Modify `src-tauri/CLAUDE.md`
  - Record remote Workbench architecture and relevant Rust verification commands.

### Frontend

- Modify `web/src/lib/types.ts`
  - Add remote directory picker types and narrow `WorkbenchProjectKind` to include `remote`.
- Modify `web/src/api/workbench.ts`
  - Add remote directory picker APIs and remote project open API.
- Modify `web/src/hooks/workbenchProjectsContext.ts`
  - Add remote add-project action to context.
- Modify `web/src/hooks/useWorkbenchProjects.tsx`
  - Support local-vs-remote add flow and remote project insertion.
- Modify `web/src/components/domain/WorkbenchProjectRail/WorkbenchProjectRail.tsx`
  - Replace direct `+` behavior with a small source chooser.
- Create `web/src/components/domain/WorkbenchRemoteProjectPicker/`
  - Device list, root list, directory list, breadcrumb, selection, and open action.
- Modify `web/src/pages/Workbench/Workbench.tsx`
  - Add remote offline messaging and ensure existing stale guards handle remote IDs.
- Modify `web/src/i18n/locales/{zh,en}/workbench.json`
  - Add remote picker and remote error text.
- Modify `web/CLAUDE.md`
  - Record remote Workbench frontend conventions and targeted validation commands.

### Tests

- Create `src-tauri/src/workbench/remote_ids.rs` unit tests.
- Create `src-tauri/src/workbench/remote_directory.rs` unit tests.
- Extend `src-tauri/src/commands/workbench.rs` tests for remote gateway dispatch helpers.
- Create `web/src/pages/Workbench/workbenchRemoteProjects.test.ts`.
- Extend `web/src/pages/Workbench/workbenchFiles.test.ts` only if remote IDs affect file tab keys.

## Implementation Rule

This feature will exceed 100 lines and crosses frontend, Rust backend, P2P networking, and docs. Execute implementation in a git worktree branch such as:

```bash
git worktree add -b codex/workbench-remote-projects ../cc-partner-workbench-remote
```

Use subagents for disjoint implementation slices if executing this plan:

- Rust remote directory + HTTP routes.
- Rust gateway + remote client + events.
- Frontend remote project picker.
- Verification and docs.

## Task 1: Remote ID Helpers

**Files:**
- Create: `src-tauri/src/workbench/remote_ids.rs`
- Modify: `src-tauri/src/workbench/mod.rs`
- Test: `src-tauri/src/workbench/remote_ids.rs`

- [ ] **Step 1: Write failing unit tests**

Add tests for deterministic remote project IDs and remote entity ID parsing:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_project_id_is_stable_for_device_and_path() {
        let first = remote_project_id("device-a", "/Users/hans/web_project/app");
        let second = remote_project_id("device-a", "/Users/hans/web_project/app");

        assert_eq!(first, second);
        assert!(first.starts_with("remote:device-a:"));
    }

    #[test]
    fn parse_remote_id_returns_device_and_inner_id() {
        let parsed = parse_remote_entity_id("remote:device-a:session-1").unwrap();

        assert_eq!(parsed.device_id, "device-a");
        assert_eq!(parsed.inner_id, "session-1");
    }

    #[test]
    fn parse_local_id_returns_none() {
        assert!(parse_remote_entity_id("local-session").is_none());
    }
}
```

- [ ] **Step 2: Run test and confirm failure**

Run:

```bash
cd src-tauri
cargo test workbench::remote_ids --lib
```

Expected: compile failure because `remote_ids` does not exist.

- [ ] **Step 3: Implement `remote_ids`**

Implement:

```rust
//! workbench/remote_ids.rs — Workbench remote ID mapping

use sha2::{Digest, Sha256};

const REMOTE_PREFIX: &str = "remote";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntityId {
    pub device_id: String,
    pub inner_id: String,
}

pub fn remote_project_id(device_id: &str, path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(device_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(path.as_bytes());
    let digest = hasher.finalize();
    format!("{REMOTE_PREFIX}:{device_id}:{:x}", digest)
}

pub fn remote_entity_id(device_id: &str, inner_id: &str) -> String {
    format!("{REMOTE_PREFIX}:{device_id}:{inner_id}")
}

pub fn parse_remote_entity_id(value: &str) -> Option<RemoteEntityId> {
    let mut parts = value.splitn(3, ':');
    let prefix = parts.next()?;
    if prefix != REMOTE_PREFIX {
        return None;
    }
    let device_id = parts.next()?.to_string();
    let inner_id = parts.next()?.to_string();
    if device_id.is_empty() || inner_id.is_empty() {
        return None;
    }
    Some(RemoteEntityId { device_id, inner_id })
}

pub fn is_remote_id(value: &str) -> bool {
    parse_remote_entity_id(value).is_some()
}
```

Export it from `src-tauri/src/workbench/mod.rs`:

```rust
pub mod remote_ids;
```

- [ ] **Step 4: Run test and confirm pass**

Run:

```bash
cd src-tauri
cargo test workbench::remote_ids --lib
```

Expected: all `remote_ids` tests pass.

## Task 2: Remote Directory Browser Backend

**Files:**
- Create: `src-tauri/src/workbench/remote_directory.rs`
- Modify: `src-tauri/src/workbench/mod.rs`
- Modify: `src-tauri/src/workbench/models.rs`
- Test: `src-tauri/src/workbench/remote_directory.rs`

- [ ] **Step 1: Add DTOs to `workbench/models.rs`**

Add camelCase DTOs:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchRemoteRootDto {
    pub label: String,
    pub path: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchRemoteDirectoryEntryDto {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub modified_at: Option<String>,
    pub is_git_repo: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchRemotePathInfoDto {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub readable: bool,
    pub is_git_repo: bool,
    pub suggested_project_name: String,
}
```

- [ ] **Step 2: Write failing tests**

Add tests for roots and Git detection:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn path_info_marks_git_repo() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();

        let info = remote_path_info(temp.path()).unwrap();

        assert_eq!(info.kind, "dir");
        assert!(info.is_git_repo);
    }

    #[test]
    fn list_directory_sorts_dirs_before_files() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("README.md"), "# Readme").unwrap();

        let entries = list_remote_directory(temp.path()).unwrap();

        assert_eq!(entries[0].name, "src");
        assert_eq!(entries[0].kind, "dir");
        assert_eq!(entries[1].name, "README.md");
    }
}
```

- [ ] **Step 3: Run test and confirm failure**

Run:

```bash
cd src-tauri
cargo test workbench::remote_directory --lib
```

Expected: compile failure until module exists and is exported.

- [ ] **Step 4: Implement remote directory helpers**

Create helpers:

```rust
pub fn remote_roots() -> Vec<WorkbenchRemoteRootDto>;
pub fn list_remote_directory(path: &Path) -> Result<Vec<WorkbenchRemoteDirectoryEntryDto>, AppError>;
pub fn remote_path_info(path: &Path) -> Result<WorkbenchRemotePathInfoDto, AppError>;
```

Rules:

- Expand roots with `dirs::home_dir()`, `Desktop`, `Documents`, `Downloads`, common code directories, and platform root entries.
- Filter roots that do not exist.
- Use `std::fs::read_dir`.
- Sort directories before files, then case-insensitive name.
- Mark Git repos by checking for `.git` child path.
- Reject empty path strings in command/route layers before calling helper.

- [ ] **Step 5: Export module and run tests**

Modify `src-tauri/src/workbench/mod.rs`:

```rust
pub mod remote_directory;
```

Run:

```bash
cd src-tauri
cargo test workbench::remote_directory --lib
```

Expected: pass.

## Task 3: Remote Workbench HTTP Routes

**Files:**
- Create: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/net/routes/mod.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Test: route handler unit tests in `src-tauri/src/net/routes/workbench.rs`

- [ ] **Step 1: Create route request/response structs**

Create `src-tauri/src/net/routes/workbench.rs` with request structs:

```rust
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePathReq {
    pub path: String,
}
```

Add handlers:

```rust
pub async fn remote_roots() -> Result<Json<Vec<WorkbenchRemoteRootDto>>, AppError>;
pub async fn remote_list_dir(Json(req): Json<RemotePathReq>) -> Result<Json<Vec<WorkbenchRemoteDirectoryEntryDto>>, AppError>;
pub async fn remote_path_info(Json(req): Json<RemotePathReq>) -> Result<Json<WorkbenchRemotePathInfoDto>, AppError>;
pub async fn open_remote_project(State(state): State<AppState>, Json(req): Json<RemotePathReq>) -> Result<Json<WorkbenchProjectDto>, AppError>;
```

- [ ] **Step 2: Implement `open_remote_project` by reusing project add logic**

Extract from `commands/workbench.rs::add_workbench_project` into a shared function:

```rust
pub async fn add_local_workbench_project_from_path(
    state: &AppState,
    path: String,
) -> Result<WorkbenchProjectDto, AppError>
```

Then call that function from both the Tauri command and `open_remote_project`.

- [ ] **Step 3: Register routes**

Modify `src-tauri/src/net/routes/mod.rs`:

```rust
pub mod workbench;
```

Modify `src-tauri/src/net/http_server.rs`:

```rust
.route("/api/workbench/fs/roots", get(workbench::remote_roots))
.route("/api/workbench/fs/list", post(workbench::remote_list_dir))
.route("/api/workbench/fs/info", post(workbench::remote_path_info))
.route("/api/workbench/projects/open", post(workbench::open_remote_project))
```

- [ ] **Step 4: Run Rust checks**

Run:

```bash
cd src-tauri
cargo test workbench::remote_directory --lib
cargo check
```

Expected: pass.

## Task 4: Remote Client and Device Resolution

**Files:**
- Create: `src-tauri/src/workbench/remote_client.rs`
- Modify: `src-tauri/src/workbench/mod.rs`
- Modify: `src-tauri/src/commands/workbench.rs`
- Test: `src-tauri/src/workbench/remote_client.rs`

- [ ] **Step 1: Implement discovered-device resolver helper**

In `commands/workbench.rs` or a small shared module, add:

```rust
fn device_base_url(state: &AppState, device_id: &str) -> Result<String, AppError> {
    let devices = state.devices.read().expect("devices 读锁中毒");
    let device = devices
        .get(device_id)
        .ok_or_else(|| AppError::generic("远端设备不在线"))?;
    Ok(device.base_url())
}
```

- [ ] **Step 2: Implement remote client methods**

Create `RemoteWorkbenchClient`:

```rust
#[derive(Clone)]
pub struct RemoteWorkbenchClient {
    client: reqwest::Client,
}
```

Methods:

```rust
pub async fn roots(&self, base_url: &str) -> Result<Vec<WorkbenchRemoteRootDto>, AppError>;
pub async fn list_dir(&self, base_url: &str, path: &str) -> Result<Vec<WorkbenchRemoteDirectoryEntryDto>, AppError>;
pub async fn path_info(&self, base_url: &str, path: &str) -> Result<WorkbenchRemotePathInfoDto, AppError>;
pub async fn open_project(&self, base_url: &str, path: &str) -> Result<WorkbenchProjectDto, AppError>;
```

- [ ] **Step 3: Add Tauri commands for remote directory picker**

Add commands:

```rust
#[tauri::command]
pub async fn list_workbench_remote_roots(
    state: State<'_, AppState>,
    device_id: String,
) -> Result<Vec<WorkbenchRemoteRootDto>, AppError>;

#[tauri::command]
pub async fn list_workbench_remote_dir(
    state: State<'_, AppState>,
    device_id: String,
    path: String,
) -> Result<Vec<WorkbenchRemoteDirectoryEntryDto>, AppError>;

#[tauri::command]
pub async fn get_workbench_remote_path_info(
    state: State<'_, AppState>,
    device_id: String,
    path: String,
) -> Result<WorkbenchRemotePathInfoDto, AppError>;
```

Register them in `src-tauri/src/lib.rs`.

- [ ] **Step 4: Add remote shortcut creation command**

Add command:

```rust
#[tauri::command]
pub async fn open_workbench_remote_project(
    state: State<'_, AppState>,
    device_id: String,
    path: String,
) -> Result<WorkbenchProjectDto, AppError>;
```

Implementation:

1. Resolve device base URL.
2. Call remote `/api/workbench/projects/open`.
3. Build local `WorkbenchProjectRow` with `kind = "remote"`.
4. Use deterministic `remote_project_id(device_id, remote.path)`.
5. Persist local shortcut in `workbench_project_repo`.
6. Return local shortcut DTO.

- [ ] **Step 5: Run targeted checks**

Run:

```bash
cd src-tauri
cargo test workbench::remote_ids --lib
cargo test workbench::remote_directory --lib
cargo check
```

Expected: pass.

## Task 5: Frontend Remote Project Picker

**Files:**
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/api/workbench.ts`
- Modify: `web/src/hooks/workbenchProjectsContext.ts`
- Modify: `web/src/hooks/useWorkbenchProjects.tsx`
- Create: `web/src/components/domain/WorkbenchRemoteProjectPicker/WorkbenchRemoteProjectPicker.tsx`
- Create: `web/src/components/domain/WorkbenchRemoteProjectPicker/WorkbenchRemoteProjectPicker.module.css`
- Create: `web/src/components/domain/WorkbenchRemoteProjectPicker/index.ts`
- Modify: `web/src/components/domain/WorkbenchProjectRail/WorkbenchProjectRail.tsx`
- Modify: `web/src/i18n/locales/{zh,en}/workbench.json`
- Test: `web/src/pages/Workbench/workbenchRemoteProjects.test.ts`

- [ ] **Step 1: Add TypeScript types**

Add:

```ts
export type WorkbenchProjectKind = 'local' | 'remote' | string;

export interface WorkbenchRemoteRoot {
  label: string;
  path: string;
  kind: string;
}

export interface WorkbenchRemoteDirectoryEntry {
  name: string;
  path: string;
  kind: 'dir' | 'file' | string;
  modifiedAt: string | null;
  isGitRepo: boolean;
}

export interface WorkbenchRemotePathInfo {
  name: string;
  path: string;
  kind: 'dir' | 'file' | string;
  readable: boolean;
  isGitRepo: boolean;
  suggestedProjectName: string;
}
```

- [ ] **Step 2: Add API methods**

In `web/src/api/workbench.ts`:

```ts
remote: {
  roots: (deviceId: string) =>
    invoke<WorkbenchRemoteRoot[]>('list_workbench_remote_roots', { deviceId }),
  listDir: (deviceId: string, path: string) =>
    invoke<WorkbenchRemoteDirectoryEntry[]>('list_workbench_remote_dir', { deviceId, path }),
  info: (deviceId: string, path: string) =>
    invoke<WorkbenchRemotePathInfo>('get_workbench_remote_path_info', { deviceId, path }),
  openProject: (deviceId: string, path: string) =>
    invoke<WorkbenchProject>('open_workbench_remote_project', { deviceId, path }),
}
```

- [ ] **Step 3: Add picker component**

Component behavior:

- Hooks before any early return.
- Uses `devicesApi.list()` to load online peers.
- Calls `workbenchApi.remote.roots(deviceId)` after device selection.
- Calls `workbenchApi.remote.listDir(deviceId, path)` when navigating.
- Enables “Open project” only for selected directory path.
- Calls `onProjectOpened(project)` after `openProject`.

Use existing primitives: `Button`, `Card`, `Pill`, `StatusDot`.

- [ ] **Step 4: Update project provider**

Add context method:

```ts
openRemoteProject: (deviceId: string, path: string) => Promise<WorkbenchProject | null>;
```

Provider logic mirrors `addProjectFromPath`:

```ts
const project = await workbenchApi.remote.openProject(deviceId, path);
setProjects((current) => [project, ...current.filter((item) => item.id !== project.id)]);
setActiveProjectId(project.id);
void refreshProjectSessionStats(project.id);
return project;
```

- [ ] **Step 5: Update `WorkbenchProjectRail`**

Change `+` action:

- First click opens a small local/remote source popover or modal.
- Local keeps `chooseAndAddProject`.
- Remote opens `WorkbenchRemoteProjectPicker`.

- [ ] **Step 6: Write frontend tests**

Create `web/src/pages/Workbench/workbenchRemoteProjects.test.ts` with pure helper tests for:

- Inserting remote project moves it to top without duplicates.
- Breadcrumb parent path handles `/`, `/Users/hans/app`, and Windows-like `C:\Users\hans\app`.
- Directory entries sort dirs before files when using the UI helper.

- [ ] **Step 7: Run frontend checks**

Run:

```bash
cd web
npx --yes tsx src/pages/Workbench/workbenchRemoteProjects.test.ts
npm run build
```

Expected: pass.

## Task 6: Remote Gateway for Worktrees, Git, and Files

**Files:**
- Modify: `src-tauri/src/workbench/remote_client.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/commands/workbench.rs`
- Test: extend command helper tests

- [ ] **Step 1: Add remote client methods for worktrees/git/files**

Add methods matching existing DTOs:

```rust
pub async fn list_worktrees(&self, base_url: &str, project_id: &str) -> Result<Vec<WorkbenchWorktreeDto>, AppError>;
pub async fn list_git_commits(&self, base_url: &str, project_id: &str, worktree_id: Option<&str>, limit: i64) -> Result<Vec<WorkbenchGitCommitDto>, AppError>;
pub async fn list_workbench_dir(&self, base_url: &str, project_id: &str, worktree_id: Option<&str>, path: Option<&str>) -> Result<Vec<WorkbenchFileNode>, AppError>;
pub async fn open_file(&self, base_url: &str, project_id: &str, worktree_id: Option<&str>, path: &str) -> Result<WorkbenchOpenFileDto, AppError>;
pub async fn save_text_file(&self, base_url: &str, req: RemoteSaveTextReq) -> Result<WorkbenchSaveTextResultDto, AppError>;
```

- [ ] **Step 2: Add remote HTTP route handlers**

Expose routes under `/api/workbench/...` that call local service helpers:

```rust
POST /api/workbench/worktrees/list
POST /api/workbench/git/commits
POST /api/workbench/files/list-dir
POST /api/workbench/files/open
POST /api/workbench/files/save-text
POST /api/workbench/files/create-file
POST /api/workbench/files/create-dir
POST /api/workbench/files/rename
POST /api/workbench/files/delete
```

Each route uses remote-side local `projectId/worktreeId` without `remote:` prefixes.

- [ ] **Step 3: Add command gateway branches**

In each relevant Tauri command:

1. Load project row.
2. If `project.kind == "remote"`, resolve base URL.
3. Call remote client.
4. Map returned IDs with `remote_entity_id`.
5. Return mapped DTOs.
6. Otherwise execute existing local code path.

For commands that accept only `worktreeId`, parse `remote:` prefix first and route directly by embedded `deviceId`.

- [ ] **Step 4: Preserve local behavior tests**

Run:

```bash
cd src-tauri
cargo test workbench::git --lib
cargo test workbench::file_content --lib
cargo test workbench::file_preview --lib
cargo test commands::workbench --lib
cargo check
```

Expected: existing local tests still pass.

## Task 7: Remote Terminal Session API and Event Stream

**Files:**
- Modify: `src-tauri/src/workbench/remote_client.rs`
- Create: `src-tauri/src/workbench/remote_events.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/src/commands/workbench.rs`
- Modify: `src-tauri/src/state.rs`
- Test: unit tests for event ID mapping

- [ ] **Step 1: Add terminal HTTP methods**

Remote client methods:

```rust
pub async fn list_sessions(&self, base_url: &str, project_id: Option<&str>) -> Result<Vec<WorkbenchSessionDto>, AppError>;
pub async fn create_session(&self, base_url: &str, req: RemoteCreateSessionReq) -> Result<WorkbenchSessionDto, AppError>;
pub async fn write_input(&self, base_url: &str, session_id: &str, data: &str) -> Result<(), AppError>;
pub async fn resize(&self, base_url: &str, session_id: &str, cols: u16, rows: u16) -> Result<(), AppError>;
pub async fn focus(&self, base_url: &str, session_id: &str) -> Result<(), AppError>;
pub async fn focused(&self, base_url: &str, project_id: &str, worktree_id: Option<&str>) -> Result<Option<String>, AppError>;
pub async fn split_pane(&self, base_url: &str, session_id: &str, direction: &str) -> Result<(), AppError>;
pub async fn close_pane(&self, base_url: &str, session_id: &str) -> Result<bool, AppError>;
pub async fn close_session(&self, base_url: &str, session_id: &str) -> Result<(), AppError>;
pub async fn rename_session(&self, base_url: &str, session_id: &str, name: &str) -> Result<WorkbenchSessionDto, AppError>;
```

- [ ] **Step 2: Add terminal HTTP routes**

Routes:

```text
POST /api/workbench/sessions/list
POST /api/workbench/sessions/create
POST /api/workbench/sessions/write
POST /api/workbench/sessions/resize
POST /api/workbench/sessions/focus
POST /api/workbench/sessions/focused
POST /api/workbench/sessions/split-pane
POST /api/workbench/sessions/close-pane
POST /api/workbench/sessions/close
POST /api/workbench/sessions/rename
```

Remote route handlers call the same local session registry code as Tauri commands.

- [ ] **Step 3: Implement event stream**

Add remote route:

```text
GET /api/workbench/events
```

First version can be NDJSON:

```json
{"type":"terminalOutput","payload":{"sessionId":"...","chunk":"...","seq":1,"ts":...}}
{"type":"terminalStatus","payload":{"sessionId":"...","status":"running","exitCode":null,"ts":...}}
{"type":"mergeProgress","payload":{"projectId":"...","worktreeId":"...","stages":[...]}}
```

Implementation option:

- Add a broadcast channel to `AppState` for Workbench remote events.
- When local Tauri event emission happens in sessions/merge code, also publish typed event to that broadcast channel.
- `/api/workbench/events` streams broadcast messages as NDJSON.

- [ ] **Step 4: Bridge remote events locally**

`remote_events.rs` behavior:

1. Start connection after remote project/session list/create.
2. Keep one connection per `deviceId`.
3. Map remote IDs to `remote:{deviceId}:{innerId}`.
4. Emit local Tauri events:
   - `workbench:terminal-output`
   - `workbench:terminal-status`
   - `workbench:merge-progress`
5. Reconnect after short delay on network failure.

- [ ] **Step 5: Gate terminal commands**

In Tauri terminal commands:

- If `sessionId` is remote, parse device ID and inner ID.
- Call remote client.
- Return the existing response shape with mapped `sessionId`.

- [ ] **Step 6: Run terminal checks**

Run:

```bash
cd src-tauri
cargo test workbench::sessions --lib
cargo test commands::workbench --lib
cargo check
```

Expected: existing local terminal tests pass and remote ID mapping tests pass.

## Task 8: Remote Prompt Optimizer Routing

**Files:**
- Modify: `src-tauri/src/commands/prompt_optimizer.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/workbench/remote_client.rs`
- Test: targeted prompt optimizer API tests

- [ ] **Step 1: Add remote route**

Expose:

```text
POST /api/workbench/prompt-optimizer/stream-to-session
```

Request:

```rust
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePromptOptimizerReq {
    pub prompt: String,
    pub working_directory: String,
    pub target_language: String,
    pub session_id: String,
}
```

Remote route calls the existing stream-to-session implementation against the remote local session.

- [ ] **Step 2: Route local Tauri command for remote sessions**

In `stream_optimize_prompt_to_workbench_session`:

- If `sessionId` is local, keep existing behavior.
- If `sessionId` is remote, parse device ID and remote inner session ID.
- Forward request to remote device.
- Return the same success DTO.

- [ ] **Step 3: Validate target language and cwd behavior**

Ensure remote request uses:

- Remote working directory path from remote worktree DTO.
- Remote session ID without `remote:` prefix.
- Existing target language values `zh | en`.

- [ ] **Step 4: Run checks**

Run:

```bash
cd src-tauri
cargo check
cd ../web
npx --yes tsx src/api/promptOptimizer.test.ts
```

Expected: pass.

## Task 9: Offline and Error UX

**Files:**
- Modify: `web/src/pages/Workbench/Workbench.tsx`
- Modify: `web/src/components/domain/WorkbenchProjectRail/WorkbenchProjectRail.tsx`
- Modify: `web/src/i18n/locales/{zh,en}/workbench.json`
- Modify: `web/src/pages/Workbench/workbenchRemoteProjects.test.ts`

- [ ] **Step 1: Add remote project display affordance**

Project rail should show:

- Device name.
- Remote badge for `project.kind === "remote"`.
- Remote path unchanged as remote platform path.

- [ ] **Step 2: Disable actions when remote device is offline**

If remote API returns "远端设备不在线":

- Show inline Workbench notice.
- Disable new terminal, file write, Git commit/push/merge, Prompt optimizer.
- Allow removing the project shortcut.

- [ ] **Step 3: Keep existing dirty tab confirmations**

Do not add remote-specific dirty behavior. Reuse existing dirty tab guard before project/worktree switch and close.

- [ ] **Step 4: Run frontend checks**

Run:

```bash
cd web
npx --yes tsx src/pages/Workbench/workbenchRemoteProjects.test.ts
npm run build
```

Expected: pass.

## Task 10: Project Memory and PRD Updates

**Files:**
- Modify: `src-tauri/CLAUDE.md`
- Modify: `web/CLAUDE.md`
- Modify: `AGENTS.md`
- Modify: `docs/prd.md`

- [ ] **Step 1: Update Rust memory**

In `src-tauri/CLAUDE.md`, add concise remote Workbench notes:

- Remote routes live in `net/routes/workbench.rs`.
- Remote gateway lives in Workbench commands/client modules.
- Remote terminal output is bridged back to the same Tauri event names.
- Relevant tests.

- [ ] **Step 2: Update frontend memory**

In `web/CLAUDE.md`, add concise frontend notes:

- Project `+` supports local and LAN remote sources.
- Remote picker component path.
- Remote projects reuse Workbench UI and API; do not create a duplicate page.
- Relevant tests.

- [ ] **Step 3: Update root memory only if top-level map changes**

Root `AGENTS.md` should only receive a concise Workbench entry adjustment if needed. Keep it high level and avoid implementation changelog text.

- [ ] **Step 4: Update PRD if behavior changed during implementation**

Keep `docs/prd.md` aligned with delivered scope. Do not add a summary markdown file.

## Task 11: Full Verification

**Files:**
- No source edits unless verification reveals failures.

- [ ] **Step 1: Rust targeted verification**

Run:

```bash
cd src-tauri
cargo test workbench::remote_ids --lib
cargo test workbench::remote_directory --lib
cargo test workbench::sessions --lib
cargo test workbench::git --lib
cargo test workbench::file_content --lib
cargo test workbench::file_preview --lib
cargo test workbench::sqlite_preview --lib
cargo test commands::workbench --lib
cargo check
```

Expected: pass.

- [ ] **Step 2: Frontend targeted verification**

Run:

```bash
cd web
npx --yes tsx src/pages/Workbench/workbenchRemoteProjects.test.ts
npx --yes tsx src/pages/Workbench/workbenchWorktrees.test.ts
npx --yes tsx src/pages/Workbench/workbenchFiles.test.ts
npx --yes tsx src/components/domain/WorkbenchHtmlPreview/htmlAssets.test.ts
npx --yes tsx src/pages/Workbench/terminalReplay.test.ts
npx --yes tsx src/pages/Workbench/promptOptimizerWidget.test.ts
npm run build
```

Expected: pass.

- [ ] **Step 3: Manual LAN smoke test**

Use two cc-partner instances on the same LAN:

1. Start app on device A and B.
2. Confirm A sees B in devices.
3. On A, choose Workbench `+` → LAN device project → B.
4. Browse B's directories and open a Git project.
5. Verify A shows remote project card.
6. Verify file tree loads.
7. Open and save a small text file on B through A.
8. Create a terminal window and run `pwd`; output should show B's remote path.
9. Run `git status` in terminal and compare with Git history panel.
10. Use Prompt optimizer to stream text into the remote terminal without auto-enter.

Expected: all operations execute on B while UI remains on A.

## Self-Review

- Spec coverage: The plan covers direct remote directory selection, no pre-authorization, remote project shortcut persistence, remote Workbench command proxying, terminal event bridging, Prompt optimizer routing, offline UX, docs, and verification.
- Placeholder scan: No implementation placeholders are intentionally left in this plan.
- Type consistency: Rust DTO names use `WorkbenchRemote*Dto`; TypeScript names use matching `WorkbenchRemote*`; remote IDs use `remote:{deviceId}:{innerId}` consistently.
