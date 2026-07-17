# 侧栏设置入口下沉至 Footer 设计

**日期**: 2026-07-17  
**状态**: 已确认  
**范围**: 桌面 AppShell 侧栏导航与 footer 工具区

## 问题

小屏 macOS（短窗口高度）下，左侧主导航内容较长，System 分组底部的「设置」入口常需滚动才能看到，首屏不可达。同时现有 `SettingsIcon` 为中心圆 + 射线，与主题切换的 `SunIcon` 视觉高度相似，并排放置时易混淆。

## 目标

1. 设置入口始终固定在侧栏底部 footer，与语言、主题、移动端入口同区，小屏首屏可见。
2. 设置图标改为经典齿轮，与主题 sun/moon 明确区分。
3. 去掉 System 导航中的重复设置项，避免双入口。

## 非目标

- 不改动 Mobile Workbench 导航结构。
- 不新增独立 SettingsToggle 组件文件（仅一处使用）。
- 不新增 i18n key（复用 `nav:settings`）。
- 不为 footer 设置按钮增加可见文字标签。
- 不调整健康提醒导航位置。

## 方案

采用 **Footer 圆形图标按钮**（方案 A）。

### 导航

- 删除 `AppShell` System 组中的  
  `NavItem to="/settings" ... icon={<SettingsIcon />}`。
- System 组仅保留健康提醒（`/health`）。
- `/settings` 路由、Settings 页面、深链（如 `/settings?tab=dependencies`）行为不变。

### Footer 布局

现有结构：

```
footer
  version + appName
  footerToggle
    LanguageSwitcher
    footerIconGroup
      ThemeToggle
      手机访问按钮
```

调整为：

```
footerIconGroup
  ThemeToggle
  设置按钮（NavLink → /settings）
  手机访问按钮
```

顺序固定：**主题 → 设置 → 手机**。

### 设置按钮行为与样式

- 使用 `react-router-dom` 的 `NavLink`（或等价 `Link` + `useLocation`/`useMatch`）指向 `/settings`。
- 视觉：复用手机按钮同款 26×26 圆形样式（border / surface-warm hover / focus-visible）。
- Active：路径以 `/settings` 开头时应用 active 样式（对齐手机按钮 expanded 态：`background: var(--surface-warm); color: var(--fg)`），并设置 `aria-current="page"`。
- 无障碍：`aria-label` 与 `title` 均使用 `t('nav:settings')`。
- 图标：`<SettingsIcon size={14} />`（与主题、手机一致）。

### 图标

重写 `web/src/lib/icons.tsx` 中的 `SettingsIcon`：

- 从「射线圆」改为经典齿轮：中心圆 + 外圈齿形 path。
- 保持现有 `IconProps` / `baseProps` / 16 viewBox 约定。
- 导出名称不变，调用方无需改 import：
  - AppShell footer
  - Orchestrator 跳转设置链接
  - MobileWorkbenchShell `settings` 面板图标
  - DesignSystem 图标展示

## 涉及文件

| 文件 | 变更 |
| --- | --- |
| `web/src/components/layout/AppShell/AppShell.tsx` | 移除 System 设置 NavItem；footer 增加设置 NavLink |
| `web/src/components/layout/AppShell/AppShell.module.css` | 设置按钮样式（可复用 mobileAccessButton 类或抽取共享类） |
| `web/src/lib/icons.tsx` | `SettingsIcon` 改为齿轮 |
| `web/CLAUDE.md` | 更新侧栏导航与 footer 描述 |

## 验收标准

1. 小屏高度下不滚动侧栏内容区即可点击设置。
2. System 导航中不再出现「设置」文字项。
3. 点击 footer 设置齿轮进入 `/settings`；已在设置页时按钮呈现 active。
4. 设置齿轮与主题 sun/moon 视觉可区分。
5. 语言 / 主题 / 手机访问行为不变。

## 验证

- 目视：侧栏 footer 三图标顺序与 active 态。
- 若有 AppShell 相关单测/E2E 深链到 settings，确认导航仍可达。
- 类型与 lint：`cd web && npm run build`（或至少 tsc 相关路径）按需执行。
