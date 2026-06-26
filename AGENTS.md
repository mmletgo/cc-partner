# cc-partner — 项目开发指南

> 面向 AI Agent / 新加入开发者的项目说明。修改代码前请先通读本文档。

## 1. 项目概述

**cc-partner** 是一款跨平台局域网协作桌面应用（macOS / Windows / Ubuntu），核心功能：

- **局域网文件传输** — 任意大小分块传输，支持断点续传
- **区域截图** — 框选截图保存到剪贴板，可直接粘贴到 Claude Code
- **Prompt 管理** — 记录 / 复制 / 打标签 / 跨设备同步
- **Prompt 优化** — 调用本机 Claude Code CLI pure/headless 模式生成中英文优化版 Prompt
- **速记本** — 多页面自动保存文本，支持页面标题、局域网与 GitHub 同步
- **工作台** — 指定项目文件夹，管理 Git worktree、多个本机项目终端、可交互工作区文件树、文件浏览/编辑工作区和 Git 提交树
- **P2P 自动互联** — 局域网内 mDNS 自动发现
- **自动更新** — GitHub Releases 检测 / 下载 / 安装

**技术栈**：
- **桌面宿主**: Tauri 2（Rust 主进程）
- **后端**: Rust · axum HTTP server · reqwest peer client · mdns-sd 发现 · sqlx (SQLite) · xcap 抓屏 · arboard 剪贴板 · tracing 日志
- **前端**: React 19 · TypeScript · Vite · React Router v6 · CSS Modules
- **打包/更新**: Tauri CLI · tauri-plugin-updater · tauri-plugin-global-shortcut · tauri-plugin-process · tauri-plugin-dialog

桌面端架构：Tauri 2 主进程用 Rust 实现全部后端能力，前端复用 `web/` 的 React。本地前端通过 `@tauri-apps/api` 的 `invoke()` 调用 Rust `#[tauri::command]`（无本地端口暴露）；跨设备 P2P 走 axum HTTP server（动态端口）+ reqwest 客户端 + mdns-sd 发现。两条通道共享同一份 `AppState`。

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
│   │   │   │   └── ProgressBar/
│   │   │   ├── layout/           # 布局组件
│   │   │   │   ├── AppShell/     # 完整应用外壳（TitleBar + Sidebar + main）
│   │   │   │   ├── Window/       # 模拟 macOS 窗口
│   │   │   │   ├── TitleBar/     # traffic lights + 拖拽区
│   │   │   │   ├── Sidebar/
│   │   │   │   ├── NavItem/      # 路由导航
│   │   │   │   └── ThemeToggle/  # 浅/深色切换
│   │   │   └── domain/           # 业务组件（组合 primitives + layout）
│   │   │       ├── PromptCard/
│   │   │       ├── DeviceCard/
│   │   │       ├── TransferItem/
│   │   │       ├── GithubRepoCard/
│   │   │       ├── WorkbenchProjectRail/
│   │   │       └── PermissionCard/
│   │   ├── pages/                # 页面（每个一个文件夹）
│   │   │   ├── Home/             # 01-main.html
│   │   │   ├── Transfer/         # 02-transfer.html
│   │   │   ├── Prompts/          # 03-prompts.html
│   │   │   ├── PromptOptimizer/  # Prompt 优化（本机 Claude CLI pure/headless）
│   │   │   ├── Workbench/        # 项目文件夹 + 多项目终端 + 文件树/文件工作区 + Git 提交树
│   │   │   ├── Devices/          # 04-devices.html
│   │   │   ├── Settings/         # 05-settings.html
│   │   │   ├── Welcome/          # 06-welcome.html
│   │   │   └── DesignSystem/     # 🆕 设计系统预览（仅 dev）
│   │   ├── api/                  # HTTP 客户端（fetch 封装）
│   │   ├── hooks/                # 自定义 hooks（useTheme 等）
│   │   ├── lib/                  # 通用工具 + icon 库
│   │   └── assets/
│   ├── public/
│   ├── index.html
│   ├── vite.config.ts            # Tauri dev 时由 tauri 自动接管，无 /api proxy
│   ├── tsconfig.json
│   └── package.json
├── src-tauri/                    # Tauri 2 Rust 后端（见 src-tauri/CLAUDE.md）
│   ├── src/                      # lib.rs(入口) config/state/error/commands/models/storage/sync/net/transfer/screenshot/workbench/permissions/hotkey/tray
│   ├── migrations/               # SQL schema 文档
│   ├── capabilities/             # Tauri 权限清单（default.json）
│   ├── icons/                    # 应用图标
│   ├── tauri.conf.json           # Tauri 配置 + bundle + updater（版本号单一来源）
│   └── Cargo.toml
├── scripts/                      # 发版脚本 + 应用图标源（app 图标透明外圈；tray 图标为 macOS template）
├── uiux/                         # 设计稿（参考资源，不参与构建）
├── docs/
│   ├── prd.md
│   └── superpowers/specs/        # 设计文档
├── AGENTS.md                     # 本文件
└── web/dist/                     # Vite 构建产物（git ignored）
```

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
| **primitives** | 单一 UI 元素，无业务语义，无数据依赖 | Button, Card, Input, Tag, Pill, StatusDot, ProgressBar |
| **layout** | 页面结构骨架，无业务数据 | AppShell, Window, TitleBar, Sidebar, NavItem, ThemeToggle |
| **domain** | 组合 primitives + layout，承担具体业务对象的展示/交互 | PromptCard, DeviceCard, TransferItem, PermissionCard |
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
| ProgressBar | value, tone, size | 进度条 |

**layout（布局）**：

| 组件 | 关键 Props | 用途 |
|------|-----------|------|
| AppShell | children | 整个应用外壳 |
| Window | width, height | 模拟 macOS 窗口 |
| TitleBar | title, children, onClose | 顶部标题栏 |
| Sidebar | children, footer | 侧边栏 |
| NavItem | icon, label, to, badge | 路由导航项 |
| ThemeToggle | - | 主题切换按钮 |
| WorkbenchWorkspaceNav | ariaLabel, actionsAriaLabel, tabs, actions | Workbench 终端/文件预览共享导航栏 |

**domain（业务）**：

| 组件 | 关键 Props | 用途 |
|------|-----------|------|
| PromptCard | prompt, onEdit, onDelete, onCopy | Prompt 卡片 |
| DeviceCard | device, onClick | 设备卡片 |
| TransferItem | task, onPause, onCancel, onRetry | 传输项 |
| PermissionCard | icon, title, description, granted | 权限卡片 |
| GithubRepoCard | repo, language, onOpen | GitHub 周热门项目卡片 |
| ClaudeAssetRow | asset, onToggle, onRemove, onSelect | Claude Code 资产行 |
| RemoteAssetPicker | assets, selectedKeys, kind, search, onSelectMany | 局域网远端资产选择器 |
| WorkbenchProjectRail | - | 侧栏设置项下方的项目文件夹入口 |
| WorkbenchDependencyCard | compact, className | Workbench tmux 依赖状态与安装引导卡片 |
| WorkbenchCodeEditor | value, language, readOnly, onChange | Workbench 代码/源码文件的 CodeMirror 编辑器 |
| WorkbenchMarkdownEditor | value, mode, onModeChange, onChange | Workbench Markdown WYSIWYG/source/split 编辑器 |
| WorkbenchHtmlPreview | value, mode, readOnly, onModeChange, onChange | Workbench HTML 源码/渲染预览/split 编辑器 |
| WorkbenchImagePreview | preview, name | Workbench 图片只读预览 |
| WorkbenchCsvPreview | preview | Workbench CSV 只读表格预览 |
| WorkbenchSqlitePreview | preview, onSelectTable | Workbench SQLite 只读表/数据预览 |
| WorkbenchFileWorkspace | tabs, activeTabId, callbacks | Workbench 文件 tab 工作区容器 |

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

`src/api/` 下按业务模块拆分（`prompts.ts` / `devices.ts` / `transfer.ts`），统一通过 `client.ts` 包装的 `fetch`。SSE 订阅封装在 `events.ts`。

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

### 7.1 启动开发模式

```bash
cd web
npm install                          # 首次
./node_modules/.bin/tauri dev        # 同时起 Vite dev server + Rust 主进程，热重载
```

本地前端与 Rust 后端通过 Tauri `invoke()` IPC 通信，无 `/api` proxy、无本地端口暴露。

### 7.2 访问设计系统预览

开发模式下访问 `http://localhost:1420/design-system` 查看所有组件（Tauri dev 默认占用 1420 端口）。生产构建后该路由不可访问。

### 7.3 类型检查

```bash
cd web
npx tsc --noEmit
```

### 7.4 生产构建

```bash
cd web
./node_modules/.bin/tauri build      # 产物在 src-tauri/target/release/bundle/
```

Tauri 自动打包三平台本平台产物（macOS→dmg/app、Windows→nsis/msi、Linux→AppImage/deb），前端构建产物嵌入应用。

### 7.5 发版版本同步

```bash
node scripts/bump-version.mjs <新版本号>
```

发版必须通过该脚本统一同步 `src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`、`src-tauri/Cargo.lock`、`web/package.json`、`web/package-lock.json`；提交后推送 `v<新版本号>` tag 触发 `.github/workflows/release-tauri.yml`。

## 8. 与 Rust 后端协作

### 8.1 通信通道

- **本地前端 ↔ Rust**：Tauri `invoke('<command>')` IPC（`#[tauri::command]`）。前端 `web/src/api/` 底层走 `@tauri-apps/api/core` 的 `invoke`，组件层无感知。
- **跨设备 P2P**：axum HTTP server（动态端口），供对端 reqwest 调用。仅用于设备间通信，前端不直接访问。

### 8.2 前端 invoke 命令（由 `src-tauri/src/commands/` 注册）

| 命令 | 说明 |
|------|------|
| ping | 健康检查 |
| config.get_config / config.get_default_config / config.update_config | 配置读写；恢复默认取后端环境默认值，update_config 支持快捷键热更新 |
| config.get_version | 应用版本号 |
| prompts.list / get / create / update / delete / list_tags | Prompt CRUD（delete 为软删除，自增 vector_clock） |
| optimize_prompt / stream_optimize_prompt_to_workbench_session | 调用本机 Claude Code CLI 优化用户输入；普通页返回中英文 Prompt，Workbench 可按设置语种用 stream-json 把优化结果流式写入当前终端 |
| trigger_sync | 触发全网 Prompt 同步，返回 {accepted, synced, note} |
| get_claude_md / update_claude_md / push_claude_md | CLAUDE.md 读取 / 保存 / 主动推送本机配置到局域网设备和 GitHub 云端 |
| list_scratchpad_pages / get_scratchpad_page / create_scratchpad_page / update_scratchpad_page_content / rename_scratchpad_page / delete_scratchpad_page / sync_scratchpad | 速记本多页面 CRUD / 自动保存 / 同步 |
| get_cloud_sync_config / get_default_cloud_sync_config / update_cloud_sync_config / trigger_cloud_sync_cmd / test_cloud_sync | GitHub 私有仓库云端同步配置 / 恢复默认 / 手动同步 / 连通性测试 |
| list_transfers / send_transfer / cancel_transfer | 文件传输任务管理 |
| check_permissions / request_permission | macOS 权限检查与申请（屏幕录制 / 输入监控） |
| check_update / download_update / get_download_status / cancel_download / install_update | 自动更新 5 命令 |
| start_region_capture / get_region_snapshot / save_clipboard_image / cancel_region_capture | 区域截图 |
| list_github_trending_repos / get_github_trending_config / get_default_github_trending_config / update_github_trending_config / test_claude_cli | GitHub 周热门项目 + Claude CLI 双语解说配置 / 恢复默认 |
| list_workbench_projects / add_workbench_project / remove_workbench_project / touch_workbench_project / list_workbench_worktrees / create_workbench_worktree / commit_workbench_worktree / push_workbench_worktree / merge_workbench_worktree / remove_workbench_worktree / list_workbench_git_commits / list_workbench_sessions / create_workbench_session / write_workbench_session_input / resize_workbench_session / focus_workbench_session / get_focused_workbench_session / split_workbench_pane / close_workbench_pane / close_workbench_session / rename_workbench_session / list_workbench_dir / get_workbench_path_info / open_workbench_file / save_workbench_text_file / format_workbench_structured_content / preview_workbench_sqlite / create_workbench_file / create_workbench_dir / rename_workbench_path / delete_workbench_path | 工作台本机项目、Git worktree、带本地/远端 ref 标识的 Git 提交树、tmux-backed terminal window/pane、工作区文件树和文件浏览/编辑 |
| preview_workbench_html_asset | Workbench HTML sandbox 预览读取当前 active worktree 根内的相对 CSS/图片等资源并返回 data URL；拒绝外链、绝对路径、根外路径和跨根 symlink |

### 8.3 P2P HTTP 端点（对端调用，由 `src-tauri/src/net/routes/` 注册）

| 端点 | 方法 | 说明 |
|------|------|------|
| /api/health | GET | {ok, device_id, device_name, http_port, ts} |
| /api/sync/pull | POST | 接收对端摘要，返回对端需要的 prompt |
| /api/sync/push | POST | 接收对端推送的 prompt |
| /api/scratchpad/sync/pull | POST | 接收对端速记本页面摘要，返回本端需要推送的页面 |
| /api/scratchpad/sync/push | POST | 接收对端推送的速记本页面并逐页合并 |
| /api/transfer/init | POST | 初始化文件接收 |
| /api/transfer/chunk/{id} | POST | 接收文件分块（header `X-Chunk-Offset`） |
| /api/transfer/status/{id} | GET | 查询接收端传输状态 |

### 8.4 添加新能力

1. **Rust 端**: 在 `src-tauri/src/commands/<module>.rs` 加 `#[tauri::command]`，在 `lib.rs` 的 `invoke_handler!` 注册；需要 P2P 则在 `net/routes/` 加 axum 路由
2. **前端**: 在 `web/src/api/<module>.ts` 加对应 `invoke` 封装
3. 类型同步更新（Rust `#[serde(rename_all="camelCase")]` 对齐前端，前端 `lib/types.ts`）

### 8.5 事件订阅（Tauri emit/listen，替代 SSE）

Rust 侧用 `app_handle.emit("<event>", payload)`，前端 `listen("<event>", cb)`：

```tsx
import { listen } from '@tauri-apps/api/event';

useEffect(() => {
  const unlisten = listen('transfer:progress', (e) => {
    const { id, transferredBytes, size, progress } = e.payload;
    // ...
  });
  return () => { unlisten.then(fn => fn()); };
}, []);
```

常用事件：`transfer:progress` / `transfer:completed` / `transfer:failed` / `transfer:cancelled` / `region-capture:result` / `update:download-progress`。

## 9. 后续开发注意事项

1. **不要硬编码颜色/字体/间距** — 一律用 `var(--xxx)`
2. **不要在 primitives 写业务逻辑** — 业务在 domain
3. **不要在 domain 跨组件 import** — 提到 page 层
4. **不要修改 uiux/ 目录** — 它是设计稿参考
5. **新组件必须更新 AGENTS.md 组件清单**
6. **新增 icon 必须在 `lib/icons.tsx` 集中管理**
7. **TypeScript 必须 strict 通过** — `npx tsc --noEmit`
8. **优先扩展已有组件，谨慎新建**
9. **设计 token 新增必须同时给浅色/深色两套值**
10. **前端调后端一律走 `web/src/api/` 的 invoke 封装，不要直接 fetch** — Rust 命令在 `src-tauri/src/commands/` 注册，新命令记得在 `lib.rs` 的 invoke_handler 加入

## 10. 关键文件索引

| 文件 | 作用 | 修改频率 |
|------|------|---------|
| `web/src/styles/tokens.css` | 设计 token 总入口 | 中（新增 token） |
| `web/src/lib/icons.tsx` | Icon 库 | 低（新增 icon） |
| `web/src/App.tsx` | 路由根 | 低（新增页面） |
| `web/src/components/primitives/*` | 原子组件 | 中（扩展 variant） |
| `web/src/components/layout/*` | 布局组件 | 低 |
| `web/src/components/domain/*` | 业务组件 | 中（业务迭代） |
| `web/src/pages/*` | 页面 | 高 |
| `web/src/pages/Workbench/*` | 工作台页面（三栏、worktree 管理、多终端、工作区文件树/文件工作区、Git 提交树） | 高 |
| `src-tauri/src/lib.rs` | Tauri 入口 + 命令注册 + setup 装配 | 中（新增命令时改） |
| `src-tauri/src/commands/*` | Rust invoke 命令层 | 中（后端迭代） |
| `src-tauri/src/workbench/*` | 工作台领域逻辑（项目、Git worktree、PTY/tmux 会话、文件系统） | 高 |
| `src-tauri/tauri.conf.json` | Tauri 配置 + bundle + updater（版本号单一来源） | 低（发版改） |

---

**📌 在你修改任何代码前，请确保已读懂本文档第 4 节「组件分层与复用规范」。**
