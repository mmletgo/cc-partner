# 工作台本机 MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现工作台第一期：本机项目文件夹、多个本机 Claude Code PTY 终端、会话状态、右侧可交互项目文件树。

**Architecture:** 第一阶段只实现本机能力，远端项目、远端终端与设备信任系统不进入本计划。Rust 后端新增 `workbench` 领域模块，分成 `projects`、`sessions`、`fs` 三个边界；前端新增 `/workbench` 页面，用 xterm 渲染终端，通过 Tauri invoke + event 与后端交互。项目记录持久化到 SQLite，终端会话只驻留内存。

**Tech Stack:** Rust + Tauri 2 + sqlx(SQLite) + portable-pty 0.9.0；React 19 + TypeScript + Vite + @xterm/xterm 6.0.0 + @xterm/addon-fit 0.11.0；react-i18next；Playwright。

---

## Global Constraints

- 只做本机 MVP；远端项目和远端 PTY 另写计划。
- 所有 Rust 新增函数、结构体、impl 方法必须有中文 doc comment，包含 Business Logic / Code Logic。
- Tauri 命令返回前端的 DTO 使用 `#[serde(rename_all = "camelCase")]`。
- 前端用户可见文案走 `web/src/i18n/locales/{zh,en}/workbench.json`，组件里不硬编码中英文文案。
- React hooks 必须放在所有 early return 之前。
- CSS 使用现有 design token，不硬编码颜色、字体、间距、圆角、阴影；终端内部深色区域除外，但外围仍用 token。
- 页面右侧只显示“当前会话状态 + 项目文件夹”，不加入 CLI 配置、事件日志、AGENTS/CLAUDE.md 状态。
- 执行本计划前使用 git worktree 新分支；实现任务超过 100 行，按项目规则使用 subagent。编码 subagent 显式传 `model: gpt-5.5`。

## Scope Split

本计划覆盖：

- 本机项目列表与目录选择。
- 本机 Claude Code 终端会话。
- 本机文件树与基础文件操作。
- 前端工作台页面。
- 设计文档、PRD、项目记忆更新。

本计划不覆盖：

- 局域网远端项目。
- 远端信任 token。
- 远端 PTY stream。
- 文件内容预览。
- 会话日志持久化。
- Prompt 库发送到终端。

## File Structure

### Rust 后端

| 文件 | 责任 |
|---|---|
| `src-tauri/src/workbench/mod.rs` | workbench 模块入口，导出 projects/sessions/fs |
| `src-tauri/src/workbench/models.rs` | WorkbenchProjectRow/Dto、WorkbenchSessionDto、WorkbenchFileNode/PathInfo |
| `src-tauri/src/workbench/projects.rs` | 项目名称推断、路径校验、Row/Dto 转换辅助 |
| `src-tauri/src/workbench/sessions.rs` | portable-pty 会话 registry、spawn/input/resize/stop/close |
| `src-tauri/src/workbench/fs.rs` | 本机目录读取、文件元信息、创建、重命名、删除 |
| `src-tauri/src/storage/workbench_project_repo.rs` | workbench_projects 表 CRUD |
| `src-tauri/src/storage/mod.rs` | 导出 WorkbenchProjectRepo |
| `src-tauri/src/commands/workbench.rs` | Tauri command thin layer |
| `src-tauri/src/commands/mod.rs` | 注册 workbench command 模块 |
| `src-tauri/src/state.rs` | AppState 加 project repo 和 session registry |
| `src-tauri/src/lib.rs` | 建表、setup 注入、invoke_handler 注册 |
| `src-tauri/Cargo.toml` | 增加 `portable-pty = "0.9.0"` |

### React 前端

| 文件 | 责任 |
|---|---|
| `web/src/api/workbench.ts` | workbench invoke API |
| `web/src/lib/types.ts` | WorkbenchProject/Session/FileNode/PathInfo 类型 |
| `web/src/pages/Workbench/Workbench.tsx` | 页面 orchestrator |
| `web/src/pages/Workbench/Workbench.module.css` | 页面布局与样式 |
| `web/src/pages/Workbench/TerminalPane.tsx` | xterm 单 pane |
| `web/src/pages/Workbench/FileTree.tsx` | 右侧文件树 |
| `web/src/pages/Workbench/SessionStatus.tsx` | 右侧当前会话状态 |
| `web/src/pages/Workbench/index.ts` | 页面导出 |
| `web/src/App.tsx` | 路由 |
| `web/src/components/layout/AppShell/AppShell.tsx` | 侧栏导航 |
| `web/src/lib/icons.tsx` | 如已有 TerminalIcon 则复用；缺少工作台图标时新增 WorkbenchIcon |
| `web/src/i18n/locales/{zh,en}/workbench.json` | 工作台页面文案 |
| `web/src/i18n/locales/{zh,en}/nav.json` | 增加 `workbench` |
| `web/src/i18n/index.ts` | 注册 workbench namespace |
| `web/package.json` / `web/package-lock.json` | 增加 @xterm 依赖 |

### 文档

| 文件 | 责任 |
|---|---|
| `docs/prd.md` | 新增工作台一期需求 |
| `AGENTS.md` | 根目录地图和关键功能索引补工作台（保持精简） |
| `web/CLAUDE.md` | 前端页面/API/i18n/验证命令补工作台 |
| `src-tauri/CLAUDE.md` | Rust workbench 模块、命令和验证命令补充 |

## IPC Contract

### Tauri Commands

```ts
list_workbench_projects(): WorkbenchProject[]
add_workbench_project(path: string): WorkbenchProject
remove_workbench_project(projectId: string): { ok: boolean }
touch_workbench_project(projectId: string): WorkbenchProject

list_workbench_sessions(projectId?: string): WorkbenchSession[]
create_workbench_session(projectId: string): WorkbenchSession
write_workbench_session_input(sessionId: string, data: string): { ok: boolean }
resize_workbench_session(sessionId: string, cols: number, rows: number): { ok: boolean }
close_workbench_session(sessionId: string): { ok: boolean }
rename_workbench_session(sessionId: string, name: string): WorkbenchSession

list_workbench_dir(projectId: string, path?: string): WorkbenchFileNode[]
get_workbench_path_info(projectId: string, path: string): WorkbenchPathInfo
create_workbench_file(projectId: string, parentPath: string, name: string): WorkbenchPathInfo
create_workbench_dir(projectId: string, parentPath: string, name: string): WorkbenchPathInfo
rename_workbench_path(projectId: string, path: string, newName: string): WorkbenchPathInfo
delete_workbench_path(projectId: string, path: string): { ok: boolean }
```

### Events

```ts
workbench:terminal-output {
  sessionId: string;
  chunk: string;
  seq: number;
  ts: number;
}

workbench:terminal-status {
  sessionId: string;
  status: 'starting' | 'running' | 'exited' | 'disconnected';
  exitCode?: number | null;
  ts: number;
}
```

## Phase 0 — Worktree and Dependency Setup

### Task 0.1: Create Worktree

**Files:** none

- [ ] **Step 1: 创建隔离 worktree**

Run:

```bash
git worktree add ../cc-partner-workbench -b codex/workbench-local-mvp
cd ../cc-partner-workbench
```

Expected:

```text
Preparing worktree (new branch 'codex/workbench-local-mvp')
HEAD is now at <current commit>
```

- [ ] **Step 2: 确认工作树干净**

Run:

```bash
git status --short
```

Expected: no output.

### Task 0.2: Add Dependencies

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `web/package.json`
- Modify: `web/package-lock.json`

- [ ] **Step 1: 增加 Rust PTY 依赖**

Edit `src-tauri/Cargo.toml` dependencies section:

```toml
# Workbench: cross-platform PTY for interactive Claude Code sessions
portable-pty = "0.9.0"
```

- [ ] **Step 2: 增加前端终端依赖**

Run:

```bash
cd web
npm install @xterm/xterm@6.0.0 @xterm/addon-fit@0.11.0
```

Expected: `package.json` and `package-lock.json` updated.

- [ ] **Step 3: 编译依赖检查**

Run:

```bash
cd src-tauri
cargo check
cd ../web
npm run build
```

Expected: both exit 0. If existing unrelated errors appear, fix them only if they block this work and mention in final summary.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock web/package.json web/package-lock.json
git commit -m "chore(workbench): add terminal dependencies"
```

## Phase A — Rust Models and Project Persistence

### Task A1: Workbench Models

**Files:**
- Create: `src-tauri/src/workbench/mod.rs`
- Create: `src-tauri/src/workbench/models.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: 新建 `src-tauri/src/workbench/mod.rs`**

```rust
//! workbench — 项目工作台领域模块
//!
//! Business Logic（为什么需要这个模块）:
//!     工作台把“项目文件夹 + 多个 Claude Code 终端 + 文件树”聚合为一个运行态工作空间。
//!     该模块只承载本机 MVP 能力；局域网远端项目和信任机制后续单独扩展。
//!
//! Code Logic（这个模块做什么）:
//!     按职责拆分 models / projects / sessions / fs，commands 层只做参数转发。

pub mod models;
```

- [ ] **Step 2: 新建 `src-tauri/src/workbench/models.rs`**

```rust
//! workbench/models.rs — 工作台 DTO 与内部数据模型
//!
//! Business Logic（为什么需要这个模块）:
//!     前端工作台需要稳定的项目、会话、文件节点结构；后端也需要 SQLite row 表达项目记录。
//!
//! Code Logic（这个模块做什么）:
//!     定义 WorkbenchProjectRow/Dto、WorkbenchSessionDto、WorkbenchFileNode、WorkbenchPathInfo，
//!     所有前端 DTO 使用 camelCase。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct WorkbenchProjectRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub device_id: String,
    pub device_name: String,
    pub path: String,
    pub last_opened_at: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchProjectDto {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub device_id: String,
    pub device_name: String,
    pub path: String,
    pub last_opened_at: String,
    pub created_at: String,
    pub updated_at: String,
}

impl WorkbenchProjectRow {
    /// Business Logic（为什么需要这个函数）:
    ///     前端只消费 camelCase DTO，数据库 row 不应直接泄露给 UI。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆 row 字段并转换为 `WorkbenchProjectDto`。
    pub fn to_dto(&self) -> WorkbenchProjectDto {
        WorkbenchProjectDto {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            device_id: self.device_id.clone(),
            device_name: self.device_name.clone(),
            path: self.path.clone(),
            last_opened_at: self.last_opened_at.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchSessionDto {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub command: String,
    pub status: String,
    pub cols: u16,
    pub rows: u16,
    pub started_at: String,
    pub exited_at: Option<String>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchFileNode {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub size: Option<u64>,
    pub modified_at: Option<String>,
    pub children: Option<Vec<WorkbenchFileNode>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchPathInfo {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub size: Option<u64>,
    pub modified_at: Option<String>,
}
```

- [ ] **Step 3: 在 `src-tauri/src/lib.rs` module 区加**

```rust
mod workbench;
```

- [ ] **Step 4: 验证**

Run: `cd src-tauri && cargo check`
Expected: exit 0.

### Task A2: Project Repo and Schema

**Files:**
- Create: `src-tauri/src/storage/workbench_project_repo.rs`
- Modify: `src-tauri/src/storage/mod.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: 新建 repo**

Create `src-tauri/src/storage/workbench_project_repo.rs`:

```rust
//! storage/workbench_project_repo.rs — 工作台项目记录仓库
//!
//! Business Logic（为什么需要这个模块）:
//!     用户添加过的本机项目需要在重启后保留，用于工作台左侧最近项目列表。
//!
//! Code Logic（这个模块做什么）:
//!     封装 workbench_projects 表 CRUD；使用运行期 sqlx::query，不依赖编译期 DATABASE_URL。

use crate::error::AppError;
use crate::workbench::models::WorkbenchProjectRow;
use sqlx::{Row, SqlitePool};

#[derive(Clone)]
pub struct WorkbenchProjectRepo {
    pool: SqlitePool,
}

impl WorkbenchProjectRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     Tauri setup 需要用同一个 SQLite pool 构造项目仓库，供命令层共享。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 SqlitePool clone；pool 内部是 Arc，clone 廉价。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     工作台左侧需要按最近打开顺序展示项目。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询全部项目，按 last_opened_at DESC 排序，转换为 Row。
    pub async fn list(&self) -> Result<Vec<WorkbenchProjectRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at \
             FROM workbench_projects ORDER BY last_opened_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_project).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     会话和文件系统命令需要用 project_id 找到项目根路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 id 查询单条记录，不存在返回 None。
    pub async fn get(&self, id: &str) -> Result<Option<WorkbenchProjectRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at \
             FROM workbench_projects WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_project(&r)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户添加项目或重新打开项目时，需要保存/覆盖项目记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 INSERT OR REPLACE 写入完整 row。
    pub async fn upsert(&self, row: &WorkbenchProjectRow) -> Result<(), AppError> {
        sqlx::query(
            "INSERT OR REPLACE INTO workbench_projects \
             (id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.id)
        .bind(&row.name)
        .bind(&row.kind)
        .bind(&row.device_id)
        .bind(&row.device_name)
        .bind(&row.path)
        .bind(&row.last_opened_at)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户可以从工作台左侧移除最近项目；移除不删除磁盘文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 id 删除项目记录。
    pub async fn delete(&self, id: &str) -> Result<(), AppError> {
        sqlx::query("DELETE FROM workbench_projects WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Business Logic（为什么需要这个函数）:
///     sqlx Row 字段读取逻辑在 list/get 中复用，避免字段顺序出错。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 读取列并构造 WorkbenchProjectRow。
fn row_to_project(row: &sqlx::sqlite::SqliteRow) -> Result<WorkbenchProjectRow, AppError> {
    Ok(WorkbenchProjectRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        kind: row.try_get("kind")?,
        device_id: row.try_get("device_id")?,
        device_name: row.try_get("device_name")?,
        path: row.try_get("path")?,
        last_opened_at: row.try_get("last_opened_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}
```

- [ ] **Step 2: 导出 repo**

In `src-tauri/src/storage/mod.rs` add:

```rust
pub mod workbench_project_repo;
pub use workbench_project_repo::WorkbenchProjectRepo;
```

- [ ] **Step 3: AppState 增加字段**

In `src-tauri/src/state.rs`, storage imports add `WorkbenchProjectRepo`, and struct add:

```rust
/// 工作台项目仓库（workbench_projects 表访问）
pub workbench_project_repo: Arc<WorkbenchProjectRepo>,
```

- [ ] **Step 4: lib.rs 建表和 setup**

Add schema constant near other schema constants:

```rust
const WORKBENCH_PROJECT_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS workbench_projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    device_id TEXT NOT NULL,
    device_name TEXT NOT NULL,
    path TEXT NOT NULL,
    last_opened_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
)";
```

In `init_db`, execute:

```rust
sqlx::query(WORKBENCH_PROJECT_SCHEMA).execute(&pool).await?;
```

In setup, construct:

```rust
let workbench_project_repo = Arc::new(WorkbenchProjectRepo::new(pool.clone()));
```

And set `AppState { workbench_project_repo, ... }`.

- [ ] **Step 5: Repo tests**

Add tests in `workbench_project_repo.rs` for:

```rust
#[tokio::test]
async fn list_orders_by_last_opened_desc() { /* insert two rows, newer first */ }

#[tokio::test]
async fn delete_removes_project_record_only() { /* insert then delete then get None */ }
```

Expected assertions:

```rust
assert_eq!(listed[0].id, "p2");
assert!(repo.get("p1").await.unwrap().is_none());
```

- [ ] **Step 6: Verify**

Run:

```bash
cd src-tauri
cargo test storage::workbench_project_repo
cargo check
```

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/storage/workbench_project_repo.rs src-tauri/src/storage/mod.rs src-tauri/src/state.rs src-tauri/src/lib.rs
git commit -m "feat(workbench): persist local workbench projects"
```

## Phase B — Rust File System Commands

### Task B1: Project Helpers and FS Module

**Files:**
- Create: `src-tauri/src/workbench/projects.rs`
- Create: `src-tauri/src/workbench/fs.rs`
- Modify: `src-tauri/src/workbench/mod.rs`

- [ ] **Step 1: `projects.rs`**

Create helper functions:

```rust
//! workbench/projects.rs — 工作台项目辅助逻辑
//!
//! Business Logic（为什么需要这个模块）:
//!     添加项目时需要校验目录存在、生成显示名，并保证后续文件操作不能逃出项目根目录。
//!
//! Code Logic（这个模块做什么）:
//!     提供 infer_project_name、canonical_project_root、resolve_project_path 三个纯辅助。

use crate::error::AppError;
use std::path::{Path, PathBuf};

/// Business Logic（为什么需要这个函数）:
///     用户选择目录后，左侧项目卡片需要一个可读名称。
///
/// Code Logic（这个函数做什么）:
///     取路径最后一段作为项目名，取不到时回退为完整路径字符串。
pub fn infer_project_name(path: &Path) -> String {
    path.file_name()
        .and_then(|v| v.to_str())
        .filter(|v| !v.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

/// Business Logic（为什么需要这个函数）:
///     工作台只允许添加真实存在的本机目录，避免后续 PTY cwd 或文件树读取失败。
///
/// Code Logic（这个函数做什么）:
///     canonicalize 输入路径，要求结果是目录。
pub fn canonical_project_root(path: &str) -> Result<PathBuf, AppError> {
    let root = PathBuf::from(path)
        .canonicalize()
        .map_err(|e| AppError::generic(format!("项目路径不可访问: {e}")))?;
    if !root.is_dir() {
        return Err(AppError::generic("项目路径必须是文件夹"));
    }
    Ok(root)
}

/// Business Logic（为什么需要这个函数）:
///     文件树操作必须限制在项目根目录内，防止通过 `../` 误删或读取项目外文件。
///
/// Code Logic（这个函数做什么）:
///     把相对路径拼到 root 后 canonicalize，并校验结果仍以 root 开头。
pub fn resolve_project_path(root: &Path, relative: &str) -> Result<PathBuf, AppError> {
    let target = if relative.trim().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let canonical = target
        .canonicalize()
        .map_err(|e| AppError::generic(format!("路径不可访问: {e}")))?;
    if !canonical.starts_with(root) {
        return Err(AppError::generic("不能访问项目目录之外的路径"));
    }
    Ok(canonical)
}
```

- [ ] **Step 2: `fs.rs`**

Implement:

```rust
pub async fn list_dir(root: &Path, relative: &str) -> Result<Vec<WorkbenchFileNode>, AppError>
pub async fn path_info(root: &Path, relative: &str) -> Result<WorkbenchPathInfo, AppError>
pub async fn create_file(root: &Path, parent: &str, name: &str) -> Result<WorkbenchPathInfo, AppError>
pub async fn create_dir(root: &Path, parent: &str, name: &str) -> Result<WorkbenchPathInfo, AppError>
pub async fn rename_path(root: &Path, relative: &str, new_name: &str) -> Result<WorkbenchPathInfo, AppError>
pub async fn delete_path(root: &Path, relative: &str) -> Result<(), AppError>
```

Required rules:

```rust
fn validate_child_name(name: &str) -> Result<(), AppError> {
    if name.trim().is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(AppError::generic("名称不能包含路径分隔符"));
    }
    Ok(())
}
```

Directory listing sorting:

```rust
entries.sort_by(|a, b| match (a.kind.as_str(), b.kind.as_str()) {
    ("dir", "file") => std::cmp::Ordering::Less,
    ("file", "dir") => std::cmp::Ordering::Greater,
    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
});
```

- [ ] **Step 3: 导出 projects/fs 模块**

Modify `src-tauri/src/workbench/mod.rs`:

```rust
pub mod fs;
pub mod models;
pub mod projects;
```

- [ ] **Step 4: FS tests**

Add tests covering:

```rust
#[tokio::test]
async fn resolve_rejects_parent_escape()
#[tokio::test]
async fn list_dir_sorts_dirs_before_files()
#[tokio::test]
async fn create_file_rejects_path_separator_in_name()
#[tokio::test]
async fn rename_path_keeps_target_inside_root()
```

Use `std::env::temp_dir().join(format!("ccp-workbench-test-{}", uuid::Uuid::new_v4()))` and clean with `std::fs::remove_dir_all`.

- [ ] **Step 5: Verify**

Run:

```bash
cd src-tauri
cargo test workbench::projects workbench::fs
```

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/workbench/projects.rs src-tauri/src/workbench/fs.rs src-tauri/src/workbench/mod.rs
git commit -m "feat(workbench): add local project filesystem helpers"
```

## Phase C — Rust PTY Sessions

### Task C1: Session Registry

**Files:**
- Create: `src-tauri/src/workbench/sessions.rs`
- Modify: `src-tauri/src/workbench/mod.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Implement `WorkbenchSessionRegistry`**

Create `sessions.rs` with:

```rust
pub struct WorkbenchSessionRegistry {
    sessions: Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
}

struct WorkbenchSessionHandle {
    dto: WorkbenchSessionDto,
    writer: Box<dyn std::io::Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}
```

Required methods:

```rust
pub fn new() -> Self
pub fn list(&self, project_id: Option<&str>) -> Vec<WorkbenchSessionDto>
pub fn create(&self, app: AppHandle, project: WorkbenchProjectRow, cli_path: String) -> Result<WorkbenchSessionDto, AppError>
pub fn write_input(&self, session_id: &str, data: &str) -> Result<(), AppError>
pub fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), AppError>
pub fn close(&self, session_id: &str) -> Result<(), AppError>
pub fn rename(&self, session_id: &str, name: &str) -> Result<WorkbenchSessionDto, AppError>
```

Create PTY:

```rust
let pty_system = portable_pty::native_pty_system();
let pair = pty_system.openpty(portable_pty::PtySize {
    rows: 32,
    cols: 98,
    pixel_width: 0,
    pixel_height: 0,
})?;
let mut cmd = portable_pty::CommandBuilder::new(cli_path);
cmd.cwd(project.path.clone());
let child = pair.slave.spawn_command(cmd)?;
let reader = pair.master.try_clone_reader()?;
let writer = pair.master.take_writer()?;
```

Spawn reader thread:

```rust
std::thread::spawn(move || {
    let mut reader = reader;
    let mut buf = [0_u8; 8192];
    let mut seq: u64 = 0;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                seq += 1;
                let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = app.emit("workbench:terminal-output", TerminalOutputEvent {
                    session_id: session_id.clone(),
                    chunk,
                    seq,
                    ts: chrono::Utc::now().timestamp_millis(),
                });
            }
            Err(_) => break,
        }
    }
});
```

Use `tauri::Emitter`.

- [ ] **Step 2: 导出 sessions 模块**

Modify `src-tauri/src/workbench/mod.rs`:

```rust
pub mod fs;
pub mod models;
pub mod projects;
pub mod sessions;
```

- [ ] **Step 3: AppState add registry**

In `state.rs`:

```rust
/// 工作台 PTY 会话注册表（内存态，应用退出即关闭）
pub workbench_sessions: Arc<crate::workbench::sessions::WorkbenchSessionRegistry>,
```

In `lib.rs` setup:

```rust
let workbench_sessions = Arc::new(crate::workbench::sessions::WorkbenchSessionRegistry::new());
```

Add to `AppState`.

- [ ] **Step 4: Session tests**

Add pure tests for list/rename missing session:

```rust
#[test]
fn list_empty_registry_returns_empty() {
    let registry = WorkbenchSessionRegistry::new();
    assert!(registry.list(None).is_empty());
}

#[test]
fn rename_missing_session_returns_error() {
    let registry = WorkbenchSessionRegistry::new();
    assert!(registry.rename("missing", "name").is_err());
}
```

- [ ] **Step 5: Verify**

Run:

```bash
cd src-tauri
cargo test workbench::sessions
cargo check
```

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/workbench/sessions.rs src-tauri/src/workbench/mod.rs src-tauri/src/state.rs src-tauri/src/lib.rs
git commit -m "feat(workbench): manage local Claude PTY sessions"
```

## Phase D — Tauri Commands

### Task D1: Workbench Commands

**Files:**
- Create: `src-tauri/src/commands/workbench.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Implement command module**

Create commands that map exactly to IPC Contract. Important snippets:

```rust
#[tauri::command]
pub async fn add_workbench_project(
    state: State<'_, AppState>,
    path: String,
) -> Result<WorkbenchProjectDto, AppError> {
    let root = crate::workbench::projects::canonical_project_root(&path)?;
    let now = chrono::Utc::now().to_rfc3339();
    let row = WorkbenchProjectRow {
        id: uuid::Uuid::new_v4().to_string(),
        name: crate::workbench::projects::infer_project_name(&root),
        kind: "local".to_string(),
        device_id: state.device_id.as_ref().clone(),
        device_name: state.device_name(),
        path: root.to_string_lossy().to_string(),
        last_opened_at: now.clone(),
        created_at: now.clone(),
        updated_at: now,
    };
    state.workbench_project_repo.upsert(&row).await?;
    Ok(row.to_dto())
}
```

For session create:

```rust
let project = state.workbench_project_repo.get(&project_id).await?
    .ok_or_else(|| AppError::generic("项目不存在"))?;
let cli_path = state.config.read().expect("config 读锁中毒")
    .github_trending.claude_cli_path.clone();
state.workbench_sessions.create(app, project, cli_path)
```

For fs commands:

```rust
let project = load_project(state.inner(), &project_id).await?;
let root = std::path::PathBuf::from(project.path);
let nodes = crate::workbench::fs::list_dir(&root, path.as_deref().unwrap_or("")).await?;
```

- [ ] **Step 2: Register module**

In `commands/mod.rs`:

```rust
pub mod workbench;
```

In `lib.rs` command aliases:

```rust
workbench as workbench_cmd,
```

In `invoke_handler!` add all workbench commands.

- [ ] **Step 3: Verify**

Run:

```bash
cd src-tauri
cargo check
```

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands/workbench.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs
git commit -m "feat(workbench): expose local workbench commands"
```

## Phase E — Frontend API and Types

### Task E1: Types and API

**Files:**
- Modify: `web/src/lib/types.ts`
- Create: `web/src/api/workbench.ts`

- [ ] **Step 1: Add types**

Append to `types.ts`:

```ts
export type WorkbenchProjectKind = 'local' | 'remote';
export type WorkbenchSessionStatus = 'starting' | 'running' | 'exited' | 'disconnected';
export type WorkbenchFileKind = 'file' | 'dir';

export interface WorkbenchProject {
  id: string;
  name: string;
  kind: WorkbenchProjectKind;
  deviceId: string;
  deviceName: string;
  path: string;
  lastOpenedAt: string;
  createdAt: string;
  updatedAt: string;
}

export interface WorkbenchSession {
  id: string;
  projectId: string;
  name: string;
  command: string;
  status: WorkbenchSessionStatus;
  cols: number;
  rows: number;
  startedAt: string;
  exitedAt?: string | null;
  exitCode?: number | null;
}

export interface WorkbenchFileNode {
  name: string;
  path: string;
  kind: WorkbenchFileKind;
  size?: number | null;
  modifiedAt?: string | null;
  children?: WorkbenchFileNode[] | null;
}

export interface WorkbenchPathInfo {
  name: string;
  path: string;
  kind: WorkbenchFileKind;
  size?: number | null;
  modifiedAt?: string | null;
}
```

- [ ] **Step 2: Create API**

Create `web/src/api/workbench.ts`:

```ts
import { invoke } from './client';
import type { WorkbenchFileNode, WorkbenchPathInfo, WorkbenchProject, WorkbenchSession } from '@/lib/types';

export const workbenchApi = {
  listProjects: () => invoke<WorkbenchProject[]>('list_workbench_projects'),
  addProject: (path: string) => invoke<WorkbenchProject>('add_workbench_project', { path }),
  removeProject: (projectId: string) => invoke<{ ok: boolean }>('remove_workbench_project', { projectId }),
  touchProject: (projectId: string) => invoke<WorkbenchProject>('touch_workbench_project', { projectId }),

  listSessions: (projectId?: string) => invoke<WorkbenchSession[]>('list_workbench_sessions', { projectId }),
  createSession: (projectId: string) => invoke<WorkbenchSession>('create_workbench_session', { projectId }),
  writeSessionInput: (sessionId: string, data: string) =>
    invoke<{ ok: boolean }>('write_workbench_session_input', { sessionId, data }),
  resizeSession: (sessionId: string, cols: number, rows: number) =>
    invoke<{ ok: boolean }>('resize_workbench_session', { sessionId, cols, rows }),
  closeSession: (sessionId: string) => invoke<{ ok: boolean }>('close_workbench_session', { sessionId }),
  renameSession: (sessionId: string, name: string) =>
    invoke<WorkbenchSession>('rename_workbench_session', { sessionId, name }),

  listDir: (projectId: string, path = '') =>
    invoke<WorkbenchFileNode[]>('list_workbench_dir', { projectId, path }),
  getPathInfo: (projectId: string, path: string) =>
    invoke<WorkbenchPathInfo>('get_workbench_path_info', { projectId, path }),
  createFile: (projectId: string, parentPath: string, name: string) =>
    invoke<WorkbenchPathInfo>('create_workbench_file', { projectId, parentPath, name }),
  createDir: (projectId: string, parentPath: string, name: string) =>
    invoke<WorkbenchPathInfo>('create_workbench_dir', { projectId, parentPath, name }),
  renamePath: (projectId: string, path: string, newName: string) =>
    invoke<WorkbenchPathInfo>('rename_workbench_path', { projectId, path, newName }),
  deletePath: (projectId: string, path: string) =>
    invoke<{ ok: boolean }>('delete_workbench_path', { projectId, path }),
};
```

- [ ] **Step 3: Verify**

Run: `cd web && npm run build`

- [ ] **Step 4: Commit**

```bash
git add web/src/lib/types.ts web/src/api/workbench.ts
git commit -m "feat(workbench): add frontend workbench API"
```

## Phase F — Frontend Page

### Task F1: Workbench Components

**Files:**
- Create: `web/src/pages/Workbench/TerminalPane.tsx`
- Create: `web/src/pages/Workbench/FileTree.tsx`
- Create: `web/src/pages/Workbench/SessionStatus.tsx`
- Create: `web/src/pages/Workbench/Workbench.module.css`

- [ ] **Step 1: TerminalPane**

Implement `TerminalPane` with xterm:

```tsx
import { useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import type { WorkbenchSession } from '@/lib/types';
import { workbenchApi } from '@/api/workbench';
import styles from './Workbench.module.css';

export interface TerminalPaneProps {
  session: WorkbenchSession;
  active: boolean;
  onResize: (sessionId: string, cols: number, rows: number) => void;
}

export function TerminalPane({ session, active, onResize }: TerminalPaneProps): JSX.Element {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);

  useEffect(() => {
    if (!hostRef.current) return;
    const terminal = new Terminal({ cursorBlink: true, fontSize: 12, fontFamily: 'var(--font-mono)' });
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(hostRef.current);
    fit.fit();
    terminal.onData((data) => {
      void workbenchApi.writeSessionInput(session.id, data);
    });
    terminalRef.current = terminal;
    fitRef.current = fit;
    onResize(session.id, terminal.cols, terminal.rows);
    return () => {
      terminal.dispose();
      terminalRef.current = null;
      fitRef.current = null;
    };
  }, [onResize, session.id]);

  useEffect(() => {
    if (active) terminalRef.current?.focus();
  }, [active]);

  return (
    <article className={styles.terminalPane}>
      <div className={styles.paneHeader}>
        <span>{session.name}</span>
        <span className={styles.paneMeta}>{session.status}</span>
      </div>
      <div ref={hostRef} className={styles.terminalHost} />
    </article>
  );
}
```

The parent page will subscribe to `workbench:terminal-output` and call `terminalRef.current?.write(chunk)` by exposing a `writeTerminal(sessionId, chunk)` callback. If imperative refs become noisy, keep output routing in `TerminalPane` by subscribing per pane.

- [ ] **Step 2: FileTree**

Props:

```tsx
interface FileTreeProps {
  nodes: WorkbenchFileNode[];
  selectedPath: string;
  onSelect: (node: WorkbenchFileNode) => void;
  onRefresh: () => void;
  onCreateFile: () => void;
  onCreateDir: () => void;
  onRename: () => void;
  onDelete: () => void;
}
```

Render nodes recursively, using buttons not divs. Folder rows show twisty; file rows show size.

- [ ] **Step 3: SessionStatus**

Props:

```tsx
interface SessionStatusProps {
  project: WorkbenchProject | null;
  session: WorkbenchSession | null;
  onRename: () => void;
  onRestart: () => void;
  onStop: () => void;
  onClose: () => void;
}
```

Render only fields in spec: session, device, project, path, command, status, runtime.

- [ ] **Step 4: CSS**

Use the prototype layout:

```css
.page {
  width: 100%;
  min-height: 100%;
  background: var(--bg);
}

.workbench {
  height: 100%;
  display: grid;
  grid-template-columns: 240px minmax(0, 1fr) 340px;
  overflow: hidden;
}

.terminalPane {
  min-width: 0;
  min-height: 0;
  display: grid;
  grid-template-rows: 38px 1fr;
  border-radius: var(--radius-lg);
  overflow: hidden;
  background: #171513;
}

.terminalHost {
  min-height: 0;
  padding: var(--space-2);
}
```

- [ ] **Step 5: Verify**

Run: `cd web && npm run build`

### Task F2: Workbench Page Orchestrator

**Files:**
- Create: `web/src/pages/Workbench/Workbench.tsx`
- Create: `web/src/pages/Workbench/index.ts`

- [ ] **Step 1: Page state**

State required:

```tsx
const [projects, setProjects] = useState<WorkbenchProject[]>([]);
const [activeProjectId, setActiveProjectId] = useState<string>('');
const [sessions, setSessions] = useState<WorkbenchSession[]>([]);
const [activeSessionId, setActiveSessionId] = useState<string>('');
const [files, setFiles] = useState<WorkbenchFileNode[]>([]);
const [selectedFile, setSelectedFile] = useState<WorkbenchPathInfo | null>(null);
const [layout, setLayout] = useState<'single' | 'split' | 'quad'>('split');
const [loading, setLoading] = useState<boolean>(true);
const [error, setError] = useState<string | null>(null);
```

All hooks before early returns.

- [ ] **Step 2: Load flow**

On mount:

```tsx
const list = await workbenchApi.listProjects();
setProjects(list);
const first = list[0];
if (first) {
  setActiveProjectId(first.id);
  setSessions(await workbenchApi.listSessions(first.id));
  setFiles(await workbenchApi.listDir(first.id));
}
```

- [ ] **Step 3: Add local project**

Use Tauri dialog plugin already available:

```tsx
import { open } from '@tauri-apps/plugin-dialog';

const selected = await open({ directory: true, multiple: false });
if (typeof selected === 'string') {
  const project = await workbenchApi.addProject(selected);
  setProjects((prev) => [project, ...prev.filter((item) => item.id !== project.id)]);
  setActiveProjectId(project.id);
}
```

- [ ] **Step 4: Create session**

```tsx
const session = await workbenchApi.createSession(activeProjectId);
setSessions((prev) => [session, ...prev]);
setActiveSessionId(session.id);
```

- [ ] **Step 5: Event subscribe**

Use `listen` guarded by Tauri availability pattern from `App.tsx`:

```tsx
const unlisten = await listen<TerminalOutputPayload>('workbench:terminal-output', (event) => {
  // route to TerminalPane writer
});
```

If exposing writers from panes is too complex, keep per-pane listener in `TerminalPane`.

- [ ] **Step 6: Export**

`index.ts`:

```ts
export { Workbench } from './Workbench';
```

- [ ] **Step 7: Verify**

Run:

```bash
cd web
npm run build
```

- [ ] **Step 8: Commit**

```bash
git add web/src/pages/Workbench
git commit -m "feat(workbench): add workbench page shell"
```

## Phase G — Routing and i18n

### Task G1: Navigation and Translations

**Files:**
- Modify: `web/src/App.tsx`
- Modify: `web/src/components/layout/AppShell/AppShell.tsx`
- Modify: `web/src/i18n/index.ts`
- Modify: `web/src/i18n/locales/zh/nav.json`
- Modify: `web/src/i18n/locales/en/nav.json`
- Create: `web/src/i18n/locales/zh/workbench.json`
- Create: `web/src/i18n/locales/en/workbench.json`

- [ ] **Step 1: Route**

In `App.tsx` import and route:

```tsx
import { Workbench } from './pages/Workbench';
...
<Route path="/workbench" element={<Workbench />} />
```

- [ ] **Step 2: Nav**

In AppShell, add nav item near Claude history / Claude Code:

```tsx
<NavItem to="/workbench" label={t('nav:workbench')} icon={<TerminalIcon />} />
```

- [ ] **Step 3: i18n**

`zh/workbench.json` must include:

```json
{
  "title": "工作台",
  "newClaudeSession": "新建 Claude 终端",
  "openFolder": "打开文件夹",
  "addProject": "添加项目",
  "sessionStatus": "当前会话状态",
  "projectFiles": "项目文件夹",
  "emptyProjects": "还没有项目",
  "emptySessions": "当前项目还没有终端会话",
  "loading": "加载中…",
  "status": {
    "starting": "启动中",
    "running": "运行中",
    "exited": "已退出",
    "disconnected": "断线"
  }
}
```

Add matching English keys.

- [ ] **Step 4: Verify**

Run: `cd web && npm run build`

- [ ] **Step 5: Commit**

```bash
git add web/src/App.tsx web/src/components/layout/AppShell/AppShell.tsx web/src/i18n
git commit -m "feat(workbench): wire route and translations"
```

## Phase H — Tests and Docs

### Task H1: Focused Verification

**Files:** tests may be added under `web/src/pages/Workbench/` if pure helpers exist.

- [ ] **Step 1: Rust tests**

Run:

```bash
cd src-tauri
cargo test workbench
cargo test storage::workbench_project_repo
cargo clippy -- -D warnings
```

Expected: all pass. If clippy flags pre-existing unrelated warnings, fix if touched area; otherwise report exact warning.

- [ ] **Step 2: Frontend build**

Run:

```bash
cd web
npm run build
```

Expected: tsc + vite pass.

- [ ] **Step 3: Manual Tauri smoke**

Run:

```bash
./web/node_modules/.bin/tauri dev
```

Manual checks:

1. Open `/workbench`.
2. Add current repo directory.
3. Create Claude terminal.
4. Type a harmless command into Claude prompt or exit with Ctrl+C.
5. Click files in right tree.
6. Create/delete a temporary file under project root and confirm UI updates.

### Task H2: Documentation Updates

**Files:**
- Modify: `docs/prd.md`
- Modify: `AGENTS.md`
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`

- [ ] **Step 1: PRD**

Add section `2.12 工作台`:

```md
### 2.12 工作台

**描述**：按项目文件夹管理 Claude Code 终端会话，第一期支持本机项目与本机多终端。

**功能点**：
- 添加本机项目文件夹，并在左侧最近项目列表中切换
- 在当前项目路径下创建多个 Claude Code 终端
- 支持单窗、双列、四宫格终端布局
- 右侧显示当前会话状态
- 右侧显示可交互项目文件树，支持刷新、新建、重命名、删除和复制相对路径
- 文件预览、远端项目、远端终端和信任机制作为后续阶段扩展
```

- [ ] **Step 2: AGENTS.md**

Keep root concise. Add top-level directory map line only if absent:

```md
│   │   ├── Workbench/         # 工作台：项目文件夹 + Claude Code 终端 + 文件树
```

Add command index entry:

```md
| workbench.* | 工作台项目、终端会话和文件树命令 |
```

- [ ] **Step 3: web/CLAUDE.md**

Add page summary:

```md
Workbench（工作台）：本机项目文件夹 + 多 Claude Code 终端 + 右侧当前会话状态和可交互文件树；API 封装在 `src/api/workbench.ts`，终端渲染使用 `@xterm/xterm`。
```

Add verification command remains `npm run build`.

- [ ] **Step 4: src-tauri/CLAUDE.md**

Add backend summary:

```md
workbench/ — 本机工作台领域模块：`projects` 校验项目根目录，`fs` 限制文件操作在项目根内，`sessions` 用 portable-pty 管理交互式 Claude Code PTY，会话输出通过 Tauri event `workbench:terminal-output` 推送给前端。
```

- [ ] **Step 5: Commit**

```bash
git add docs/prd.md AGENTS.md web/CLAUDE.md src-tauri/CLAUDE.md
git commit -m "docs(workbench): document local workbench behavior"
```

## Phase I — Final Review and Merge

### Task I1: Diff Review

- [ ] **Step 1: Review staged/uncommitted state**

Run:

```bash
git status --short
git log --oneline -8
```

Expected: no uncommitted files; recent commits are workbench-only.

- [ ] **Step 2: Full relevant verification**

Run:

```bash
cd src-tauri && cargo test workbench storage::workbench_project_repo
cd ../web && npm run build
```

Expected: all pass.

- [ ] **Step 3: Merge back**

Only after user approves implementation result:

```bash
cd /Users/hans/web_project/cc-partner
git status --short
git merge --no-ff codex/workbench-local-mvp
git worktree remove ../cc-partner-workbench
git branch -d codex/workbench-local-mvp
```

If main branch has uncommitted user changes, do not merge; report and wait for instruction.

## Self-Review

- Spec coverage: covers first-phase local project, multiple Claude terminal sessions, right-side session status, right-side interactive project folder, no preview. Remote project and trust system are intentionally deferred and listed as out of scope.
- Placeholder/red-flag scan: no deferred-work markers remain; every task names concrete files, commands, and expected checks.
- Type consistency: command names match IPC contract; frontend types use camelCase; Rust DTOs use camelCase; project kind is string in Rust DTO and narrowed in TypeScript.
