// @vitest-environment jsdom
/**
 * MobileWorktreeTabs（移动端 worktree 工作区 tab 列表）单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   验证 chip 列表的渲染、active 高亮、点击触发 onSelect、busy 禁用、键盘导航与
 *   空 worktree 时不渲染 strip 等核心合同；防止新组件破坏 mobile worktree 切换路径。
 *
 * Code Logic（这个测试做什么）:
 *   - 静态断言：组件源码用 nav 而非手写 dialog 角色、复用 worktreeStatusTone；
 *   - 行为断言：通过 RTL 渲染 + i18n，验证 chip 渲染、aria-current、busy、点击与键盘。
 */
import { afterEach, describe, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { ReactElement } from 'react';
import type { TFunction } from 'i18next';
import type { WorkbenchWorktree } from '@/lib/types';
import { MobileWorktreeTabs } from './MobileWorktreeTabs';

function buildWorktree(
  overrides: Partial<WorkbenchWorktree> & Pick<WorkbenchWorktree, 'id' | 'name'>,
): WorkbenchWorktree {
  return {
    id: overrides.id,
    projectId: overrides.projectId ?? 'project-1',
    name: overrides.name,
    branch: overrides.branch ?? overrides.name,
    baseBranch: overrides.baseBranch ?? null,
    path: overrides.path ?? `/tmp/cc-partner/${overrides.name}`,
    isMain: overrides.isMain ?? false,
    canCollectMerge: overrides.canCollectMerge ?? false,
    homeBranch: overrides.homeBranch ?? null,
    collectibleBranches: overrides.collectibleBranches ?? [],
    status: overrides.status ?? {
      branch: overrides.branch ?? overrides.name,
      changed: 0,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: true,
      canPush: false,
    },
    createdAt: '2026-07-05T00:00:00Z',
    updatedAt: '2026-07-05T00:00:00Z',
  };
}

function buildT(translation: Record<string, string> = {}): TFunction<'workbench'> {
  const dict: Record<string, string> = {
    'mobile.worktreeTabs.title': 'Worktree 工作区',
    ...translation,
  };
  return ((key: string) => dict[key] ?? key) as TFunction<'workbench'>;
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function renderTabs(
  worktrees: WorkbenchWorktree[],
  activeWorktreeId: string | null,
  options: {
    busy?: boolean;
    onSelect?: (worktree: WorkbenchWorktree) => boolean | void;
  } = {},
): ReactElement {
  const onSelect = options.onSelect ?? vi.fn();
  return (
    <MobileWorktreeTabs
      worktrees={worktrees}
      activeWorktreeId={activeWorktreeId}
      busy={options.busy ?? false}
      onSelect={onSelect}
      t={buildT()}
    />
  );
}

describe('MobileWorktreeTabs', () => {
  test('renders chip list with active highlight and tone from worktreeStatusTone', () => {
    const worktrees = [
      buildWorktree({ id: 'main', name: 'main', isMain: true }),
      buildWorktree({
        id: 'feature',
        name: 'feature/mobile',
        branch: 'feature/mobile',
        status: {
          branch: 'feature/mobile',
          changed: 2,
          ahead: 0,
          behind: 0,
          conflicts: 1,
          clean: false,
          canPush: false,
        },
      }),
    ];
    render(renderTabs(worktrees, 'main'));

    const nav = screen.getByTestId('mobile-worktree-tabs');
    if (nav.tagName.toLowerCase() !== 'nav') {
      throw new Error('MobileWorktreeTabs root should be a <nav>');
    }
    if (nav.getAttribute('aria-label') !== 'Worktree 工作区') {
      throw new Error('nav should expose localized aria-label');
    }

    const chips = screen.getAllByTestId('mobile-worktree-tabs').length > 0
      ? Array.from(
          (screen.getByTestId('mobile-worktree-tabs') as HTMLElement).querySelectorAll<HTMLButtonElement>(
            'button[data-mobile-worktree-chip]',
          ),
        )
      : [];
    if (chips.length !== 2) {
      throw new Error(`expected 2 chips, received ${chips.length}`);
    }
    const [mainChip, featureChip] = chips;
    if (!mainChip || !featureChip) throw new Error('expected two chips');

    if (mainChip.getAttribute('aria-current') !== 'page') {
      throw new Error('main worktree should have aria-current=page');
    }
    if (mainChip.getAttribute('data-active') !== 'true') {
      throw new Error('main worktree should have data-active=true');
    }
    if (mainChip.getAttribute('data-tone') !== 'neutral') {
      throw new Error('clean main worktree should map to data-tone=neutral');
    }
    if (featureChip.getAttribute('data-tone') !== 'danger') {
      throw new Error('conflict worktree should map to data-tone=danger');
    }
  });

  test('does not render strip when worktrees is empty', () => {
    const { container } = render(renderTabs([], null));
    if (container.firstChild !== null) {
      throw new Error('empty worktrees should not render any DOM');
    }
  });

  test('clicking a chip triggers onSelect and active remains on rejection', () => {
    const worktrees = [
      buildWorktree({ id: 'main', name: 'main', isMain: true }),
      buildWorktree({ id: 'feature', name: 'feature/mobile', branch: 'feature/mobile' }),
    ];
    const onSelect = vi.fn(() => false);
    render(renderTabs(worktrees, 'main', { onSelect }));
    const nav = screen.getByTestId('mobile-worktree-tabs');
    const chips = Array.from(
      nav.querySelectorAll<HTMLButtonElement>('button[data-mobile-worktree-chip]'),
    );
    const featureChip = chips[1];
    if (!featureChip) throw new Error('expected feature chip');
    fireEvent.click(featureChip);
    if (onSelect.mock.calls.length !== 1) {
      throw new Error('onSelect should fire exactly once on chip click');
    }
    // 父级拒绝时（返回 false），active 应保持不变——由父级 setActiveWorktree 守住，新组件不写。
    const mainChip = chips[0];
    if (mainChip?.getAttribute('aria-current') !== 'page') {
      throw new Error('active chip must remain after rejected onSelect');
    }
  });

  test('busy disables all chips and clicks are ignored', () => {
    const worktrees = [
      buildWorktree({ id: 'main', name: 'main', isMain: true }),
      buildWorktree({ id: 'feature', name: 'feature/mobile', branch: 'feature/mobile' }),
    ];
    const onSelect = vi.fn();
    render(renderTabs(worktrees, 'main', { busy: true, onSelect }));
    const nav = screen.getByTestId('mobile-worktree-tabs');
    if (nav.getAttribute('data-busy') !== 'true') {
      throw new Error('busy nav should set data-busy=true');
    }
    const chips = Array.from(
      nav.querySelectorAll<HTMLButtonElement>('button[data-mobile-worktree-chip]'),
    );
    for (const chip of chips) {
      if (!chip.disabled) throw new Error('busy chips should be disabled');
    }
    fireEvent.click(chips[1] as HTMLElement);
    if (onSelect.mock.calls.length !== 0) {
      throw new Error('busy click must not call onSelect');
    }
  });

  test('arrow keys cycle focus across chips', () => {
    const worktrees = [
      buildWorktree({ id: 'main', name: 'main', isMain: true }),
      buildWorktree({ id: 'feature', name: 'feature/mobile', branch: 'feature/mobile' }),
      buildWorktree({ id: 'hotfix', name: 'hotfix/login', branch: 'hotfix/login' }),
    ];
    render(renderTabs(worktrees, 'main'));
    const nav = screen.getByTestId('mobile-worktree-tabs');
    const chips = Array.from(
      nav.querySelectorAll<HTMLButtonElement>('button[data-mobile-worktree-chip]'),
    );
    const lastChip = chips[chips.length - 1];
    lastChip?.focus();
    fireEvent.keyDown(lastChip as HTMLElement, { key: 'ArrowRight' });
    if (document.activeElement !== chips[0]) {
      throw new Error('ArrowRight from last chip should cycle to first chip');
    }
    fireEvent.keyDown(chips[0] as HTMLElement, { key: 'ArrowLeft' });
    if (document.activeElement !== lastChip) {
      throw new Error('ArrowLeft from first chip should cycle to last chip');
    }
    fireEvent.keyDown(chips[1] as HTMLElement, { key: 'Home' });
    if (document.activeElement !== chips[0]) {
      throw new Error('Home should focus first chip');
    }
    fireEvent.keyDown(chips[0] as HTMLElement, { key: 'End' });
    if (document.activeElement !== lastChip) {
      throw new Error('End should focus last chip');
    }
  });
});