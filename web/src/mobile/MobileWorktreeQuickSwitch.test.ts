import { register } from 'node:module';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ComponentType, ReactNode } from 'react';
import type { TFunction } from 'i18next';
import type { WorkbenchProject, WorkbenchWorktree } from '@/lib/types';
import type { MobileWorkbenchPanel } from './mobileWorkbenchState';

register('../pages/Settings/css-stub.mjs', import.meta.url);

const { default: i18n } = await import('../i18n');
await i18n.changeLanguage('zh');
const { MobileWorktreeQuickSwitch } = await import('./components/MobileWorktreeQuickSwitch');
const { MobileWorkbenchShell } = await import('./components/MobileWorkbenchShell');

type TestableMobileWorkbenchShellProps = {
  panel: MobileWorkbenchPanel;
  project: string | null;
  worktree: string | null;
  session: string | null;
  worktreeStatusDisabled?: boolean;
  onWorktreeStatusClick?: () => void;
  onPanelChange: (panel: MobileWorkbenchPanel) => void;
  children?: ReactNode;
};

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 quick switch 测试需要明确断言渲染结果，避免缺少关键无障碍属性或状态文案。
 *
 * Code Logic（这个函数做什么）:
 *   判断 rendered 是否包含 expected；缺失时抛出 Error 让 tsx 进程失败。
 */
function assertIncludes(rendered: string, expected: string, message: string): void {
  if (!rendered.includes(expected)) {
    throw new Error(`${message}: expected rendered markup to include ${expected}`);
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

const openMarkup = renderToStaticMarkup(createElement(MobileWorktreeQuickSwitch, baseProps));
assertIncludes(openMarkup, 'role="dialog"', 'quick switch should render dialog role');
assertIncludes(openMarkup, 'aria-modal="true"', 'quick switch should be modal');
assertIncludes(openMarkup, 'aria-current="true"', 'active worktree should use aria-current');
assertIncludes(openMarkup, '切换 Worktree', 'quick switch should render title');
assertIncludes(openMarkup, 'main', 'quick switch should render main worktree');
assertIncludes(openMarkup, '主工作区', 'quick switch should render main badge');
assertIncludes(openMarkup, 'feature/mobile', 'quick switch should render feature worktree');
assertIncludes(openMarkup, '1 处冲突', 'status label should use mobile status helper semantics');

const noProjectMarkup = renderToStaticMarkup(
  createElement(MobileWorktreeQuickSwitch, { ...baseProps, project: null, worktrees: [] }),
);
assertIncludes(noProjectMarkup, '先选择项目', 'quick switch should render no-project state');

const emptyMarkup = renderToStaticMarkup(
  createElement(MobileWorktreeQuickSwitch, { ...baseProps, worktrees: [] }),
);
assertIncludes(emptyMarkup, '暂无 worktree', 'quick switch should render empty state');

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
      worktreeStatusDisabled: true,
      onWorktreeStatusClick: () => undefined,
      onPanelChange: () => undefined,
    },
    createElement('section', null, 'panel'),
  ),
);
assertIncludes(shellMarkup, 'aria-haspopup="dialog"', 'worktree status pill should open a dialog');
assertIncludes(shellMarkup, 'disabled=""', 'disabled worktree status button should be disabled');
assertIncludes(shellMarkup, 'feature/mobile', 'worktree status button should render worktree name');
assertIncludes(shellMarkup, '自动化', 'mobile shell navigation should expose automation as a panel');
assertEqual(
  openMarkup.includes('自动化') ? 'present' : 'absent',
  'absent',
  'quick switch should not contain automation entry',
);

console.log('MobileWorktreeQuickSwitch.test.ts passed');
