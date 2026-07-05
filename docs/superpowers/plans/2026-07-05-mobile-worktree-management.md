# Mobile Worktree Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add mobile Workbench worktree visibility, quick switching, full worktree management, merge, and delete interactions using the approved `B + A` design.

**Architecture:** Keep `MobileWorkbench` as the single owner of active project/worktree/session state. Make the shell worktree status pill clickable, render a mobile quick-switch bottom sheet from the mobile business layer, and upgrade the existing Worktrees panel into the full management surface. Reuse the existing HTTP worktree transport and destructive flow helpers.

**Tech Stack:** React 19, TypeScript, Vite, CSS Modules, react-i18next, existing Workbench HTTP routes.

---

## Execution Setup

This implementation is expected to exceed 100 changed lines and touches multiple mobile frontend files. Execute it in an isolated git worktree and branch, then merge back only after verification:

```bash
git worktree add ../cc-partner-mobile-worktrees -b codex/mobile-worktree-management
cd ../cc-partner-mobile-worktrees
```

Expected: a new worktree on branch `codex/mobile-worktree-management`.

## File Structure

- Create `web/src/lib/workbenchWorktreeBranches.ts`: shared branch prefix/suffix helpers used by desktop and mobile.
- Modify `web/src/pages/Workbench/workbenchWorktrees.ts`: import and re-export branch helpers so existing desktop imports keep working.
- Modify `web/src/mobile/mobileWorkbenchState.ts`: add mobile worktree display/action pure helpers.
- Modify `web/src/mobile/mobileWorkbenchState.test.ts`: cover new quick switch and destructive-action helpers.
- Modify `web/src/mobile/mobilePanelState.ts`: make merge flow distinguish active and non-active source worktrees.
- Modify `web/src/mobile/mobilePanelState.test.ts`: cover inactive merge keeping Files dirty context untouched.
- Create `web/src/mobile/components/MobileWorktreeQuickSwitch.tsx`: bottom-sheet quick switch UI.
- Modify `web/src/mobile/components/MobileWorkbenchShell.tsx`: make the worktree status pill optionally clickable.
- Modify `web/src/mobile/MobileWorkbench.tsx`: own quick switch open state and wire shell, quick switch, Worktrees panel, and merge confirm.
- Modify `web/src/mobile/components/MobileWorktreePanel.tsx`: add prefix/suffix creation, richer status display, and merge action.
- Keep `web/src/mobile/components/MobileGitPanel.tsx` behavior intact; its existing merge button will inherit the parent-level merge confirm and destructive flow.
- Modify `web/src/mobile/MobileWorkbench.module.css`: add tokenized quick switch, status button, prefix form, and richer worktree card styles.
- Modify `web/src/i18n/locales/zh/workbench.json` and `web/src/i18n/locales/en/workbench.json`: add mobile quick switch and worktree panel strings.
- Modify `docs/prd.md`: update mobile Workbench/worktree requirements.
- Modify `web/CLAUDE.md`: concise memory update for mobile worktree quick switch and full management.

### Task 1: Shared Worktree Helpers And Mobile State Tests

**Files:**
- Create: `web/src/lib/workbenchWorktreeBranches.ts`
- Modify: `web/src/pages/Workbench/workbenchWorktrees.ts`
- Modify: `web/src/mobile/mobileWorkbenchState.ts`
- Modify: `web/src/mobile/mobileWorkbenchState.test.ts`
- Test: `web/src/pages/Workbench/workbenchWorktrees.test.ts`
- Test: `web/src/mobile/mobileWorkbenchState.test.ts`

- [ ] **Step 1: Create shared branch helper file**

Create `web/src/lib/workbenchWorktreeBranches.ts`:

```typescript
export const WORKTREE_BRANCH_PREFIXES = [
  'feature',
  'fix',
  'chore',
  'docs',
  'refactor',
  'test',
  'hotfix',
] as const;

export type WorktreeBranchPrefix = (typeof WORKTREE_BRANCH_PREFIXES)[number];
export const DEFAULT_WORKTREE_BRANCH_PREFIX: WorktreeBranchPrefix = 'feature';

/**
 * Business Logic（为什么需要这个函数）:
 *   新建 worktree 的分支名来自用户输入，空白输入不应触发后端创建。
 *
 * Code Logic（这个函数做什么）:
 *   清理输入两侧空白；结果为空时返回 null，否则返回可提交给后端的分支名。
 */
export function normalizeWorktreeBranchName(input: string): string | null {
  const branchName = input.trim();
  return branchName.length > 0 ? branchName : null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面端和移动端创建 worktree 都应使用同一套固定分支前缀，避免两个入口生成不同命名风格。
 *
 * Code Logic（这个函数做什么）:
 *   复用后缀清理逻辑；有效后缀返回 `prefix/suffix`，空后缀返回 null。
 */
export function composeWorktreeBranchName(
  prefix: WorktreeBranchPrefix,
  suffix: string,
): string | null {
  const branchSuffix = normalizeWorktreeBranchName(suffix);
  return branchSuffix ? `${prefix}/${branchSuffix}` : null;
}
```

- [ ] **Step 2: Re-export helpers from desktop worktree module**

In `web/src/pages/Workbench/workbenchWorktrees.ts`, remove the local definitions for `WORKTREE_BRANCH_PREFIXES`, `WorktreeBranchPrefix`, `DEFAULT_WORKTREE_BRANCH_PREFIX`, `normalizeWorktreeBranchName`, and `composeWorktreeBranchName`. Add this import/re-export block after the existing type imports:

```typescript
import {
  DEFAULT_WORKTREE_BRANCH_PREFIX,
  WORKTREE_BRANCH_PREFIXES,
  composeWorktreeBranchName,
  normalizeWorktreeBranchName,
} from '@/lib/workbenchWorktreeBranches';
import type { WorktreeBranchPrefix } from '@/lib/workbenchWorktreeBranches';

export {
  DEFAULT_WORKTREE_BRANCH_PREFIX,
  WORKTREE_BRANCH_PREFIXES,
  composeWorktreeBranchName,
  normalizeWorktreeBranchName,
};
export type { WorktreeBranchPrefix };
```

Keep existing desktop imports working; do not change `Workbench.tsx` imports in this task.

- [ ] **Step 3: Add mobile worktree pure helpers**

In `web/src/mobile/mobileWorkbenchState.ts`, add these exports after `selectPreferredMobileWorktree`:

```typescript
export type MobileWorktreeStatusKind = 'clean' | 'dirty' | 'conflict';

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 quick switch 和完整 Worktrees 面板都需要一致地判断 Git 状态优先级。
 *
 * Code Logic（这个函数做什么）:
 *   冲突优先，其次 dirty，最后 clean，返回供组件选择 i18n 文案的状态 kind。
 */
export function getMobileWorktreeStatusKind(worktree: WorkbenchWorktree): MobileWorktreeStatusKind {
  if (worktree.status.conflicts > 0) return 'conflict';
  if (worktree.status.changed > 0 || !worktree.status.clean) return 'dirty';
  return 'clean';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机端顶部 worktree pill 只有在选中本机项目且详情不忙时才应打开快速切换抽屉。
 *
 * Code Logic（这个函数做什么）:
 *   有 project 且 busy=false 时返回 true。
 */
export function canOpenMobileWorktreeSwitcher(
  project: WorkbenchProject | null,
  busy: boolean,
): boolean {
  return project !== null && !busy;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   合并和删除 worktree 是危险操作，主工作区和 busy 状态都必须禁用。
 *
 * Code Logic（这个函数做什么）:
 *   有 worktree、非 main 且 busy=false 时返回 true。
 */
export function canRunMobileWorktreeDestructiveAction(
  worktree: WorkbenchWorktree | null,
  busy: boolean,
): boolean {
  return worktree !== null && !worktree.isMain && !busy;
}
```

- [ ] **Step 4: Extend mobile state tests**

In `web/src/mobile/mobileWorkbenchState.test.ts`, import the new helpers and add these tests before the final test invocation block:

```typescript
function testMobileWorktreeStatusKindPrioritizesConflictDirtyClean(): void {
  const clean = createWorktree({ id: 'clean', name: 'clean', isMain: true });
  const dirty = createWorktree({
    id: 'dirty',
    name: 'dirty',
    status: { ...clean.status, changed: 2, clean: false },
  });
  const conflict = createWorktree({
    id: 'conflict',
    name: 'conflict',
    status: { ...clean.status, changed: 2, conflicts: 1, clean: false },
  });

  assertEqual(getMobileWorktreeStatusKind(clean), 'clean');
  assertEqual(getMobileWorktreeStatusKind(dirty), 'dirty');
  assertEqual(getMobileWorktreeStatusKind(conflict), 'conflict');
}

function testCanOpenMobileWorktreeSwitcherRequiresProjectAndIdleState(): void {
  const project = createProject({ id: 'local', name: 'local-app', kind: 'local' });

  assertEqual(canOpenMobileWorktreeSwitcher(project, false), true);
  assertEqual(canOpenMobileWorktreeSwitcher(project, true), false);
  assertEqual(canOpenMobileWorktreeSwitcher(null, false), false);
}

function testMobileDestructiveWorktreeActionRequiresLinkedIdleWorktree(): void {
  const main = createWorktree({ id: 'main', name: 'main', isMain: true });
  const feature = createWorktree({ id: 'feature', name: 'feature/task' });

  assertEqual(canRunMobileWorktreeDestructiveAction(feature, false), true);
  assertEqual(canRunMobileWorktreeDestructiveAction(feature, true), false);
  assertEqual(canRunMobileWorktreeDestructiveAction(main, false), false);
  assertEqual(canRunMobileWorktreeDestructiveAction(null, false), false);
}
```

Also call the three new test functions in the final invocation block.

- [ ] **Step 5: Run focused tests**

Run:

```bash
cd web
npx --yes tsx src/pages/Workbench/workbenchWorktrees.test.ts
npx --yes tsx src/mobile/mobileWorkbenchState.test.ts
```

Expected:

```text
workbenchWorktrees.test.ts passed
mobileWorkbenchState.test.ts passed
```

- [ ] **Step 6: Commit**

```bash
git add web/src/lib/workbenchWorktreeBranches.ts web/src/pages/Workbench/workbenchWorktrees.ts web/src/mobile/mobileWorkbenchState.ts web/src/mobile/mobileWorkbenchState.test.ts
git commit -m "refactor: share worktree branch helpers"
```

### Task 2: Quick Switch Bottom Sheet Component

**Files:**
- Create: `web/src/mobile/components/MobileWorktreeQuickSwitch.tsx`
- Modify: `web/src/mobile/MobileWorkbench.module.css`

- [ ] **Step 1: Create quick switch component**

Create `web/src/mobile/components/MobileWorktreeQuickSwitch.tsx`:

```tsx
import type { ReactElement } from 'react';
import { useCallback } from 'react';
import type { TFunction } from 'i18next';
import { useTranslation } from 'react-i18next';
import type { WorkbenchProject, WorkbenchWorktree } from '@/lib/types';
import {
  getMobileWorktreeStatusKind,
  type MobileWorkbenchPanel,
} from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';

export interface MobileWorktreeQuickSwitchProps {
  open: boolean;
  project: WorkbenchProject | null;
  worktrees: WorkbenchWorktree[];
  activeWorktreeId: string | null;
  busy?: boolean;
  onClose: () => void;
  onSelect: (worktree: WorkbenchWorktree) => boolean | void;
  onPanelChange: (panel: MobileWorkbenchPanel) => void;
  onRefresh: () => Promise<void> | void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   快速切换抽屉和完整 Worktrees 面板需要一致展示 Git 状态文案，帮助用户判断切换风险。
 *
 * Code Logic（这个函数做什么）:
 *   根据状态 kind 选择对应 i18n key；dirty/conflict 使用后端返回的数量插值。
 */
function formatWorktreeStatusLabel(
  worktree: WorkbenchWorktree,
  t: TFunction<'workbench'>,
): string {
  const kind = getMobileWorktreeStatusKind(worktree);
  if (kind === 'conflict') {
    return t('workbench:worktrees.status.conflict', { count: worktree.status.conflicts });
  }
  if (kind === 'dirty') {
    return t('workbench:worktrees.status.dirty', { count: worktree.status.changed });
  }
  return t('workbench:worktrees.status.clean');
}

/**
 * MobileWorktreeQuickSwitch（移动端 worktree 快速切换抽屉）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端用户在 Terminal、Files、Git、Prompt 任意面板里都需要快速查看并切换当前 worktree，而不必先打开完整管理页。
 *
 * Code Logic（这个组件做什么）:
 *   作为 fixed bottom sheet 展示当前项目的 worktree 列表；列表项复用父级 onSelect 触发 dirty guard，管理按钮切到 Worktrees 面板。
 */
export function MobileWorktreeQuickSwitch({
  open,
  project,
  worktrees,
  activeWorktreeId,
  busy = false,
  onClose,
  onSelect,
  onPanelChange,
  onRefresh,
}: MobileWorktreeQuickSwitchProps): ReactElement | null {
  const { t } = useTranslation(['workbench']);

  const handleManage = useCallback((): void => {
    onPanelChange('worktrees');
    onClose();
  }, [onClose, onPanelChange]);

  if (!open) return null;

  return (
    <>
      <button
        type="button"
        className={styles.quickSwitchBackdrop}
        aria-label={t('workbench:mobile.worktreeQuickSwitch.close')}
        onClick={onClose}
      />
      <section
        className={styles.quickSwitchSheet}
        aria-labelledby="mobile-worktree-quick-switch-title"
      >
        <div className={styles.quickSwitchHeader}>
          <div className={styles.titleBlock}>
            <p className={styles.topTitle}>{project?.name ?? t('workbench:mobile.noProject')}</p>
            <h2 id="mobile-worktree-quick-switch-title" className={styles.quickSwitchTitle}>
              {t('workbench:mobile.worktreeQuickSwitch.title')}
            </h2>
          </div>
          <button type="button" className={styles.secondaryButton} onClick={onClose}>
            {t('workbench:mobile.worktreeQuickSwitch.close')}
          </button>
        </div>

        <div className={styles.mobileToolbar}>
          <button
            type="button"
            className={styles.secondaryButton}
            disabled={!project || busy}
            onClick={() => void onRefresh()}
          >
            {t('workbench:refresh')}
          </button>
          <button type="button" className={styles.mobileTerminalPrimaryButton} onClick={handleManage}>
            {t('workbench:mobile.worktreeQuickSwitch.manage')}
          </button>
        </div>

        {!project ? (
          <p className={styles.panelState}>{t('workbench:mobile.worktreePanel.noProject')}</p>
        ) : null}
        {project && worktrees.length === 0 ? (
          <p className={styles.panelState}>{t('workbench:worktrees.empty')}</p>
        ) : null}

        <div className={styles.quickSwitchList}>
          {worktrees.map((worktree) => {
            const isActive = worktree.id === activeWorktreeId;
            return (
              <button
                key={worktree.id}
                type="button"
                className={`${styles.quickSwitchItem} ${isActive ? styles.mobileListItemActive : ''}`}
                aria-pressed={isActive}
                disabled={busy}
                onClick={() => {
                  const didSelect = onSelect(worktree);
                  if (didSelect !== false) onClose();
                }}
              >
                <span className={styles.mobileListTitleRow}>
                  <strong className={styles.mobileListTitle}>{worktree.name}</strong>
                  <span className={`${styles.mobileBadge} ${worktree.isMain ? styles.mobileBadgeAccent : ''}`}>
                    {worktree.isMain ? t('workbench:worktrees.main') : t('workbench:worktrees.linked')}
                  </span>
                </span>
                <span className={styles.mobileListPath}>
                  {worktree.branch ?? t('workbench:emptyValue')}
                </span>
                <span className={styles.mobileBadgeRow}>
                  <span className={styles.mobileBadge}>{formatWorktreeStatusLabel(worktree, t)}</span>
                  <span className={styles.mobileBadge}>
                    {t('workbench:mobile.worktreeQuickSwitch.sync', {
                      ahead: worktree.status.ahead,
                      behind: worktree.status.behind,
                    })}
                  </span>
                </span>
              </button>
            );
          })}
        </div>
      </section>
    </>
  );
}
```

- [ ] **Step 2: Add quick switch CSS**

Append to `web/src/mobile/MobileWorkbench.module.css` before the media queries:

```css
.quickSwitchBackdrop {
  position: fixed;
  inset: 0;
  background: color-mix(in oklab, var(--fg) 42%, transparent);
  z-index: var(--z-overlay);
  transition: all var(--motion-fast) var(--ease-standard);
}

.quickSwitchSheet {
  position: fixed;
  inset: auto var(--space-2) calc(env(safe-area-inset-bottom) + var(--space-2));
  max-height: min(72dvh, calc(var(--space-24) * 6));
  padding: var(--space-3);
  display: grid;
  gap: var(--space-3);
  overflow: auto;
  color: var(--fg);
  background: var(--surface);
  border: 1px solid var(--border-soft);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-lg);
  z-index: var(--z-modal);
}

.quickSwitchHeader {
  min-width: 0;
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--space-3);
}

.quickSwitchTitle {
  margin: 0;
  color: var(--muted);
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
  line-height: var(--leading-tight);
  letter-spacing: var(--tracking-normal);
}

.quickSwitchList {
  min-width: 0;
  display: grid;
  gap: var(--space-2);
}

.quickSwitchItem {
  width: 100%;
  min-width: 0;
  padding: var(--space-3);
  display: grid;
  gap: var(--space-2);
  color: var(--fg);
  background: var(--surface);
  border: 1px solid var(--border-soft);
  border-radius: var(--radius-md);
  text-align: left;
  transition: all var(--motion-fast) var(--ease-standard);
}

.quickSwitchItem:hover:not(:disabled) {
  color: var(--accent);
  background: var(--accent-soft);
  border-color: var(--accent);
}

.quickSwitchItem:disabled {
  cursor: not-allowed;
  color: var(--muted);
  background: var(--surface-warm);
}
```

- [ ] **Step 3: Commit**

```bash
git add web/src/mobile/components/MobileWorktreeQuickSwitch.tsx web/src/mobile/MobileWorkbench.module.css
git commit -m "feat: add mobile worktree quick switch sheet"
```

### Task 3: Shell Status Pill And Parent Wiring

**Files:**
- Modify: `web/src/lib/icons.tsx`
- Modify: `web/src/mobile/components/MobileWorkbenchShell.tsx`
- Modify: `web/src/mobile/MobileWorkbench.tsx`
- Modify: `web/src/mobile/MobileWorkbench.module.css`

- [ ] **Step 1: Add dropdown icon**

In `web/src/lib/icons.tsx`, add this icon near other chevrons:

```tsx
export const ChevronDownIcon = ({ size, ...rest }: IconProps) => (
  <svg width={size ?? 16} height={size ?? 16} viewBox="0 0 16 16" fill="none" {...rest}>
    <path d="M4 6l4 4 4-4" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);
```

- [ ] **Step 2: Make shell worktree pill clickable**

In `web/src/mobile/components/MobileWorkbenchShell.tsx`:

Add `ChevronDownIcon` import.

Extend `MobileWorkbenchShellProps`:

```tsx
  worktreeStatusDisabled?: boolean;
  onWorktreeStatusClick?: () => void;
```

Destructure these props in `MobileWorkbenchShell`.

Replace the static worktree status span with:

```tsx
          {onWorktreeStatusClick ? (
            <button
              type="button"
              className={`${styles.statusPill} ${styles.statusPillButton}`}
              disabled={worktreeStatusDisabled}
              aria-haspopup="dialog"
              onClick={onWorktreeStatusClick}
            >
              <span>{worktree ?? t('workbench:mobile.status.worktree')}</span>
              <ChevronDownIcon size={14} aria-hidden="true" />
            </button>
          ) : (
            <span className={styles.statusPill}>
              {worktree ?? t('workbench:mobile.status.worktree')}
            </span>
          )}
```

- [ ] **Step 3: Add status pill button CSS**

In `web/src/mobile/MobileWorkbench.module.css`, add:

```css
.statusPillButton {
  gap: var(--space-1);
  max-width: calc(var(--space-24) * 3);
  cursor: pointer;
  transition: all var(--motion-fast) var(--ease-standard);
}

.statusPillButton span {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
}

.statusPillButton:hover:not(:disabled) {
  color: var(--accent);
  background: var(--accent-soft);
  border-color: var(--accent);
}

.statusPillButton:disabled {
  cursor: not-allowed;
  color: var(--muted);
  background: var(--surface-warm);
}
```

Also include `.statusPillButton:focus-visible` in the existing focus-visible selector with `menuButton`, `closeButton`, and `navItem`.

- [ ] **Step 4: Wire parent quick switch state**

In `web/src/mobile/MobileWorkbench.tsx`:

Import `MobileWorktreeQuickSwitch` and `canOpenMobileWorktreeSwitcher`.

Add state after `projectDetailsLoading`:

```tsx
  const [worktreeSwitcherOpen, setWorktreeSwitcherOpen] = useState<boolean>(false);
```

Add handlers after `handleSelectWorktree`:

```tsx
  /**
   * Business Logic（为什么需要这个函数）:
   *   顶部状态栏 worktree pill 是移动端快速切换入口，只有项目详情可用时才打开。
   *
   * Code Logic（这个函数做什么）:
   *   复用 canOpenMobileWorktreeSwitcher 判断可用性，通过 state 展开 bottom sheet。
   */
  const handleOpenWorktreeSwitcher = useCallback((): void => {
    if (!canOpenMobileWorktreeSwitcher(activeProjectRef.current, projectDetailsLoading)) return;
    setWorktreeSwitcherOpen(true);
  }, [projectDetailsLoading]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户从快速切换抽屉进入完整管理页后，应关闭抽屉并切到 Worktrees 面板。
   *
   * Code Logic（这个函数做什么）:
   *   写入 panel='worktrees' 并关闭 quick switch。
   */
  const handleQuickSwitchPanelChange = useCallback((nextPanel: MobileWorkbenchPanel): void => {
    setPanel(nextPanel);
    setWorktreeSwitcherOpen(false);
  }, []);
```

Close the quick switch when selecting a new project by adding `setWorktreeSwitcherOpen(false);` in `selectProject` before `setProjectDetailsLoading(true)`.

Pass shell props:

```tsx
      worktreeStatusDisabled={!canOpenMobileWorktreeSwitcher(activeProject, projectDetailsLoading)}
      onWorktreeStatusClick={handleOpenWorktreeSwitcher}
```

Render quick switch inside `MobileWorkbenchShell` children, after `panelContent`:

```tsx
      <MobileWorktreeQuickSwitch
        open={worktreeSwitcherOpen}
        project={activeProject}
        worktrees={worktrees}
        activeWorktreeId={activeWorktree?.id ?? null}
        busy={projectDetailsLoading}
        onClose={() => setWorktreeSwitcherOpen(false)}
        onSelect={handleSelectWorktree}
        onPanelChange={handleQuickSwitchPanelChange}
        onRefresh={() => refreshWorktrees({ expectedProjectId: activeProjectRef.current?.id })}
      />
```

- [ ] **Step 5: Run focused type check**

Run:

```bash
cd web
npx tsc --noEmit
```

Expected: command exits 0.

- [ ] **Step 6: Commit**

```bash
git add web/src/lib/icons.tsx web/src/mobile/components/MobileWorkbenchShell.tsx web/src/mobile/MobileWorkbench.tsx web/src/mobile/MobileWorkbench.module.css
git commit -m "feat: wire mobile worktree quick switch"
```

### Task 4: Full Worktrees Panel Merge And Prefix Form

**Files:**
- Modify: `web/src/mobile/components/MobileWorktreePanel.tsx`
- Modify: `web/src/mobile/MobileWorkbench.tsx`
- Modify: `web/src/mobile/mobilePanelState.ts`
- Modify: `web/src/mobile/mobilePanelState.test.ts`
- Modify: `web/src/mobile/MobileWorkbench.module.css`

- [ ] **Step 1: Fix merge flow active-source semantics**

In `web/src/mobile/mobilePanelState.ts`, extend `MobileWorktreeMergeFlowOptions`:

```typescript
export interface MobileWorktreeMergeFlowOptions {
  worktrees: WorkbenchWorktree[];
  activeWorktreeId: string | null;
  sourceWorktree: WorkbenchWorktree;
  confirmActiveWorktreeChange: (nextActive: WorkbenchWorktree | null) => boolean;
  mergeWorktree: () => Promise<void>;
  applyMergeSuccess: (plan: MobileWorktreeRemovalPlan) => void | Promise<void>;
}
```

Update `runMobileWorktreeMergeFlow` to pass the real active id:

```typescript
  const plan = getMobileWorktreeRemovalPlan(
    options.worktrees,
    options.activeWorktreeId,
    options.sourceWorktree,
  );

  if (
    plan.requiresActivePreflight &&
    !options.confirmActiveWorktreeChange(plan.nextActive)
  ) {
    return 'cancelled';
  }
```

This keeps dirty guard limited to merges that remove the current active worktree.

- [ ] **Step 2: Add inactive merge tests**

In `web/src/mobile/mobilePanelState.test.ts`, update existing `runMobileWorktreeMergeFlow` calls to pass `activeWorktreeId: feature.id` where the source is active.

Add this test before `testMergeSuccessAppliedStateUsesRemovalPlan`:

```typescript
/**
 * Business Logic（为什么需要这个函数）:
 *   完整 Worktrees 面板允许合并非 active worktree；此时用户没有离开当前 Files 草稿上下文，不能弹 dirty guard 或清草稿。
 *
 * Code Logic（这个函数做什么）:
 *   构造 main 为 active、feature 为 merge source，断言 confirm 不执行且 apply 仍保留 main 为 active。
 */
async function testInactiveMergeSkipsDirtyGuardAndKeepsActive(): Promise<void> {
  const main = createWorktree('main', true);
  const feature = createWorktree('feature/merge-me', false);
  let didConfirm = false;
  let appliedActiveId: string | null = null;

  const result = await runMobileWorktreeMergeFlow({
    worktrees: [main, feature],
    activeWorktreeId: main.id,
    sourceWorktree: feature,
    confirmActiveWorktreeChange: () => {
      didConfirm = true;
      return true;
    },
    mergeWorktree: async () => undefined,
    applyMergeSuccess: (plan) => {
      appliedActiveId = plan.nextActive?.id ?? null;
    },
  });

  assertEqual(result, 'applied', 'inactive merge should still apply after backend success');
  assertEqual(didConfirm, false, 'inactive merge should not run dirty guard confirm');
  assertEqual(appliedActiveId, main.id, 'inactive merge should keep current active worktree');
}
```

Call it in `runTests()`.

- [ ] **Step 3: Extend panel props**

In `MobileWorktreePanelProps`, add:

```tsx
  onMergeWorktree: (worktree: WorkbenchWorktree) => Promise<boolean>;
```

Import branch helpers:

```tsx
import {
  DEFAULT_WORKTREE_BRANCH_PREFIX,
  WORKTREE_BRANCH_PREFIXES,
  composeWorktreeBranchName,
} from '@/lib/workbenchWorktreeBranches';
import type { WorktreeBranchPrefix } from '@/lib/workbenchWorktreeBranches';
import {
  canRunMobileWorktreeDestructiveAction,
  getMobileWorktreeStatusKind,
} from '../mobileWorkbenchState';
```

- [ ] **Step 4: Replace free branch input with prefix/suffix state**

Replace:

```tsx
  const [branchName, setBranchName] = useState<string>('');
```

with:

```tsx
  const [branchPrefix, setBranchPrefix] = useState<WorktreeBranchPrefix>(
    DEFAULT_WORKTREE_BRANCH_PREFIX,
  );
  const [branchSuffix, setBranchSuffix] = useState<string>('');
  const composedBranchName = composeWorktreeBranchName(branchPrefix, branchSuffix);
```

Update `handleCreateWorktree` to use `composedBranchName`:

```tsx
    if (!project || !composedBranchName) return;
    setActionBusy('create');
    setError(null);
    try {
      const created = await httpWorkbenchTransport.worktrees.create(
        project.id,
        composedBranchName,
        null,
      );
      const nextWorktrees = [...worktrees.filter((item) => item.id !== created.id), created];
      const didApplyActive = applyWorktrees(nextWorktrees, created);
      setBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
      setBranchSuffix('');
      if (didApplyActive) {
        await onRefreshWorktrees?.({ expectedProjectId: project.id });
      }
```

- [ ] **Step 5: Add merge handler**

Add after `handleRemoveWorktree`:

```tsx
  /**
   * Business Logic（为什么需要这个函数）:
   *   完整 Worktrees 面板是移动端合并功能 worktree 的主入口，主工作区不能合并。
   *
   * Code Logic（这个函数做什么）:
   *   调用父级 merge flow；父级负责二次确认、dirty guard、后端 merge 与 active fallback。
   */
  const handleMergeWorktree = useCallback(
    async (worktree: WorkbenchWorktree): Promise<void> => {
      if (!canRunMobileWorktreeDestructiveAction(worktree, isActionDisabled)) return;
      setActionBusy(`merge-${worktree.id}`);
      setError(null);
      try {
        await onMergeWorktree(worktree);
      } catch (reason) {
        setError(`${t('workbench:errors.mergeWorktree')}: ${getErrorMessage(reason)}`);
      } finally {
        setActionBusy(null);
      }
    },
    [isActionDisabled, onMergeWorktree, t],
  );
```

- [ ] **Step 6: Update create form JSX**

Replace the single input with:

```tsx
      <div className={styles.mobileFormInline}>
        <label className={styles.mobileField}>
          <span>{t('workbench:worktrees.prefixLabel')}</span>
          <select
            className={styles.mobileSelect}
            value={branchPrefix}
            disabled={!project || isActionDisabled}
            onChange={(event) => setBranchPrefix(event.target.value as WorktreeBranchPrefix)}
          >
            {WORKTREE_BRANCH_PREFIXES.map((prefix) => (
              <option key={prefix} value={prefix}>
                {prefix}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.mobileField}>
          <span>{t('workbench:worktrees.suffixLabel')}</span>
          <input
            className={styles.mobileInput}
            value={branchSuffix}
            disabled={!project || isActionDisabled}
            placeholder={t('workbench:worktrees.suffixPlaceholder')}
            onChange={(event) => setBranchSuffix(event.target.value)}
          />
        </label>
        <button
          type="button"
          className={styles.mobileTerminalPrimaryButton}
          disabled={!project || !composedBranchName || isActionDisabled}
          onClick={() => void handleCreateWorktree()}
        >
          {actionBusy === 'create'
            ? t('workbench:mobile.worktreePanel.creating')
            : t('workbench:worktrees.create')}
        </button>
      </div>
```

- [ ] **Step 7: Render richer worktree cards**

Inside `worktrees.map`, compute:

```tsx
          const statusKind = getMobileWorktreeStatusKind(worktree);
          const statusLabel =
            statusKind === 'conflict'
              ? t('workbench:worktrees.status.conflict', { count: worktree.status.conflicts })
              : statusKind === 'dirty'
                ? t('workbench:worktrees.status.dirty', { count: worktree.status.changed })
                : t('workbench:worktrees.status.clean');
          const canMergeOrRemove = canRunMobileWorktreeDestructiveAction(
            worktree,
            isActionDisabled,
          );
```

In the card body, after branch, add path and badge row:

```tsx
                <span className={styles.mobileListPath}>
                  {worktree.branch ?? t('workbench:emptyValue')}
                </span>
                <span className={styles.mobileListPath}>{worktree.path}</span>
                <span className={styles.mobileBadgeRow}>
                  <span className={styles.mobileBadge}>{statusLabel}</span>
                  <span className={styles.mobileBadge}>
                    {t('workbench:mobile.worktreePanel.sync', {
                      ahead: worktree.status.ahead,
                      behind: worktree.status.behind,
                    })}
                  </span>
                  <span className={styles.mobileBadge}>
                    {worktree.status.canPush
                      ? t('workbench:mobile.gitPanel.canPushAllowed')
                      : t('workbench:mobile.gitPanel.canPushBlocked')}
                  </span>
                </span>
```

Add Merge button before Remove:

```tsx
                <button
                  type="button"
                  className={styles.secondaryButton}
                  disabled={!canMergeOrRemove}
                  onClick={() => void handleMergeWorktree(worktree)}
                >
                  {actionBusy === `merge-${worktree.id}`
                    ? t('workbench:mobile.worktreePanel.merging')
                    : t('workbench:worktrees.merge')}
                </button>
```

Change Remove disabled to `disabled={!canMergeOrRemove}`.

- [ ] **Step 8: Wire merge prop and parent confirm**

In `web/src/mobile/MobileWorkbench.tsx`, pass `onMergeWorktree={handleMergeWorktree}` into `MobileWorktreePanel`.

Update `handleMergeWorktree` to include destructive confirmation before `runMobileWorktreeMergeFlow`:

```tsx
      if (sourceWorktree.isMain) return false;
      if (!window.confirm(t('workbench:worktrees.mergeConfirm', { name: sourceWorktree.name }))) {
        return false;
      }
```

Place this before `const operationProjectId = ...`.

Also pass the real active id into the merge flow and only discard Files dirty context when the source was active:

```tsx
      const result = await runMobileWorktreeMergeFlow({
        worktrees,
        activeWorktreeId: activeWorktreeRef.current?.id ?? null,
        sourceWorktree,
        confirmActiveWorktreeChange: (nextActive) =>
          handleConfirmActiveWorktreeChange(nextActive),
        mergeWorktree: async () => {
          await httpWorkbenchTransport.worktrees.merge(sourceWorktree.id);
        },
        applyMergeSuccess: async (plan) => {
          if (activeProjectRef.current?.id !== operationProjectId) return;
          const appliedState = getMobileWorktreeMergeAppliedState(plan);
          setWorktrees(appliedState.nextWorktrees);
          if (plan.requiresActivePreflight) {
            discardConfirmedFileContextSwitch();
            setActiveWorktreeWithSession(appliedState.nextActive);
          }
          await refreshWorktrees({
            skipFileContextConfirm: plan.requiresActivePreflight,
            expectedProjectId: operationProjectId ?? undefined,
          });
        },
      });
```

- [ ] **Step 9: Run merge helper tests**

Run:

```bash
cd web
npx --yes tsx src/mobile/mobilePanelState.test.ts
```

Expected:

```text
mobilePanelState.test.ts passed
```

- [ ] **Step 10: Commit**

```bash
git add web/src/mobile/components/MobileWorktreePanel.tsx web/src/mobile/MobileWorkbench.tsx web/src/mobile/mobilePanelState.ts web/src/mobile/mobilePanelState.test.ts web/src/mobile/MobileWorkbench.module.css
git commit -m "feat: complete mobile worktree management panel"
```

### Task 5: I18n And Mobile Style Polish

**Files:**
- Modify: `web/src/i18n/locales/zh/workbench.json`
- Modify: `web/src/i18n/locales/en/workbench.json`
- Modify: `web/src/mobile/MobileWorkbench.module.css`

- [ ] **Step 1: Add Chinese strings**

In `web/src/i18n/locales/zh/workbench.json`, add under `mobile`:

```json
"worktreeQuickSwitch": {
  "title": "快速切换 worktree",
  "close": "关闭",
  "manage": "管理 Worktrees",
  "sync": "领先 {{ahead}} / 落后 {{behind}}"
}
```

Add under `mobile.worktreePanel`:

```json
"merging": "合并中",
"sync": "领先 {{ahead}} / 落后 {{behind}}"
```

- [ ] **Step 2: Add English strings**

In `web/src/i18n/locales/en/workbench.json`, add under `mobile`:

```json
"worktreeQuickSwitch": {
  "title": "Quick switch worktree",
  "close": "Close",
  "manage": "Manage Worktrees",
  "sync": "Ahead {{ahead}} / behind {{behind}}"
}
```

Add under `mobile.worktreePanel`:

```json
"merging": "Merging",
"sync": "Ahead {{ahead}} / behind {{behind}}"
```

- [ ] **Step 3: Ensure touch targets and safe-area behavior**

Verify `MobileWorkbench.module.css` has these properties:

```css
.quickSwitchSheet {
  inset: auto var(--space-2) calc(env(safe-area-inset-bottom) + var(--space-2));
}

.quickSwitchItem,
.statusPillButton,
.secondaryButton,
.mobileTerminalPrimaryButton {
  transition: all var(--motion-fast) var(--ease-standard);
}
```

If `.mobileTerminalPrimaryButton` is already defined with transition, do not duplicate it.

- [ ] **Step 4: Run type check**

Run:

```bash
cd web
npx tsc --noEmit
```

Expected: command exits 0 with no i18n key type errors.

- [ ] **Step 5: Commit**

```bash
git add web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json web/src/mobile/MobileWorkbench.module.css
git commit -m "feat: localize mobile worktree controls"
```

### Task 6: Documentation And Verification

**Files:**
- Modify: `docs/prd.md`
- Modify: `web/CLAUDE.md`
- Test: mobile helper tests and TypeScript.

- [ ] **Step 1: Update PRD mobile Workbench requirement**

In `docs/prd.md`, update the mobile Workbench bullet around the existing `/mobile` section to include:

```markdown
移动端 shell 的 worktree 状态 pill 可点击打开快速切换抽屉，抽屉只提供查看、刷新、切换和进入完整 Worktrees 面板，不承载合并/删除等危险操作；完整 Worktrees 面板展示所有 worktree 的 branch/path/clean-dirty-conflict/ahead-behind/canPush 状态，并支持 prefix+suffix 新建、切换、合并非主 worktree 和删除非主 worktree。所有切换、合并和删除必须复用 Files dirty guard，合并/删除只有后端成功后才清理草稿并切到 fallback worktree。
```

Keep the existing PRD wording about remote shortcuts, default active worktree/session, terminal replay, and pane controls.

- [ ] **Step 2: Update web project memory**

In `web/CLAUDE.md`, update the Mobile SPA paragraph concisely:

```markdown
`/mobile` shell 的 worktree status pill 是快速切换入口，点击打开 bottom sheet，仅支持查看/刷新/切换/进入 Worktrees 管理页；合并和删除不放在 quick switch 内。`MobileWorktreePanel` 是完整管理面，展示 branch/path/status/ahead-behind/canPush，创建 worktree 使用桌面端同源 prefix+suffix 规则，合并/删除非 main worktree 复用父级 destructive flow 与 Files dirty guard。
```

Keep the document concise and avoid adding a change log.

- [ ] **Step 3: Run focused verification**

Run:

```bash
cd web
npx --yes tsx src/pages/Workbench/workbenchWorktrees.test.ts
npx --yes tsx src/mobile/mobileWorkbenchState.test.ts
npx --yes tsx src/mobile/mobilePanelState.test.ts
npx --yes tsx src/mobile/mobileTerminalReplay.test.ts
npx tsc --noEmit
```

Expected:

```text
workbenchWorktrees.test.ts passed
mobileWorkbenchState.test.ts passed
mobilePanelState.test.ts passed
mobileTerminalReplay.test.ts passed
```

`npx tsc --noEmit` exits 0.

- [ ] **Step 4: Inspect final diff**

Run:

```bash
git diff --stat master...HEAD
git diff -- web/src/mobile web/src/lib/workbenchWorktreeBranches.ts web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json docs/prd.md web/CLAUDE.md
```

Expected: diff only contains mobile worktree management, shared branch helpers, i18n, and required docs.

- [ ] **Step 5: Commit docs and verification-ready state**

```bash
git add docs/prd.md web/CLAUDE.md
git commit -m "docs: document mobile worktree management"
```

If Step 3 required small fixes, include those files in the same commit only when they are direct fixes for verification failures.

## Plan Self-Review

- Spec coverage: quick switch, full management panel, merge/delete safety, prefix/suffix creation, tests, PRD, and project memory are all covered.
- No backend task is included because the approved design reuses existing mobile HTTP worktree routes.
- No quick-switch merge/delete shortcut is included, matching the resolved design decision.
- Verification is scoped to mobile worktree helpers, existing mobile replay safety, shared desktop worktree helper tests, and TypeScript.
