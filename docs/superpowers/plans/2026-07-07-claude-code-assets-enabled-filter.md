# Claude Code 资产页「按启用状态筛选」实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 `/claude-code` 资产页筛选区新增「状态」分段控件（全部状态 / 已启用 / 已禁用），与现有类别分段并列，对 local 与 remote 两个 tab 同时生效。

**Architecture:** 纯前端筛选。把 `kind + search + enabled` 三维度匹配逻辑提取为 `web/src/lib/claudeCodeAssets.ts` 的纯函数（含单测），页面与 `RemoteAssetPicker` 共用，消除现有 filter 逻辑重复。UI 沿用现有 `.segmented` 分段控件样式。

**Tech Stack:** React 19 + TypeScript + CSS Modules + i18next。测试用项目惯用的 `npx tsx xxx.test.ts` + 自写 `assert` + `console.log('xxx passed')` 方式（项目无 vitest）。

**Spec:** `docs/superpowers/specs/2026-07-07-claude-code-assets-enabled-filter-design.md`

---

## 文件结构

| 文件 | 操作 | 职责 |
|------|------|------|
| `web/src/lib/claudeCodeAssets.ts` | 新建 | 纯函数：`KindFilter` / `EnabledFilter` 类型、`KIND_OPTIONS`、`ENABLED_OPTIONS`、`matchesClaudeCodeAsset()` |
| `web/src/lib/claudeCodeAssets.test.ts` | 新建 | 对 `matchesClaudeCodeAsset` 的三维度组合做断言测试 |
| `web/src/pages/ClaudeCodeAssets/ClaudeCodeAssets.tsx` | 修改 | 删除本地 `matchesAsset`/`KindFilter`/`KIND_OPTIONS`，改从 lib 导入；新增 `enabledFilter` state + 状态分段控件 JSX；把 `enabledFilter` 传给 `RemoteAssetPicker` |
| `web/src/pages/ClaudeCodeAssets/ClaudeCodeAssets.module.css` | 修改 | 新增 `.filterGroup` 样式（让 kind 分段 + 状态分段并列、flex-wrap） |
| `web/src/components/domain/RemoteAssetPicker/RemoteAssetPicker.tsx` | 修改 | props 增加 `enabledFilter`；内部 `visible` 改用 lib 的 `matchesClaudeCodeAsset` |
| `web/src/i18n/locales/zh/claudeCodeAssets.json` | 修改 | 新增 `allStates` / `stateFilterLabel` |
| `web/src/i18n/locales/en/claudeCodeAssets.json` | 修改 | 新增 `allStates` / `stateFilterLabel` |
| `web/CLAUDE.md` | 修改 | 在 ClaudeCodeAssets 页面功能描述里补充「按启用状态筛选」 |

---

## Task 1: 提取并扩展 filter 到 lib（TDD）

**Files:**
- Create: `web/src/lib/claudeCodeAssets.ts`
- Test: `web/src/lib/claudeCodeAssets.test.ts`

- [ ] **Step 1: 写失败的测试**

创建 `web/src/lib/claudeCodeAssets.test.ts`：

```ts
import type { ClaudeCodeAsset } from '@/lib/types';
import {
  ENABLED_OPTIONS,
  KIND_OPTIONS,
  matchesClaudeCodeAsset,
} from './claudeCodeAssets';
import type { EnabledFilter, KindFilter } from './claudeCodeAssets';

/**
 * Business Logic（为什么需要这个函数）:
 *   matchesClaudeCodeAsset 是 Claude Code 资产页 local/remote 两个 tab 共享的筛选契约，
 *   任意维度判断错误都会导致用户看到与筛选条件不符的资产，必须用断言锁死三维度组合行为。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让 tsx 测试进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * 构造一个最小可用的 ClaudeCodeAsset 测试夹具。
 */
function makeAsset(overrides: Partial<ClaudeCodeAsset>): ClaudeCodeAsset {
  return {
    kind: 'skill',
    id: 'skill-1',
    name: 'Test Skill',
    scope: 'user',
    enabled: true,
    source: 'local',
    canEnable: true,
    canUninstall: true,
    canExport: true,
    warnings: [],
    ...overrides,
  };
}

const enabledSkill = makeAsset({ kind: 'skill', id: 's1', name: 'Alpha', enabled: true, source: 'src-a', description: 'desc-a' });
const disabledSkill = makeAsset({ kind: 'skill', id: 's2', name: 'Beta', enabled: false, source: 'src-b', description: '' });
const enabledPlugin = makeAsset({ kind: 'plugin', id: 'p1', name: 'Gamma', enabled: true, source: 'src-c', description: '' });
const disabledMcp = makeAsset({ kind: 'mcp', id: 'm1', name: 'Delta', enabled: false, source: 'src-d', description: 'keyword-zzz' });

// kind 维度
assert(
  matchesClaudeCodeAsset(enabledSkill, 'all', '', 'all'),
  'all kind / 空 search / all enabled 应匹配任意资产',
);
assert(
  !matchesClaudeCodeAsset(enabledPlugin, 'skill', '', 'all'),
  'kind=skill 应拒绝 plugin',
);
assert(
  matchesClaudeCodeAsset(enabledPlugin, 'plugin', '', 'all'),
  'kind=plugin 应匹配 plugin',
);

// search 维度（匹配 name / id / source / description，大小写不敏感）
assert(
  matchesClaudeCodeAsset(enabledSkill, 'all', 'alpha', 'all'),
  'search 应匹配 name（大小写不敏感）',
);
assert(
  matchesClaudeCodeAsset(enabledSkill, 'all', 'src-a', 'all'),
  'search 应匹配 source',
);
assert(
  matchesClaudeCodeAsset(disabledMcp, 'all', 'keyword-zzz', 'all'),
  'search 应匹配 description',
);
assert(
  !matchesClaudeCodeAsset(enabledSkill, 'all', 'not-found', 'all'),
  'search 找不到时应拒绝',
);

// enabled 维度
assert(
  matchesClaudeCodeAsset(enabledSkill, 'all', '', 'enabled'),
  'enabled 筛选应匹配 enabled 资产',
);
assert(
  !matchesClaudeCodeAsset(disabledSkill, 'all', '', 'enabled'),
  'enabled 筛选应拒绝 disabled 资产',
);
assert(
  matchesClaudeCodeAsset(disabledSkill, 'all', '', 'disabled'),
  'disabled 筛选应匹配 disabled 资产',
);
assert(
  !matchesClaudeCodeAsset(enabledSkill, 'all', '', 'disabled'),
  'disabled 筛选应拒绝 enabled 资产',
);

// 三维度组合
assert(
  matchesClaudeCodeAsset(disabledSkill, 'skill', 'beta', 'disabled'),
  '三维度同时命中应匹配',
);
assert(
  !matchesClaudeCodeAsset(disabledSkill, 'skill', 'beta', 'enabled'),
  'enabled 维度不命中时应拒绝，即使其他维度命中',
);
assert(
  !matchesClaudeCodeAsset(disabledMcp, 'skill', 'delta', 'disabled'),
  'kind 维度不命中时应拒绝，即使其他维度命中',
);

// 常量
assert(KIND_OPTIONS[0] === 'all', 'KIND_OPTIONS 首项应为 all');
assert(ENABLED_OPTIONS[0] === 'all', 'ENABLED_OPTIONS 首项应为 all');
assert(
  ENABLED_OPTIONS.length === 3 && ENABLED_OPTIONS.includes('enabled') && ENABLED_OPTIONS.includes('disabled'),
  'ENABLED_OPTIONS 应为 all/enabled/disabled 三态',
);

console.log('claudeCodeAssets.test.ts passed');
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd web && npx tsx src/lib/claudeCodeAssets.test.ts`
Expected: FAIL（报错找不到 `./claudeCodeAssets` 模块）

- [ ] **Step 3: 实现 lib**

创建 `web/src/lib/claudeCodeAssets.ts`：

```ts
/**
 * Claude Code 资产页筛选纯函数
 *
 * Business Logic（为什么需要这个文件）:
 *   Claude Code 资产页的 local tab 和 remote（局域网拉取）tab 共享同一套筛选维度
 *   （类别 / 关键字 / 启用状态），把匹配逻辑提取为纯函数供两处复用，避免逻辑漂移
 *   导致两个 tab 行为不一致。
 *
 * Code Logic（这个文件做什么）:
 *   导出 KindFilter / EnabledFilter 类型、KIND_OPTIONS / ENABLED_OPTIONS 常量，
 *   以及 matchesClaudeCodeAsset 纯函数（按 kind + search + enabledFilter 三维度
 *   对单个 ClaudeCodeAsset 求值，全部命中返回 true）。
 */

import type { ClaudeCodeAsset, ClaudeCodeAssetKind } from './types';

export type KindFilter = ClaudeCodeAssetKind | 'all';
export type EnabledFilter = 'all' | 'enabled' | 'disabled';

export const KIND_OPTIONS: KindFilter[] = ['all', 'skill', 'command', 'plugin', 'mcp'];
export const ENABLED_OPTIONS: EnabledFilter[] = ['all', 'enabled', 'disabled'];

/**
 * 判断资产是否同时命中 kind + search + enabledFilter 三个筛选维度。
 */
export function matchesClaudeCodeAsset(
  asset: ClaudeCodeAsset,
  kind: KindFilter,
  search: string,
  enabledFilter: EnabledFilter,
): boolean {
  const matchesKind = kind === 'all' || asset.kind === kind;
  const matchesEnabled =
    enabledFilter === 'all' ||
    (enabledFilter === 'enabled' && asset.enabled) ||
    (enabledFilter === 'disabled' && !asset.enabled);
  if (!matchesKind || !matchesEnabled) return false;
  const q = search.trim().toLowerCase();
  if (!q) return true;
  const haystack = `${asset.name} ${asset.id} ${asset.source} ${asset.description ?? ''}`.toLowerCase();
  return haystack.includes(q);
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd web && npx tsx src/lib/claudeCodeAssets.test.ts`
Expected: 输出 `claudeCodeAssets.test.ts passed`，退出码 0

- [ ] **Step 5: 类型检查**

Run: `cd web && npx tsc --noEmit`
Expected: 无错误

- [ ] **Step 6: 提交**

```bash
git add web/src/lib/claudeCodeAssets.ts web/src/lib/claudeCodeAssets.test.ts
git commit -m "feat(claude-code-assets): extract matchesClaudeCodeAsset helper with enabled filter"
```

---

## Task 2: RemoteAssetPicker 接入 enabledFilter

**Files:**
- Modify: `web/src/components/domain/RemoteAssetPicker/RemoteAssetPicker.tsx`

- [ ] **Step 1: 修改 Props 与 filter 逻辑**

在 `web/src/components/domain/RemoteAssetPicker/RemoteAssetPicker.tsx`：

1. 把现有 import 行：
```ts
import type { ClaudeCodeAsset, ClaudeCodeAssetKind } from '@/lib/types';
```
替换为：
```ts
import type { ClaudeCodeAsset } from '@/lib/types';
import { matchesClaudeCodeAsset } from '@/lib/claudeCodeAssets';
import type { EnabledFilter, KindFilter } from '@/lib/claudeCodeAssets';
```

2. 把 `RemoteAssetPickerProps` 中的：
```ts
  kind: ClaudeCodeAssetKind | 'all';
  search: string;
```
替换为：
```ts
  kind: KindFilter;
  search: string;
  enabledFilter: EnabledFilter;
```

3. 把函数签名解构：
```ts
export function RemoteAssetPicker({
  assets,
  selectedKeys,
  kind,
  search,
  onSelect,
  onSelectMany,
}: RemoteAssetPickerProps) {
```
替换为：
```ts
export function RemoteAssetPicker({
  assets,
  selectedKeys,
  kind,
  search,
  enabledFilter,
  onSelect,
  onSelectMany,
}: RemoteAssetPickerProps) {
```

4. 把 `visible` 的 useMemo：
```ts
  const visible = useMemo(() => {
    const q = search.trim().toLowerCase();
    return assets.filter((asset) => {
      const matchesKind = kind === 'all' || asset.kind === kind;
      const haystack = `${asset.name} ${asset.id} ${asset.source} ${asset.description ?? ''}`.toLowerCase();
      return matchesKind && (!q || haystack.includes(q));
    });
  }, [assets, kind, search]);
```
替换为：
```ts
  const visible = useMemo(
    () => assets.filter((asset) => matchesClaudeCodeAsset(asset, kind, search, enabledFilter)),
    [assets, kind, search, enabledFilter],
  );
```

- [ ] **Step 2: 类型检查（预期会有页面未传 enabledFilter 的错误，下一 task 修复）**

Run: `cd web && npx tsc --noEmit`
Expected: 在 `ClaudeCodeAssets.tsx` 处报 `Property 'enabledFilter' is missing` 错误（本 task 不修，下 task 修）

- [ ] **Step 3: 提交**

```bash
git add web/src/components/domain/RemoteAssetPicker/RemoteAssetPicker.tsx
git commit -m "feat(claude-code-assets): RemoteAssetPicker accepts enabledFilter prop"
```

---

## Task 3: 页面接入 lib + 新增 enabledFilter state 与分段控件

**Files:**
- Modify: `web/src/pages/ClaudeCodeAssets/ClaudeCodeAssets.tsx`

- [ ] **Step 1: 删除本地类型/常量/函数，从 lib 导入**

在 `web/src/pages/ClaudeCodeAssets/ClaudeCodeAssets.tsx`：

1. 把 import 块中：
```ts
import type {
  ClaudeCodeAsset,
  ClaudeCodeAssetInstallReport,
  ClaudeCodeAssetKind,
  ClaudeCodeAssetSelector,
  Device,
} from '@/lib/types';
```
替换为：
```ts
import type {
  ClaudeCodeAsset,
  ClaudeCodeAssetInstallReport,
  ClaudeCodeAssetSelector,
  Device,
} from '@/lib/types';
import {
  ENABLED_OPTIONS,
  KIND_OPTIONS,
  matchesClaudeCodeAsset,
} from '@/lib/claudeCodeAssets';
import type { EnabledFilter, KindFilter } from '@/lib/claudeCodeAssets';
```

2. 删除页面顶部的这三段（已被 lib 取代）：
```ts
type KindFilter = ClaudeCodeAssetKind | 'all';
type AssetTab = 'remote' | 'local';

const KIND_OPTIONS: KindFilter[] = ['all', 'skill', 'command', 'plugin', 'mcp'];
const ASSET_TABS: AssetTab[] = ['local', 'remote'];
```
替换为（保留 AssetTab，删除已在 lib 里的 KindFilter 和 KIND_OPTIONS）：
```ts
type AssetTab = 'remote' | 'local';

const ASSET_TABS: AssetTab[] = ['local', 'remote'];
```

3. 删除本地 `matchesAsset` 函数（整段，约第 43-50 行）：
```ts
/**
 * 判断资产是否匹配筛选条件。
 */
function matchesAsset(asset: ClaudeCodeAsset, kind: KindFilter, search: string): boolean {
  const q = search.trim().toLowerCase();
  const matchesKind = kind === 'all' || asset.kind === kind;
  const haystack = `${asset.name} ${asset.id} ${asset.source} ${asset.description ?? ''}`.toLowerCase();
  return matchesKind && (!q || haystack.includes(q));
}
```

- [ ] **Step 2: 新增 enabledFilter state**

在组件内 `const [kind, setKind] = useState<KindFilter>('all');` 这一行后面插入：
```ts
  const [enabledFilter, setEnabledFilter] = useState<EnabledFilter>('all');
```

- [ ] **Step 3: filteredAssets 改用新函数 + 依赖**

把：
```ts
  const filteredAssets = useMemo(
    () => assets.filter((asset) => matchesAsset(asset, kind, search)),
    [assets, kind, search],
  );
```
替换为：
```ts
  const filteredAssets = useMemo(
    () => assets.filter((asset) => matchesClaudeCodeAsset(asset, kind, search, enabledFilter)),
    [assets, kind, search, enabledFilter],
  );
```

- [ ] **Step 4: 筛选区右列加状态分段控件**

把现有筛选区 JSX（约 313-333 行）：
```tsx
            <div className={styles.filters}>
              <Input
                icon={<SearchIcon />}
                value={search}
                onChange={(e: ChangeEvent<HTMLInputElement>) => setSearch(e.currentTarget.value)}
                placeholder={t('claudeCodeAssets:searchPlaceholder')}
                aria-label={t('claudeCodeAssets:searchPlaceholder')}
              />
              <div className={styles.segmented}>
                {KIND_OPTIONS.map((option) => (
                  <button
                    key={option}
                    type="button"
                    className={option === kind ? styles.segmentActive : styles.segment}
                    onClick={() => setKind(option)}
                  >
                    {option === 'all' ? t('claudeCodeAssets:allKinds') : t(`claudeCodeAssets:kinds.${option}`)}
                  </button>
                ))}
              </div>
            </div>
```
替换为（用 `.filterGroup` 包装两个分段）：
```tsx
            <div className={styles.filters}>
              <Input
                icon={<SearchIcon />}
                value={search}
                onChange={(e: ChangeEvent<HTMLInputElement>) => setSearch(e.currentTarget.value)}
                placeholder={t('claudeCodeAssets:searchPlaceholder')}
                aria-label={t('claudeCodeAssets:searchPlaceholder')}
              />
              <div className={styles.filterGroup}>
                <div className={styles.segmented} role="group" aria-label={t('claudeCodeAssets:kindFilterLabel')}>
                  {KIND_OPTIONS.map((option) => (
                    <button
                      key={option}
                      type="button"
                      className={option === kind ? styles.segmentActive : styles.segment}
                      aria-pressed={option === kind}
                      onClick={() => setKind(option)}
                    >
                      {option === 'all' ? t('claudeCodeAssets:allKinds') : t(`claudeCodeAssets:kinds.${option}`)}
                    </button>
                  ))}
                </div>
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
              </div>
            </div>
```

- [ ] **Step 5: 给 RemoteAssetPicker 传 enabledFilter**

把：
```tsx
              <RemoteAssetPicker
                assets={remoteAssets}
                selectedKeys={selectedRemoteKeys}
                kind={kind}
                search={search}
                onSelect={handleRemoteSelect}
                onSelectMany={handleRemoteSelectMany}
              />
```
替换为：
```tsx
              <RemoteAssetPicker
                assets={remoteAssets}
                selectedKeys={selectedRemoteKeys}
                kind={kind}
                search={search}
                enabledFilter={enabledFilter}
                onSelect={handleRemoteSelect}
                onSelectMany={handleRemoteSelectMany}
              />
```

- [ ] **Step 6: 类型检查**

Run: `cd web && npx tsc --noEmit`
Expected: 无错误（页面与 RemoteAssetPicker 已对齐）

- [ ] **Step 7: 提交**

```bash
git add web/src/pages/ClaudeCodeAssets/ClaudeCodeAssets.tsx
git commit -m "feat(claude-code-assets): add enabled state segmented filter"
```

---

## Task 4: CSS 新增 .filterGroup

**Files:**
- Modify: `web/src/pages/ClaudeCodeAssets/ClaudeCodeAssets.module.css`

- [ ] **Step 1: 新增 .filterGroup 样式**

在 `web/src/pages/ClaudeCodeAssets/ClaudeCodeAssets.module.css` 现有 `.segmented { ... }` 规则块**之前**插入：

```css
.filterGroup {
  display: inline-flex;
  align-items: center;
  gap: var(--space-3);
  flex-wrap: wrap;
  justify-self: start;
}

```

`.filters` 的 `grid-template-columns` 和窄屏 `@media (max-width: 980px)` 现有规则**保持不变**（右列已是 `1fr`，能容纳新的 `.filterGroup`；窄屏 `1fr` 兜底已能让分段换行）。

- [ ] **Step 2: 提交**

```bash
git add web/src/pages/ClaudeCodeAssets/ClaudeCodeAssets.module.css
git commit -m "style(claude-code-assets): layout for kind + state segmented filters"
```

---

## Task 5: i18n 新增文案

**Files:**
- Modify: `web/src/i18n/locales/zh/claudeCodeAssets.json`
- Modify: `web/src/i18n/locales/en/claudeCodeAssets.json`

- [ ] **Step 1: zh 新增**

在 `web/src/i18n/locales/zh/claudeCodeAssets.json` 的 `"allKinds": "全部",` 这一行**后面**插入两行：
```json
  "allStates": "全部状态",
  "stateFilterLabel": "按启用状态筛选",
  "kindFilterLabel": "按类型筛选",
```

（最终该区域应为：）
```json
  "searchPlaceholder": "搜索名称、来源或说明",
  "allKinds": "全部",
  "allStates": "全部状态",
  "stateFilterLabel": "按启用状态筛选",
  "kindFilterLabel": "按类型筛选",
  "tabsLabel": "Claude Code 资产模块",
```

- [ ] **Step 2: en 新增**

在 `web/src/i18n/locales/en/claudeCodeAssets.json` 的 `"allKinds": "All",` 这一行**后面**插入两行：
```json
  "allStates": "All states",
  "stateFilterLabel": "Filter by enabled state",
  "kindFilterLabel": "Filter by type",
```

（最终该区域应为：）
```json
  "searchPlaceholder": "Search name, source, or description",
  "allKinds": "All",
  "allStates": "All states",
  "stateFilterLabel": "Filter by enabled state",
  "kindFilterLabel": "Filter by type",
  "tabsLabel": "Claude Code asset sections",
```

- [ ] **Step 3: 提交**

```bash
git add web/src/i18n/locales/zh/claudeCodeAssets.json web/src/i18n/locales/en/claudeCodeAssets.json
git commit -m "i18n(claude-code-assets): add state filter labels"
```

---

## Task 6: 验证 + 更新 CLAUDE.md

**Files:**
- Modify: `web/CLAUDE.md`

- [ ] **Step 1: 类型检查全量通过**

Run: `cd web && npx tsc --noEmit`
Expected: 无错误

- [ ] **Step 2: 跑 lib 单测**

Run: `cd web && npx tsx src/lib/claudeCodeAssets.test.ts`
Expected: 输出 `claudeCodeAssets.test.ts passed`

- [ ] **Step 3: 更新 web/CLAUDE.md**

在 `web/CLAUDE.md` 中找到 ClaudeCodeAssets 页面相关描述（搜索 `Claude Code` 或 `/claude-code` 或 `ClaudeCodeAssets`），在该页面的功能要点里补一条「按启用状态筛选（全部/已启用/已禁用）」，与现有「按类别筛选 / 文本搜索」并列。

如果该处现有描述是「支持搜索、分类筛选、启停、卸载」，改为「支持搜索、分类筛选、按启用状态筛选、启停、卸载」。

- [ ] **Step 4: 提交**

```bash
git add web/CLAUDE.md
git commit -m "docs(claude-code-assets): note enabled state filter in CLAUDE.md"
```

- [ ] **Step 5: 人工验收清单（交付给用户）**

启动 `./node_modules/.bin/tauri dev` 后访问 `/claude-code`，确认：
- 本机 tab：三态切换正确过滤列表（已启用 / 已禁用 / 全部）
- 类别 + 状态 + 搜索可叠加（如：plugin + 已禁用 + "gamma"）
- 切到局域网拉取 tab，状态筛选同样生效
- 窄屏（窗口拉窄到 < 980px）：两个分段能换行，不溢出
- kindSummary 统计面板数字不随状态筛选变化（仍是全量统计）

---

## Self-Review

**1. Spec 覆盖：**
- ✅ 三态筛选 → Task 1（lib 函数）+ Task 3（state + UI）
- ✅ 与 kind 分段并列的 inline 分段控件 → Task 3 Step 4 + Task 4 CSS
- ✅ 对 local + remote tab 同时生效 → Task 2（RemoteAssetPicker）+ Task 3（页面传 prop）
- ✅ 不改 Pill / 启停按钮 / kindSummary → 计划无任何对它们的修改
- ✅ i18n 新增 allStates / stateFilterLabel → Task 5（额外加了 kindFilterLabel 给 kind 分段的 aria-label，因为新 UI 给两个分段都加了 role=group）

**2. 占位符扫描：** 无 TBD/TODO/等价表达。所有代码块完整。

**3. 类型一致性：**
- `EnabledFilter` 在 Task 1 定义、Task 2 与 Task 3 引用 — 一致
- `matchesClaudeCodeAsset(asset, kind, search, enabledFilter)` 4 参数签名在 Task 1/2/3 三处使用 — 一致
- `KIND_OPTIONS` / `ENABLED_OPTIONS` 命名一致
- `enabledFilter` prop 名在 Task 2（RemoteAssetPickerProps）与 Task 3（页面传 prop）一致

**4. 改动文件清单：** 8 个文件（比 spec 的 4 个多 4 个：lib + lib.test + CLAUDE.md + 把 RemoteAssetPicker 改动单列），均为合理范围。
