# 侧栏移动端按钮上移至版本行 — 设计

**日期**: 2026-08-15
**范围**: `web/src/components/layout/AppShell/AppShell.tsx`（`AppShell` 组件 footer 区） + 一条单元测试
**类型**: UI 局部调整（无产品行为变更、无协议变更、无 token 变更）

## 1. 背景

桌面侧栏 footer 当前两行布局：

```
Row 1 (footerVersionRow): [v0.1.0] ······· [Game]
Row 2 (footerToggle):     [Language] [Battery] [Theme] [Settings] [Mobile]
```

手机访问按钮（`MobileAccessCard` 入口）位于第二行最右；游戏按钮位于第一行最右。两者都属于「桌面端辅助/娱乐入口」语义，但视觉上被 footer 分行割裂。

## 2. 目标

将移动端按钮上移到版本行，与游戏按钮同一行，平衡 footer 视觉重量（第一行原本只有版本号 + 单一图标），同时把第二行让给固定设置控制（语言 / 充电 / 主题 / 设置）。

## 3. 目标布局

```
Row 1 (footerVersionRow): [v0.1.0] ······· [Game] [Mobile]
Row 2 (footerToggle):     [Language] [Battery] [Theme] [Settings]
```

- 版本行右端用 `footerIconGroup` 同时承载 `Game` + `Mobile`，两个按钮之间走 `gap: var(--space-2)` (= 8px)，与另一行 `Battery + Theme + Settings` 的成组视觉一致。
- 顺序：游戏在前、手机在后。游戏保持原版本行「主功能」位置；手机顺位粘贴到右侧，与原 `footerToggle` 最右元素的视觉习惯一致。

## 4. 改动清单

### 4.1 `AppShell.tsx`

**JSX**：把 `<button ref={mobileAccessButtonRef} ...>...<SmartphoneIcon .../></button>` 整体从 `footerIconGroup`（第二行）迁移到 `footerVersionRow` 内，与 game 按钮共享新包裹的 `<span className={styles.footerIconGroup}>`。

```tsx
<span className={styles.footerVersionRow}>
  <span className={styles.footerVersion}>{`v${version ?? '—'}`}</span>
  <span className={styles.footerIconGroup}>
    <button
      type="button"
      className={styles.footerIconButton}
      onClick={openGameHub}
      aria-haspopup="dialog"
      aria-expanded={gameHubOpen}
      aria-label={t('wordgame:gameButtonTitle')}
      title={t('wordgame:gameButtonTitle')}
    >
      <GameIcon size={14} />
    </button>
    <button
      ref={mobileAccessButtonRef}
      type="button"
      className={styles.footerIconButton}
      onClick={toggleMobileAccess}
      aria-label={t('settings:mobileAccess.buttonLabel')}
      aria-haspopup="dialog"
      aria-expanded={mobileAccessOpen}
      aria-controls={MOBILE_ACCESS_DIALOG_ID}
      title={t('settings:mobileAccess.buttonTitle')}
    >
      <SmartphoneIcon size={14} />
    </button>
  </span>
</span>
```

`footerToggle` 仅保留 `LanguageSwitcher` 和 `Battery + Theme + Settings` 三个图标按钮。

**docstring 同步**（line 8-9, 16）：

- 原："侧边栏 footer 区域集中展示版本号、版本行最右的 game 图标按钮、语言/主题/设置齿轮与移动端访问入口"
- 现："侧边栏 footer 区域集中展示版本号、版本行最右的游戏与手机访问按钮组、语言/主题/设置齿轮"
- line 16 同步：`Sidebar 内包含 Logo、分组导航...、footer（版本号 + 版本行最右的游戏与手机访问按钮组 + 语言/主题/设置齿轮）`

### 4.2 `AppShell.module.css`

**无改动**。`footerIconGroup` 已存在并提供 `display: inline-flex; align-items: center; gap: var(--space-2);`，与第二行其他按钮组共用。

### 4.3 `AppShell.test.tsx`

新增 1 条测试用例，断言：

- `mobile access button` 通过 `closest('[class*="footerVersionRow"]')` 命中版本行；
- 同一按钮通过 `closest('[class*="footerIconGroup"]')` 命中图标组；
- DOM 顺序：mobile 按钮 `compareDocumentPosition` 在 game 按钮之后（`DOCUMENT_POSITION_FOLLOWING`）。

i18n mock 已经把 `wordgame:gameButtonTitle` / `settings:mobileAccess.buttonLabel` 翻译为中文 "打开游戏大厅" / "手机访问"，无需新增 mock。

### 4.4 i18n / AGENTS.md

**无新增 key**，无新增组件清单条目（仍是同一类按钮组合，只是所在行变化）。

## 5. 不变更

- 卫星窗 `footer`（已不渲染移动按钮 / 游戏按钮 / 设置 / 主题）；
- `MobileAccessCard` 与 `GameHubDialog` 组件本身；
- `mobileAccessOpen` / `gameHubOpen` 状态机、`mobileAccessButtonRef` focus 恢复逻辑；
- `Dialog` 合同、`useModalLayer` 焦点陷阱；
- 任何 token / CSS 变量 / i18n 资源；
- 路由、P2P 协议、Tauri 命令。

## 6. 验收

1. `cd web && npm run lint && npm test -- src/components/layout/AppShell && npm run build` 全 `exit 0`；
2. 桌面端主窗口侧栏底部：第一行 = `v0.1.0 · [Game] [Mobile]`，第二行 = `[Language] [Battery] [Theme] [Settings]`；
3. `mobile-access` 按钮点开仍弹出 `MobileAccessCard` Dialog，关闭后焦点恢复至该按钮；
4. 卫星窗 `footer` 维持 `BatteryModeToggle` + 剩余时间，不出现新按钮；
5. 主题（浅/深）切换不改变按钮与图标组的几何位置；
6. 仓库根 `git diff` 改动行数 ≤ 30（仅 `AppShell.tsx` + `AppShell.test.tsx`）。

## 7. 风险

- 极低。仅 DOM 顺序变化，无业务逻辑变化；
- 现有测试 `places settings entry in footer icon group with gear link`、`places battery toggle before ThemeToggle in the footer icon group`、`places a game icon button at the far right of the version row` 全部继续通过（不动相应元素）；
- DOC 顺序变化只影响屏幕阅读器的版本行 Tab 顺序：`<version> [Game] [Mobile]`，与视觉一致，无 a11y 风险。
