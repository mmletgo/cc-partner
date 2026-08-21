// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, describe, test } from 'vitest';
import { createElement } from 'react';
import { cleanup, render, screen } from '@testing-library/react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ComponentType, ReactNode } from 'react';
import type { TFunction } from 'i18next';
import type { WorkbenchProject, WorkbenchWorktree } from '@/lib/types';
import type { MobileWorkbenchPanel } from './mobileWorkbenchState';

const MOBILE_DIR = path.dirname(fileURLToPath(import.meta.url));

type TestableMobileWorkbenchShellProps = {
  panel: MobileWorkbenchPanel;
  project: string | null;
  worktree: string | null;
  session: string | null;
  hasActiveProject?: boolean;
  onPanelChange: (panel: MobileWorkbenchPanel) => void;
  onBackToProjects?: () => void;
  children?: ReactNode;
};

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 quick switch 测试需要明确断言渲染结果，避免缺少关键无障碍属性或状态文案。
 *
 * Code Logic（这个函数做什么）:
 *   判断 rendered 是否包含 expected；缺失时抛出 Error。
 */
function assertIncludes(rendered: string, expected: string, message: string): void {
  if (!rendered.includes(expected)) {
    throw new Error(`${message}: expected rendered markup to include ${expected}`);
  }
}

function assertNotIncludes(rendered: string, unexpected: string, message: string): void {
  if (rendered.includes(unexpected)) {
    throw new Error(`${message}: expected rendered markup not to include ${unexpected}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   quick switch 关闭态应完全不渲染，测试需要直接比较 SSR 输出。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual 与 expected 字符串；不一致时抛出 Error。
 */
function assertEqual(actual: string, expected: string, message: string): void {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${expected}, received ${actual}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   组件测试需要最小合法 WorkbenchProject DTO，避免重复手写字段造成样本漂移。
 *
 * Code Logic（这个函数做什么）:
 *   补齐 WorkbenchProject 必填字段并返回本机项目样本。
 */
function createProject(): WorkbenchProject {
  return {
    id: 'project-1',
    name: 'cc-partner',
    kind: 'local',
    deviceId: 'device-1',
    deviceName: 'This Mac',
    path: '/tmp/cc-partner',
    lastOpenedAt: '2026-07-05T00:00:00Z',
    createdAt: '2026-07-05T00:00:00Z',
    updatedAt: '2026-07-05T00:00:00Z',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   quick switch 列表需要展示不同 worktree 状态，测试要构造可复用 worktree DTO。
 *
 * Code Logic（这个函数做什么）:
 *   接收局部覆盖字段，补齐 WorkbenchWorktree 与 Git status 必填字段。
 */
function createWorktree(
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
    createdAt: overrides.createdAt ?? '2026-07-05T00:00:00Z',
    updatedAt: overrides.updatedAt ?? '2026-07-05T00:00:00Z',
  };
}

afterEach(() => {
  cleanup();
  document.body.style.overflow = '';
});

describe('MobileWorktreeQuickSwitch', () => {
  test('source consumes Dialog primitive without raw dialog role', () => {
    const source = readFileSync(
      path.join(MOBILE_DIR, 'components/MobileWorktreeQuickSwitch.tsx'),
      'utf8',
    );
    assertIncludes(source, '<Dialog', 'quick switch should render shared Dialog');
    assertIncludes(source, "from '@/components/primitives'", 'quick switch should import primitives');
    assertIncludes(source, 'titleId', 'quick switch Dialog should wire titleId');
    if (/role\s*=\s*["']dialog["']/.test(source)) {
      throw new Error('quick switch should not hand-write role=dialog');
    }
    if (/aria-modal\s*=\s*["']true["']/.test(source)) {
      throw new Error('quick switch should not hand-write aria-modal');
    }
    if (source.includes("window.addEventListener('keydown'")) {
      throw new Error('quick switch Escape should be owned by Dialog, not local listener');
    }
  });

  test(
    'renders dialog, worktree list, states, and shell nav',
    async () => {
      const { default: i18n } = await import('../i18n');
      await i18n.changeLanguage('zh');
      const { MobileWorktreeQuickSwitch } = await import('./components/MobileWorktreeQuickSwitch');
      const { MobileWorkbenchShell } = await import('./components/MobileWorkbenchShell');

      const t = i18n.getFixedT('zh', 'workbench') as TFunction<'workbench'>;
      const baseProps = {
        open: true,
        project: createProject(),
        worktrees: [
          createWorktree({ id: 'main', name: 'main', isMain: true }),
          createWorktree({
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
        ],
        activeWorktreeId: 'feature',
        t,
        onClose: () => undefined,
        onSelect: () => undefined,
        onPanelChange: () => undefined,
        onRefresh: () => undefined,
      };

      const closedMarkup = renderToStaticMarkup(
        createElement(MobileWorktreeQuickSwitch, { ...baseProps, open: false }),
      );
      assertEqual(closedMarkup, '', 'closed quick switch should render nothing');

      // Dialog portal 挂到 document.body，须用 jsdom + Testing Library，不能依赖 renderToStaticMarkup。
      const { unmount } = render(createElement(MobileWorktreeQuickSwitch, baseProps));
      const dialog = await screen.findByRole('dialog');
      assertIncludes(
        dialog.getAttribute('aria-modal') ?? '',
        'true',
        'quick switch should be modal via Dialog primitive',
      );
      assertIncludes(dialog.textContent ?? '', '切换 Worktree', 'quick switch should render title');
      assertIncludes(dialog.textContent ?? '', 'main', 'quick switch should render main worktree');
      assertIncludes(dialog.textContent ?? '', '主工作区', 'quick switch should render main badge');
      assertIncludes(
        dialog.textContent ?? '',
        'feature/mobile',
        'quick switch should render feature worktree',
      );
      assertIncludes(
        dialog.textContent ?? '',
        '1 处冲突',
        'status label should use mobile status helper semantics',
      );
      if (dialog.querySelector('[aria-current="true"]') == null) {
        throw new Error('active worktree should use aria-current');
      }
      assertEqual(
        (dialog.textContent ?? '').includes('自动化') ? 'present' : 'absent',
        'absent',
        'quick switch should not contain automation entry',
      );
      unmount();

      const { unmount: unmountNoProject } = render(
        createElement(MobileWorktreeQuickSwitch, {
          ...baseProps,
          project: null,
          worktrees: [],
        }),
      );
      assertIncludes(
        (await screen.findByRole('dialog')).textContent ?? '',
        '先选择项目',
        'quick switch should render no-project state',
      );
      unmountNoProject();

      const { unmount: unmountEmpty } = render(
        createElement(MobileWorktreeQuickSwitch, { ...baseProps, worktrees: [] }),
      );
      assertIncludes(
        (await screen.findByRole('dialog')).textContent ?? '',
        '暂无 worktree',
        'quick switch should render empty state',
      );
      unmountEmpty();

      const TestableMobileWorkbenchShell = MobileWorkbenchShell as ComponentType<
        TestableMobileWorkbenchShellProps
      >;
      const shellMarkup = renderToStaticMarkup(
        createElement(
          TestableMobileWorkbenchShell,
          {
            panel: 'terminal',
            project: 'cc-partner',
            worktree: 'feature/mobile',
            session: 'shell',
            hasActiveProject: true,
            onPanelChange: () => undefined,
            onBackToProjects: () => undefined,
          },
          createElement('section', null, 'panel'),
        ),
      );
      // 移动端 worktree 切换入口已迁移到 shell 固定 chrome 的 `MobileWorktreeTabs`；
      // shell 顶部的 worktree pill 现为只读 span，不再有 aria-haspopup/disabled 按钮交互。
      if (shellMarkup.includes('aria-haspopup="dialog"')) {
        throw new Error('shell worktree pill should not open a dialog after migration');
      }
      assertIncludes(
        shellMarkup,
        'feature/mobile',
        'worktree status pill should still render worktree name',
      );
      assertNotIncludes(
        shellMarkup,
        '自动化',
        'project-mode shell navigation should hide automation until experimental flag is on',
      );
      assertIncludes(
        shellMarkup,
        'data-nav-mode="project"',
        'terminal panel with active project should use project nav mode',
      );
    },
    20_000,
  );
});
