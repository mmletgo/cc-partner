# Plan: Workbench 终端 Claude Session 搜索与 Resume

> **状态**：待审核
> **对应 Spec**：`docs/superpowers/specs/workbench-session-search.md`
> **方法论**：superpowers subagent-driven-development
> **审核人**：用户

---

## 0. 执行策略概览

### 0.1 分支策略（遵循 AGENTS.md 第 14 条）

复杂任务（多 subagent）→ **git worktree 新分支开发**：

```bash
# 从 master 切出 worktree 分支
git worktree add -b feat/workbench-session-search ../cc-partner-session-search master
cd ../cc-partner-session-search
# ... 所有开发在新 worktree 进行 ...
# 完成后切回 master 合并
```

### 0.2 模块依赖图

```
                    ┌─────────────────────────────────────┐
                    │  Phase 0: 共享 helper 提取（前置）   │
                    │  - encode_claude_project_path 提到   │
                    │    workbench/claude_path.rs（共享）  │
                    │  - test_claude_cli 提到 claude_cli.rs│
                    └──────────────┬──────────────────────┘
                                   │
                ┌──────────────────┼──────────────────┐
                ▼                  ▼                  ▼
       ┌────────────────┐  ┌────────────────┐  ┌────────────────┐
       │ Phase 1 (后端) │  │ Phase 2 (后端) │  │ Phase 3 (前端) │
       │ 扫描+索引模块  │  │ 命令+P2P 路由  │  │ Command        │
       │ claude_sessions│  │ 3 命令+3 路由  │  │ Palette 组件   │
       │ + 文件监听     │  │ + DTO + client │  │ + API + 接入   │
       └───────┬────────┘  └───────┬────────┘  └───────┬────────┘
               │  依赖              │  依赖              │ 依赖
               └─────────┬──────────┘                  │
                         │ Phase 1→2 串行               │
                         ▼                              │
              ┌────────────────────┐                    │
              │ Phase 4: 集成测试   │◄───────────────────┘
              │ + CLAUDE.md 更新    │
              │ + 验证              │
              └────────────────────┘
```

### 0.3 并行/串行策略

- **Phase 0**（共享 helper 提取）：先做，是后续基础。串行。
- **Phase 1**（扫描+索引+文件监听，纯 Rust 模块）：依赖 Phase 0。可独立做。
- **Phase 2**（3 命令 + 3 路由 + DTO + remote_client）：依赖 Phase 1 的扫描方法。串行在 Phase 1 后。
- **Phase 3**（前端组件 + API + i18n）：**与 Phase 1-2 并行**（前端用 mock 数据先行，不阻塞后端）。
- **Phase 4**（集成 + CLAUDE.md + 验证）：所有 phase 完成后串行。
- **Review**：交给 codex（AGENTS.md 第 22 条），我审 git diff。

---

## Phase 0：共享 helper 提取（前置，1 subagent，串行）

**目标**：为 Phase 1-2 准备干净的复用基础，避免跨模块依赖混乱。

### Task 0.1：提取 `encode_claude_project_path` 到共享模块

**问题**：`encode_claude_project_path` 当前在 `orchestrator/claude_runtime.rs:145`。Phase 1 的 `workbench/claude_sessions.rs` 也要用。直接跨包 import 会破坏模块边界（workbench 不应依赖 orchestrator）。

**做法**：
1. 新建 `src-tauri/src/workbench/claude_path.rs`，把 `encode_claude_project_path` 函数 + 单测迁移过来。
2. `orchestrator/claude_runtime.rs` 改为 `use crate::workbench::claude_path::encode_claude_project_path;`（orchestrator 依赖 workbench 是允许的，反之不允许）。
3. `workbench/mod.rs` 加 `pub mod claude_path;`。
4. 验证 `cargo test orchestrator::claude_runtime --lib` 不回归。

**子任务输出**：`workbench/claude_path.rs` 新文件 + `claude_runtime.rs` import 改动 + `mod.rs` 声明。

### Task 0.2：提取 `test_claude_cli` 核心逻辑到 `claude_cli.rs`

**问题**：`test_claude_cli`（`commands/github_trending.rs:189`）执行 `claude --version`。Phase 2 的 `resume_claude_session` 命令要复用这个检测能力。但 workbench 命令跨包调 github_trending 命令不干净。

**做法**：
1. 在现有 `src-tauri/src/claude_cli.rs`（已存在，是 GitHub Trending + Prompt 优化共享 helper）新增 `pub async fn check_claude_cli_available(cli_path: &str) -> Result<(), String>`，核心是跑 `claude --version`，失败返回中文错误。
2. `commands/github_trending.rs::test_claude_cli` 改为调用这个 helper（保持命令签名不变，只是内部委托）。
3. 验证 `cargo test commands::github_trending --lib` 不回归。

**子任务输出**：`claude_cli.rs` 新增方法 + `github_trending.rs` 改为委托。

### Phase 0 验证
```bash
cd src-tauri
cargo test orchestrator::claude_runtime --lib
cargo test commands::github_trending --lib
cargo check
```

**subagent**：1 个 sonnet subagent 串行做完 Task 0.1 + 0.2（都是小重构，<100 行改动）。

---

## Phase 1：扫描+索引+文件监听模块（1 subagent，依赖 Phase 0）

**目标**：实现 spec 第 3.1、3.2 节的纯 Rust 模块，含完整单测。

### Task 1.1：新建 `src-tauri/src/workbench/claude_sessions.rs`

**模块结构**：
```rust
// claude_sessions.rs

// ── 数据结构 ──
pub struct ClaudeSessionIndex { ... }       // spec 3.1
pub struct RecentMessage { ... }
pub struct WorktreeSessionIndex { ... }      // 内存索引，HashMap<session_id, ClaudeSessionIndex>
pub struct SessionSearchHit { ... }          // spec 3.2

// ── 解析 ──
fn extract_text_from_content(content: &serde_json::Value) -> String;  // 处理 string / array[text blocks]
fn parse_jsonl_line(line: &str) -> Option<ParsedLine>;                // 容错解析
fn build_session_index(transcript_path: &Path) -> Option<ClaudeSessionIndex>;  // 流式读单个 jsonl

// ── 扫描 ──
pub fn scan_worktree_sessions(worktree_path: &Path) -> WorktreeSessionIndex;
// 1. encode_claude_project_path(worktree_path) → encoded_cwd
// 2. ~/.claude/projects/<encoded_cwd>/ 枚举 *.jsonl
// 3. 逐文件 build_session_index
// 4. 组装 WorktreeSessionIndex

// ── 搜索 ──
pub fn search_sessions(index: &WorktreeSessionIndex, query: &str, limit: usize) -> Vec<SessionSearchHit>;
// spec 2.3 语义：空 query 全部倒序；有关键词按 titleHit > userHit > assistantHit 优先级 + lastActivityAt 倒序

// ── preview ──
pub fn get_recent_messages(index: &WorktreeSessionIndex, session_id: &str) -> Option<&Vec<RecentMessage>>;

// ── 单测 ──
#[cfg(test)] mod tests {
    // encode 一致性回归
    // jsonl 解析：lastPrompt 标题、user 文本、assistant 文本、recent messages
    // 搜索：空 query、关键词命中优先级、limit 截断
    // malformed jsonl 行不崩溃（serde default + 行级 try）
    // 大文件流式（用临时 jsonl 模拟）
}
```

**关键实现要点**：
- jsonl 解析参考 `cc/collector.rs::extract_prompt` 的过滤逻辑（忽略 `/` `!` 开头、忽略 array content 里的 tool_result），但要**额外保留 assistant text blocks**（spec 决策 a）。
- `lastPrompt` 取该 session **最后一条** `type=="last-prompt"` 行的 `lastPrompt` 字段；若无则回退第一条 user 文本。
- `recent_messages` 只保留最近 20 条（user + assistant 交替），按 timestamp 排序后取尾部。
- 流式读：`BufReader::new(file).lines()`，单文件解析超时 2s（用 `std::time::Instant` 检查，超时跳过该文件并 warn）。
- 搜索高亮区间：后端返回原始文本，前端做高亮（spec 2.3）。但 `previewSnippets` 在后端算（命中位置前后各 30 字符，最多 3 段）。

### Task 1.2：AppState 集成 + 文件监听

**做法**：
1. `state.rs::AppState` 新增字段：
   ```rust
   pub workbench_session_indexes: Arc<RwLock<HashMap<String, Arc<RwLock<WorktreeSessionIndex>>>>>,
   // key = worktree_path canonical string
   ```
2. `lib.rs` setup 阶段：不主动建索引（按需）。改为在 Phase 2 的 `search_claude_sessions` 命令里 lazy 初始化：首次搜索某 worktree 时调 `scan_worktree_sessions` 建索引 + 启动该 worktree 的文件监听。
3. 文件监听用 `notify` crate（**新增依赖**，spec 5.3 已注明）：
   - 监听 `~/.claude/projects/<encoded_cwd>/` 目录的 `Create`/`Modify` 事件。
   - debounce 500ms（避免 claude 高频写 jsonl 触发频繁重扫）。
   - 触发时只重扫变化的那个 jsonl 文件（`build_session_index` 单文件），更新索引 HashMap。
   - 监听句柄存 `AppState.workbench_session_watchers: Arc<RwLock<HashMap<String, notify::RecommendedWatcher>>>`。
4. **兜底**：若 `notify` 初始化失败（权限/平台问题），warn 日志后降级为「每次搜索重扫」（spec 风险表已注）。

**Cargo.toml 新增**：
```toml
notify = "6"  # 或最新稳定版，subagent 实施时确认
```

### Task 1.3：单测覆盖（强制）

subagent 必须交付以下单测全绿：
- `encode_returns_same_as_orchestrator_version`（回归）
- `parse_extracts_last_prompt_as_title`
- `parse_falls_back_to_first_user_when_no_last_prompt`
- `parse_extracts_user_text_from_string_content`
- `parse_extracts_user_text_from_array_text_blocks`
- `parse_extracts_assistant_text_ignoring_thinking_and_tool_use`
- `parse_ignores_slash_and_bash_commands`
- `parse_skips_malformed_lines_without_panicking`
- `search_empty_query_returns_all_sorted_by_last_activity_desc`
- `search_keyword_prioritizes_title_hit_over_user_over_assistant`
- `search_respects_limit`
- `search_preview_snippets_extract_context_around_hit`

### Phase 1 验证
```bash
cd src-tauri
cargo test workbench::claude_sessions --lib
cargo test workbench::claude_path --lib
cargo check
```

**subagent**：1 个 sonnet subagent。任务规模中等（~600 行 Rust 含测试）。

---

## Phase 2：3 命令 + 3 P2P 路由 + DTO + remote_client（1 subagent，依赖 Phase 1）

**目标**：把 Phase 1 的能力暴露为 Tauri 命令和 P2P 路由，含远端代理。

### Task 2.1：定义 DTO（`workbench/remote_protocol.rs` 新增）

```rust
// 请求 DTO（对齐现有 Remote*Req 命名）
pub struct RemoteSearchClaudeSessionsReq {
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub query: String,
}
pub struct RemoteClaudeSessionReq {  // preview + resume 共用
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub session_id: String,
}

// 响应 DTO（camelCase，前端对齐）
#[derive(Serialize)] #[serde(rename_all = "camelCase")]
pub struct SessionSearchHitDto { ... }       // 对应 SessionSearchHit
#[derive(Serialize)] #[serde(rename_all = "camelCase")]
pub struct SessionPreviewDto { ... }
#[derive(Serialize)] #[serde(rename_all = "camelCase")]
pub struct ResumeClaudeSessionResultDto {
    pub ok: bool,
    pub session_id: String,  // remote 时包装为 remote:<deviceId>:<inner>
}
```

### Task 2.2：3 个 Tauri 命令（`commands/workbench.rs` 新增）

复用现有 `*_for_state` remote-aware 模式（如 `list_workbench_sessions_for_state` workbench.rs:2946）：

```rust
#[tauri::command]
pub async fn search_claude_sessions(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    query: String,
) -> Result<Vec<SessionSearchHitDto>, AppError> {
    // 1. 读 project row（local/remote）
    // 2. 解析 worktree_path（local 直接 resolve；remote 代理时传 worktree_id）
    // 3. local: lazy 建索引（首次）→ search_sessions → 转 DTO
    // 4. remote: remote_client::search_claude_sessions → 转 DTO（sessionId 不需包装，前端按 projectId 判定）
}

#[tauri::command]
pub async fn get_claude_session_preview(...) -> Result<SessionPreviewDto, AppError> { ... }

#[tauri::command]
pub async fn resume_claude_session(...) -> Result<ResumeClaudeSessionResultDto, AppError> {
    // 1. 检测 claude CLI 可用性（local: check_claude_cli_available; remote: 代理远端检测）
    // 2. local: resolve worktree_path → cwd
    // 3. 调 local_create_workbench_session(project_id, worktree_id, cols=120, rows=32)（复用 workbench.rs:2993）
    // 4. 调 local_write_workbench_session_input(state, session.id, "claude --dangerously-skip-permissions --resume <session_id>\n")（复用 workbench.rs:3124）
    // 5. 返回 { ok: true, sessionId: session.id }
    // remote: 代理到远端设备执行上述流程，返回的 sessionId 包装为 remote:<deviceId>:<inner>
}
```

**注册**：`lib.rs` invoke_handler 在 workbench 命令区（line 673 附近）新增 3 行。

### Task 2.3：3 个 P2P 路由（`net/routes/workbench.rs` 新增）

```rust
pub async fn search_claude_sessions_route(
    State(state): State<AppState>,
    Json(req): Json<RemoteSearchClaudeSessionsReq>,
) -> Result<Json<Vec<SessionSearchHitDto>>, AppError> {
    // 1. 确认 req.project_id 是本设备 local project（拒绝 remote shortcut 递归）
    // 2. 解析 worktree_path
    // 3. lazy 建索引 → search_sessions → DTO
}

pub async fn claude_session_preview_route(...) -> Result<Json<SessionPreviewDto>, AppError> { ... }
pub async fn resume_claude_session_route(...) -> Result<Json<ResumeClaudeSessionResultDto>, AppError> { ... }
```

**注册**：`net/http_server.rs` 在现有 `/api/workbench/*` 路由区新增 3 行 `.route()`：
```rust
.route("/api/workbench/claude-sessions/search", post(workbench::search_claude_sessions_route))
.route("/api/workbench/claude-sessions/preview", post(workbench::claude_session_preview_route))
.route("/api/workbench/claude-sessions/resume", post(workbench::resume_claude_session_route))
```

### Task 2.4：remote_client 方法（`workbench/remote_client.rs` 新增）

```rust
impl WorkbenchRemoteClient {
    pub async fn search_claude_sessions(&self, req: RemoteSearchClaudeSessionsReq) -> Result<Vec<SessionSearchHitDto>, WorkbenchRemoteError> { ... }
    pub async fn get_claude_session_preview(&self, req: RemoteClaudeSessionReq) -> Result<SessionPreviewDto, WorkbenchRemoteError> { ... }
    pub async fn resume_claude_session(&self, req: RemoteClaudeSessionReq) -> Result<ResumeClaudeSessionResultDto, WorkbenchRemoteError> { ... }
    // search/preview 用短 timeout(15s)，resume 用长 timeout(60s)
}
```

错误处理复用现有 `WorkbenchRemoteError` 中文文案映射。

### Phase 2 验证
```bash
cd src-tauri
cargo test commands::workbench --lib
cargo test net::routes::workbench --lib
cargo test workbench::remote_client --lib
cargo check
```

**subagent**：1 个 sonnet subagent。任务规模中等（~500 行 Rust）。

---

## Phase 3：前端 Command Palette + API + 接入（1 subagent，与 Phase 1-2 并行）

**目标**：实现 v3 Command Palette 组件 + API 封装 + 接入 Workbench.tsx。

> **并行策略**：Phase 3 可与 Phase 1-2 并行，因为前端开发时不依赖后端真实实现——用本地 mock 数据先行。集成在 Phase 4 做。

### Task 3.1：前端类型定义（`web/src/lib/types.ts` 新增）

```ts
// 对齐 spec 2.3 / 3.3 DTO
export interface SessionSearchHit {
  sessionId: string;
  title: string;
  titleHit: boolean;
  userHit: boolean;
  assistantHit: boolean;
  firstActivityAt: string;
  lastActivityAt: string;
  messageCount: number;
  previewSnippets: string[];
}

export interface SessionPreviewMessage {
  role: 'user' | 'assistant';
  text: string;
  timestamp: string;
}

export interface SessionPreview {
  sessionId: string;
  title: string;
  cwd: string;
  gitBranch: string | null;
  firstActivityAt: string;
  lastActivityAt: string;
  messageCount: number;
  recentMessages: SessionPreviewMessage[];
}
```

### Task 3.2：API 封装（`web/src/api/workbench.ts` 新增 `claudeSessions` 分组）

在现有 `sessions:` 分组（line 121）后新增：
```ts
claudeSessions: {
  search: (projectId: string, worktreeId: string | null, query: string) =>
    invoke<SessionSearchHit[]>('search_claude_sessions', { projectId, worktreeId, query }),
  preview: (projectId: string, worktreeId: string | null, sessionId: string) =>
    invoke<SessionPreview>('get_claude_session_preview', { projectId, worktreeId, sessionId }),
  resume: (projectId: string, worktreeId: string | null, sessionId: string) =>
    invoke<{ ok: boolean; sessionId: string }>('resume_claude_session', { projectId, worktreeId, sessionId }),
},
```

### Task 3.3：新建 `WorkbenchSessionSearch` 组件

```
web/src/components/domain/WorkbenchSessionSearch/
├── WorkbenchSessionSearch.tsx
├── WorkbenchSessionSearch.module.css
└── index.ts
```

**组件 Props**：
```tsx
interface WorkbenchSessionSearchProps {
  open: boolean;
  onClose: () => void;
  projectId: string | null;
  worktreeId: string | null;
  isRemote: boolean;                          // 远端项目标识，影响离线态/错误态文案
  onResumed: (newSessionId: string) => void;  // resume 成功后回调，父组件刷新 sessions + focusSession
}
```

**内部结构**（hooks 全部在 early return 之前，AGENTS.md 第 20 条）：
```tsx
export function WorkbenchSessionSearch(props: WorkbenchSessionSearchProps): JSX.Element {
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<SessionSearchHit[]>([]);
  const [activeIndex, setActiveIndex] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [previewSession, setPreviewSession] = useState<SessionSearchHit | null>(null);
  const [previewData, setPreviewData] = useState<SessionPreview | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [resuming, setResuming] = useState(false);

  // debounce 搜索（300ms）
  useEffect(() => { ... }, [query, projectId, worktreeId, open]);

  // 键盘导航
  const handleKeyDown = useCallback((e: React.KeyboardEvent) => { ... }, []);

  // 打开 preview
  const openPreview = useCallback(async (hit: SessionSearchHit) => { ... }, []);

  // 执行 resume
  const handleResume = useCallback(async () => { ... }, []);

  if (!props.open) return null;  // early return 在所有 hooks 之后

  // 三态 + preview 切换渲染
  return ( ... );
}
```

**视觉规范**（严格按 v3 原型 + tokens.css）：
- 所有颜色用 `var(--xxx)`，禁止硬编码。
- scrim: `position: fixed; inset: 0; z-index: var(--z-overlay); backdrop-filter: blur(2px); background: color-mix(in oklab, var(--bg) 60%, transparent);`
- palette: `position: fixed; top: 12vh; left: 50%; transform: translateX(-50%); width: 640px; max-width: calc(100vw - 32px); z-index: var(--z-modal); background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius-xl); box-shadow: var(--shadow-window);`
- 入场动画 `palette-in` 200ms `--ease-emphasized`。
- 命中高亮 `<mark>` 用 `accent-soft` + `accent`。
- 三态 UI（空/错误/离线）按 spec 4.5。
- 预览面板用 flex 布局，user/assistant 消息左右区分 + role 标签 + 相对时间。

### Task 3.4：接入 Workbench.tsx

1. import `WorkbenchSessionSearch`。
2. 新增 `const [sessionSearchOpen, setSessionSearchOpen] = useState(false);`（放在现有 useState 区域，所有 early return 之前）。
3. 在 `WorkbenchWorkspaceNav` 的 `actions` slot 里，现有按钮组**最右侧**新增「搜索 session」按钮（accent-soft 配色 + SearchIcon + `⌘K` kbd），仅在 `workspaceView === 'terminal'` 时渲染：
   ```tsx
   <Button
     className={styles.terminalActionButton}
     variant="secondary"
     size="sm"
     icon={<SearchIcon />}
     onClick={() => setSessionSearchOpen(true)}
     title={t('workbench:sessionSearch.open')}
   >
     {t('workbench:sessionSearch.open')}
   </Button>
   ```
4. 新增全局快捷键监听（复用现有 `useEffect` keydown 模式，Workbench.tsx:1885 附近）：
   ```tsx
   useEffect(() => {
     if (workspaceView !== 'terminal') return;
     const handler = (e: KeyboardEvent) => {
       if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
         e.preventDefault();
         setSessionSearchOpen(true);
       }
     };
     window.addEventListener('keydown', handler);
     return () => window.removeEventListener('keydown', handler);
   }, [workspaceView]);
   ```
5. 渲染 `<WorkbenchSessionSearch>` 在 Workbench 根节点末尾：
   ```tsx
   <WorkbenchSessionSearch
     open={sessionSearchOpen}
     onClose={() => setSessionSearchOpen(false)}
     projectId={activeProjectId}
     worktreeId={activeWorktreeId}
     isRemote={activeProject?.kind === 'remote'}
     onResumed={(newSessionId) => {
       void loadSessions(activeProjectId);  // 刷新 sessions 列表
       focusSession(newSessionId);          // 切到新 window
       setSessionSearchOpen(false);
     }}
   />
   ```

### Task 3.5：新增 SearchIcon（`web/src/lib/icons.tsx`）

在 icons.tsx 末尾新增（如果已有 SearchIcon 则跳过，确认后决定）：
```tsx
export function SearchIcon({ size = 16 }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
      stroke="currentColor" strokeWidth={1.6} strokeLinecap="round" strokeLinejoin="round">
      <circle cx="7" cy="7" r="5" />
      <path d="m11 11 3 3" />
    </svg>
  );
}
```

### Task 3.6：i18n 文案（`web/src/i18n/locales/{en,zh}/workbench.json` 新增）

```json
{
  "sessionSearch": {
    "open": "搜索 session / Search session",
    "placeholder": "搜索 Claude session 标题或对话内容…",
    "scopeWorktree": "worktree",
    "empty": "该 worktree 下暂无 Claude session",
    "emptyHint": "在使用 Claude Code 工作后，会话会自动出现在这里",
    "error": "扫描 session 失败",
    "retry": "重试",
    "loadingScan": "正在扫描 Claude session…",
    "loadingPreview": "正在加载对话…",
    "resultCount": "{{count}} 个结果",
    "resumeButton": "在新窗口 resume",
    "cancelButton": "取消",
    "backToList": "返回列表",
    "previewTitle": "对话预览",
    "metaCwd": "工作目录",
    "metaGitBranch": "Git 分支",
    "metaMessageCount": "消息数",
    "metaFirstActivity": "首次活动",
    "metaLastActivity": "最近活动",
    "resumeFailed": "Resume 失败",
    "claudeCliMissing": "未检测到 Claude CLI，请先安装",
    "footerNavigate": "选择",
    "footerResume": "新窗口 resume",
    "footerClose": "关闭"
  }
}
```
（en/zh 各一份，subagent 实施时补全双语）

### Phase 3 验证
```bash
cd web
npx tsc --noEmit
npm run build
# 手动：用 mock 数据在浏览器打开 /workbench，按 ⌘K 看 palette 渲染
```

**subagent**：1 个 sonnet subagent。任务规模中等（~500 行 TS/CSS）。

---

## Phase 4：集成 + CLAUDE.md + 全量验证（串行，我亲自做）

> Phase 1-3 完成后，我审查 git diff，确认无冲突后做集成验证。

### Task 4.1：审查 git diff

```bash
cd ../cc-partner-session-search
git diff master --stat
git diff master  # 逐文件审查
```

重点检查：
- Phase 0 重构是否破坏现有 orchestrator/github_trending 行为
- Phase 1 单测全绿
- Phase 2 命令注册完整、远端代理逻辑正确
- Phase 3 组件 hooks 顺序、CSS 全用 token、三态完整
- 前后端 DTO 字段一致（camelCase 对齐）

### Task 4.2：交给 codex review（AGENTS.md 第 22 条）

如果 codex 可用，把 diff 交给 codex 做独立 review。我根据 review 意见决定是否返工。

### Task 4.3：CLAUDE.md 更新（AGENTS.md 第 5 条）

- `src-tauri/CLAUDE.md`「工作台已落地行为约定」节新增「Claude session 搜索与 resume」子节。
- `web/CLAUDE.md` Workbench 约定节新增 `WorkbenchSessionSearch` 组件说明。
- 根 `AGENTS.md` 第 4.4 节组件清单 + 第 8.2 节 invoke 命令清单新增条目。

### Task 4.4：全量验证

```bash
# 后端
cd src-tauri
cargo test workbench::claude_sessions --lib
cargo test workbench::claude_path --lib
cargo test commands::workbench --lib
cargo test net::routes::workbench --lib
cargo test orchestrator::claude_runtime --lib   # 回归
cargo test commands::github_trending --lib       # 回归
cargo check
cargo clippy -- -D warnings   # 可选

# 前端
cd ../web
npx tsc --noEmit
npm run build

# 手动集成测试（需启动 tauri dev）
./node_modules/.bin/tauri dev
# 1. 本机项目 worktree 终端页按 ⌘K → 搜索 → preview → resume
# 2. 远端项目同流程
# 3. 空态/错误态/离线态
# 4. 浅色/深色主题
```

### Task 4.5：合并到 master

```bash
cd ..
git checkout master
git merge --no-ff feat/workbench-session-search
# 解决冲突（如有）
# 验证合并后 cargo check + npm run build 通过
git worktree remove ../cc-partner-session-search
git branch -d feat/workbench-session-search
```

---

## 验收标准（Definition of Done）

- [ ] Phase 0-3 所有单测通过
- [ ] `cargo check` + `cargo clippy` 无新 warning
- [ ] `npx tsc --noEmit` + `npm run build` 通过
- [ ] 本机项目：⌘K 搜索 → preview → resume 全流程跑通
- [ ] 远端项目：同流程，claude 在远端设备执行
- [ ] 空态/错误态/离线态 UI 正确
- [ ] 浅色/深色主题视觉与 v3 原型一致
- [ ] CLAUDE.md（3 处）+ AGENTS.md 组件清单已更新
- [ ] 合并到 master，worktree 清理

---

## subagent 调度总览

| Phase | subagent 数 | model | 任务规模 | 依赖 |
|---|---|---|---|---|
| Phase 0 | 1 | sonnet | ~100 行（小重构） | 无 |
| Phase 1 | 1 | sonnet | ~600 行（模块+测试） | Phase 0 |
| Phase 2 | 1 | sonnet | ~500 行（命令+路由） | Phase 1 |
| Phase 3 | 1 | sonnet | ~500 行（组件+API） | 无（与 1-2 并行，mock 数据） |
| Phase 4 | 我亲自 | - | 集成+验证+合并 | Phase 0-3 全部 |

**总时长预估**：Phase 0 + 1 串行 → Phase 2 串行 → Phase 4；Phase 3 与 1-2 并行。关键路径是 Phase 0→1→2→4。

**并行执行**：我会在同一消息里同时启动 Phase 1（等 Phase 0 完成后）和 Phase 3 的 subagent，最大化并行度。

---

## 风险与应对（开发期）

| 风险 | 应对 |
|---|---|
| `notify` crate 在某些平台行为异常 | Phase 1 实现时加 fallback：监听初始化失败 → warn + 降级每次重扫 |
| Phase 3 用 mock 数据开发，集成时 DTO 不匹配 | Phase 3 subagent prompt 里明确写出最终 DTO 字段（与 Phase 2 对齐），减少集成摩擦 |
| Phase 0 重构破坏 orchestrator/github_trending | Phase 0 完成后立即跑回归测试，不过 Phase 1 |
| commands/workbench.rs 已 4834 行，再加命令更臃肿 | 接受现状（与现有模式一致），不强行拆分（避免无关重构） |
| 远端 resume 的 claude 进程归属远端设备，本机看不到输出 | 这是预期行为（spec 已确认），通过远端事件桥复用现有 terminal-output 事件流 |

---

**请你审核这份 plan**。重点关注：
1. **Phase 划分**是否合理？有没有该合并或该拆分的？
2. **并行策略**（Phase 3 与 1-2 并行用 mock）是否接受？还是你更希望前后端串行（后端先完成，前端再基于真实 API 做）？
3. **subagent 调度**（4 个 sonnet subagent）是否合适？
4. **验收标准**是否有遗漏？
5. **风险应对**是否充分？

通过后我开始执行。