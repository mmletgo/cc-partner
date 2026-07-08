# Workbench Browser Preview Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 Workbench 中新增与终端/文件预览同级的内置浏览器预览，首版支持本机项目、远端项目 shortcut 和 `/mobile` 手机端访问。

**Architecture:** 使用“后端预览代理 + 前端 iframe”的统一路径。项目真实 dev server 地址只在项目所在设备解析；本机或手机访问远端项目时先创建本机 relay preview，再由本机代理到远端设备，避免手机直接访问远端设备或错误解析 `localhost`。

**Tech Stack:** Rust/Tauri 2, axum HTTP routes, reqwest, tokio, sqlx SQLite, React 19, TypeScript, Vite, CSS Modules, existing Workbench transport abstraction.

## Global Constraints

- 对话和新增项目记忆说明保持中文；代码文件使用 UTF-8。
- 首版必须支持本机项目、远端项目 shortcut 和移动端 `/mobile`。
- Workbench 浏览器预览以“项目开发预览”为主，不做通用浏览器，不提供任意公网开放代理能力。
- 自动发现 dev server 地址按优先级执行：已记忆目标 -> 当前 worktree 终端输出 URL -> 项目配置/框架默认端口 -> owner device loopback 端口探测 -> 手动输入。
- 远端项目的 `localhost` 必须在项目所在设备解析；本机和移动端只访问 cc-partner 暴露的 preview proxy URL。
- 桌面 Tauri iframe 使用 `http://127.0.0.1:<actual_http_port>/api/workbench/browser/proxy/...` 绝对 URL；移动端 iframe 使用同源 `/api/mobile/workbench/browser/proxy/...`。
- 代理会话必须由 `create preview` 创建，preview id 使用随机不可预测 id，默认 TTL 为 30 分钟并在访问时续期。
- 默认只允许代理 `127.0.0.1`、`localhost`、`[::1]` 和项目配置显式产生的 owner-local dev server URL；拒绝外链、file URL、根外路径和未登记 preview id。
- React hooks 必须放在任何 loading/error/空态 early return 之前。
- 新增函数/组件必须写中文 Business Logic / Code Logic 注释；修改函数逻辑时同步更新注释。
- 样式颜色/字体/间距/圆角/阴影必须使用 `web/src/styles/tokens.css` token。
- 执行本计划时先使用 `superpowers:using-git-worktrees` 创建隔离开发 worktree；编码 subagent 使用用户配置的 `gpt-5.5(xhigh)`。

---

## File Structure

### Backend

- Create `src-tauri/src/workbench/browser_models.rs`
  - Workbench 浏览器预览 DTO、URL source enum、proxy session DTO。
- Create `src-tauri/src/workbench/browser.rs`
  - dev server URL 发现、端口探测、URL 规范化、project/worktree root 解析。
- Create `src-tauri/src/workbench/browser_proxy.rs`
  - in-memory preview registry、HTTP proxy、WebSocket/HMR proxy、preview TTL。
- Create `src-tauri/src/storage/workbench_browser_repo.rs`
  - `workbench_browser_targets` 最近目标持久化读写。
- Modify `src-tauri/src/workbench/mod.rs`
  - 导出 `browser`, `browser_models`, `browser_proxy`。
- Modify `src-tauri/src/storage/mod.rs`
  - 导出 `WorkbenchBrowserRepo`。
- Modify `src-tauri/src/state.rs`
  - 在 `AppState` 挂载 browser repo 和 preview registry。
- Modify `src-tauri/src/lib.rs`
  - 初始化表 schema、repo、registry，注册 Tauri commands。
- Modify `src-tauri/src/commands/workbench.rs`
  - 新增 desktop invoke helper，复用现有 remote-aware 项目上下文。
- Modify `src-tauri/src/workbench/remote_protocol.rs`
  - 新增 browser discover/preview 请求 DTO。
- Modify `src-tauri/src/workbench/remote_client.rs`
  - 新增 browser discover/preview JSON client。
- Modify `src-tauri/src/net/routes/workbench.rs`
  - 新增 P2P local-only browser routes 和 mobile remote-aware routes。
- Modify `src-tauri/src/net/http_server.rs`
  - 注册 JSON routes 和 `any` proxy routes。
- Modify `src-tauri/Cargo.toml`
  - 给 axum/reqwest 增加 proxy 需要的 feature，并增加 `regex`, `futures-util`, `tokio-tungstenite`。
- Modify `src-tauri/migrations/0001_init.sql`
  - 记录 `workbench_browser_targets` schema 文档。
- Modify `src-tauri/CLAUDE.md`
  - 记录 Workbench browser preview 后端边界和目标测试命令。

### Frontend

- Modify `web/src/lib/types.ts`
  - 新增 `WorkbenchBrowserTarget`, `WorkbenchBrowserDiscovery`, `WorkbenchBrowserPreview`。
- Modify `web/src/api/workbench.ts`
  - 新增 Tauri `workbenchApi.browser` invoke adapter。
- Modify `web/src/api/workbenchTransport.ts`
  - 在 `WorkbenchTransport` 增加 `browser` 分组。
- Modify `web/src/api/workbenchHttp.ts`
  - 在 HTTP adapter 增加 mobile browser routes。
- Modify `web/src/lib/icons.tsx`
  - 新增 `BrowserIcon`。
- Create `web/src/components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspace.tsx`
  - 桌面端浏览器预览工作区组件。
- Create `web/src/components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspace.module.css`
  - 浏览器 toolbar、候选列表、iframe 状态样式。
- Create `web/src/components/domain/WorkbenchBrowserWorkspace/index.ts`
  - domain component export。
- Modify `web/src/components/domain/index.ts`
  - 导出 WorkbenchBrowserWorkspace。
- Modify `web/src/pages/Workbench/workbenchFiles.ts`
  - 扩展 workspace view 类型为 `'terminal' | 'browser' | 'files'`。
- Modify `web/src/pages/Workbench/Workbench.tsx`
  - 新增 browser layer，terminal toolbar 加“预览”入口，文件层和 terminal 层切换保持已挂载状态。
- Modify `web/src/pages/Workbench/Workbench.module.css`
  - 新增 `.browserLayer` 叠层样式。
- Modify `web/src/pages/Workbench/workbenchAutomationView.test.ts`
  - 保留 automation 不进入 workspace view 的断言，增加 browser 进入 workspace view 的断言。
- Create `web/src/pages/Workbench/workbenchBrowserPreview.test.ts`
  - 测试 workspace view、iframe URL 选择、desktop/mobile proxy URL 区分。
- Modify `web/src/mobile/mobileWorkbenchState.ts`
  - 新增 `browser` panel，顺序为 projects/automation/terminal/browser/files/git/worktrees/prompt/settings。
- Modify `web/src/mobile/MobileWorkbench.tsx`
  - 接入 MobileBrowserPanel。
- Create `web/src/mobile/components/MobileBrowserPanel.tsx`
  - 移动端浏览器预览面板。
- Create `web/src/mobile/components/MobileBrowserPanel.module.css`
  - 移动端预览面板样式。
- Modify `web/src/mobile/components/MobileWorkbenchShell.tsx`
  - 导航图标和标签加入 browser。
- Modify `web/src/mobile/mobileWorkbenchState.test.ts`
  - 更新面板顺序和 worktree 选择跳转断言。
- Create `web/src/mobile/mobileBrowserPanel.test.ts`
  - 测试移动端同源 proxy path 与 panel 可选性。
- Modify `web/src/i18n/locales/zh/workbench.json`
  - 新增浏览器预览中文文案。
- Modify `web/src/i18n/locales/en/workbench.json`
  - 新增英文文案。
- Modify `web/CLAUDE.md`
  - 记录 Workbench browser preview 前端边界和目标测试命令。

---

## Shared Interfaces

### Rust DTOs

Implement these in `src-tauri/src/workbench/browser_models.rs` and reuse them in commands, routes and remote client:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkbenchBrowserTargetSource {
    Remembered,
    TerminalOutput,
    ProjectConfig,
    PortProbe,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchBrowserTarget {
    pub id: String,
    pub url: String,
    pub display_url: String,
    pub source: WorkbenchBrowserTargetSource,
    pub label: String,
    pub reachable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchBrowserDiscovery {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub targets: Vec<WorkbenchBrowserTarget>,
    pub selected_target_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchBrowserPreview {
    pub preview_id: String,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub target_url: String,
    pub desktop_proxy_url: String,
    pub mobile_proxy_path: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchBrowserDiscoverReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchBrowserPreviewReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub target_url: String,
}
```

### TypeScript DTOs

Implement these in `web/src/lib/types.ts` with matching camelCase fields:

```ts
export type WorkbenchBrowserTargetSource =
  | 'remembered'
  | 'terminalOutput'
  | 'projectConfig'
  | 'portProbe'
  | 'manual';

export interface WorkbenchBrowserTarget {
  id: string;
  url: string;
  displayUrl: string;
  source: WorkbenchBrowserTargetSource;
  label: string;
  reachable: boolean;
}

export interface WorkbenchBrowserDiscovery {
  projectId: string;
  worktreeId: string | null;
  targets: WorkbenchBrowserTarget[];
  selectedTargetId: string | null;
}

export interface WorkbenchBrowserPreview {
  previewId: string;
  projectId: string;
  worktreeId: string | null;
  targetUrl: string;
  desktopProxyUrl: string;
  mobileProxyPath: string;
  expiresAtMs: number;
}
```

---

### Task 1: Backend Browser Models, Persistence And Discovery

**Files:**
- Create: `src-tauri/src/workbench/browser_models.rs`
- Create: `src-tauri/src/workbench/browser.rs`
- Create: `src-tauri/src/storage/workbench_browser_repo.rs`
- Modify: `src-tauri/src/workbench/mod.rs`
- Modify: `src-tauri/src/storage/mod.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/migrations/0001_init.sql`

**Interfaces:**
- Consumes: `AppState.workbench_project_repo`, `AppState.workbench_worktree_repo`, `AppState.workbench_sessions`, existing `WorkbenchProjectRow` and `WorkbenchWorktree` repo APIs.
- Produces:
  - `WorkbenchBrowserRepo::new(pool: SqlitePool) -> Self`
  - `WorkbenchBrowserRepo::get_target(project_id: &str, worktree_id: Option<&str>) -> Result<Option<String>, AppError>`
  - `WorkbenchBrowserRepo::upsert_target(project_id: &str, worktree_id: Option<&str>, target_url: &str) -> Result<(), AppError>`
  - `discover_workbench_browser_targets(state: &AppState, project_id: String, worktree_id: Option<String>) -> Result<WorkbenchBrowserDiscovery, AppError>`
  - `normalize_browser_target_url(raw: &str) -> Result<String, AppError>`
  - `extract_dev_server_urls(text: &str) -> Vec<String>`

- [ ] **Step 1: Add dependency declarations**

In `src-tauri/Cargo.toml`, make these dependency changes:

```toml
axum = { version = "0.7", features = ["macros", "ws"] }
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "stream"] }
regex = "1"
futures-util = "0.3"
tokio-tungstenite = { version = "0.24", features = ["rustls-tls-webpki-roots"] }
```

Run:

```bash
cd /Users/hans/web_project/cc-partner/src-tauri
cargo check --quiet
```

Expected: dependency resolution succeeds. Existing compile errors unrelated to browser preview should be fixed in the same task if they are triggered by these dependency changes.

- [ ] **Step 2: Write failing model and parser tests**

Create `src-tauri/src/workbench/browser_models.rs` with the DTOs from “Shared Interfaces / Rust DTOs”.

Create `src-tauri/src/workbench/browser.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_local_dev_server_urls_from_terminal_output() {
        let output = r#"
          VITE v6.0.0 ready
          Local:   http://localhost:5173/
          Network: http://192.168.1.23:5173/
          ready started server on 0.0.0.0:3000
        "#;

        let urls = extract_dev_server_urls(output);

        assert!(urls.contains(&"http://localhost:5173/".to_string()));
        assert!(urls.contains(&"http://127.0.0.1:3000/".to_string()));
        assert!(!urls.iter().any(|url| url.contains("192.168.1.23")));
    }

    #[test]
    fn normalizes_allowed_loopback_targets() {
        assert_eq!(
            normalize_browser_target_url("localhost:5173").unwrap(),
            "http://127.0.0.1:5173/".to_string(),
        );
        assert_eq!(
            normalize_browser_target_url("http://0.0.0.0:3000/app").unwrap(),
            "http://127.0.0.1:3000/app".to_string(),
        );
        assert_eq!(
            normalize_browser_target_url("https://localhost:3443").unwrap(),
            "https://127.0.0.1:3443/".to_string(),
        );
    }

    #[test]
    fn rejects_open_proxy_targets() {
        assert!(normalize_browser_target_url("https://example.com").is_err());
        assert!(normalize_browser_target_url("file:///etc/passwd").is_err());
        assert!(normalize_browser_target_url("http://169.254.169.254/latest").is_err());
    }
}
```

Run:

```bash
cd /Users/hans/web_project/cc-partner/src-tauri
cargo test workbench::browser::tests --quiet
```

Expected: FAIL because functions are not implemented.

- [ ] **Step 3: Implement URL extraction and normalization**

Implement in `browser.rs`:

```rust
use crate::error::AppError;
use regex::Regex;
use std::sync::OnceLock;
use url::Url;

static DEV_URL_RE: OnceLock<Regex> = OnceLock::new();
static HOST_PORT_RE: OnceLock<Regex> = OnceLock::new();

fn dev_url_re() -> &'static Regex {
    DEV_URL_RE.get_or_init(|| {
        Regex::new(r#"https?://(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\])(?::\d{2,5})(?:/[^\s'"<)]*)?"#)
            .expect("valid dev server url regex")
    })
}

fn host_port_re() -> &'static Regex {
    HOST_PORT_RE.get_or_init(|| {
        Regex::new(r#"(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]):(\d{2,5})"#)
            .expect("valid host port regex")
    })
}

/// 从终端输出中提取本机 dev server URL。
///
/// Business Logic（为什么需要这个函数）:
///     用户启动 Vite/Next/Astro 等 dev server 后，希望 Workbench 自动发现预览地址，不需要手动输入 URL。
///
/// Code Logic（这个函数做什么）:
///     扫描终端文本中的 loopback http(s) URL 和 host:port 片段，归一化为可代理 URL，去重后返回。
pub fn extract_dev_server_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for matched in dev_url_re().find_iter(text).map(|m| m.as_str()) {
        if let Ok(url) = normalize_browser_target_url(matched) {
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    for captures in host_port_re().captures_iter(text) {
        if let Some(port) = captures.get(1) {
            let raw = format!("http://127.0.0.1:{}/", port.as_str());
            if let Ok(url) = normalize_browser_target_url(&raw) {
                if !urls.contains(&url) {
                    urls.push(url);
                }
            }
        }
    }
    urls
}

/// 规范化并校验浏览器预览目标 URL。
///
/// Business Logic（为什么需要这个函数）:
///     Browser preview 代理不能成为开放代理，只允许访问项目所在设备上的本机 dev server。
///
/// Code Logic（这个函数做什么）:
///     补齐 scheme，拒绝非 http(s)，把 localhost/0.0.0.0/[::1] 归一化到 127.0.0.1，并要求显式端口。
pub fn normalize_browser_target_url(raw: &str) -> Result<String, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::generic("预览地址不能为空"));
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = Url::parse(&with_scheme)
        .map_err(|_| AppError::generic("预览地址格式无效"))?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err(AppError::generic("预览地址只支持 http 或 https")),
    }
    let host = url
        .host_str()
        .ok_or_else(|| AppError::generic("预览地址缺少 host"))?
        .to_ascii_lowercase();
    let allowed = matches!(host.as_str(), "localhost" | "127.0.0.1" | "0.0.0.0" | "::1");
    if !allowed {
        return Err(AppError::generic("预览地址必须指向项目所在设备的本机 dev server"));
    }
    if url.port().is_none() {
        return Err(AppError::generic("预览地址必须包含端口"));
    }
    url.set_host(Some("127.0.0.1"))
        .map_err(|_| AppError::generic("预览地址 host 无法归一化"))?;
    if url.path().is_empty() {
        url.set_path("/");
    }
    Ok(url.to_string())
}
```

Run the same test command. Expected: parser tests pass.

- [ ] **Step 4: Add browser target persistence**

Create `src-tauri/src/storage/workbench_browser_repo.rs`:

```rust
use crate::error::AppError;
use sqlx::{Row, SqlitePool};

#[derive(Clone)]
pub struct WorkbenchBrowserRepo {
    pool: SqlitePool,
}

impl WorkbenchBrowserRepo {
    /// 创建 Workbench browser target 仓库。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户为某个项目/worktree 选择过 dev server 后，下次打开预览应优先使用同一目标。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 SQLite pool，后续方法通过运行期 sqlx query 读写 `workbench_browser_targets`。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 读取项目/worktree 最近一次预览目标。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     自动发现需要把用户上次确认的 URL 放在候选第一位。
    ///
    /// Code Logic（这个函数做什么）:
    ///     worktree_id 为空时匹配主项目目标；非空时匹配对应 worktree 目标。
    pub async fn get_target(
        &self,
        project_id: &str,
        worktree_id: Option<&str>,
    ) -> Result<Option<String>, AppError> {
        let row = sqlx::query(
            "SELECT target_url FROM workbench_browser_targets
             WHERE project_id = ?1 AND IFNULL(worktree_id, '') = IFNULL(?2, '')
             LIMIT 1",
        )
        .bind(project_id)
        .bind(worktree_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| row.get::<String, _>("target_url")))
    }

    /// 写入项目/worktree 最近一次预览目标。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户创建预览后，后续自动发现应把该目标作为可信候选。
    ///
    /// Code Logic（这个函数做什么）:
    ///     使用 project_id + coalesced worktree_id 唯一键 upsert，并刷新 updated_at。
    pub async fn upsert_target(
        &self,
        project_id: &str,
        worktree_id: Option<&str>,
        target_url: &str,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO workbench_browser_targets
             (project_id, worktree_id, target_url, updated_at)
             VALUES (?1, ?2, ?3, strftime('%s','now'))
             ON CONFLICT(project_id, worktree_key)
             DO UPDATE SET target_url = excluded.target_url, updated_at = excluded.updated_at",
        )
        .bind(project_id)
        .bind(worktree_id)
        .bind(target_url)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
```

Add schema to the inline schema section in `src-tauri/src/lib.rs` and mirror it in `src-tauri/migrations/0001_init.sql`:

```sql
CREATE TABLE IF NOT EXISTS workbench_browser_targets (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT NOT NULL,
  worktree_id TEXT,
  worktree_key TEXT GENERATED ALWAYS AS (IFNULL(worktree_id, '')) STORED,
  target_url TEXT NOT NULL,
  updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
  UNIQUE(project_id, worktree_key)
);
CREATE INDEX IF NOT EXISTS idx_workbench_browser_targets_project
  ON workbench_browser_targets(project_id, updated_at DESC);
```

Modify `src-tauri/src/storage/mod.rs`:

```rust
pub mod workbench_browser_repo;
pub use workbench_browser_repo::WorkbenchBrowserRepo;
```

Add `WorkbenchBrowserRepo` to `AppState` in `src-tauri/src/state.rs` and instantiate it in `src-tauri/src/lib.rs` next to the other Workbench repos.

Run:

```bash
cd /Users/hans/web_project/cc-partner/src-tauri
cargo check --quiet
```

Expected: PASS.

- [ ] **Step 5: Implement discovery with target ranking**

Add these functions to `src-tauri/src/workbench/browser.rs`:

```rust
pub async fn discover_workbench_browser_targets(
    state: &crate::state::AppState,
    project_id: String,
    worktree_id: Option<String>,
) -> Result<crate::workbench::browser_models::WorkbenchBrowserDiscovery, crate::error::AppError> {
    let root = resolve_browser_worktree_root(state, &project_id, worktree_id.as_deref()).await?;
    let remembered = state
        .workbench_browser_repo
        .get_target(&project_id, worktree_id.as_deref())
        .await?
        .into_iter()
        .filter_map(|url| browser_target_from_url(&url, WorkbenchBrowserTargetSource::Remembered, true).ok());
    let terminal = terminal_output_targets(state, &project_id, worktree_id.as_deref()).await?;
    let config = project_config_targets(&root).await?;
    let probed = probe_default_port_targets(state, &root).await?;
    let targets = rank_browser_targets(
        remembered
            .chain(terminal)
            .chain(config)
            .chain(probed)
            .collect::<Vec<_>>(),
    );
    let selected_target_id = targets.iter().find(|target| target.reachable).map(|target| target.id.clone());
    Ok(WorkbenchBrowserDiscovery {
        project_id,
        worktree_id,
        targets,
        selected_target_id,
    })
}
```

Add the helper functions referenced above in the same file:

- `resolve_browser_worktree_root(...)` loads the local project row and returns the selected worktree path when `worktree_id` is present; otherwise it returns the project path.
- `terminal_output_targets(...)` reads replay text from sessions scoped to the project/worktree and maps `extract_dev_server_urls` results to `TerminalOutput` targets.
- `project_config_targets(...)` reads `package.json` and maps known scripts/dependencies to ports: vite/nuxt/sveltekit -> 5173, next/remix -> 3000, astro -> 4321, storybook -> 6006.
- `probe_default_port_targets(...)` probes `[5173, 3000, 4173, 5174, 8080, 8000, 4321, 6006]` with a 300 ms timeout, excluding `state.actual_http_port`.
- `browser_target_from_url(...)` normalizes a URL, builds id as `"{source}:{normalized_url}"`, sets `display_url` to the user-facing localhost URL, and sets `label` from the source.
- `rank_browser_targets(...)` dedupes by normalized URL and sorts by source priority: Remembered, TerminalOutput, ProjectConfig, PortProbe, Manual.

The resulting target ranking must be deterministic and must dedupe by normalized URL.

Add unit tests for ranking without requiring a live server by extracting a pure function:

```rust
#[test]
fn ranks_remembered_then_terminal_then_config_then_probe() {
    let ranked = rank_browser_targets_for_test(vec![
        target_for_test("http://127.0.0.1:8080/", WorkbenchBrowserTargetSource::PortProbe, true),
        target_for_test("http://127.0.0.1:5173/", WorkbenchBrowserTargetSource::TerminalOutput, true),
        target_for_test("http://127.0.0.1:3000/", WorkbenchBrowserTargetSource::Remembered, true),
        target_for_test("http://127.0.0.1:4321/", WorkbenchBrowserTargetSource::ProjectConfig, true),
    ]);

    assert_eq!(ranked[0].url, "http://127.0.0.1:3000/");
    assert_eq!(ranked[1].url, "http://127.0.0.1:5173/");
    assert_eq!(ranked[2].url, "http://127.0.0.1:4321/");
    assert_eq!(ranked[3].url, "http://127.0.0.1:8080/");
}
```

Run:

```bash
cd /Users/hans/web_project/cc-partner/src-tauri
cargo test workbench::browser --quiet
```

Expected: PASS.

- [ ] **Step 6: Commit backend discovery foundation**

```bash
cd /Users/hans/web_project/cc-partner
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/workbench/browser_models.rs src-tauri/src/workbench/browser.rs src-tauri/src/workbench/mod.rs src-tauri/src/storage/workbench_browser_repo.rs src-tauri/src/storage/mod.rs src-tauri/src/state.rs src-tauri/src/lib.rs src-tauri/migrations/0001_init.sql
git commit -m "feat: add workbench browser discovery foundation"
```

---

### Task 2: Backend Preview Registry, Proxy And Routes

**Files:**
- Create: `src-tauri/src/workbench/browser_proxy.rs`
- Modify: `src-tauri/src/workbench/mod.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/commands/workbench.rs`
- Modify: `src-tauri/src/workbench/remote_protocol.rs`
- Modify: `src-tauri/src/workbench/remote_client.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/net/http_server.rs`

**Interfaces:**
- Consumes:
  - `discover_workbench_browser_targets(...)`
  - `normalize_browser_target_url(...)`
  - `WorkbenchBrowserRepo::upsert_target(...)`
- Produces:
  - `WorkbenchBrowserPreviewRegistry::create_local(...) -> WorkbenchBrowserPreview`
  - `WorkbenchBrowserPreviewRegistry::create_remote_relay(...) -> WorkbenchBrowserPreview`
  - `proxy_workbench_browser_request(...)`
  - Tauri commands: `discover_workbench_browser_targets`, `create_workbench_browser_preview`
  - HTTP routes:
    - `POST /api/workbench/browser/discover`
    - `POST /api/workbench/browser/preview`
    - `ANY /api/workbench/browser/proxy/:previewId/*path`
    - `POST /api/mobile/workbench/browser/discover`
    - `POST /api/mobile/workbench/browser/preview`
    - `ANY /api/mobile/workbench/browser/proxy/:previewId/*path`

- [ ] **Step 1: Write failing registry tests**

Create tests in `src-tauri/src/workbench/browser_proxy.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_rejects_unknown_preview_id() {
        let registry = WorkbenchBrowserPreviewRegistry::new();
        assert!(registry.lookup("missing").is_none());
    }

    #[test]
    fn registry_creates_unpredictable_preview_ids() {
        let registry = WorkbenchBrowserPreviewRegistry::new();
        let first = registry.create_local_for_test("project-a", None, "http://127.0.0.1:5173/");
        let second = registry.create_local_for_test("project-a", None, "http://127.0.0.1:5173/");

        assert_ne!(first.preview_id, second.preview_id);
        assert!(first.preview_id.len() >= 32);
    }

    #[test]
    fn registry_expires_old_sessions() {
        let registry = WorkbenchBrowserPreviewRegistry::new();
        let preview = registry.create_local_for_test("project-a", None, "http://127.0.0.1:5173/");
        registry.force_expire_for_test(&preview.preview_id);

        assert!(registry.lookup(&preview.preview_id).is_none());
    }
}
```

Run:

```bash
cd /Users/hans/web_project/cc-partner/src-tauri
cargo test workbench::browser_proxy::tests --quiet
```

Expected: FAIL because registry does not exist.

- [ ] **Step 2: Implement registry and preview URL generation**

Implement `WorkbenchBrowserPreviewRegistry` with:

```rust
#[derive(Clone)]
pub struct WorkbenchBrowserPreviewRegistry {
    inner: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<String, BrowserPreviewSession>>>,
}

#[derive(Debug, Clone)]
pub enum BrowserPreviewTarget {
    Local { target_url: String },
    RemoteRelay { base_url: String, remote_preview_id: String, target_url: String },
}

#[derive(Debug, Clone)]
pub struct BrowserPreviewSession {
    pub preview_id: String,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub target: BrowserPreviewTarget,
    pub expires_at: std::time::Instant,
    pub expires_at_ms: i64,
}
```

Implementation rules:
- Generate preview ids with `uuid::Uuid::new_v4().simple().to_string()`.
- TTL is `Duration::from_secs(30 * 60)`.
- `lookup` removes expired entries before returning.
- `lookup` renews the session TTL by another 30 minutes.
- `desktop_proxy_url` is `http://127.0.0.1:{actual_http_port}/api/workbench/browser/proxy/{preview_id}/`.
- `mobile_proxy_path` is `/api/mobile/workbench/browser/proxy/{preview_id}/`.

Add to `AppState`:

```rust
pub workbench_browser_previews: Arc<crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry>,
```

Instantiate in `src-tauri/src/lib.rs`.

Run:

```bash
cd /Users/hans/web_project/cc-partner/src-tauri
cargo test workbench::browser_proxy::tests --quiet
```

Expected: PASS.

- [ ] **Step 3: Add remote protocol and remote client methods**

In `src-tauri/src/workbench/remote_protocol.rs`, re-export or define:

```rust
pub type RemoteWorkbenchBrowserDiscoverReq = crate::workbench::browser_models::WorkbenchBrowserDiscoverReq;
pub type RemoteWorkbenchBrowserPreviewReq = crate::workbench::browser_models::WorkbenchBrowserPreviewReq;
```

In `src-tauri/src/workbench/remote_client.rs`, add methods:

```rust
pub async fn discover_browser_targets(
    &self,
    base_url: &str,
    req: &RemoteWorkbenchBrowserDiscoverReq,
) -> Result<WorkbenchBrowserDiscovery, AppError> {
    self.post_json(base_url, "/api/workbench/browser/discover", req).await
}

pub async fn create_browser_preview(
    &self,
    base_url: &str,
    req: &RemoteWorkbenchBrowserPreviewReq,
) -> Result<WorkbenchBrowserPreview, AppError> {
    self.post_json(base_url, "/api/workbench/browser/preview", req).await
}
```

Add unit tests following existing `RemoteWorkbenchClient` route tests:

```rust
#[tokio::test]
async fn browser_discover_posts_project_and_worktree() {
    // Build a one-route axum test app that asserts JSON body contains projectId/worktreeId
    // and returns WorkbenchBrowserDiscovery.
}

#[tokio::test]
async fn browser_preview_posts_target_url() {
    // Build a one-route axum test app that asserts targetUrl and returns WorkbenchBrowserPreview.
}
```

The test bodies should use the same local server helper pattern already present in `remote_client.rs` tests.

Run:

```bash
cd /Users/hans/web_project/cc-partner/src-tauri
cargo test workbench::remote_client::tests::browser --quiet
```

Expected: PASS.

- [ ] **Step 4: Implement commands with remote-aware behavior**

In `src-tauri/src/commands/workbench.rs`, add Tauri commands and helper functions:

```rust
#[tauri::command]
pub async fn discover_workbench_browser_targets(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
) -> Result<WorkbenchBrowserDiscovery, AppError> {
    discover_workbench_browser_targets_for_state(&state, project_id, worktree_id).await
}

#[tauri::command]
pub async fn create_workbench_browser_preview(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    target_url: String,
) -> Result<WorkbenchBrowserPreview, AppError> {
    create_workbench_browser_preview_for_state(&state, project_id, worktree_id, target_url).await
}
```

Helper behavior:
- Load project row by `project_id`.
- If `project.kind == "remote"`, call `ensure_remote_project_context`.
- For remote worktree ids, use existing `remote_inner_worktree_id(...)`.
- Remote discover: call owner `/api/workbench/browser/discover`, then map `project_id` back to local shortcut id and keep target URLs unchanged because they are owner-local.
- Remote preview: call owner `/api/workbench/browser/preview`, then create a local relay preview using owner `base_url` and owner `preview_id`; return local `desktopProxyUrl` and `mobileProxyPath`.
- Local discover/preview: call Task 1 discovery and registry directly.
- On successful preview creation, persist the normalized target with `workbench_browser_repo.upsert_target(...)`.

Register commands in `src-tauri/src/lib.rs` invoke handler:

```rust
commands::workbench::discover_workbench_browser_targets,
commands::workbench::create_workbench_browser_preview,
```

Run:

```bash
cd /Users/hans/web_project/cc-partner/src-tauri
cargo check --quiet
```

Expected: PASS.

- [ ] **Step 5: Implement HTTP JSON routes**

In `src-tauri/src/net/routes/workbench.rs`, add handlers:

```rust
pub async fn discover_browser_targets(
    State(state): State<AppState>,
    Json(req): Json<WorkbenchBrowserDiscoverReq>,
) -> Result<Json<WorkbenchBrowserDiscovery>, ApiError> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id).await?;
    let discovery = crate::workbench::browser::discover_workbench_browser_targets(
        &state,
        req.project_id,
        req.worktree_id,
    )
    .await?;
    Ok(Json(discovery))
}

pub async fn create_browser_preview(
    State(state): State<AppState>,
    Json(req): Json<WorkbenchBrowserPreviewReq>,
) -> Result<Json<WorkbenchBrowserPreview>, ApiError> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id).await?;
    let preview = crate::commands::workbench::create_workbench_browser_preview_for_state(
        &state,
        req.project_id,
        req.worktree_id,
        req.target_url,
    )
    .await?;
    Ok(Json(preview))
}

pub async fn mobile_discover_browser_targets(
    State(state): State<AppState>,
    Json(req): Json<WorkbenchBrowserDiscoverReq>,
) -> Result<Json<WorkbenchBrowserDiscovery>, ApiError> {
    let discovery = crate::commands::workbench::discover_workbench_browser_targets_for_state(
        &state,
        req.project_id,
        req.worktree_id,
    )
    .await?;
    Ok(Json(discovery))
}

pub async fn mobile_create_browser_preview(
    State(state): State<AppState>,
    Json(req): Json<WorkbenchBrowserPreviewReq>,
) -> Result<Json<WorkbenchBrowserPreview>, ApiError> {
    let preview = crate::commands::workbench::create_workbench_browser_preview_for_state(
        &state,
        req.project_id,
        req.worktree_id,
        req.target_url,
    )
    .await?;
    Ok(Json(preview))
}
```

Run:

```bash
cd /Users/hans/web_project/cc-partner/src-tauri
cargo check --quiet
```

Expected: PASS.

- [ ] **Step 6: Implement HTTP and WebSocket proxy**

In `browser_proxy.rs`, implement:

```rust
pub async fn proxy_workbench_browser_request(
    state: crate::state::AppState,
    preview_id: String,
    tail_path: String,
    req: axum::http::Request<axum::body::Body>,
) -> Result<axum::response::Response, crate::net::routes::ApiError> {
    let Some(session) = state.workbench_browser_previews.lookup(&preview_id) else {
        return Err(crate::net::routes::ApiError::not_found("预览会话不存在或已过期"));
    };
    if is_websocket_upgrade(req.headers()) {
        return proxy_workbench_browser_websocket(state, session, tail_path, req).await;
    }
    let upstream_url = build_upstream_proxy_url(&session, &tail_path, req.uri().query())?;
    let method = req.method().clone();
    let headers = filtered_proxy_headers(req.headers());
    let body = axum::body::to_bytes(req.into_body(), usize::MAX)
        .await
        .map_err(|_| crate::net::routes::ApiError::bad_request("读取预览请求失败"))?;
    let response = reqwest::Client::new()
        .request(method, upstream_url)
        .headers(headers)
        .body(body)
        .send()
        .await?;
    response_to_axum_response(&session, response).await
}
```

Add the helper functions referenced above in the same file:

- `is_websocket_upgrade(headers: &HeaderMap) -> bool` checks `connection` and `upgrade` headers case-insensitively.
- `build_upstream_proxy_url(session, tail_path, query)` joins either local `target_url` or remote owner `/api/workbench/browser/proxy/{remote_preview_id}/` with path and query.
- `filtered_proxy_headers(headers)` drops hop-by-hop headers: `connection`, `keep-alive`, `proxy-authenticate`, `proxy-authorization`, `te`, `trailer`, `transfer-encoding`, `upgrade`, and `host`.
- `response_to_axum_response(session, response)` copies status/body/headers and rewrites `Location` values that point at the owner target back under this preview proxy.
- `proxy_workbench_browser_websocket(...)` uses `tokio_tungstenite::connect_async` for upstream WebSocket and `axum::extract::ws::WebSocketUpgrade` for downstream when `Upgrade: websocket` is present.

In `src-tauri/src/net/http_server.rs`, register routes before the SPA fallback:

```rust
.route(
    "/api/workbench/browser/discover",
    post(workbench::discover_browser_targets),
)
.route(
    "/api/workbench/browser/preview",
    post(workbench::create_browser_preview),
)
.route(
    "/api/workbench/browser/proxy/:previewId/*path",
    any(workbench::proxy_browser_preview),
)
.route(
    "/api/mobile/workbench/browser/discover",
    post(workbench::mobile_discover_browser_targets),
)
.route(
    "/api/mobile/workbench/browser/preview",
    post(workbench::mobile_create_browser_preview),
)
.route(
    "/api/mobile/workbench/browser/proxy/:previewId/*path",
    any(workbench::mobile_proxy_browser_preview),
)
```

Add route-level tests in `workbench.rs` or `browser_proxy.rs` that assert:
- unknown preview id returns 404 or JSON error without upstream request;
- local preview proxy forwards GET path and query to a test upstream;
- remote relay forwards to the owner proxy path, not directly to target URL.

Run:

```bash
cd /Users/hans/web_project/cc-partner/src-tauri
cargo test workbench::browser_proxy --quiet
cargo check --quiet
```

Expected: PASS.

- [ ] **Step 7: Commit backend preview routes and proxy**

```bash
cd /Users/hans/web_project/cc-partner
git add src-tauri/src/workbench/browser_proxy.rs src-tauri/src/workbench/mod.rs src-tauri/src/state.rs src-tauri/src/lib.rs src-tauri/src/commands/workbench.rs src-tauri/src/workbench/remote_protocol.rs src-tauri/src/workbench/remote_client.rs src-tauri/src/net/routes/workbench.rs src-tauri/src/net/http_server.rs
git commit -m "feat: add workbench browser preview proxy"
```

---

### Task 3: Frontend API, Transport And Browser Workspace Component

**Files:**
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/api/workbench.ts`
- Modify: `web/src/api/workbenchTransport.ts`
- Modify: `web/src/api/workbenchHttp.ts`
- Modify: `web/src/lib/icons.tsx`
- Create: `web/src/components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspace.tsx`
- Create: `web/src/components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspace.module.css`
- Create: `web/src/components/domain/WorkbenchBrowserWorkspace/index.ts`
- Modify: `web/src/components/domain/index.ts`
- Create: `web/src/pages/Workbench/workbenchBrowserPreview.test.ts`

**Interfaces:**
- Consumes:
  - `WorkbenchTransport.browser.discover(projectId, worktreeId)`
  - `WorkbenchTransport.browser.createPreview(projectId, worktreeId, targetUrl)`
- Produces:
  - `WorkbenchBrowserWorkspace` component
  - `getWorkbenchBrowserFrameSrc(preview, surface)` pure helper returning desktop/mobile iframe URL

- [ ] **Step 1: Add failing frontend API tests**

Create `web/src/pages/Workbench/workbenchBrowserPreview.test.ts`:

```ts
import assert from 'node:assert/strict';
import { getWorkbenchBrowserFrameSrc } from '@/components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspace';
import type { WorkbenchBrowserPreview } from '@/lib/types';

const preview: WorkbenchBrowserPreview = {
  previewId: 'preview-1',
  projectId: 'project-1',
  worktreeId: 'worktree-1',
  targetUrl: 'http://127.0.0.1:5173/',
  desktopProxyUrl: 'http://127.0.0.1:62116/api/workbench/browser/proxy/preview-1/',
  mobileProxyPath: '/api/mobile/workbench/browser/proxy/preview-1/',
  expiresAtMs: 1893456000000,
};

assert.equal(
  getWorkbenchBrowserFrameSrc(preview, 'desktop'),
  'http://127.0.0.1:62116/api/workbench/browser/proxy/preview-1/',
);
assert.equal(
  getWorkbenchBrowserFrameSrc(preview, 'mobile'),
  '/api/mobile/workbench/browser/proxy/preview-1/',
);

console.log('workbenchBrowserPreview tests passed');
```

Run:

```bash
cd /Users/hans/web_project/cc-partner/web
npx --yes tsx src/pages/Workbench/workbenchBrowserPreview.test.ts
```

Expected: FAIL because component/helper/types do not exist.

- [ ] **Step 2: Add shared types and transport methods**

Add TypeScript DTOs from “Shared Interfaces / TypeScript DTOs” to `web/src/lib/types.ts`.

In `web/src/api/workbenchTransport.ts`, extend the interface:

```ts
browser: {
  discover: (
    projectId: string,
    worktreeId?: string | null,
  ) => Promise<WorkbenchBrowserDiscovery>;
  createPreview: (
    projectId: string,
    worktreeId: string | null | undefined,
    targetUrl: string,
  ) => Promise<WorkbenchBrowserPreview>;
};
```

Add imports for `WorkbenchBrowserDiscovery` and `WorkbenchBrowserPreview`. Extend `tauriWorkbenchTransport`:

```ts
browser: {
  discover: (projectId, worktreeId) =>
    workbenchApi.browser.discover(projectId, worktreeId ?? null),
  createPreview: (projectId, worktreeId, targetUrl) =>
    workbenchApi.browser.createPreview(projectId, worktreeId ?? null, targetUrl),
},
```

In `web/src/api/workbench.ts`, add:

```ts
browser: {
  discover: (
    projectId: string,
    worktreeId?: string | null,
  ): Promise<WorkbenchBrowserDiscovery> =>
    invoke<WorkbenchBrowserDiscovery>('discover_workbench_browser_targets', {
      projectId,
      worktreeId: worktreeId ?? null,
    }),
  createPreview: (
    projectId: string,
    worktreeId: string | null | undefined,
    targetUrl: string,
  ): Promise<WorkbenchBrowserPreview> =>
    invoke<WorkbenchBrowserPreview>('create_workbench_browser_preview', {
      projectId,
      worktreeId: worktreeId ?? null,
      targetUrl,
    }),
},
```

In `web/src/api/workbenchHttp.ts`, extend `httpWorkbenchTransport`:

```ts
browser: {
  discover: (projectId, worktreeId) =>
    postJson<WorkbenchBrowserDiscovery>(`${MOBILE_WORKBENCH_API_PREFIX}/browser/discover`, {
      projectId,
      worktreeId: worktreeId ?? null,
    }),
  createPreview: (projectId, worktreeId, targetUrl) =>
    postJson<WorkbenchBrowserPreview>(`${MOBILE_WORKBENCH_API_PREFIX}/browser/preview`, {
      projectId,
      worktreeId: worktreeId ?? null,
      targetUrl,
    }),
},
```

Run:

```bash
cd /Users/hans/web_project/cc-partner/web
npx tsc --noEmit
```

Expected: FAIL only because component/helper is still missing.

- [ ] **Step 3: Add BrowserIcon**

In `web/src/lib/icons.tsx`, add a 16x16 icon:

```tsx
/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 浏览器预览需要在桌面 toolbar 和移动端导航中使用统一图标。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 16x16 线性浏览器窗口图标，继承 currentColor 并支持 size 覆盖。
 */
export function BrowserIcon({ size = 16 }: IconProps): JSX.Element {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <rect x="2.4" y="3.2" width="11.2" height="9.6" rx="1.6" stroke="currentColor" strokeWidth={1.6} />
      <path d="M2.8 6h10.4" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round" />
      <path d="M5 4.6h.01M7 4.6h.01" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" />
    </svg>
  );
}
```

Format if the file uses a multi-line SVG style around nearby icons.

- [ ] **Step 4: Implement WorkbenchBrowserWorkspace component**

Create `web/src/components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspace.tsx`:

```tsx
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Input, Pill } from '@/components/primitives';
import { BrowserIcon, ExternalLinkIcon, RefreshIcon } from '@/lib/icons';
import type {
  WorkbenchBrowserDiscovery,
  WorkbenchBrowserPreview,
  WorkbenchBrowserTarget,
  WorkbenchProject,
  WorkbenchWorktree,
} from '@/lib/types';
import type { WorkbenchTransport } from '@/api/workbenchTransport';
import styles from './WorkbenchBrowserWorkspace.module.css';

export type WorkbenchBrowserSurface = 'desktop' | 'mobile';

export interface WorkbenchBrowserWorkspaceProps {
  surface: WorkbenchBrowserSurface;
  transport: WorkbenchTransport;
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
  onReturnToTerminal?: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面 Tauri 和移动端浏览器访问 preview proxy 的 URL 不同，组件需要稳定选择 iframe src。
 *
 * Code Logic（这个函数做什么）:
 *   desktop 返回后端提供的绝对 loopback URL，mobile 返回同源 path。
 */
export function getWorkbenchBrowserFrameSrc(
  preview: WorkbenchBrowserPreview | null,
  surface: WorkbenchBrowserSurface,
): string | null {
  if (!preview) return null;
  return surface === 'desktop' ? preview.desktopProxyUrl : preview.mobileProxyPath;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 用户需要在终端和文件工作区旁快速查看当前项目 dev server 效果，并能自动发现或手动输入 URL。
 *
 * Code Logic（这个组件做什么）:
 *   根据 project/worktree 调用 transport.browser.discover，展示候选目标，创建 preview session 后用 iframe 加载代理 URL。
 */
export function WorkbenchBrowserWorkspace({
  surface,
  transport,
  project,
  worktree,
  onReturnToTerminal,
}: WorkbenchBrowserWorkspaceProps): JSX.Element {
  const { t } = useTranslation(['workbench']);
  const [discovery, setDiscovery] = useState<WorkbenchBrowserDiscovery | null>(null);
  const [preview, setPreview] = useState<WorkbenchBrowserPreview | null>(null);
  const [manualUrl, setManualUrl] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const frameSrc = useMemo(() => getWorkbenchBrowserFrameSrc(preview, surface), [preview, surface]);

  const loadDiscovery = useCallback(async () => {
    if (!project) return;
    setBusy(true);
    setError(null);
    try {
      const next = await transport.browser.discover(project.id, worktree?.id ?? null);
      setDiscovery(next);
      const selected = next.targets.find((target) => target.id === next.selectedTargetId) ?? next.targets[0];
      if (selected) {
        const created = await transport.browser.createPreview(project.id, worktree?.id ?? null, selected.url);
        setPreview(created);
        setManualUrl(selected.displayUrl);
      }
    } catch (unknownError) {
      setError(unknownError instanceof Error ? unknownError.message : String(unknownError));
    } finally {
      setBusy(false);
    }
  }, [project, transport, worktree?.id]);

  const openTarget = useCallback(
    async (target: WorkbenchBrowserTarget | string) => {
      if (!project) return;
      const targetUrl = typeof target === 'string' ? target : target.url;
      setBusy(true);
      setError(null);
      try {
        const created = await transport.browser.createPreview(project.id, worktree?.id ?? null, targetUrl);
        setPreview(created);
        setManualUrl(typeof target === 'string' ? target : target.displayUrl);
      } catch (unknownError) {
        setError(unknownError instanceof Error ? unknownError.message : String(unknownError));
      } finally {
        setBusy(false);
      }
    },
    [project, transport, worktree?.id],
  );

  useEffect(() => {
    setDiscovery(null);
    setPreview(null);
    setManualUrl('');
    setError(null);
    if (project) void loadDiscovery();
  }, [loadDiscovery, project?.id, worktree?.id]);

  return (
    <section className={styles.workspace} aria-label={t('workbench:browserPreview.title')}>
      <header className={styles.toolbar}>
        <div className={styles.heading}>
          <BrowserIcon />
          <span>{t('workbench:browserPreview.title')}</span>
          {preview ? <Pill tone="success">{t('workbench:browserPreview.connected')}</Pill> : null}
        </div>
        <div className={styles.actions}>
          {onReturnToTerminal ? (
            <Button variant="secondary" size="sm" onClick={onReturnToTerminal}>
              {t('workbench:returnToTerminal')}
            </Button>
          ) : null}
          <Button
            variant="secondary"
            size="sm"
            icon={<RefreshIcon />}
            loading={busy}
            disabled={!project}
            onClick={() => void loadDiscovery()}
          >
            {t('workbench:browserPreview.refresh')}
          </Button>
        </div>
      </header>
      <div className={styles.targetBar}>
        <Input
          value={manualUrl}
          placeholder={t('workbench:browserPreview.urlPlaceholder')}
          mono
          onChange={(event) => setManualUrl(event.target.value)}
          onKeyDown={(event) => {
            if (event.key !== 'Enter') return;
            event.preventDefault();
            void openTarget(manualUrl);
          }}
        />
        <Button
          variant="primary"
          size="sm"
          icon={<ExternalLinkIcon />}
          disabled={!project || !manualUrl.trim() || busy}
          onClick={() => void openTarget(manualUrl)}
        >
          {t('workbench:browserPreview.open')}
        </Button>
      </div>
      {error ? <div className={styles.error}>{error}</div> : null}
      {discovery?.targets.length ? (
        <div className={styles.targets}>
          {discovery.targets.map((target) => (
            <button
              key={target.id}
              type="button"
              className={styles.targetChip}
              data-active={preview?.targetUrl === target.url || undefined}
              onClick={() => void openTarget(target)}
            >
              <span>{target.label}</span>
              <span>{target.displayUrl}</span>
            </button>
          ))}
        </div>
      ) : null}
      <div className={styles.frameShell}>
        {frameSrc ? (
          <iframe className={styles.frame} src={frameSrc} title={t('workbench:browserPreview.frameTitle')} />
        ) : (
          <div className={styles.empty}>{t('workbench:browserPreview.empty')}</div>
        )}
      </div>
    </section>
  );
}
```

Create the CSS with token-only values:

```css
.workspace {
  min-width: 0;
  min-height: 0;
  display: grid;
  grid-template-rows: auto auto auto minmax(0, 1fr);
  background: var(--bg);
}

.toolbar,
.targetBar,
.targets {
  min-width: 0;
  display: flex;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-2) var(--space-3);
  border-bottom: 1px solid var(--border);
  background: var(--surface);
}

.heading {
  min-width: 0;
  display: flex;
  align-items: center;
  gap: var(--space-2);
  font-weight: var(--weight-semibold);
  color: var(--fg);
}

.actions {
  margin-left: auto;
  display: flex;
  align-items: center;
  gap: var(--space-2);
}

.targetBar {
  background: var(--bg);
}

.targets {
  overflow-x: auto;
  background: var(--bg-subtle);
}

.targetChip {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  max-width: 320px;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  padding: var(--space-1) var(--space-2);
  background: var(--surface);
  color: var(--fg);
  font: inherit;
  cursor: pointer;
  transition: all var(--motion-fast) var(--ease-standard);
}

.targetChip[data-active='true'] {
  border-color: var(--accent);
  color: var(--accent);
}

.targetChip span:last-child {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--fg-muted);
}

.error {
  padding: var(--space-2) var(--space-3);
  color: var(--danger);
  background: var(--danger-bg);
  border-bottom: 1px solid var(--danger-border);
}

.frameShell {
  min-width: 0;
  min-height: 0;
  background: var(--surface);
}

.frame {
  width: 100%;
  height: 100%;
  border: 0;
  background: var(--bg);
}

.empty {
  height: 100%;
  display: grid;
  place-items: center;
  color: var(--fg-muted);
  padding: var(--space-6);
  text-align: center;
}
```

If any token name above is absent, use the closest existing token in `web/src/styles/tokens.css` instead of introducing a hard-coded color.

Add `index.ts`:

```ts
export { WorkbenchBrowserWorkspace, getWorkbenchBrowserFrameSrc } from './WorkbenchBrowserWorkspace';
export type { WorkbenchBrowserSurface, WorkbenchBrowserWorkspaceProps } from './WorkbenchBrowserWorkspace';
```

Export it from `web/src/components/domain/index.ts`.

Run:

```bash
cd /Users/hans/web_project/cc-partner/web
npx --yes tsx src/pages/Workbench/workbenchBrowserPreview.test.ts
npx tsc --noEmit
```

Expected: PASS.

- [ ] **Step 5: Commit frontend browser component foundation**

```bash
cd /Users/hans/web_project/cc-partner
git add web/src/lib/types.ts web/src/api/workbench.ts web/src/api/workbenchTransport.ts web/src/api/workbenchHttp.ts web/src/lib/icons.tsx web/src/components/domain/WorkbenchBrowserWorkspace web/src/components/domain/index.ts web/src/pages/Workbench/workbenchBrowserPreview.test.ts
git commit -m "feat: add workbench browser preview component"
```

---

### Task 4: Desktop Workbench Integration

**Files:**
- Modify: `web/src/pages/Workbench/workbenchFiles.ts`
- Modify: `web/src/pages/Workbench/Workbench.tsx`
- Modify: `web/src/pages/Workbench/Workbench.module.css`
- Modify: `web/src/pages/Workbench/workbenchAutomationView.test.ts`
- Modify: `web/src/i18n/locales/zh/workbench.json`
- Modify: `web/src/i18n/locales/en/workbench.json`

**Interfaces:**
- Consumes: `WorkbenchBrowserWorkspace`, `tauriWorkbenchTransport`, `BrowserIcon`.
- Produces: Browser view is a sibling workspace layer to terminal/files and does not change automation layer semantics.

- [ ] **Step 1: Update workspace view pure type and tests**

In `web/src/pages/Workbench/workbenchFiles.ts`, change:

```ts
export type WorkbenchFileWorkspaceView = 'terminal' | 'browser' | 'files';
```

Update `web/src/pages/Workbench/workbenchAutomationView.test.ts` so it asserts:

```ts
import assert from 'node:assert/strict';
import type { WorkbenchFileWorkspaceView } from './workbenchFiles';

const workspaceViews: WorkbenchFileWorkspaceView[] = ['terminal', 'browser', 'files'];
assert.deepEqual(workspaceViews, ['terminal', 'browser', 'files']);
assert.equal(workspaceViews.includes('automation' as WorkbenchFileWorkspaceView), false);
```

Run:

```bash
cd /Users/hans/web_project/cc-partner/web
npx --yes tsx src/pages/Workbench/workbenchAutomationView.test.ts
```

Expected: PASS.

- [ ] **Step 2: Add desktop browser layer to Workbench.tsx**

In `web/src/pages/Workbench/Workbench.tsx`:
- Import `WorkbenchBrowserWorkspace` and `BrowserIcon`.
- Add a terminal toolbar button near the file workspace button:

```tsx
{!terminalFullscreen ? (
  <Button
    className={styles.terminalActionButton}
    variant="secondary"
    size="sm"
    icon={<BrowserIcon />}
    title={t('workbench:browserPreview.openWorkspace')}
    aria-label={t('workbench:browserPreview.openWorkspace')}
    disabled={!activeProject || !activeWorktree}
    onClick={() => setWorkspaceView('browser')}
  >
    {t('workbench:browserPreview.openWorkspace')}
  </Button>
) : null}
```

- Insert browser layer between terminal layer and file layer:

```tsx
<div
  className={styles.browserLayer}
  data-hidden={(automationConsoleOpen || workspaceView !== 'browser') || undefined}
>
  <WorkbenchBrowserWorkspace
    surface="desktop"
    transport={tauriWorkbenchTransport}
    project={activeProject}
    worktree={activeWorktree}
    onReturnToTerminal={handleReturnToTerminal}
  />
</div>
```

Rules:
- Do not unmount terminal sessions when browser view is active.
- Keep `automationConsoleOpen` behavior unchanged; automation stays project-level and overlays the workspace.
- Do not place hooks after early returns.

Run:

```bash
cd /Users/hans/web_project/cc-partner/web
npx tsc --noEmit
```

Expected: PASS.

- [ ] **Step 3: Add browser layer CSS**

In `web/src/pages/Workbench/Workbench.module.css`, change the layer selectors:

```css
.terminalLayer,
.browserLayer,
.fileLayer {
  position: absolute;
  inset: 0;
  min-width: 0;
  min-height: 0;
  display: grid;
}

.terminalLayer[data-hidden='true'],
.browserLayer[data-hidden='true'],
.fileLayer[data-hidden='true'] {
  opacity: 0;
  visibility: hidden;
  pointer-events: none;
}

.browserLayer,
.fileLayer {
  z-index: var(--z-sticky);
  background: var(--bg);
}
```

Keep existing `.terminalLayer[data-fullscreen='true']` unchanged.

Run:

```bash
cd /Users/hans/web_project/cc-partner/web
npx tsc --noEmit
```

Expected: PASS.

- [ ] **Step 4: Add i18n keys**

In `web/src/i18n/locales/zh/workbench.json`, add under an appropriate Workbench section:

```json
"browserPreview": {
  "title": "项目预览",
  "openWorkspace": "预览",
  "connected": "已连接",
  "refresh": "重新发现",
  "open": "打开",
  "urlPlaceholder": "http://localhost:5173",
  "frameTitle": "项目浏览器预览",
  "empty": "启动项目 dev server 后，系统会自动发现预览地址；也可以手动输入 localhost 端口。"
}
```

In `web/src/i18n/locales/en/workbench.json`, add:

```json
"browserPreview": {
  "title": "Project Preview",
  "openWorkspace": "Preview",
  "connected": "Connected",
  "refresh": "Rediscover",
  "open": "Open",
  "urlPlaceholder": "http://localhost:5173",
  "frameTitle": "Project browser preview",
  "empty": "Start the project's dev server and cc-partner will discover the preview URL, or enter a localhost port manually."
}
```

If the JSON file already has a nearby namespace style, preserve its ordering and comma placement.

Run:

```bash
cd /Users/hans/web_project/cc-partner/web
npx --yes tsx src/pages/Workbench/workbenchBrowserPreview.test.ts
npx --yes tsx src/pages/Workbench/workbenchAutomationView.test.ts
npx tsc --noEmit
```

Expected: PASS.

- [ ] **Step 5: Commit desktop Workbench integration**

```bash
cd /Users/hans/web_project/cc-partner
git add web/src/pages/Workbench/workbenchFiles.ts web/src/pages/Workbench/Workbench.tsx web/src/pages/Workbench/Workbench.module.css web/src/pages/Workbench/workbenchAutomationView.test.ts web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json
git commit -m "feat: integrate browser preview into workbench"
```

---

### Task 5: Mobile Workbench Browser Panel

**Files:**
- Modify: `web/src/mobile/mobileWorkbenchState.ts`
- Modify: `web/src/mobile/mobileWorkbenchState.test.ts`
- Modify: `web/src/mobile/MobileWorkbench.tsx`
- Modify: `web/src/mobile/components/MobileWorkbenchShell.tsx`
- Create: `web/src/mobile/components/MobileBrowserPanel.tsx`
- Create: `web/src/mobile/components/MobileBrowserPanel.module.css`
- Create: `web/src/mobile/mobileBrowserPanel.test.ts`
- Modify: `web/src/i18n/locales/zh/workbench.json`
- Modify: `web/src/i18n/locales/en/workbench.json`

**Interfaces:**
- Consumes: `WorkbenchBrowserWorkspace` with `surface="mobile"` and `httpWorkbenchTransport`.
- Produces: mobile panel id `browser`, same-origin iframe preview path, navigation entry.

- [ ] **Step 1: Update mobile state tests first**

Modify `web/src/mobile/mobileWorkbenchState.test.ts` to assert the exact panel order:

```ts
import assert from 'node:assert/strict';
import { getMobileWorkbenchPanelOrder, selectMobileWorktreeWorkspacePanel } from './mobileWorkbenchState';

assert.deepEqual(getMobileWorkbenchPanelOrder(), [
  'projects',
  'automation',
  'terminal',
  'browser',
  'files',
  'git',
  'worktrees',
  'prompt',
  'settings',
]);

assert.equal(selectMobileWorktreeWorkspacePanel(true), 'terminal');
assert.equal(selectMobileWorktreeWorkspacePanel(false), null);

console.log('mobileWorkbenchState tests passed');
```

Run:

```bash
cd /Users/hans/web_project/cc-partner/web
npx --yes tsx src/mobile/mobileWorkbenchState.test.ts
```

Expected: FAIL until `browser` is added.

- [ ] **Step 2: Add browser panel id and order**

In `web/src/mobile/mobileWorkbenchState.ts`, add `'browser'` to `MobileWorkbenchPanel` and order:

```ts
const MOBILE_WORKBENCH_PANEL_ORDER: readonly MobileWorkbenchPanel[] = [
  'projects',
  'automation',
  'terminal',
  'browser',
  'files',
  'git',
  'worktrees',
  'prompt',
  'settings',
];
```

Run the state test. Expected: PASS.

- [ ] **Step 3: Add mobile browser panel component**

Create `web/src/mobile/components/MobileBrowserPanel.tsx`:

```tsx
import { useTranslation } from 'react-i18next';
import { WorkbenchBrowserWorkspace } from '@/components/domain';
import type { WorkbenchTransport } from '@/api/workbenchTransport';
import type { WorkbenchProject, WorkbenchWorktree } from '@/lib/types';
import styles from './MobileBrowserPanel.module.css';

export interface MobileBrowserPanelProps {
  transport: WorkbenchTransport;
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   手机端 `/mobile` 需要查看本机或远端项目 dev server 效果，且必须使用手机可访问的同源代理路径。
 *
 * Code Logic（这个组件做什么）:
 *   包装 WorkbenchBrowserWorkspace，固定 surface 为 mobile，并提供移动端外层布局。
 */
export function MobileBrowserPanel({
  transport,
  project,
  worktree,
}: MobileBrowserPanelProps): JSX.Element {
  const { t } = useTranslation(['workbench']);
  return (
    <section className={styles.panel} aria-label={t('workbench:mobile.browser.title')}>
      <WorkbenchBrowserWorkspace
        surface="mobile"
        transport={transport}
        project={project}
        worktree={worktree}
      />
    </section>
  );
}
```

Create CSS:

```css
.panel {
  min-width: 0;
  min-height: 0;
  height: 100%;
  display: grid;
  background: var(--bg);
}
```

- [ ] **Step 4: Wire mobile shell navigation**

In `web/src/mobile/components/MobileWorkbenchShell.tsx`:
- Import `BrowserIcon`.
- Add icon mapping:

```tsx
browser: <BrowserIcon />,
```

- Add label key lookup for browser:

```tsx
browser: t('workbench:mobile.nav.browser'),
```

In `web/src/mobile/MobileWorkbench.tsx`:
- Import `MobileBrowserPanel`.
- In the panel render branch, add:

```tsx
{activePanel === 'browser' ? (
  <MobileBrowserPanel
    transport={httpWorkbenchTransport}
    project={activeProject}
    worktree={activeWorktree}
  />
) : null}
```

All hooks in `MobileWorkbench.tsx` must remain before conditional returns.

- [ ] **Step 5: Add mobile browser test**

Create `web/src/mobile/mobileBrowserPanel.test.ts`:

```ts
import assert from 'node:assert/strict';
import { getWorkbenchBrowserFrameSrc } from '@/components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspace';
import type { WorkbenchBrowserPreview } from '@/lib/types';

const preview: WorkbenchBrowserPreview = {
  previewId: 'mobile-preview',
  projectId: 'project-1',
  worktreeId: null,
  targetUrl: 'http://127.0.0.1:5173/',
  desktopProxyUrl: 'http://127.0.0.1:62116/api/workbench/browser/proxy/mobile-preview/',
  mobileProxyPath: '/api/mobile/workbench/browser/proxy/mobile-preview/',
  expiresAtMs: 1893456000000,
};

assert.equal(
  getWorkbenchBrowserFrameSrc(preview, 'mobile'),
  '/api/mobile/workbench/browser/proxy/mobile-preview/',
);
assert.notEqual(getWorkbenchBrowserFrameSrc(preview, 'mobile'), preview.desktopProxyUrl);

console.log('mobileBrowserPanel tests passed');
```

Run:

```bash
cd /Users/hans/web_project/cc-partner/web
npx --yes tsx src/mobile/mobileBrowserPanel.test.ts
npx --yes tsx src/mobile/mobileWorkbenchState.test.ts
npx tsc --noEmit
```

Expected: PASS.

- [ ] **Step 6: Add mobile i18n keys**

In zh:

```json
"mobile": {
  "nav": {
    "browser": "预览"
  },
  "browser": {
    "title": "移动端项目预览"
  }
}
```

In en:

```json
"mobile": {
  "nav": {
    "browser": "Preview"
  },
  "browser": {
    "title": "Mobile project preview"
  }
}
```

Merge these into the existing `mobile` object rather than duplicating the object key.

- [ ] **Step 7: Commit mobile browser panel**

```bash
cd /Users/hans/web_project/cc-partner
git add web/src/mobile/mobileWorkbenchState.ts web/src/mobile/mobileWorkbenchState.test.ts web/src/mobile/MobileWorkbench.tsx web/src/mobile/components/MobileWorkbenchShell.tsx web/src/mobile/components/MobileBrowserPanel.tsx web/src/mobile/components/MobileBrowserPanel.module.css web/src/mobile/mobileBrowserPanel.test.ts web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json
git commit -m "feat: add mobile workbench browser preview"
```

---

### Task 6: End-To-End Verification, Project Memory And PRD

**Files:**
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `docs/prd.md`

**Interfaces:**
- Consumes: all previous tasks.
- Produces: documented behavior and verified local/remote/mobile preview flow.

- [ ] **Step 1: Update project memory files**

In `web/CLAUDE.md`, add a concise Workbench browser preview note in the Workbench section:

```md
- Workbench 浏览器预览：桌面端把 `browser` 作为 terminal/files 同级 workspace view；移动端把 `browser` 作为主面板。前端只使用 `WorkbenchTransport.browser`，桌面 iframe 用 `desktopProxyUrl`，移动端 iframe 用 `mobileProxyPath`。
- 相关验证：`npx --yes tsx src/pages/Workbench/workbenchBrowserPreview.test.ts`、`npx --yes tsx src/mobile/mobileBrowserPanel.test.ts`、`npx tsc --noEmit`。
```

In `src-tauri/CLAUDE.md`, add:

```md
- Workbench 浏览器预览：后端用 `workbench::browser` 做 dev server 发现和 loopback URL 校验，用 `workbench::browser_proxy` 管理 preview session 与 HTTP/WebSocket 代理。远端 shortcut 必须在 owner device 创建 preview，本机只创建 relay preview。
- 相关验证：`cargo test workbench::browser --quiet`、`cargo test workbench::browser_proxy --quiet`、`cargo check --quiet`。
```

Keep both files concise and place commands in the most relevant section.

- [ ] **Step 2: Update PRD**

In `docs/prd.md`, update Workbench capability wording to include:

```md
- 工作台内置项目预览：可在终端/文件预览旁打开 dev server 浏览器预览，自动发现终端输出和常见框架端口；本机、远端项目 shortcut 与移动端 `/mobile` 均通过 cc-partner 安全代理访问。
```

- [ ] **Step 3: Run targeted backend verification**

```bash
cd /Users/hans/web_project/cc-partner/src-tauri
cargo test workbench::browser --quiet
cargo test workbench::browser_proxy --quiet
cargo test workbench::remote_client --quiet
cargo check --quiet
```

Expected: all commands PASS. If `cargo check` exposes unrelated pre-existing type errors, fix them in the smallest relevant file and mention them in the final implementation summary.

- [ ] **Step 4: Run targeted frontend verification**

```bash
cd /Users/hans/web_project/cc-partner/web
npx --yes tsx src/pages/Workbench/workbenchBrowserPreview.test.ts
npx --yes tsx src/pages/Workbench/workbenchAutomationView.test.ts
npx --yes tsx src/mobile/mobileWorkbenchState.test.ts
npx --yes tsx src/mobile/mobileBrowserPanel.test.ts
npx tsc --noEmit
```

Expected: all commands PASS.

- [ ] **Step 5: Manual smoke test local preview**

Use the app dev workflow:

```bash
cd /Users/hans/web_project/cc-partner/web
./node_modules/.bin/tauri dev
```

In Workbench:
1. Open a local Vite/Next/Astro project.
2. Start its dev server in a Workbench terminal.
3. Click `预览`.
4. Confirm the browser workspace auto-loads the discovered dev server.
5. Confirm refreshing the page keeps using a cc-partner proxy URL, not the raw target URL.

Expected: iframe displays the project and HMR works after editing a visible file.

- [ ] **Step 6: Manual smoke test remote and mobile preview**

Remote shortcut:
1. From device A, open a Workbench remote project shortcut pointing to device B.
2. Start dev server on device B through Workbench terminal.
3. Click `预览` on device A.
4. Confirm device A iframe uses device A `desktopProxyUrl` and renders device B project.

Mobile:
1. Open device A `/mobile` URL from a phone browser.
2. Select the same local or remote project.
3. Open `预览`.
4. Confirm iframe src begins with `/api/mobile/workbench/browser/proxy/`.
5. Confirm the phone never needs direct access to device B or `localhost`.

Expected: local and remote preview both render through the current cc-partner host.

- [ ] **Step 7: Commit docs and verification updates**

```bash
cd /Users/hans/web_project/cc-partner
git add web/CLAUDE.md src-tauri/CLAUDE.md docs/prd.md
git commit -m "docs: document workbench browser preview"
```

---

## Final Integration Checklist

- [ ] `git status --short` contains only intentional changes before final commit.
- [ ] No `.module.css` introduces hard-coded colors.
- [ ] `rg -n "workspaceView.*automation|automation.*WorkspaceView" web/src/pages/Workbench web/src/mobile` shows automation remains project-level, not a workspace view.
- [ ] `rg -n "browserPreview|MobileBrowserPanel|WorkbenchBrowserWorkspace" web/src` shows desktop and mobile both use the same domain component.
- [ ] No temporary marker comments or panic stubs remain in changed source files.
- [ ] Targeted backend and frontend verification commands from Task 6 pass.
- [ ] Manual local/remote/mobile smoke tests have been run or the exact unavailable environment has been reported.
