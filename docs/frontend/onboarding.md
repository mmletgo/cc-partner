# 前端 Onboarding 指南

> 适合新加入 Claude Partner 项目的开发者快速上手前端部分

## 5 分钟概览

Claude Partner 前端是**桌面端内嵌 Web 应用**：

```
┌─────────────────────────────────────┐
│ Tauri 2 主进程 (Rust)                │
│  ┌───────────────────────────────┐  │
│  │ Webview 窗口                  │  │
│  │  ┌─────────────────────────┐  │  │
│  │  │ React + Vite 前端        │  │  │
│  │  │  - 路由：/ /prompts/...  │  │  │
│  │  │  - invoke() → Rust 命令  │  │  │
│  │  └─────────────────────────┘  │  │
│  └───────────────────────────────┘  │
│  托盘 / 快捷键 / axum 都在 Rust       │
└─────────────────────────────────────┘
```

## 目录速查

| 路径 | 作用 |
|------|------|
| `web/src/styles/tokens.css` | ⭐ 设计 token 单一来源（颜色/字体/间距） |
| `web/src/lib/icons.tsx` | 25+ inline SVG icon |
| `web/src/components/primitives/` | 原子组件（Button/Card/Input/...） |
| `web/src/components/layout/` | 布局组件（AppShell/TitleBar/Sidebar/...） |
| `web/src/components/domain/` | 业务组件（PromptCard/DeviceCard/...） |
| `web/src/pages/` | 6 个路由页面 + DesignSystem 预览 |
| `web/src/api/` | invoke 封装（prompts/devices/transfer，调 Rust 命令） |
| `AGENTS.md` | ⭐ 必读：组件复用规范 |

## 启动 dev server

```bash
cd web
npm install                        # 首次
./node_modules/.bin/tauri dev      # Vite + Rust 主进程，热重载
```

Tauri dev 默认占用 1420 端口；`/design-system` 查看组件库。

## 一天开发流程

1. **修改样式**：先看 `tokens.css` 有无 token，没有就加（同时给两套主题）
2. **添加按钮/卡片**：用 primitives 的 variant 扩展，不要新建相似组件
3. **新页面**：在 `pages/` 下新建文件夹，组合 primitives + layout + domain
4. **新 icon**：在 `lib/icons.tsx` 末尾追加，遵循 viewBox/stroke 规范
5. **联调 API**：在 `api/<module>.ts` 添加函数，类型在 `lib/types.ts`

## 三大"不要"

1. **不要**硬编码 `#xxx` 颜色 — 一律 `var(--accent)`
2. **不要**在 primitives 写业务逻辑 — 业务在 domain
3. **不要**为相似组件建新文件 — 用 variant 扩展

## 三大"必须"

1. **必须**先看 AGENTS.md 第 4 节
2. **必须**新组件同步更新 AGENTS.md 组件清单
3. **必须**新 token 在浅色/深色两套都加

## 常见任务

### 加一个新按钮样式
```tsx
// 1. 找 Button 组件，看现有 variant
// 2. 如果有 secondary，加 size='sm' 就够
<Button variant="secondary" size="sm" icon={<XIcon />}>取消</Button>

// 3. 真的不够用？扩展 Button 组件：
//    web/src/components/primitives/Button/Button.tsx
//    加 variant='warning'，改 .module.css
```

### 加一个新页面
```bash
# 1. 新建文件夹
mkdir web/src/pages/MyPage
# 2. 创建 .tsx + .module.css + index.ts
# 3. 在 App.tsx 注册路由
# 4. 在 sidebar NavItem 中加链接（AppShell.tsx）
```

### 改一个颜色
```css
/* web/src/styles/tokens.css */
:root {
  --accent: #你的颜色;        /* 浅色 */
}
[data-theme="dark"] {
  --accent: #你的深色;        /* 深色 */
}
```
全局生效，无需改组件。

## 调试

| 问题 | 排查 |
|------|------|
| 页面空白 | F12 打开控制台看 invoke / Rust 错误 |
| invoke 报错 | 检查 `src-tauri/src/commands/` 是否注册该命令 + `lib.rs` invoke_handler |
| 主题没切换 | 检查 `useTheme` 是否在组件树顶层 |
| TypeScript 错误 | `cd web && npx tsc --noEmit` |
| 组件样式没生效 | 确认 import 了 `tokens.css` + `reset.css` |
