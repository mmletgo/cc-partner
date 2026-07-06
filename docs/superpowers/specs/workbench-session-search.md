# Spec: Workbench 终端 Claude Session 搜索与 Resume

> **状态**：待审核
> **作者**：ZCode（基于用户需求 + 三轮细节确认）
> **日期**：2026-07-07
> **审核人**：用户

---

## 1. 背景与目标

### 1.1 用户场景

cc-partner 的 Workbench 终端用户经常在多个 worktree / 多次 Claude Code 会话中工作。Claude Code 把每次会话的完整 transcript 写到磁盘：

```
~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl
```

当用户在某个 worktree 终端里工作，想**找回之前某个 Claude session 继续对话**时，当前没有任何搜索能力——只能在文件管理器里翻 jsonl 文件名（UUID，无意义），或用 `claude --resume` 后在 CLI 的列表里滚屏找。

**目标**：在 Workbench 终端页提供「搜索 Claude session → preview → 一键新建 window resume」的能力，让用户能在**当前 worktree 范围**内按标题或对话内容找到目标 session，选中后自动新建终端 window 并执行 `claude --dangerously-skip-permissions --resume <session-id>`。

### 1.2 不在范围内（明确排除）

- ❌ 跨 worktree / 跨项目的全局 session 搜索
- ❌ 修改现有 `claude_history` 表结构（本次完全独立于 cc_history 子系统，直接读 jsonl 原文件）
- ❌ Workbench session（tmux window）级别的搜索——本功能只搜 Claude Code 的 session（jsonl）
- ❌ 给现有 cc_history 采集器加 assistant 内容采集

---

## 2. 核心数据契约

### 2.1 Claude session 的磁盘布局

```
~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl
```

- `<encoded-cwd>`：`encode_claude_project_path(cwd)`，规则是 `/` 和 `\` → `-`，其它非安全字符 → `-`，安全字符 = `[a-zA-Z0-9_-.]`。例：`/Users/hans/foo` → `-Users-hans-foo`。**已在 `orchestrator/claude_runtime.rs::encode_claude_project_path` 实现，本次复用**。
- `<session-uuid>`：jsonl 文件名 stem 即为 Claude session id（resume 时用）。

### 2.2 jsonl 行结构（本次关注的字段）

每行是一个 JSON 对象，关键字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `type` | string | `"user"` / `"assistant"` / `"last-prompt"` / 其它（忽略） |
| `message.role` | string | `"user"` / `"assistant"` |
| `message.content` | string \| array | string = 纯文本；array = content blocks（type=`text`/`thinking`/`tool_use`/`tool_result` 等） |
| `timestamp` | string | ISO 时间，用于排序 |
| `cwd` | string | 记录时的工作目录（备选匹配，本次主用文件目录） |
| `sessionId` | string | 行内 sessionId（与文件名一致，备选） |
| `lastPrompt` | string | 仅 `type=="last-prompt"` 行有，session 最近一次用户输入的摘要 |

**文本提取规则**（搜索内容来源）：
- **标题来源** = `lastPrompt` 字段（取该 session 最后一条 `type=="last-prompt"` 行的 `lastPrompt`）。若整文件无该类型行，回退为第一条 `user` 文本消息。
- **user 文本** = 所有 `type=="user" && message.role=="user" && message.content` 为 string 的 `content`；若 content 是 array，取其中 `type=="text"` 块的 `text` 字段拼接。
- **assistant 文本** = 所有 `type=="assistant" && message.role=="assistant" && message.content` 为 array 时，取其中 `type=="text"` 块的 `text` 字段（忽略 `thinking` / `tool_use`）。
- 忽略空字符串、`/` 开头的 slash 命令、`!` 开头的 bash 命令（对齐现有 `cc/collector.rs::extract_prompt` 过滤规则）。

### 2.3 搜索匹配语义

- **范围**：仅当前 active worktree 的 path 对应的 `<encoded-cwd>` 目录下的所有 jsonl。
- **匹配**：用户输入的关键词（不区分大小写）在以下字段中做子串匹配（类似现有 cc_history 的 LIKE 语义）：
  - 标题（lastPrompt）
  - user 文本拼接
  - assistant 文本拼接
- **空关键词**：返回所有 session，按 lastActivityAt 倒序，最多 50 条。
- **有关键词**：先按「命中字段优先级」排序（标题命中 > user 命中 > assistant 命中），同优先级按 lastActivityAt 倒序，最多 50 条。
- **高亮**：列表项里命中位置用 `<mark>` 包裹（前端做，后端返回原始文本 + 命中区间）。

---

## 3. 后端能力（Rust）

### 3.1 Session 扫描与索引模块

**新增模块**：`src-tauri/src/workbench/claude_sessions.rs`

职责：
- 扫描指定 worktree path 的 `<encoded-cwd>` 目录，解析所有 jsonl，提取每个 session 的：`sessionId`、`title`(lastPrompt)、`firstActivityAt`、`lastActivityAt`、`messageCount`、`userText`(拼接)、`assistantText`(拼接)、`recentMessages`(最近 20 条对话预览数据)。
- 启动时对**已打开的 Workbench 项目**的 worktree path 建立内存索引；新增 worktree / 切换 active worktree 时按需建索引。
- 用 `notify` crate（或项目已有依赖）监听 `~/.claude/projects/<encoded-cwd>/` 目录变化，jsonl 文件新增/修改时增量更新索引。
- 索引数据结构（内存）：

```rust
pub struct ClaudeSessionIndex {
    pub session_id: String,
    pub title: String,                 // lastPrompt
    pub transcript_path: PathBuf,
    pub first_activity_at: String,
    pub last_activity_at: String,
    pub message_count: u32,
    pub user_text: String,             // 拼接，用于搜索
    pub assistant_text: String,        // 拼接，用于搜索
    pub recent_messages: Vec<RecentMessage>,  // 最近 20 条，用于 preview
}

pub struct RecentMessage {
    pub role: String,                  // "user" | "assistant"
    pub text: String,                  // 提取后的纯文本（已过滤 thinking/tool_use）
    pub timestamp: String,
}

pub struct WorktreeSessionIndex {
    pub worktree_path: PathBuf,
    pub encoded_cwd: String,
    pub sessions: HashMap<String, ClaudeSessionIndex>,  // key = session_id
    pub last_scan_at: String,
}
```

**复用**：`encode_claude_project_path` 从 `orchestrator/claude_runtime.rs` 提取到 `workbench/claude_sessions.rs` 或共享模块，供两处复用（避免 AGENTS.md 第 9 条「相近功能重复实现」）。

### 3.2 搜索方法

```rust
pub struct SessionSearchHit {
    pub session_id: String,
    pub title: String,
    pub title_hit: bool,
    pub user_hit: bool,
    pub assistant_hit: bool,
    pub first_activity_at: String,
    pub last_activity_at: String,
    pub message_count: u32,
    pub preview_snippets: Vec<String>,  // 命中上下文片段（前后各 30 字符），最多 3 段
}

/// 在指定 worktree 的索引里搜索。
/// query 为空 → 返回全部（按 lastActivityAt 倒序，最多 limit）。
/// query 非空 → 按优先级 + lastActivityAt 排序，最多 limit。
pub fn search_sessions(
    index: &WorktreeSessionIndex,
    query: &str,
    limit: usize,
) -> Vec<SessionSearchHit>;
```

### 3.3 Tauri 命令（`commands/workbench.rs` 新增）

| 命令 | 签名 | 说明 |
|---|---|---|
| `search_claude_sessions` | `(project_id, worktree_id?, query) -> Vec<SessionSearchHitDto>` | 搜索当前 worktree 的 Claude session。limit 固定 50。local 项目直接查内存索引；remote 项目代理到远端设备。 |
| `get_claude_session_preview` | `(project_id, worktree_id?, session_id) -> SessionPreviewDto` | 取某 session 的最近 20 条对话（`RecentMessage[]`）+ 元信息（cwd、gitBranch、首末时间、消息数）。用于 preview 面板。 |
| `resume_claude_session` | `(project_id, worktree_id?, session_id) -> { ok, sessionId: WorkbenchSessionId }` | 创建新 terminal window（复用 `create_workbench_session`），自动写入并执行 `claude --dangerously-skip-permissions --resume <session_id>\n`。**执行前先调 `test_claude_cli` 检测 CLI 可用性**，缺失则返回中文业务错误。 |

**camelCase DTO** 对齐前端 `lib/types.ts`：
```ts
// web/src/lib/types.ts 新增
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

### 3.4 远端 P2P 路由（`net/routes/workbench.rs` 新增）

| 端点 | 方法 | 说明 |
|---|---|---|
| `POST /api/workbench/claude-sessions/search` | body `{projectId, worktreeId, query}` → `{hits: SessionSearchHitDto[]}` | 远端设备扫描自己的 jsonl。必须确认收到的 project 是对端 local project（拒绝 remote shortcut 递归）。 |
| `POST /api/workbench/claude-sessions/preview` | body `{projectId, worktreeId, sessionId}` → `SessionPreviewDto` | 同上。 |
| `POST /api/workbench/claude-sessions/resume` | body `{projectId, worktreeId, sessionId}` → `{ok, sessionId}` | 在远端设备创建 terminal window 并执行 resume 命令。返回的 sessionId 包装为 `remote:<deviceId>:<inner>`。 |

`workbench/remote_client.rs` 新增对应 client 方法；`commands/workbench.rs` 的三个命令在 `project.kind == "remote"` 时走 `remote_client` 代理，与现有 worktree/session 命令的 remote-aware 模式一致。

### 3.5 resume 命令注入逻辑

**复用 Orchestrator runner 范式**（`orchestrator/runner.rs:222`）：
1. 调 `local_create_workbench_session(project_id, worktree_id, cols, rows)` 创建纯 shell window（cols/rows 由前端测量传入）。
2. 调 `local_write_workbench_session_input(state, session.id, "claude --dangerously-skip-permissions --resume <session_id>\n")` 注入命令。
3. 返回新建 window 的 sessionId 给前端，前端 `setSessions` + `focusSession` 切到新 window。

**注意**：`workbench/sessions.rs` 明确规定"工作台打开终端只应进入普通 shell，用户自己决定是否在里面运行 claude"——本次 resume 是**用户主动触发**的一次性命令注入（非自动启动），与该原则不冲突，因为创建的仍是普通 shell window，只是随后由用户意图注入了 resume 命令。

---

## 4. 前端能力（React）

### 4.1 新增组件

```
web/src/components/domain/WorkbenchSessionSearch/
├── WorkbenchSessionSearch.tsx      # Command Palette 主组件
├── WorkbenchSessionSearch.module.css
└── index.ts
```

**主组件职责**：
- 受控浮层（`open` / `onClose` props）。
- 内部 state：`query`（搜索词）、`hits`（结果）、`activeIndex`（键盘高亮）、`previewSession`（选中的 session，非 null 时显示 preview 面板）、`loading`、`error`。
- hooks 放所有 early return 之前（AGENTS.md 第 20 条）。
- 搜索 debounce 300ms。
- 键盘：`↑↓` 移动 activeIndex、`⏎` 进入 preview、`esc` 关闭、`⌘K`/`Ctrl+K` 由父组件监听后调 `setOpen(true)`。
- preview 面板：占据 palette 下半部分，显示 `SessionPreview.recentMessages`（user/assistant 交替，带 role 标签 + 时间），顶部有「← 返回」按钮 + session 元信息（cwd、gitBranch、消息数、首末时间），底部有「在新窗口 resume」主按钮（terracotta accent）+ 「取消」ghost 按钮。

### 4.2 API 封装（`web/src/api/workbench.ts` 新增分组）

```ts
// workbench.ts 新增
claudeSessions: {
  search: (projectId: string, worktreeId: string | null, query: string) =>
    invoke<SessionSearchHit[]>('search_claude_sessions', { projectId, worktreeId, query }),
  preview: (projectId: string, worktreeId: string | null, sessionId: string) =>
    invoke<SessionPreview>('get_claude_session_preview', { projectId, worktreeId, sessionId }),
  resume: (projectId: string, worktreeId: string | null, sessionId: string) =>
    invoke<{ ok: boolean; sessionId: string }>('resume_claude_session', { projectId, worktreeId, sessionId }),
},
```

### 4.3 接入 Workbench.tsx

- 在 `WorkbenchWorkspaceNav` 的 `actions` slot 现有按钮组里新增「搜索 session」按钮（accent-soft 配色 + SearchIcon + `⌘K` kbd 标签）。
- 新增 `useState<boolean>` 控制 palette open。
- 新增 `useEffect` 监听全局 `keydown`：仅在 `workspaceView === 'terminal'` 且非 input 聚焦时（避免与 xterm 输入冲突），按 `⌘K`/`Ctrl+K` 触发 `setOpen(true)`。
- palette 通过 `activeProjectId` + `activeWorktreeId` 传入搜索范围；resume 成功后调用现有的 `loadSessions` + `focusSession` 刷新并切到新 window。

### 4.4 视觉规范（基于 v3 原型，复用现有 token）

- scrim：`background: color-mix(in oklab, var(--bg) 60%, transparent)` + `backdrop-filter: blur(2px)`，`z-index: var(--z-overlay)`。
- palette：`top: 12vh`，居中，`width: 640px`（max-width 自适应），`var(--surface)` 背景，`var(--radius-xl)` 圆角，`var(--shadow-window)` 阴影，`z-index: var(--z-modal)`。
- 入场动画：`palette-in` 200ms `--ease-emphasized`（opacity + translateY + scale）。
- 输入框：`var(--text-lg)`，accent 色 search icon。
- 结果项：hover/active 用 `accent-soft` 背景 + `accent` border。
- 命中高亮：`<mark>` 用 `accent-soft` 背景 + `accent` 文字。
- footer：`kbd` 样式与现有 DesignSystem 一致。
- 所有颜色/间距/圆角/阴影 100% 来自 `tokens.css`，浅色/深色自动适配。

### 4.5 三态处理

- **空态**（无 session）：palette 中部显示「该 worktree 下暂无 Claude session」+ 副文案「在使用 Claude Code 工作后，会话会自动出现在这里」。
- **错误态**（扫描失败）：显示错误信息（中文，复用 `displayErrorMessage`）+ 重试按钮。
- **远端离线态**：palette 顶部显示与现有 Workbench 一致的远端离线提示条，禁用搜索输入。

---

## 5. 边界与约束

### 5.1 性能

- jsonl 单文件可能很大（活跃 session 几 MB），扫描时流式读（`BufReader::lines()`），不一次性读入内存。
- 索引建立后搜索是内存操作，<10ms。
- 文件监听用 `notify` crate 的 debounce（500ms），避免高频写入触发频繁重扫。
- 远端搜索的网络超时：search/preview 用短 timeout（15s），resume 用长 timeout（60s，含 claude 启动）。

### 5.2 安全

- `--dangerously-skip-permissions` 是用户明确选择（本次需求 + preview 确认），非默认行为。
- jsonl 解析全程 `serde(default)`，未知字段忽略，malformed 行跳过不阻断。
- 远端 P2P 路由必须确认 project 是对端 local（拒绝 remote shortcut 递归），与现有 workbench routes 一致。

### 5.3 不引入新依赖（除非必要）

- `notify` crate（文件监听）是新增依赖，需评估是否已有替代。若项目已有类似能力则复用。
- 其余（serde/reqwest/axum/sqlx）均已存在。

---

## 6. 验证清单

### 6.1 Rust 单测（`cargo test`）

- `workbench::claude_sessions::tests::*`：
  - `encode_claude_project_path` 与 orchestrator 版本输出一致（回归）
  - jsonl 解析：提取 lastPrompt 标题、user 文本、assistant 文本、recent messages
  - 搜索：空 query 返回全部倒序、关键词命中优先级、limit 截断
  - malformed jsonl 行不崩溃
- `workbench::remote_ids` 相关已有测试不回归
- 远端协议 DTO 序列化 camelCase

### 6.2 前端验证

- `npx tsc --noEmit` 通过
- `npm run build` 通过
- 手动验证：
  - 本机项目：打开搜索 → 输入关键词 → 看到高亮结果 → ⏎ preview → 点 resume → 新 window 启动 claude resume
  - 远端项目：同上流程，命令在远端设备执行
  - 空态/错误态/离线态 UI 正确
  - ⌘K 打开、esc 关闭、↑↓ 导航
  - 浅色/深色主题视觉正确

### 6.3 集成验证命令

```bash
cd src-tauri && cargo test workbench::claude_sessions --lib && cargo check
cd web && npx tsc --noEmit && npm run build
```

---

## 7. CLAUDE.md 更新点（开发完成后）

- `src-tauri/CLAUDE.md` 第「工作台已落地行为约定」节新增「Claude session 搜索与 resume」子节，记录：扫描模块、索引结构、文件监听、三个命令、三个 P2P 路由、resume 注入范式、CLI 检测、远端代理模式。
- `web/CLAUDE.md` Workbench 约定节新增 `WorkbenchSessionSearch` 组件说明。
- 根 `AGENTS.md` 第 4.4 节组件清单新增 `WorkbenchSessionSearch`。

---

## 8. 风险与未决项

| 风险 | 缓解 |
|---|---|
| `notify` crate 在 macOS/Linux/Windows 行为差异 | 用 debounce + 启动全量扫描兜底；若监听失败回退到「每次打开 palette 重扫当前 worktree」 |
| 大 jsonl（>10MB）建索引慢 | 流式读 + 仅提取必要字段；单文件解析超时 2s 跳过 |
| 远端设备 claude CLI 版本不支持 `--resume` | resume 前检测 CLI 存在即可，版本兼容性由 claude 自身保证（`-r` 是稳定 flag） |
| worktree path 含特殊字符导致 encoded-cwd 计算偏差 | 复用已验证的 `encode_claude_project_path`，加单测覆盖 |

**无未决项**——所有细节已在三轮确认中消除。
