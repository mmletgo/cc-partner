# Claude Code 资产页「按启用状态筛选」设计（方案 A：状态分段控件）

## Context（为什么做）

`/claude-code` 页（`ClaudeCodeAssets`）当前只有两类筛选：文本搜索、类别分段（全部/skill/command/plugin/mcp）。资产本身带 `enabled: boolean`（UI 已用 Pill 显示「已启用/已关闭」），但用户**无法按启用状态筛选**——例如想快速找出所有「已禁用」的资产进行清理，或只想看「已启用」的资产，必须人眼逐行扫。

本设计新增一个与类别并列的「状态」分段控件（全部状态 / 已启用 / 已禁用），让用户按 enabled 维度筛选。该筛选与现有类别/搜索一致，对「本机资产」「局域网拉取」两个 tab 同时生效。

## 需求（已与用户确认）

1. **筛选维度**：新增 `enabled` 三态筛选 —— 全部状态 / 仅已启用 / 仅已禁用
2. **UI 形态**：在筛选区右列、kind 分段控件旁边再加一个 inline「状态」分段控件（视觉与 kind 分段完全一致）
3. **生效范围**：与现有 kind/search 一样，对 local tab 与 remote tab 同时生效
4. **不改**：资产行的 Pill、启停按钮、kindSummary 统计面板（统计仍是全量，不受筛选影响）

## 技术方案（方案 A）

**纯前端筛选，无 Rust 改动**：`ClaudeCodeAsset` DTO 已带 `enabled` 字段，前端只需新增一个筛选维度，复用现有分段控件样式。

## 详细设计

### 状态与类型

```ts
type EnabledFilter = 'all' | 'enabled' | 'disabled';

const ENABLED_OPTIONS: EnabledFilter[] = ['all', 'enabled', 'disabled'];
```

### matchesAsset 扩展

`ClaudeCodeAssets.tsx` 的 `matchesAsset` 增加第 4 个参数：

```ts
function matchesAsset(
  asset: ClaudeCodeAsset,
  kind: KindFilter,
  search: string,
  enabledFilter: EnabledFilter,
): boolean {
  const q = search.trim().toLowerCase();
  const matchesKind = kind === 'all' || asset.kind === kind;
  const matchesEnabled =
    enabledFilter === 'all' ||
    (enabledFilter === 'enabled' && asset.enabled) ||
    (enabledFilter === 'disabled' && !asset.enabled);
  const haystack = `${asset.name} ${asset.id} ${asset.source} ${asset.description ?? ''}`.toLowerCase();
  return matchesKind && matchesEnabled && (!q || haystack.includes(q));
}
```

### 页面 state

```ts
const [enabledFilter, setEnabledFilter] = useState<EnabledFilter>('all');
```

- `filteredAssets` 的 useMemo 依赖加 `enabledFilter`，调用 `matchesAsset(asset, kind, search, enabledFilter)`
- 传给 `RemoteAssetPicker` 的 props 增加 `enabledFilter={enabledFilter}`

### 筛选区 UI 布局

筛选区右列从「单个 kind 分段」改为「kind 分段 + 状态分段」并列。两段之间用 `gap` 间隔，外层容器 `flex-wrap` 兜底窄屏。

```
[ 🔍 搜索框 ]   [ 全部 skill command plugin mcp ]  [ 全部状态 已启用 已禁用 ]
```

状态分段控件结构（与现有 `.segmented` 完全同款样式）：

```tsx
<div className={styles.segmented} role="group" aria-label={t('claudeCodeAssets:stateFilterLabel')}>
  {ENABLED_OPTIONS.map((option) => (
    <button
      key={option}
      type="button"
      className={option === enabledFilter ? styles.segmentActive : styles.segment}
      aria-pressed={option === enabledFilter}
      onClick={() => setEnabledFilter(option)}
    >
      {option === 'all'
        ? t('claudeCodeAssets:allStates')
        : option === 'enabled'
          ? t('claudeCodeAssets:enabled')
          : t('claudeCodeAssets:disabled')}
    </button>
  ))}
</div>
```

### CSS 调整

筛选区右列需要一个包装容器，让两个分段并列、`flex-wrap`、`gap`：

```css
.filterGroup {
  display: inline-flex;
  align-items: center;
  gap: var(--space-3);
  flex-wrap: wrap;
  justify-self: start;
}
```

`.filters` 的 `grid-template-columns: minmax(220px, 360px) 1fr` 保持不变（右列容纳新包装容器）。窄屏 `@media (max-width: 980px)` 现有 `1fr` 兜底已能换行，无需新增。

### RemoteAssetPicker 同步

`RemoteAssetPickerProps` 增加 `enabledFilter: EnabledFilter`；内部 `visible` 的 filter 加 enabled 判断（与父组件 `matchesAsset` 同款逻辑），保持「kind/search/enabled 三维度对 remote tab 也生效」。

### i18n 新增（zh + en 各加）

zh/claudeCodeAssets.json：
- `"allStates": "全部状态"`
- `"stateFilterLabel": "按启用状态筛选"`

en/claudeCodeAssets.json：
- `"allStates": "All states"`
- `"stateFilterLabel": "Filter by enabled state"`

（「已启用 / 已禁用」复用现有 `enabled` / `disabled` key，无需新增。）

## 改动文件（4 个）

| 文件 | 改动 |
|------|------|
| `web/src/pages/ClaudeCodeAssets/ClaudeCodeAssets.tsx` | 新增 `EnabledFilter` 类型、`ENABLED_OPTIONS`、`enabledFilter` state；扩展 `matchesAsset`；`filteredAssets` 依赖；新增状态分段控件 JSX；向 `RemoteAssetPicker` 传 `enabledFilter` |
| `web/src/pages/ClaudeCodeAssets/ClaudeCodeAssets.module.css` | 新增 `.filterGroup` 样式（并列两个分段 + flex-wrap） |
| `web/src/components/domain/RemoteAssetPicker/RemoteAssetPicker.tsx` | `RemoteAssetPickerProps` 加 `enabledFilter`；内部 `visible` filter 加 enabled 判断 |
| `web/src/i18n/locales/{en,zh}/claudeCodeAssets.json` | 新增 `allStates` / `stateFilterLabel` |

## 不改 / 不影响

- 资产行 Pill、启停按钮、卸载按钮
- kindSummary 统计面板（按类别统计 enabled/warnings 数量，仍是全量，不随筛选变化）
- 任何 Rust 后端 / DTO / invoke 命令
- 现有 kind/search 行为与作用范围

## 验证

1. `cd web && npx tsc --noEmit` 通过
2. 手动：本机 tab 下切换三态，列表正确过滤；类别 + 状态 + 搜索可叠加
3. 手动：切到局域网拉取 tab，状态筛选同样生效
4. 窄屏：两段控件能换行，不溢出
5. kindSummary 统计不随状态筛选变化（仍是全量）
