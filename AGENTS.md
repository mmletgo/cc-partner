# Claude Partner — 项目开发指南

> 面向 AI Agent / 新加入开发者的项目说明。修改代码前请先通读本文档。

## 1. 项目概述

**Claude Partner** 是一款跨平台局域网协作桌面应用（macOS / Windows / Ubuntu），核心功能：

- **局域网文件传输** — 任意大小分块传输，支持断点续传
- **区域截图** — 框选截图保存到剪贴板，可直接粘贴到 Claude Code
- **Prompt 管理** — 记录 / 复制 / 打标签 / 跨设备同步
- **P2P 自动互联** — 局域网内 mDNS 自动发现
- **自动更新** — GitHub Releases 检测 / 下载 / 安装

**技术栈**：
- **后端**: Python 3.11+ · PyQt6 · aiohttp · aiosqlite · zeroconf
- **前端**: React 18 · TypeScript · Vite 5 · React Router v6 · CSS Modules
- **打包**: PyInstaller（前端构建产物作为 Python 包的资源嵌入）

桌面端架构：PyQt6 窗口内嵌 QWebEngineView，加载前端构建产物。前端通过 `fetch('/api/...')` 调用 aiohttp HTTP 服务；Python 后端无侵入。

## 2. 目录结构

```
claude-partner/
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
│   │   │       └── PermissionCard/
│   │   ├── pages/                # 页面（每个一个文件夹）
│   │   │   ├── Home/             # 01-main.html
│   │   │   ├── Transfer/         # 02-transfer.html
│   │   │   ├── Prompts/          # 03-prompts.html
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
│   ├── vite.config.ts            # 含 /api proxy → aiohttp
│   ├── tsconfig.json
│   └── package.json
├── src/claude_partner/           # Python 后端（保持）
│   ├── app.py
│   ├── ui/
│   │   ├── main_window.py        # QWebEngineView 加载前端
│   │   ├── welcome_window.py
│   │   ├── tray.py
│   │   └── widgets/              # 旧 Qt Widgets（fallback）
│   ├── models/  storage/  network/  sync/  transfer/  screenshot/  updater/  hotkey/
│   └── ...
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

**domain（业务）**：

| 组件 | 关键 Props | 用途 |
|------|-----------|------|
| PromptCard | prompt, onEdit, onDelete, onCopy | Prompt 卡片 |
| DeviceCard | device, onClick | 设备卡片 |
| TransferItem | task, onPause, onCancel, onRetry | 传输项 |
| PermissionCard | icon, title, description, granted | 权限卡片 |

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
npm install              # 首次
npm run dev              # http://localhost:5173
```

Vite dev server 启动后会代理 `/api` 请求到 aiohttp 端口（默认 8000，可通过 `PY_PORT=9000 npm run dev` 覆盖）。

### 7.2 访问设计系统预览

开发模式下访问 `http://localhost:5173/design-system` 查看所有组件。生产构建后该路由不可访问。

### 7.3 类型检查

```bash
cd web
npx tsc --noEmit
```

### 7.4 生产构建

```bash
cd web
npm run build            # 产物在 web/dist/
```

构建产物会被 PyInstaller 打包进 Python 包（`web/dist` → `claude_partner/web_dist/`）。

### 7.5 Mock API 测试服务

后端 REST 端点开发或前端联调时，可用独立 mock server 验证：

```bash
# 启动 mock API（端口 8765）
python3 scripts/mock_api_server.py
# 在同一终端或另一个终端启动 Vite（指向 mock）
cd web && PY_PORT=8765 npm run dev
# 前端 npm run build 时通过 vite proxy 自动转发 /api 到 8765
```

mock 数据：10 prompts / 3 devices / 5 transfers（覆盖 transferring/completed/failed 状态）。

### 7.6 Playwright 截图验证

```bash
# 确保 Vite dev server 在 5173 运行
python3 scripts/verify_frontend.py
# → 输出 docs/frontend/screenshots/*.png（7 个页面）
# → 统计 console 错误/警告，exit 0=渲染成功
```

需要 `@playwright/test` dev dep（已安装）和 `playwright` Python 包：
```bash
pip install playwright
npx playwright install chromium
```

### 7.7 REST 端点集成测试

```bash
python3 scripts/test_rest_endpoints.py
# → 创建临时 SQLite + 完整 APIProtocol
# → 16 个端点全部验证（CRUD/设备/传输/P2P）
# → exit 0=全部通过
```

不需要外部依赖（使用 aiohttp ClientSession 异步测试）。

## 8. 与 Python 后端协作

### 8.1 API 端点

**前端 REST（由 protocol.py 注册）**：
| 端点 | 方法 | 说明 |
|------|------|------|
| /api/health | GET | 健康检查，返回 {ok, device_id, device_name} |
| /api/prompts | GET | Prompt 列表（支持 ?search=&tag= 过滤） |
| /api/prompts | POST | 新建 Prompt {title, content, tag} |
| /api/prompts/{id} | GET | 单条 Prompt（deleted 软删除返回 404） |
| /api/prompts/{id} | PUT | 编辑 Prompt（自增 vector_clock） |
| /api/prompts/{id} | DELETE | 软删除（自增 vector_clock 后标记 deleted） |
| /api/devices | GET | 在线设备列表（从 DeviceDiscovery 回调） |
| /api/sync | POST | 触发全网同步 |
| /api/transfer/tasks | GET | 全部传输任务列表（合并 sender + receiver） |
| /api/transfer/send | POST | 启动文件发送 {deviceId, filePath} |
| /api/transfer/tasks/{id} | DELETE | 取消传输 |

**P2P 协议（对端调用）**：
| 端点 | 方法 | 说明 |
|------|------|------|
| /api/sync/pull | POST | 接收对端摘要，返回对端需要的 prompt |
| /api/sync/push | POST | 接收对端推送的 prompt |
| /api/transfer/init | POST | 初始化文件接收 |
| /api/transfer/chunk/{id} | POST | 接收文件分块 |
| /api/transfer/status/{id} | GET | 查询接收端传输状态 |

完整路由见 `src/claude_partner/network/protocol.py` 的 `setup_routes()`。
集成测试见 `scripts/test_rest_endpoints.py`（16 个端点）。

### 8.2 添加新 API 端点

1. **Python 端**: 在 `src/claude_partner/network/` 下新增 handler
2. **前端**: 在 `web/src/api/<module>.ts` 添加对应函数
3. 类型定义同步更新（前端 `types/`、后端 `models/`）

### 8.3 SSE 订阅

```tsx
useEffect(() => {
  const es = new EventSource('/api/events/stream');
  es.addEventListener('device-online', (e) => {
    const device = JSON.parse(e.data);
    // ...
  });
  return () => es.close();
}, []);
```

## 9. 后续开发注意事项

1. **不要硬编码颜色/字体/间距** — 一律用 `var(--xxx)`
2. **不要在 primitives 写业务逻辑** — 业务在 domain
3. **不要在 domain 跨组件 import** — 提到 page 层
4. **不要修改 uiux/ 目录** — 它是设计稿参考
5. **不要删除旧 PyQt6 widgets** — 作为 fallback
6. **新组件必须更新 AGENTS.md 组件清单**
7. **新增 icon 必须在 `lib/icons.tsx` 集中管理**
8. **TypeScript 必须 strict 通过** — `npx tsc --noEmit`
9. **优先扩展已有组件，谨慎新建**
10. **设计 token 新增必须同时给浅色/深色两套值**

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
| `src/claude_partner/ui/main_window.py` | PyQt6 主窗口 | 低（仅集成） |
| `claude_partner.spec` | PyInstaller 打包配置 | 低 |

---

**📌 在你修改任何代码前，请确保已读懂本文档第 4 节「组件分层与复用规范」。**
