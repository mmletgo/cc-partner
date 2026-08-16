// @vitest-environment jsdom
/**
 * MobileWorktreeTabs（移动端 worktree 工作区 tab 列表）单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   条必须像桌面 WorkbenchWorktreeBar：chip 切换、主/worktree 元信息、非主关闭、
 *   新建表单；并保证 CSS module 与 PointerPrimaryButton 接线不被回退。
 *
 * Code Logic（这个测试做什么）:
 *   静态断言源码 import；RTL 渲染验证 chip/meta/close/create 与 pointerDown 触发。
 */
import { afterEach, describe, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import type { WorkbenchWorktree } from '@/lib/types';
import { DEFAULT_WORKTREE_BRANCH_PREFIX } from '@/lib/workbenchWorktreeBranches';
import { MobileWorktreeTabs, type MobileWorktreeTabsProps } from './MobileWorktreeTabs';

const TEST_DIR = dirname(fileURLToPath(import.meta.url));

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

function defaultProps(
  overrides: Partial<MobileWorktreeTabsProps> = {},
): MobileWorktreeTabsProps {
  return {
    worktrees: [],
    activeWorktreeId: null,
    projectId: 'project-1',
    createOpen: false,
    createPrefix: DEFAULT_WORKTREE_BRANCH_PREFIX,
    createSuffix: '',
    pendingRemoval: null,
    error: null,
    onSelect: vi.fn(),
    onOpenCreate: vi.fn(),
    onCancelCreate: vi.fn(),
    onPrefixChange: vi.fn(),
    onSuffixChange: vi.fn(),
    onCreate: vi.fn(),
    onRequestRemove: vi.fn(),
    onCancelRemove: vi.fn(),
    onConfirmRemove: vi.fn(),
    ...overrides,
  };
}

function renderTabs(overrides: Partial<MobileWorktreeTabsProps> = {}) {
  return render(
    <I18nextProvider i18n={i18n}>
      <MobileWorktreeTabs {...defaultProps(overrides)} />
    </I18nextProvider>,
  );
}

function chipButtons(): HTMLButtonElement[] {
  const nav = screen.getByTestId('mobile-worktree-tabs');
  return Array.from(
    nav.querySelectorAll<HTMLButtonElement>('button[data-mobile-worktree-chip]'),
  );
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe('MobileWorktreeTabs', () => {
  test('imports its own CSS module and PointerPrimaryButton', () => {
    const source = readFileSync(join(TEST_DIR, 'MobileWorktreeTabs.tsx'), 'utf8');
    if (!source.includes("from './MobileWorktreeTabs.module.css'")) {
      throw new Error('MobileWorktreeTabs must import MobileWorktreeTabs.module.css');
    }
    if (source.includes("from '../MobileWorkbench.module.css'")) {
      throw new Error('MobileWorktreeTabs must not import MobileWorkbench.module.css');
    }
    if (!source.includes('PointerPrimaryButton')) {
      throw new Error('chip/create/close must use PointerPrimaryButton for IME first-tap');
    }
  });

  test('strip stays in shell chrome, not buried in the terminal panel', () => {
    const workbench = readFileSync(join(TEST_DIR, '../MobileWorkbench.tsx'), 'utf8');
    const terminal = readFileSync(join(TEST_DIR, 'MobileTerminalPanel.tsx'), 'utf8');
    if (!workbench.includes('worktreeStrip=')) {
      throw new Error('MobileWorkbench must pass worktreeStrip into the shell');
    }
    if (!workbench.includes('shouldShowMobileWorktreeStrip')) {
      throw new Error('MobileWorkbench must gate the strip with shouldShowMobileWorktreeStrip');
    }
    if (terminal.includes('MobileWorktreeTabs')) {
      throw new Error('MobileTerminalPanel must not render MobileWorktreeTabs');
    }
  });

  test('renders chips with main/linked meta and active highlight', () => {
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
    renderTabs({ worktrees, activeWorktreeId: 'main' });

    const nav = screen.getByTestId('mobile-worktree-tabs');
    if (nav.tagName.toLowerCase() !== 'nav') {
      throw new Error('MobileWorktreeTabs strip should be a <nav>');
    }

    const chips = chipButtons();
    if (chips.length !== 2) {
      throw new Error(`expected 2 chips, received ${chips.length}`);
    }
    const [mainChip, featureChip] = chips;
    if (!mainChip || !featureChip) throw new Error('expected two chips');
    if (mainChip.getAttribute('aria-current') !== 'page') {
      throw new Error('main worktree should have aria-current=page');
    }
    if (featureChip.getAttribute('data-tone') !== 'danger') {
      throw new Error('conflict worktree should map to data-tone=danger');
    }
    if (!mainChip.textContent?.includes('主工作区') && !mainChip.textContent?.includes('Main')) {
      throw new Error('main chip should show main/linked meta');
    }
    if (!screen.getByTestId('mobile-worktree-remove-feature')) {
      throw new Error('non-main chip should expose a remove button');
    }
  });

  test('empty worktrees still render create affordance', () => {
    renderTabs({ worktrees: [], activeWorktreeId: null });
    if (!screen.getByTestId('mobile-worktree-tabs')) {
      throw new Error('empty list should still render the strip');
    }
    if (!screen.getByTestId('mobile-worktree-create')) {
      throw new Error('empty list should still offer create');
    }
  });

  test('pointer down on a chip calls onSelect', () => {
    const worktrees = [
      buildWorktree({ id: 'main', name: 'main', isMain: true }),
      buildWorktree({ id: 'feature', name: 'feature/mobile', branch: 'feature/mobile' }),
    ];
    const onSelect = vi.fn(() => false);
    renderTabs({ worktrees, activeWorktreeId: 'main', onSelect });
    const featureChip = chipButtons()[1];
    if (!featureChip) throw new Error('expected feature chip');
    fireEvent.pointerDown(featureChip);
    if (onSelect.mock.calls.length !== 1) {
      throw new Error('onSelect should fire exactly once on pointerDown');
    }
  });

  test('pointer down on close requests remove without selecting', () => {
    const feature = buildWorktree({
      id: 'feature',
      name: 'feature/mobile',
      branch: 'feature/mobile',
    });
    const onSelect = vi.fn();
    const onRequestRemove = vi.fn();
    renderTabs({
      worktrees: [buildWorktree({ id: 'main', name: 'main', isMain: true }), feature],
      activeWorktreeId: 'main',
      onSelect,
      onRequestRemove,
    });
    fireEvent.pointerDown(screen.getByTestId('mobile-worktree-remove-feature'));
    if (onRequestRemove.mock.calls.length !== 1) {
      throw new Error('close should request remove');
    }
    if (onSelect.mock.calls.length !== 0) {
      throw new Error('close must not select the worktree');
    }
  });

  test('create button opens inline prefix/suffix form', () => {
    const onOpenCreate = vi.fn();
    renderTabs({
      worktrees: [buildWorktree({ id: 'main', name: 'main', isMain: true })],
      activeWorktreeId: 'main',
      onOpenCreate,
    });
    fireEvent.pointerDown(screen.getByTestId('mobile-worktree-create'));
    if (onOpenCreate.mock.calls.length !== 1) {
      throw new Error('create chip should open the inline form');
    }

    cleanup();
    renderTabs({
      worktrees: [buildWorktree({ id: 'main', name: 'main', isMain: true })],
      activeWorktreeId: 'main',
      createOpen: true,
    });
    if (!screen.getByTestId('mobile-worktree-create-form')) {
      throw new Error('createOpen should render prefix/suffix form');
    }
  });

  test('arrow keys cycle focus across chips', () => {
    const worktrees = [
      buildWorktree({ id: 'main', name: 'main', isMain: true }),
      buildWorktree({ id: 'feature', name: 'feature/mobile', branch: 'feature/mobile' }),
      buildWorktree({ id: 'hotfix', name: 'hotfix/login', branch: 'hotfix/login' }),
    ];
    renderTabs({ worktrees, activeWorktreeId: 'main' });
    const chips = chipButtons();
    const lastChip = chips[chips.length - 1];
    lastChip?.focus();
    fireEvent.keyDown(lastChip as HTMLElement, { key: 'ArrowRight' });
    if (document.activeElement !== chips[0]) {
      throw new Error('ArrowRight from last chip should cycle to first chip');
    }
  });
});
