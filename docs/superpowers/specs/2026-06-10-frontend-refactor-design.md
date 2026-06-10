# 前端重构设计文档

> 日期: 2026-06-10
> 状态: 已批准，进入实施
> 目标: 将 uiux/ 中的 5505 行静态 HTML 设计稿，完整迁移为 PyQt6 桌面应用内的可维护 Web 前端组件库

## 1. 背景与目标

### 1.1 现状
- **uiux/** 目录: 8 个静态 HTML 设计稿（5505 行），Anthropic 暖米色 + terracotta 强调色设计系统
- **src/claude_partner/ui/**: PyQt6 Widgets 实现，使用 macOS 偏好设置面板蓝调色板（#007AFF）
- **两套设计语言割裂**，HTML 设计稿无法被 Python 后端直接消费

### 1.2 目标
1. 完整还原 uiux/ 中 6 个页面的视觉、布局、间距、颜色、字体、交互状态
2. 抽象出可复用的前端组件库（Button / Card / Input / Window / TitleBar / Sidebar / NavItem / PromptCard / DeviceCard / TransferItem 等约 15 个）
3. 通过 CSS 变量（design tokens）实现浅色/深色模式切换
4. 新增"设计系统预览"页面，仅开发态可见，验证组件复用效果
5. 编写 AGENTS.md 明确组件复用规范
6. 保持 PyQt6 桌面端架构，Python 后端无侵入

## 2. 技术选型

| 维度 | 决策 | 理由 |
|------|------|------|
| 桌面壳 | PyQt6 + QWebEngineView | 复用 PyQt6 已有系统托盘、快捷键、qasync 异步桥 |
| 前端框架 | React 18 + TypeScript | 组件化强，props/variant 扩展机制成熟，团队熟悉 |
| 构建工具 | Vite 5 | 启动快、HMR、产物小（<300KB gzipped） |
| 路由 | React Router v6 | SPA 路由，URL 干净 |
| 样式方案 | CSS Modules + CSS 变量 | 保留 uiux 完整设计 token，无原子化框架干扰 |
| 通信 | fetch + EventSource（SSE） | 复用现有 aiohttp HTTP API，无 QWebChannel 复杂度 |
| 状态管理 | React useState + Context | 6 个页面 + 简单业务，不需要 Redux |

**为什么不用 Tailwind/UnoCSS？** uiux 设计 token 已经在 CSS 变量中定义完整（30+ 变量），原子化 CSS 反而会冲淡 token 系统、增加学习成本。

**为什么不用 QWebChannel？** aiohttp 已有 HTTP 服务，前端用 fetch 即可；避免双向桥接的额外复杂度。

## 3. 架构

### 3.1 目录结构

```
claude-partner/
├── web/                                    # 🆕 前端子项目
│   ├── src/
│   │   ├── main.tsx                        # React 入口
│   │   ├── App.tsx                         # 路由根
│   │   ├── styles/
│   │   │   ├── tokens.css                  # 30+ 设计变量（颜色/字体/间距/圆角/阴影）
│   │   │   ├── reset.css                   # 全局 reset
│   │   │   └── globals.css                 # body/root
│   │   ├── components/
│   │   │   ├── primitives/                 # 原子层（无业务语义）
│   │   │   │   ├── Button/                 # variant: primary/secondary/ghost/danger/icon
│   │   │   │   ├── Card/                   # 复合组件: Header/Body/Footer
│   │   │   │   ├── Input/
│   │   │   │   ├── Icon/                   # 统一 16x16 inline SVG
│   │   │   │   ├── Tag/                    # 标签 chip
│   │   │   │   └── Pill/                   # 状态胶囊
│   │   │   ├── layout/                     # 布局层
│   │   │   │   ├── Window/                 # 模拟 macOS 窗口容器
│   │   │   │   ├── TitleBar/               # traffic lights + drag
│   │   │   │   ├── Sidebar/
│   │   │   │   ├── NavItem/                # variant: active/default
│   │   │   │   ├── ThemeToggle/            # sun/moon icon
│   │   │   │   └── StatusDot/              # online/offline
│   │   │   └── domain/                     # 业务层（组合 primitives + layout）
│   │   │       ├── PromptCard/
│   │   │       ├── DeviceCard/
│   │   │       ├── TransferItem/
│   │   │       └── PermissionCard/
│   │   ├── pages/
│   │   │   ├── Home/                       # 01-main.html
│   │   │   ├── Transfer/                   # 02-transfer.html
│   │   │   ├── Prompts/                    # 03-prompts.html
│   │   │   ├── Devices/                    # 04-devices.html
│   │   │   ├── Settings/                   # 05-settings.html
│   │   │   ├── Welcome/                    # 06-welcome.html
│   │   │   └── DesignSystem/               # 🆕 预览页（仅 dev 路由）
│   │   ├── api/                            # fetch 封装
│   │   │   ├── client.ts
│   │   │   ├── prompts.ts
│   │   │   ├── devices.ts
│   │   │   └── transfer.ts
│   │   ├── hooks/                          # useTheme、useDevices 等
│   │   ├── lib/                            # utils、types
│   │   └── routes.tsx
│   ├── public/
│   ├── index.html
│   ├── vite.config.ts
│   ├── tsconfig.json
│   └── package.json
├── src/claude_partner/                     # Python 后端（保留）
│   ├── app.py
│   ├── ui/
│   │   ├── main_window.py                  # ✏️ 改造：内部 QWebEngineView
│   │   └── widgets/                        # 旧 Widgets（保留作 fallback）
│   └── ...
├── uiux/                                   # 设计稿（保留作参考，可后续归档）
├── docs/superpowers/specs/                 # 设计文档
├── AGENTS.md                               # 🆕 项目规范
└── web/dist/                               # Vite 构建产物（git ignored）
```

### 3.2 数据流

```
[React Component]
   ↓ useEffect / event handler
[api/*.ts] fetch('/api/...')
   ↓ HTTP
[aiohttp Handler]
   ↓ async/await
[Storage / Network / Sync 模块]
   ↓ callback / event
[app.py SignalBus] ───SSE──→ [前端 EventSource]
   ↓
[React State 更新 → 重渲染]
```

### 3.3 通信协议

**REST API（复用现有 aiohttp 路由）**
- `GET /api/health`
- `GET/POST/PUT/DELETE /api/prompts[/:id]`
- `GET /api/devices`
- `POST /api/sync`
- `POST /api/transfer/send` `GET /api/transfer/tasks`

**SSE（Server-Sent Events）**
- `GET /api/events/stream` 推送设备状态变化

**主题持久化**
- `localStorage.setItem('cp-theme', 'dark' | 'light')`
- 启动时读取并设置 `document.documentElement.dataset.theme`

## 4. 组件设计

### 4.1 原子组件规范

每个原子组件必须满足：
1. 单一职责（无业务语义）
2. 通过 `variant` / `size` / `state` 三个 prop 扩展
3. 使用 CSS Modules，文件名 `Component.module.css`
4. 完整 TypeScript 类型定义
5. 接受并合并 `className` / `style`（用于特殊场景覆盖）

### 4.2 Button 组件 API（示例）

```tsx
interface ButtonProps {
  variant?: 'primary' | 'secondary' | 'ghost' | 'danger' | 'icon'
  size?: 'sm' | 'md' | 'lg'
  disabled?: boolean
  loading?: boolean
  icon?: ReactNode         // 16x16 SVG
  iconRight?: ReactNode
  children?: ReactNode
  onClick?: (e: MouseEvent) => void
  className?: string
  type?: 'button' | 'submit' | 'reset'
}
```

### 4.3 复合组件 Card

```tsx
<Card variant="elevated" padding="lg">
  <Card.Header>
    <h3>...</h3>
  </Card.Header>
  <Card.Body>...</Card.Body>
  <Card.Footer>
    <Button>...</Button>
  </Card.Footer>
</Card>
```

实现：使用 `Card.Header` 等子组件作为命名导出，通过 React Context 共享 variant 状态。

### 4.4 业务组件 = 原子 + 布局的组合

```tsx
// PromptCard.tsx
export function PromptCard({ prompt, onEdit, onDelete }: PromptCardProps) {
  return (
    <Card>
      <Card.Header>
        <h4>{prompt.title}</h4>
        <Tag>{prompt.tag}</Tag>
      </Card.Header>
      <Card.Body>
        <p>{prompt.content}</p>
      </Card.Body>
      <Card.Footer>
        <Button variant="ghost" size="sm" icon={<EditIcon />} onClick={onEdit}>
          编辑
        </Button>
        <Button variant="danger" size="sm" icon={<TrashIcon />} onClick={onDelete}>
          删除
        </Button>
      </Card.Footer>
    </Card>
  )
}
```

## 5. 设计 Token 系统

### 5.1 颜色（来自 uiux :root）

| Token | Light | Dark | 用途 |
|-------|-------|------|------|
| `--bg` | #f5f4ed | #1f1d1b | 页面背景 |
| `--surface` | #faf9f5 | #272522 | 卡片/窗口背景 |
| `--surface-warm` | #e8e6dc | #322f2c | hover/active 状态 |
| `--fg` | #141413 | #faf9f5 | 主文字 |
| `--fg-2` | #3d3d3a | #d8d6cf | 次文字 |
| `--muted` | #5e5d59 | #a8a59e | 辅助文字 |
| `--meta` | #87867f | #787671 | 元信息 |
| `--border` | #f0eee6 | #2e2b29 | 边框 |
| `--border-soft` | #e8e6dc | #262422 | 淡边框 |
| `--accent` | #c96442 | #d97757 | terracotta 强调色 |
| `--accent-on` | #faf9f5 | #141413 | 强调色上的文字 |
| `--success` | #17a34a | #4ade80 | 成功 |
| `--warn` | #eab308 | #facc15 | 警告 |
| `--danger` | #b53333 | #ef6b6b | 危险 |

### 5.2 字体

```css
--font-display: "Anthropic Serif", Georgia, serif;       /* 大标题 */
--font-body: "Anthropic Sans", system-ui, sans-serif;     /* 正文 */
--font-mono: "Anthropic Mono", ui-monospace, monospace;   /* 代码 */
```

### 5.3 间距（8px 网格）

```css
--space-1: 4px;   --space-2: 8px;    --space-3: 12px;
--space-4: 16px;  --space-5: 20px;   --space-6: 24px;
--space-8: 32px;  --space-10: 40px;  --space-12: 48px;
--space-16: 64px; --space-24: 96px;
```

### 5.4 圆角

```css
--radius-sm: 6px;    /* 按钮、输入框 */
--radius-md: 8px;    /* 卡片 */
--radius-lg: 12px;   /* 窗口 */
--radius-xl: 16px;   /* 弹窗 */
```

### 5.5 阴影

```css
--shadow-card: 0 1px 2px rgba(0,0,0,0.04), 0 2px 8px rgba(0,0,0,0.06);
--shadow-window: 0 32px 80px rgba(0,0,0,0.5), inset 0 1px 0 rgba(255,255,255,0.05);
```

### 5.6 动效

```css
--motion-fast: 150ms;
--motion-base: 200ms;
--ease-standard: cubic-bezier(0.2, 0, 0, 1);
```

## 6. 页面还原矩阵

| uiux 文件 | 路由 | 核心组件 | 业务组件 |
|-----------|------|---------|---------|
| 01-main.html | / | Window, TitleBar, Sidebar, NavItem, ThemeToggle, Card, Tag | PromptCard |
| 02-transfer.html | /transfer | + Button(variant=icon), ProgressBar | TransferItem |
| 03-prompts.html | /prompts | + Input, Pill | PromptCard (变体) |
| 04-devices.html | /devices | + StatusDot | DeviceCard |
| 05-settings.html | /settings | + Input variants, Tag variants | - |
| 06-welcome.html | /welcome | - | PermissionCard |
| (新) DesignSystem | /design-system | 所有原子组件展示 | - |

## 7. PyQt6 集成

### 7.1 MainWindow 改造

```python
class MainWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("Claude Partner")
        self.resize(1200, 760)

        # QWebEngineView 加载 Vite 构建产物
        self.web = QWebEngineView()
        if dev_mode:
            self.web.load(QUrl("http://localhost:5173"))
        else:
            self.web.load(QUrl.fromLocalFile(dist_html_path))

        # 拦截 QWebEngineView 的请求，转发到 aiohttp
        # 或者：让前端直接请求 127.0.0.1:<port>，aiohttp 已监听
        self.setCentralWidget(self.web)
```

### 7.2 通信方式

**dev 模式**：
- Vite dev server 监听 5173
- 前端 fetch `http://localhost:<aiohttp-port>/api/...`
- vite.config.ts 配置 proxy: `/api` → aiohttp 端口

**production 模式**：
- `web/dist/` 被打包进 PyInstaller 资源
- 前端 fetch `http://127.0.0.1:<aiohttp-port>/api/...`（同 dev）

### 7.3 打包

`claude_partner.spec` 添加：
```python
datas += [('web/dist', 'web/dist')]
```

## 8. 测试策略

- **单元测试**：Vitest 测关键 hooks、api client
- **组件测试**：可选（Storybook 暂不引入）
- **手动验证**：开发态访问 `/design-system` 路由查看组件
- **截图对比**：可选 — 用 Playwright 截取新前端，与 uiux 截图人工对比

## 9. 实施阶段

| 阶段 | 产出 | 估时 |
|------|------|------|
| 1. 脚手架 | web/ + Vite + React + tokens.css | 30 min |
| 2. 原子组件 | Button, Card, Input, Icon, Tag, Pill | 1 h |
| 3. 布局组件 | Window, TitleBar, Sidebar, NavItem, ThemeToggle | 1 h |
| 4. 业务组件 | PromptCard, DeviceCard, TransferItem, PermissionCard | 1 h |
| 5. 6 页面 | Home, Transfer, Prompts, Devices, Settings, Welcome | 2 h |
| 6. DesignSystem 页 | 色板/字体/按钮/卡片/表单/布局展示 | 1 h |
| 7. AGENTS.md | 完整规范文档 | 30 min |
| 8. PyQt6 集成 | MainWindow 改造 + 打包 | 1 h |
| 9. 合并 | worktree 合并到 master | 15 min |

**总计**: 约 8-9 小时（并行 subagent 可压缩到 4-5 小时）

## 10. 风险与缓解

| 风险 | 等级 | 缓解 |
|------|------|------|
| PyQt6-WebEngine 打包体积 +100MB | 中 | 评估后用户接受；旧 Widgets 保留作 fallback |
| 前端 dev 体验：QWebEngineView 跨域 | 低 | vite proxy 转发 /api |
| 旧 PyQt6 业务代码 5500+ 行废弃 | 低 | 保留 git 历史，作为 backup；不立即删除 |
| icon 库缺失：需要逐个写 SVG | 低 | 提取公共 icon 文件，统一管理 |
| 字体回退：系统无 Anthropic | 中 | 已用 font-family fallback，Georgia/Arial 接管 |

## 11. 后续规划（暂不在本次范围）

- 接入真实数据：前端 fetch → aiohttp handlers 已有 → 仅需前端联调
- 主题切换动画（200ms 渐变）
- 拖拽文件上传（前端 dropzone + POST /api/transfer/send）
- 全局快捷键（前端 window keydown → 与 PyQt6 冲突需协调）
- i18n：当前仅中文，保留扩展点

## 12. 验收标准

1. ✅ 6 个页面与 uiux HTML 视觉、布局、间距、颜色、字体、交互状态一致
2. ✅ 浅色/深色模式切换正常
3. ✅ 至少 10 个原子组件、5 个业务组件可复用
4. ✅ DesignSystem 预览页能查看所有核心组件
5. ✅ AGENTS.md 完整记录组件复用规范
6. ✅ PyQt6 主窗口加载前端，桌面端可运行
7. ✅ 修改 token.css 单一文件可全局生效（无硬编码颜色）
