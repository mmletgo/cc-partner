# cc-partner — 项目开发指南

> 面向 AI Agent / 新加入开发者的项目说明。修改代码前请先通读本文档。

## 1. 项目概述

**cc-partner** 是一款跨平台局域网协作桌面应用（macOS / Windows / Ubuntu），核心功能：

- **局域网文件传输** — 任意大小分块传输，支持断点续传
- **区域截图** — 框选截图保存到剪贴板，可直接粘贴到 Claude Code
- **Prompt 管理** — 记录 / 复制 / 打标签 / 跨设备同步
- **Prompt 优化** — 调用本机 Claude Code CLI pure/headless 模式生成中英文优化版 Prompt
- **速记本** — 多页面自动保存文本，支持页面标题、局域网与 GitHub 同步
- **工作台** — 指定本机或局域网远端项目文件夹，管理 Git worktree、多个项目终端、文件工作区、Git 提交树和项目自动化看板
- **移动端 Workbench** — 局域网内通过 `/mobile` 手机浏览器远程操作 Workbench（无调用者身份校验；同一可达网络任意设备可读写执行）；本机可作为手机到远端设备的二级代理，统一管理远端项目的 worktree、终端、文件、Git、Prompt 优化和项目自动化，桌面端展示访问链接、二维码与固定风险提示
- **Orchestrator 自动编排器** — 项目级任务队列、可见 Runner、验证 evidence 与 full-auto 交付，桌面端作为 Workbench 自动化工作区展示
- **独立后端 CLI** — 远端设备可执行 `cc-partner-backend start` 启动 P2P/Workbench/Orchestrator 远端支持，无需打开 GUI
- **P2P 自动互联** — 局域网内 mDNS 自动发现
- **自动更新** — GitHub Releases 检测 / 下载 / 安装

**技术栈**：
- **桌面宿主**: Tauri 2（Rust 主进程）
- **后端**: Rust · axum HTTP server · reqwest peer client · mdns-sd 发现 · sqlx (SQLite) · xcap 抓屏 · arboard 剪贴板 · tracing 日志
- **前端**: React 19 · TypeScript · Vite · React Router v6 · CSS Modules
- **打包/更新**: Tauri CLI · tauri-plugin-updater · tauri-plugin-global-shortcut · tauri-plugin-process · tauri-plugin-dialog

桌面端架构：Tauri 2 主进程用 Rust 实现 GUI 和共享后端能力，前端复用 `web/` 的 React。本地前端通过 `@tauri-apps/api` 的 `invoke()` 调用 Rust `#[tauri::command]`（无本地端口暴露）；跨设备 P2P 由独立后端 sidecar/CLI 负责 axum HTTP server（固定首选端口，端口被占则自动 +1）+ reqwest 客户端 + mdns-sd 发现。局域网能力依赖防火墙放行 UDP **5353**(mDNS) 与实际 P2P HTTP TCP 端口（首选 **62116**，详见 README）。**固定 LAN 边界**：业务 API 无身份鉴权；socket peer 来自 ConnectInfo，再叠加 Host/Origin/Content-Type 与资源上限；不得写可切换 LAN 模式、路由授权矩阵或 LAN 权限 capability token，也不得把 peer 称为“已认证/可信设备/安全设备”。两条通道共享同一份 `AppState`。

## 2. 目录结构

```
cc-partner/
├── web/                          # 前端子项目（独立 npm 工程）
│   ├── src/
│   │   ├── main.tsx              # React 入口
│   │   ├── App.tsx               # 路由根
│   │   ├── styles/
│   │   │   ├── tokens.css        # ⭐ 设计 token（唯一颜色/字体/间距来源）
│   │   │   ├── reset.css         # 全局 reset
│   │   │   └── globals.css       # 高频工具类
│   │   ├── components/
│   │   │   ├── primitives/       # 原子组件（无业务语义）
│   │   │   │   ├── Button/       # variant: primary/secondary/ghost/danger/icon
│   │   │   │   ├── Card/         # 复合: Card.Header / Card.Body / Card.Footer
│   │   │   │   ├── Input/
│   │   │   │   ├── Tag/          # 可关闭 chip
│   │   │   │   ├── Pill/         # 状态标签
│   │   │   │   ├── StatusDot/    # online/offline/busy/away
│   │   │   │   ├── StatusMessage/ # 异步反馈 live region（success=status / danger=alert）
│   │   │   │   ├── ProgressBar/
│   │   │   │   ├── Dialog/       # portal 模态 + useModalLayer（focus trap/Escape/inert/scroll lock）
│   │   │   │   └── Drawer/       # 侧滑模态（side left/right，复用 useModalLayer）
│   │   │   ├── layout/           # 布局组件
│   │   │   │   ├── AppShell/     # 完整应用外壳（TitleBar + Sidebar + main）
│   │   │   │   ├── RouteErrorBoundary/ # 路由错误隔离（pathname reset，保留 shell）
│   │   │   │   ├── Window/       # 模拟 macOS 窗口
│   │   │   │   ├── TitleBar/     # traffic lights + 拖拽区
│   │   │   │   ├── Sidebar/
│   │   │   │   ├── NavItem/      # 路由导航
│   │   │   │   └── ThemeToggle/  # 浅/深色切换
│   │   │   └── domain/           # 业务组件（组合 primitives + layout）
│   │   │       ├── PromptCard/
│   │   │       ├── TransferItem/
│   │   │       ├── GithubRepoCard/
│   │   │       ├── MobileAccessCard/
│   │   │       ├── WorkbenchProjectRail/
│   │   │       └── PermissionCard/
│   │   ├── pages/                # 页面（每个一个文件夹）
│   │   │   ├── Home/             # 01-main.html
│   │   │   ├── Transfer/         # 02-transfer.html
│   │   │   ├── Prompts/          # 03-prompts.html
│   │   │   ├── PromptOptimizer/  # Prompt 优化（本机 Claude CLI pure/headless）
│   │   │   ├── Attention/        # 全局 Inbox（/attention）：实时阻塞投影，只导航不动作
│   │   │   ├── Workbench/        # 本机/远端项目文件夹 + 多项目终端 + 文件树/文件工作区 + Git 提交树 + 自动化工作区
│   │   │   ├── Orchestrator/     # 自动编排器可嵌入面板；旧路由重定向 Workbench
│   │   │   ├── Settings/         # 05-settings.html
│   │   │   ├── Welcome/          # 06-welcome.html
│   │   │   └── DesignSystem/     # 🆕 设计系统预览（仅 dev）
│   │   ├── api/                  # 桌面 invoke + mobile HTTP 封装（含 attention / workbenchHttp）
│   │   ├── hooks/                # 自定义 hooks（useTheme、useVisibilityPolling、usePermissions、ScratchpadAutosaveProvider、AttentionProvider、attentionInvalidation 等）
│   │   ├── lib/                  # 通用工具 + icon 库（含 attention pure helpers）
│   │   └── assets/
│   ├── public/
│   ├── index.html
│   ├── vite.config.ts            # Tauri dev 时由 tauri 自动接管，无 /api proxy
│   ├── tsconfig.json
│   └── package.json
├── src-tauri/                    # Tauri 2 Rust 后端（见 src-tauri/CLAUDE.md）
│   ├── src/                      # lib.rs(入口) config/state/error/commands/models/storage/sync/net/transfer/screenshot/workbench/orchestrator/attention/permissions/hotkey/tray
│   │   ├── backend/              # GUI/headless 共享运行时、独立后端 CLI lifecycle/control 和 UI adapter
│   │   ├── attention/            # 全局 Inbox 聚合（无持久化表；source/aggregator）
│   │   └── orchestrator/         # 自动编排器任务、调度、Runner 和交付逻辑
│   ├── migrations/               # SQL schema 文档
│   ├── capabilities/             # Tauri 权限清单（default.json）
│   ├── icons/                    # 应用图标
│   ├── tauri.conf.json           # Tauri 配置 + bundle + updater（版本号单一来源）
│   └── Cargo.toml
├── scripts/                      # bump-version / prepare-tauri-sidecar / check-p2p-route-inventory / check-quality-traceability / check-docs + 图标源
├── .github/workflows/            # ci/smoke/docs + 公开 macOS/Windows/Linux release + macOS 固定签名手动构建
├── uiux/                         # 设计稿（参考资源，不参与构建）
├── docs/
│   ├── prd.md
│   └── superpowers/specs/        # 设计文档
├── AGENTS.md                     # 本文件（根层开发指令）
├── CLAUDE.md                     # 根层项目概览（与本文件互补）
└── web/dist/                     # Vite 构建产物（git ignored）
```

### 2.1 分层指令地图（一跳可达）

| 场景 | 去哪读 |
|------|--------|
| 设计 token / 组件分层 / 复用 / Hooks 顺序 | 本文 §3–§5；前端细则 `web/CLAUDE.md` |
| 前端命令、Vitest/Playwright、Workbench controllers、Attention、runtime cache | `web/CLAUDE.md` |
| P2P 协议 v1 / 错误信封 / 幂等 / 端口 / CLI doctor / smoke / 发版 | `src-tauri/CLAUDE.md` |
| macOS 固定签名 / ad-hoc 手动授权 / 输入监控真机验收 | `docs/development/macos-internal-signing.md` |
| 用户向启动、防火墙、产品定位 | `README.md` |
| 持久产品行为 | `docs/prd.md` |

## 3. 设计系统架构

### 3.1 单一来源原则

**所有颜色 / 字体 / 间距 / 圆角 / 阴影 100% 来自 `web/src/styles/tokens.css`。**

修改样式时：
1. 先检查 `tokens.css` 是否已有对应 token
2. 如果没有，在 `tokens.css` 中新增（同时考虑浅色/深色两套值）
3. 在组件中使用 `var(--xxx)`

❌ **禁止** 在任何 `.module.css` 中硬编码颜色值（如 `color: #c96442`）。

### 3.2 Token 分类

| 类别 | 命名规范 | 示例 |
|------|---------|------|
| 颜色 | `--bg`, `--surface`, `--fg`, `--accent` 等 | `--accent: #c96442` |
| 字体 | `--font-display`, `--font-body`, `--font-mono` | `--font-body: system-ui, sans-serif` |
| 字号 | `--text-xs` ~ `--text-5xl` | `--text-base: 13px` |
| 字重 | `--weight-regular` ~ `--weight-bold` | `--weight-medium: 500` |
| 间距 | `--space-0` ~ `--space-24`（4px 步进） | `--space-4: 16px` |
| 圆角 | `--radius-xs` ~ `--radius-full` | `--radius-md: 8px` |
| 阴影 | `--shadow-xs` ~ `--shadow-window` | `--shadow-sm: 0 1px 2px ...` |
| 动效 | `--motion-fast/base/slow`, `--ease-standard` | `--motion-base: 200ms` |
| 层级 | `--z-base/sticky/overlay/modal/toast` | `--z-modal: 1000` |

### 3.3 浅色/深色模式

通过 `document.documentElement.dataset.theme = 'dark' | 'light'` 切换。**所有 token 都有两套值**（`:root` 浅色 / `[data-theme="dark"]` 深色）。新增 token 时**必须同时定义两套**。

持久化：localStorage `cp-theme`。
事件：`window` 派发 `cp-theme-change` 自定义事件（detail: `{ theme: 'dark' | 'light' }`）。

## 4. 组件分层与复用规范

### 4.1 三层组件架构

```
primitives  →  layout  →  domain  →  page
   │             │          │         │
   原子         布局       业务      页面
 (无业务)    (无业务)    (业务)    (页面组合)
```

| 层级 | 职责 | 例子 |
|------|------|------|
| **primitives** | 单一 UI 元素，无业务语义，无数据依赖 | Button, Card, Input, Tag, Pill, StatusDot, StatusMessage, ProgressBar, Dialog, Drawer |
| **layout** | 页面结构骨架，无业务数据 | AppShell, Window, TitleBar, Sidebar, NavItem, ThemeToggle, RouteErrorBoundary || **domain** | 组合 primitives + layout，承担具体业务对象的展示/交互 | PromptCard, TransferItem, PermissionCard |
| **page** | 一个路由对应一个页面，组合 domain 组件 + 数据 hook | Home, Transfer, Prompts, ... |

### 4.2 ⚠️ 核心开发规范（必读）

> **开发页面时，必须优先复用已有组件。**
> **如果已有组件可以通过 props / variant / className 扩展，应优先扩展，而不是新建相似组件。**
> **只有在现有组件确实无法满足需求时，才新增组件。**

#### ✅ 正确做法

```tsx
// 用 variant 扩展 Button
<Button variant="primary" size="sm" icon={<PlusIcon />}>新建 Prompt</Button>
<Button variant="ghost" size="sm" icon={<SearchIcon />}>搜索</Button>
<Button variant="danger" size="sm" icon={<TrashIcon />}>删除</Button>

// 用 Card 复合组件
<Card variant="elevated">
  <Card.Header><h3>标题</h3></Card.Header>
  <Card.Body>内容</Card.Body>
  <Card.Footer>
    <Button variant="ghost">取消</Button>
    <Button variant="primary">确认</Button>
  </Card.Footer>
</Card>

// 业务组件 = primitives 组合
function PromptCard({ prompt, onDelete }) {
  return (
    <Card>
      <Card.Header>
        <h4>{prompt.title}</h4>
        <Tag>{prompt.tag}</Tag>
      </Card.Header>
      <Card.Body>{prompt.content}</Card.Body>
      <Card.Footer>
        <Button variant="ghost" size="sm" icon={<EditIcon />}>编辑</Button>
        <Button variant="danger" size="sm" icon={<TrashIcon />} onClick={onDelete}>删除</Button>
      </Card.Footer>
    </Card>
  )
}
```

#### ❌ 错误做法

```tsx
// ❌ 不要为相似按钮建新组件
function NewPromptButton() { return <Button variant="primary">新建</Button> }

// ❌ 不要硬编码颜色
<button style={{ background: '#c96442' }}>按钮</button>

// ❌ 不要在 .module.css 里写死颜色
.button { background: #c96442; color: #faf9f5; }  // 改成 var(--accent) / var(--accent-on)

// ❌ 不要跨层直接 import（domain 组件不应该直接 import 另一个 domain 组件）
//   如果需要组合，应该提到 page 层

// ❌ 不要在 primitives 组件里写业务逻辑
function Button({ prompt, onDelete }) { /* ❌ prompt 是业务数据 */ }
```

### 4.3 扩展 vs 新建 判断流程

当你想新建组件时，先问自己：

1. **能否用现有组件 + variant 组合实现？** → 用现有组件
2. **能否扩展现有组件的 variant / size / prop？** → 扩展
3. **是否需要完全不同的结构？** → 新建

新增组件时同步更新本文件（AGENTS.md）的组件清单。

### 4.4 组件清单

**primitives（原子）**：

| 组件 | 关键 Props | 用途 |
|------|-----------|------|
| Button | variant, size, icon, loading | 所有按钮场景 |
| Card | variant, padding; 子: Header/Body/Footer | 卡片容器 |
| Input | type, icon, mono, size | 文本输入 |
| Tag | color, onClose | 标签 chip |
| Pill | tone, dot | 状态标签 |
| StatusDot | status, size | 设备在线状态点 |
| StatusMessage | tone, live?, action? | 异步反馈 live region；`success/info/warn`→`role=status`，`danger`→`role=alert`；阻断失败恰好一次 alert |
| ProgressBar | value, tone, size | 进度条 |
| Dialog | open, titleId, onClose, closeOnEscape?, closeOnBackdrop?, initialFocusRef?, className? | portal 模态；surface `role=dialog aria-modal` + **默认 `padding: var(--space-5)`**；共享 `useModalLayer`（focus trap / Escape / 背景 inert 引用计数 / body scroll lock / 关闭恢复触发焦点）；禁止业务页自建 focus trap。嵌套 Card 或自管分区 padding 时，`className` 必须显式 `padding: 0` 覆盖，避免双边距 |
| Drawer | Dialog props + side?: left\|right | 侧滑模态抽屉；surface **默认 `padding: var(--space-5)`**；header/body 自管分区或全宽分隔线时 `className` 须 `padding: 0`；复用同一层栈合同 |

> Frontend foundation 合同：`npm run check:css-tokens` / `check:i18n` / `check:bundle`；巨型页 controller/view 所有权见 `web/CLAUDE.md`；E2E 冒烟 `npm run test:e2e -- frontend-foundation.spec.ts`（Dialog 焦点、mobile Drawer Escape、Attention 单 tab stop、终端 arrow、路由崩溃恢复、reduced-motion）。手动 VoiceOver/NVDA 覆盖同类路径。禁止 Redux/Zustand/CSS framework/第三方 modal 库。

**layout（布局）**：

| 组件 | 关键 Props | 用途 |
|------|-----------|------|
| AppShell | children | 整个应用外壳 |
| Window | width, height | 模拟 macOS 窗口 |
| TitleBar | title, children, onClose | 顶部标题栏 |
| Sidebar | children, footer | 侧边栏 |
| NavItem | icon, label, to, badge | 路由导航项 |
| ThemeToggle | - | 主题切换按钮 |
| WorkbenchWorkspaceNav | ariaLabel, actionsAriaLabel, tabs, actions | Workbench 终端/文件预览/自动化共享导航栏 |
| RouteErrorBoundary | resetKey, onRetry?, children | 路由级错误隔离：保留 AppShell/providers，pathname 作 resetKey，生产不展示 stack |

**domain（业务）**：

| 组件 | 关键 Props | 用途 |
|------|-----------|------|
| PromptCard | prompt, onEdit, onDelete, onCopy | Prompt 卡片 |
| TransferItem | task, onCancel?, onPause?, onResume?, onRetry?, onOpen?, cancelling? | 传输项；每个动作仅在对应回调存在时渲染（当前页面只传 pending/transferring 的 onCancel） |
| PermissionCard | icon, title, description, granted, actionLabel?, requesting?, onRequestAccess | 权限卡片；父层按四态决定动作与文案，requesting 时按钮 busy |
| GithubRepoCard | repo, language, onOpen | GitHub 周热门项目卡片 |
| MobileAccessCard | compact, className | 局域网移动端 Workbench 访问链接与二维码卡片 |
| LanFirewallDependencyCard | className | Settings 依赖环境页展示局域网互联防火墙端口/IP、开放状态与系统打开方法 |
| AgentAssetRow | asset, onSelect, onOpenBlocks, onOpenConflicts, onToggleTarget | Agent Hub 指令/资产行（Claude/Codex/OpenCode target cells；legacy matrix 兼容） |
| WorkbenchProjectRail | - | 侧栏设置项下方的项目文件夹入口 |
| WorkbenchRemoteProjectPicker | onProjectOpened, onCancel, openProject | Workbench 局域网远端项目目录选择器 |
| WorkbenchDependencyCard | compact, className | Workbench tmux 依赖状态与安装引导卡片 |
| WorkbenchCodeEditor | value, language, readOnly, onChange | Workbench 代码/源码文件的 CodeMirror 编辑器 |
| WorkbenchMarkdownEditor | value, mode, onModeChange, onChange | Workbench Markdown WYSIWYG/source/split 编辑器 |
| LanDisclosureGate | children | App 级 LAN 风险披露守卫：未确认前挡住 Routes 与权限 onboarding；状态机 loading/required/starting/error/pass；展示地址候选/首选 TCP 62116/mDNS 5353/无身份校验风险；确认后 ensure sidecar |
| WorkbenchHtmlPreview | value, mode, readOnly, onModeChange, onChange | Workbench HTML 源码/渲染预览/split 编辑器 |
| WorkbenchImagePreview | preview, name | Workbench 图片只读预览 |
| WorkbenchCsvPreview | preview | Workbench CSV 只读表格预览 |
| WorkbenchSqlitePreview | preview, onSelectTable | Workbench SQLite 只读表/数据预览 |
| WorkbenchFileWorkspace | tabs, activeTabId, callbacks | Workbench 文件 tab 工作区容器 |
| WorkbenchSessionSearch | open, onClose, projectId, worktreeId, offline, onResumed | Workbench 终端 Claude session 搜索 Command Palette（⌘K） |
| WorkbenchBrowserWorkspace | surface, transport, project, worktree, onReturnToTerminal | Workbench 桌面/移动端共享浏览器预览工作区，按 surface 选择 iframe proxy URL |
| WorkbenchBrowserVerificationPanel | previewId, transport | 一键验证当前 live preview：默认 smoke（snapshot/console/screenshot），不提供脚本/selector 输入；复用 Browser Workspace 导航与 tokens |

## 5. 开发规范

### 5.1 文件组织

```
components/
└── Button/
    ├── Button.tsx           # 组件实现
    ├── Button.module.css    # 样式（必须用 var(--xxx)）
    └── index.ts             # export { Button } from './Button'
```

每个组件文件夹结构一致。`index.ts` 只做 re-export。

### 5.2 TypeScript

- 严格模式开启
- 所有 Props 必须有 interface 类型定义
- 组件函数必须声明 `export function ComponentName(props: ComponentNameProps): JSX.Element`
- 不使用 `any`；用 `unknown` + type guard
- 回调函数用 `() => void` 而非 `Function`
- 可选 prop 加 `?`

### 5.3 注释规范

每个函数（组件 / 工具函数）必须添加中文 docstring：

```tsx
/**
 * Business Logic（为什么需要）:
 *   描述用户需求 / 场景
 *
 * Code Logic（做什么）:
 *   技术目的 / 算法概述 / 输入输出
 */
export function ComponentName() { ... }
```

### 5.4 样式规范

- ✅ **必须**：所有颜色/字体/间距/圆角/阴影用 `var(--xxx)`
- ✅ **必须**：交互组件（hover/active/focus）加 `transition: all var(--motion-fast) var(--ease-standard)`
- ✅ **必须**：CSS Modules 文件名 `Component.module.css`
- ❌ **禁止**：行内 style 写颜色值（结构性的 margin/padding 允许）
- ❌ **禁止**：选择器跨组件影响（如 `.card .button`）
- ❌ **禁止**：使用 `!important`

### 5.5 Icon 规范

所有 icon 集中在 `src/lib/icons.tsx`，统一：
- `viewBox="0 0 16 16"`
- `fill="none"` + `stroke="currentColor"`
- `strokeWidth={1.6}` + `strokeLinecap="round"` + `strokeLinejoin="round"`
- 默认 size 16，接收 `size` prop 覆盖

**新增 icon**：在 `lib/icons.tsx` 末尾添加函数，遵循同样的规范。

### 5.6 状态管理

- 局部状态：`useState` / `useReducer`
- 跨组件共享：Context（`useTheme` 已实现）
- 服务端数据：自定义 hook（如 `usePrompts()`）+ `useEffect`
- 不引入 Redux / Zustand

### 5.7 API 调用

`web/src/api/` 按业务模块拆分。**桌面端**统一经 `client.ts` 的 `invoke()`（Tauri IPC），**禁止**组件直接 `fetch` 本机后端；**`/mobile` 浏览器**走 `workbenchHttp.ts` / `attentionHttp.ts` 等同源 HTTP helper。事件用 `@tauri-apps/api/event` 的 `listen`（替代旧 SSE）。命令名与 DTO 细节见 `web/CLAUDE.md` 与 `src-tauri/CLAUDE.md`。

### 5.8 React Hooks 顺序（必读）

所有 hooks（`useState` / `useCallback` / `useMemo` / `useEffect` / 自定义 hooks / Workbench controllers）**必须放在所有 early return（loading/error/空态守卫）之前**。条件分支或 `return` 之后再调 hooks 会破坏调用顺序，运行时 crash（React error #310）。Workbench 页面的 7 个 controller 也必须在 `Workbench.tsx` early return 前无条件调用。
禁止新增名为 `useWorkbenchController` 的页面级聚合 hook；`Workbench.tsx` 行数硬顶 **1200**。Settings/Orchestrator/MobileAutomation 拆分后 views 不得 import `@/api/*`，controllers 不得承载 board/modal JSX。

## 6. 工作流

### 6.1 开发新页面

1. 在 `pages/` 下新建文件夹 `<PageName>/`
2. 创建 `<PageName>.tsx` + `<PageName>.module.css` + `index.ts`
3. 在 `App.tsx` 添加路由（同时在 `AppShell` children 路由下）
4. 用现有 `primitives` + `layout` + `domain` 组件组合
5. 复用 `useTheme` 等 hooks
6. **不要** 直接 fetch API — 通过 `src/api/` 封装

### 6.2 开发新组件

1. 判断分层：primitive / layout / domain？
2. 选对目录：`components/<layer>/<ComponentName>/`
3. 写 .tsx + .module.css + index.ts
4. TypeScript interface 写完整
5. 颜色/间距用 `var(--xxx)`
6. **更新本文档的组件清单**

### 6.3 修改样式

1. 先在 `tokens.css` 查找是否已有 token
2. 没有则新增 token（同时给两套主题）
3. 在组件中用 `var(--xxx)` 引用

### 6.4 添加新 icon

1. 在 `lib/icons.tsx` 末尾追加新函数
2. 遵循 viewBox/stroke 规范
3. 命名用 `XxxIcon`（如 `TrashIcon`）

## 7. 验证与调试

### 7.1 主验证入口（贡献者常用）

```bash
# 桌面开发
./start.sh                         # 或 ./start.sh dev；web 仅前端：./start.sh web

# 前端质量（锁定依赖，禁止 npx --yes 浮动 runner）
cd web && npm ci && npm run lint && npm run build && npm test && npm run test:e2e

# Rust 质量
cd src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked

# 路由清单 / 质量矩阵 / 文档事实 / 后端 CLI 本地 smoke（按需）
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
node scripts/check-docs.mjs --self-test
cd src-tauri && cargo test --locked --test backend_cli_smoke -- --nocapture --test-threads=1
cd src-tauri && cargo test --locked --test backend_doctor_smoke -- --nocapture --test-threads=1
```

领域测试、Vitest/jsdom 策略、Attention/Workbench 回归命令见 `web/CLAUDE.md`；P2P/protocol、doctor/logs、macOS/Windows smoke 范围与 NOT VERIFIED 见 `src-tauri/CLAUDE.md`。人类向质量门禁与 L0–L3 分层见 [`docs/development/testing.md`](docs/development/testing.md)；机器可读 evidence 表见 [`docs/development/quality-matrix.json`](docs/development/quality-matrix.json)（稳定 `E2E-`/`L2-`/`L3-` ID，L3 未执行保持 `NOT VERIFIED`）；后端生命周期 / 端口 / doctor 见 [`docs/development/backend-operations.md`](docs/development/backend-operations.md)。

### 7.2 启动与设计系统预览

```bash
./start.sh                         # 推荐：自检工具链 + tauri dev
# macOS 固定组装 ~/Applications/cc-partner (Dev).app；检测到固定 identity 时固定签名，
# 否则 ad-hoc 签名且输入监控可手动添加授权，详见 macOS 签名文档
# 或：cd web && npm install && ./node_modules/.bin/tauri dev  # 裸 binary，无开发壳
```

本地前端 ↔ Rust 走 `invoke()` IPC（无前端本地 API 端口、无 `/api` proxy）。开发模式设计系统：`http://localhost:1420/design-system`（Tauri dev 默认 1420；生产不可访问）。

### 7.3 类型检查与生产构建

```bash
cd web && npm run build            # tsc -b && vite build（类型检查入口，勿用浮动 npx tsc）
./web/node_modules/.bin/tauri build  # 本平台产物：src-tauri/target/release/bundle/
```

### 7.4 发版与签名（摘要）

```bash
node scripts/bump-version.mjs <新版本号>   # 同步 tauri.conf.json + Cargo.toml/lock + web package.json/lock
git tag v<版本号> && git push origin v<版本号>  # 公开发布 macOS arm64 / Windows / Linux
```

- **版本号单一来源**：`src-tauri/tauri.conf.json` 的 `version`
- **公开 Release 机制**：`release-tauri.yml` 三段式原生 `tauri build`，发布 macOS arm64 ad-hoc DMG/updater/CLI、Windows 与 Linux；**不是** `tauri-apps/tauri-action`
- **macOS 固定签名构建**：与统一正式版共用 `com.cc-partner.app`，本地运行 `scripts/build-macos-internal.sh`，CI 手动触发 `internal-macos.yml`；文件名与 Environment 名保留为历史基础设施标识，不代表独立产品版本；详见 [`docs/development/macos-internal-signing.md`](docs/development/macos-internal-signing.md)
- **跨目录关键陷阱**：repo secret `TAURI_SIGNING_PRIVATE_KEY` 缺失则无 `.sig` / `latest.json` 不完整，应用内更新失败；`plugins.updater.pubkey` 必须与私钥配对；`bundle.createUpdaterArtifacts: true` 必须开启
- **实现细节、矩阵平台、sidecar、历史弃用原因**：`src-tauri/CLAUDE.md`「M9」节

### 7.5 端口与防火墙（跨目录摘要）

- P2P HTTP **首选 TCP 62116**；被占用则 **+1 递增**；`config.http_port=0`/非法表示“用首选默认”，**不是** OS `port=0` 临时端口
- 实际监听端口以 UI 或 `GET /api/health` 的 `http_port` 为准；mDNS 为 UDP **5353**
- 防火墙示例与 doctor 探测见 `src-tauri/CLAUDE.md` / README / [`docs/development/backend-operations.md`](docs/development/backend-operations.md)，文档不得宣称自动改防火墙

## 8. 与 Rust 后端协作

### 8.1 通信通道

- **本地前端 ↔ Rust**：Tauri `invoke('<command>')` IPC（`#[tauri::command]`）。前端 `web/src/api/` 底层走 `@tauri-apps/api/core` 的 `invoke`，组件层无感知。**无本地 HTTP API 端口给桌面前端。**
- **跨设备 / mobile P2P**：axum HTTP（首选 **62116**，占用则 +1）+ reqwest peer client + mDNS。`/mobile` SPA 与 P2P 共享实际 HTTP 端口。前端桌面页不直接打 P2P base URL。
- 协议 v1 health（`protocol_version` + capabilities）、错误信封、request id、幂等清单、local-only runtime-snapshot：**见 `src-tauri/CLAUDE.md`**。
- 前端 API 模块、Attention、Workbench controllers：**见 `web/CLAUDE.md`**。

### 8.2 命令与路由目录（下沉，勿在根复制长表）

| 类型 | 权威位置 |
|------|----------|
| Tauri `#[tauri::command]` 注册与领域语义 | `src-tauri/src/commands/*` + `src-tauri/CLAUDE.md` 各领域节 |
| 前端 invoke / HTTP 封装 | `web/src/api/*` + `web/CLAUDE.md` |
| P2P `/api/*` 路由、retry class、幂等键 | `docs/p2p-protocol.md` + `node scripts/check-p2p-route-inventory.mjs` + `src-tauri/CLAUDE.md`「P2P 协议…」 |
| Health 能力 token | `attention.v1` · `errors.envelope.v1` · `orchestrator.runtime-snapshot.v1`（`server_protocol_info()`） |

### 8.3 添加新能力

1. **Rust**：`src-tauri/src/commands/<module>.rs` 加 `#[tauri::command]`，`lib.rs` `invoke_handler!` 注册；P2P 则 `net/routes/` 加路由并按 `src-tauri/CLAUDE.md` 的 7 步清单更新 `docs/p2p-protocol.md` + 能力 token
2. **前端**：`web/src/api/<module>.ts` 加 `invoke` 或 mobile HTTP 封装
3. **类型**：Rust DTO `#[serde(rename_all="camelCase")]`（P2P 部分路由仍 snake_case，见后端约定）对齐 `web/src/lib/types/`（兼容 barrel `web/src/lib/types.ts`）

### 8.4 事件订阅（Tauri emit/listen）

Rust `app_handle.emit("<event>", payload)`，前端 `listen("<event>", cb)`（须先 `canListenToTauriEvents()`，纯浏览器跳过）。常用：`transfer:progress|completed|failed|cancelled`、`workbench:terminal-output|terminal-status|merge-progress`、`update:download-progress`、`health:reminder`、`screenshot:permission-needed`。

## 9. 后续开发注意事项

1. **不要硬编码颜色/字体/间距** — 一律用 `var(--xxx)`
2. **不要在 primitives 写业务逻辑** — 业务在 domain
3. **不要在 domain 跨组件 import** — 提到 page 层
4. **不要修改 uiux/ 目录** — 它是设计稿参考
5. **新组件必须更新 AGENTS.md 组件清单**
6. **新增 icon 必须在 `lib/icons.tsx` 集中管理**
7. **TypeScript 必须 strict 通过** — `cd web && npm run build`（含 `tsc -b`）
8. **优先扩展已有组件，谨慎新建**
9. **设计 token 新增必须同时给浅色/深色两套值**
10. **前端调后端一律走 `web/src/api/` 封装** — 桌面 `invoke`，mobile 同源 HTTP；Rust 命令在 `src-tauri/src/commands/` 注册
11. **Hooks 必须在 early return 之前** — 见 §5.8
12. **不要把 P2P 首选端口写成 `port=0` 动态分配** — 首选 62116 + 占用递增
13. **不要推荐 `npx --yes` / 单文件 `npx tsx` runner** — 用 `package.json` 锁定 scripts
14. **Release 不要写成 tauri-action** — 三段式原生 tauri CLI，细节在 `src-tauri/CLAUDE.md`

## 10. 关键文件索引

| 文件 | 作用 | 修改频率 |
|------|------|---------|
| `AGENTS.md` | 根层开发指南（本文件） | 中 |
| `web/CLAUDE.md` | 前端分层指令 | 高 |
| `src-tauri/CLAUDE.md` | 后端分层指令 | 高 |
| `web/src/styles/tokens.css` | 设计 token 总入口 | 中（新增 token） |
| `web/src/lib/icons.tsx` | Icon 库 | 低（新增 icon） |
| `web/src/App.tsx` | 路由根 | 低（新增页面） |
| `web/src/components/primitives/*` | 原子组件 | 中（扩展 variant） |
| `web/src/components/layout/*` | 布局组件 | 低 |
| `web/src/components/domain/*` | 业务组件 | 中（业务迭代） |
| `web/src/pages/*` | 页面 | 高 |
| `web/src/pages/Workbench/*` | 工作台页面 + controllers | 高 |
| `web/src/pages/AgentHub/shell/*` · `instructions/*` · `crossAgent/*` · `context/*` | Agent Hub 交互重设计：壳层 / 三栏提示词 / 跨 Agent 适配 / URL context | 高 |
| `src-tauri/src/lib.rs` | Tauri 入口 + 命令注册 + setup 装配 | 中（新增命令时改） |
| `src-tauri/src/commands/*` | Rust invoke 命令层 | 中（后端迭代） |
| `src-tauri/src/workbench/*` | 工作台领域逻辑 | 高 |
| `src-tauri/src/net/*` | P2P HTTP / mDNS / protocol | 高 |
| `src-tauri/tauri.conf.json` | Tauri 配置 + bundle + updater（版本号单一来源） | 低（发版改） |
| `docs/p2p-protocol.md` | P2P 路由权威清单 | 中 |
| `docs/development/macos-internal-signing.md` | macOS 固定/ad-hoc 签名与输入监控 L3 手册 | 低 |

---

**在你修改任何代码前，请确保已读懂本文档第 4 节「组件分层与复用规范」，并按 §2.1 进入对应分层 CLAUDE。**
