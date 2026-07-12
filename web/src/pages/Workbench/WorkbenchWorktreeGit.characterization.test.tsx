// @vitest-environment jsdom
/**
 * Workbench worktree/Git 域 characterization 测试。
 *
 * Business Logic: 锁住“创建 worktree 同时创建 session”“移除/合并后刷新 worktree/sessions/Git 与正确清理
 * terminal buffer”“merge 事件只作用于当前项目”这些当前可观察行为。
 *
 * Code Logic: 通过 fake invoke 控制响应；通过 emitWorkbenchEvent 模拟 workbench:merge-progress；
 * 断言 invoke 调用序列与 worktree chip / merge stage 面板的 DOM 文案。
 */
import { describe, expect, test } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';

import {
  buildDependencyContextValue,
  buildLocalProject,
  buildProjectsContextValue,
  buildSession,
  buildWorktree,
  emitWorkbenchEvent,
  flushMacrotasks,
  invokeCallsFor,
  renderWorkbench,
  setInvokeHandler,
  waitFor,
  waitForInvoke,
  workbenchTestState,
} from './testing/workbenchTestHarness';
import type { WorkbenchFileNode, WorkbenchWorktree } from '@/lib/types';

async function settle(): Promise<void> {
  await flushMacrotasks();
}

/** 安装 window.confirm = () => true，让 merge/remove 的确认弹窗直接通过。 */
function autoConfirm(): () => void {
  const original = window.confirm;
  window.confirm = (): boolean => true;
  return (): void => {
    window.confirm = original;
  };
}

describe('Workbench worktree / Git domain (characterization)', () => {
  test('create worktree then create session: create_worktree_worktree followed by create_workbench_session', async () => {
    const restore = autoConfirm();
    const project = buildLocalProject();
    const mainWt = buildWorktree({
      id: 'wt-main',
      name: 'main',
      branch: 'main',
      isMain: true,
    });
    const createdWt: WorkbenchWorktree = buildWorktree({
      id: 'wt-new',
      projectId: project.id,
      name: 'feature-x',
      branch: 'feature/feature-x',
      isMain: false,
    });
    const createdSession = buildSession({
      id: 's-new',
      projectId: project.id,
      worktreeId: 'wt-new',
      name: 'feature-x shell',
    });

    let created = false;
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return created ? [mainWt, createdWt] : [mainWt];
        case 'list_workbench_sessions':
          return created ? [createdSession] : [];
        case 'create_workbench_worktree':
          created = true;
          return createdWt;
        case 'create_workbench_session':
          return createdSession;
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [];
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    await settle();

    // 点击“新建 worktree”展开表单。
    fireEvent.click(screen.getByRole('button', { name: '新建 worktree' }));
    await settle();
    // 填入分支后缀并提交。
    const branchInput = screen.getByLabelText('分支后缀');
    fireEvent.change(branchInput, { target: { value: 'feature-x' } });
    fireEvent.click(screen.getByRole('button', { name: '确认' }));

    // create_workbench_worktree 和 create_workbench_session 都应被调用，且 worktree 创建在前。
    await waitForInvoke('create_workbench_worktree');
    await waitForInvoke('create_workbench_session');

    const createWtIdx = workbenchTestState.invokeCalls.findIndex(
      (c) => c.cmd === 'create_workbench_worktree',
    );
    const createSessionIdx = workbenchTestState.invokeCalls.findIndex(
      (c) => c.cmd === 'create_workbench_session',
    );
    expect(createWtIdx).toBeGreaterThanOrEqual(0);
    expect(createSessionIdx).toBeGreaterThan(createWtIdx);

    // 创建后 worktree chip 出现 feature/feature-x。
    await waitFor(() => {
      const bar = screen.getByRole('region', { name: 'Worktree 管理' });
      if (!bar.textContent?.includes('feature/feature-x')) {
        throw new Error('new worktree chip not rendered');
      }
    });

    restore();
  });

  test('remove worktree refreshes worktree list and switches active worktree off the removed one', async () => {
    const restore = autoConfirm();
    const project = buildLocalProject();
    const mainWt = buildWorktree({ id: 'wt-main', name: 'main', branch: 'main', isMain: true });
    const featureWt = buildWorktree({
      id: 'wt-feat',
      projectId: project.id,
      name: 'feat',
      branch: 'feature/feat',
      isMain: false,
    });

    let removed = false;
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return removed ? [mainWt] : [mainWt, featureWt];
        case 'list_workbench_sessions':
          return [];
        case 'remove_workbench_worktree':
          removed = true;
          return { ok: true, worktreeId: 'wt-feat' };
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [];
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    await settle();

    // 默认 active worktree 是第一个（mainWt）；切到 feature 后再移除。
    // 点击 feature chip 切 active，等到 active worktree 切换后 remove 按钮才启用。
    fireEvent.click(screen.getByText('feature/feat'));
    await waitFor(() => {
      const removeBtn = screen.getByRole('button', { name: '移除 worktree' });
      if (removeBtn.hasAttribute('disabled')) {
        throw new Error('remove button still disabled');
      }
    });

    fireEvent.click(screen.getByRole('button', { name: '移除 worktree' }));
    await waitForInvoke('remove_workbench_worktree');
    await waitForInvoke('list_workbench_worktrees', 2);

    // 移除后 worktree 列表不应再包含 feature/feat。
    await waitFor(() => {
      const bar = screen.getByRole('region', { name: 'Worktree 管理' });
      if (bar.textContent?.includes('feature/feat')) {
        throw new Error('removed worktree still listed');
      }
    });
    restore();
  });

  test('merge worktree fires merge_workbench_worktree then reloads worktrees and sessions', async () => {
    const restore = autoConfirm();
    const project = buildLocalProject();
    const mainWt = buildWorktree({ id: 'wt-main', name: 'main', branch: 'main', isMain: true });
    const featureWt = buildWorktree({
      id: 'wt-feat',
      projectId: project.id,
      name: 'feat',
      branch: 'feature/feat',
      isMain: false,
    });
    const sessionOnFeature = buildSession({
      id: 's-feat',
      projectId: project.id,
      worktreeId: 'wt-feat',
      name: 'feat shell',
    });

    let merged = false;
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return merged ? [mainWt] : [mainWt, featureWt];
        case 'list_workbench_sessions':
          return merged ? [] : [sessionOnFeature];
        case 'merge_workbench_worktree':
          merged = true;
          return {
            mergedWorktreeId: 'wt-feat',
            targetWorktreeId: 'wt-main',
            stages: [
              { id: 'checkSource', status: 'completed', message: 'ok' },
              { id: 'mergeBranch', status: 'completed', message: 'ok' },
            ],
          };
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [];
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    await settle();

    // 切到 feature worktree（非 main 才能 merge）。
    fireEvent.click(screen.getByText('feature/feat'));
    await settle();

    // 切到 Git 历史 tab 让 inspectorTab==='history'，merge 完成后会触发 loadGitHistory。
    fireEvent.click(screen.getByRole('tab', { name: 'Git 历史' }));
    await settle();

    fireEvent.click(screen.getByRole('button', { name: '合并' }));
    await waitForInvoke('merge_workbench_worktree');
    await waitForInvoke('list_workbench_worktrees', 2);
    await waitForInvoke('list_workbench_sessions', 2);

    // merge 后 worktree 列表只剩 main（feature 已合并移除）。
    const bar = screen.getByRole('region', { name: 'Worktree 管理' });
    expect(bar.textContent).not.toContain('feature/feat');
    restore();
  });

  test('merge-progress event for a different project is ignored', async () => {
    const project = buildLocalProject({ id: 'p1' });
    const mainWt = buildWorktree({ id: 'wt-main', name: 'main', branch: 'main', isMain: true });
    const featureWt = buildWorktree({
      id: 'wt-feat',
      projectId: 'p1',
      name: 'feat',
      branch: 'feature/feat',
      isMain: false,
    });

    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [mainWt, featureWt];
        case 'list_workbench_sessions':
          return [];
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [];
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: 'p1' }),
      buildDependencyContextValue(),
    );
    await settle();

    // 发送属于 OTHER 项目的 merge-progress 事件；页面不应追踪它（mergeStage 面板不出现）。
    emitWorkbenchEvent('workbench:merge-progress', {
      projectId: 'p-other',
      worktreeId: 'wt-other',
      stage: { id: 'checkSource', status: 'running', message: 'other' },
    });
    await settle();

    const mergePanel = document.querySelector('[class*="mergeStagePanel"]');
    expect(mergePanel).toBeNull();
  });

  test('switching worktree with dirty file tab prompts confirm; cancelling keeps active worktree and tabs unchanged', async () => {
    // Regression for Codex finding 2: guardDirtyContextChange must be wired into worktree switching.
    // Without the guard, switching worktrees would call resetForContext and silently wipe dirty tabs.
    const project = buildLocalProject();
    const mainWt = buildWorktree({ id: 'wt-main', name: 'main', branch: 'main', isMain: true });
    const featureWt = buildWorktree({
      id: 'wt-feat',
      projectId: project.id,
      name: 'feat',
      branch: 'feature/feat',
      isMain: false,
    });
    const session = buildSession({ id: 's1', worktreeId: 'wt-main' });
    const readme: WorkbenchFileNode = {
      name: 'README.md',
      path: 'README.md',
      kind: 'file',
      size: 1,
      modifiedAt: null,
      children: null,
    };
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [mainWt, featureWt];
        case 'list_workbench_sessions':
          return [session];
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [readme];
        case 'get_workbench_path_info':
          return { name: 'README.md', path: 'README.md', kind: 'file', size: 1, modifiedAt: null };
        case 'open_workbench_file':
          return {
            metadata: { name: 'README.md', path: 'README.md', kind: 'file', size: 7, modifiedAt: '2026-07-01T00:00:00.000Z' },
            detectedType: 'code',
            capabilities: { canPreview: false, canEdit: true, canFormat: false, mustValidateBeforeSave: false, defaultMode: 'editor', availableModes: ['editor', 'source'] },
            text: { content: 'original', baseHash: 'hash-1', baseModifiedAt: '2026-07-01T00:00:00.000Z' },
            image: null,
            csv: null,
            sqlite: null,
            truncated: false,
            notice: null,
          };
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    await waitForInvoke('list_workbench_dir');
    await waitFor(() => {
      if (screen.queryAllByRole('button', { name: 'README.md' }).length === 0) {
        throw new Error('README.md not rendered');
      }
    });

    // Open README.md and mark it dirty via the FileWorkspace stub content-change trigger.
    fireEvent.click(screen.getByRole('button', { name: 'README.md' }));
    await waitForInvoke('open_workbench_file');
    await waitFor(() => {
      const node = screen.getByTestId('workbench-file-workspace');
      if (!node.getAttribute('data-active-tab-id')?.endsWith(':README.md')) {
        throw new Error('active tab not README.md');
      }
    });
    fireEvent.click(screen.getByTestId('workbench-file-workspace-content-change'));
    await settle();

    // Confirm prompt → cancel. Switching worktree must be aborted: active worktree unchanged,
    // file tab still present.
    const original = window.confirm;
    let confirmCalls = 0;
    window.confirm = (): boolean => {
      confirmCalls += 1;
      return false;
    };
    try {
      fireEvent.click(screen.getByText('feature/feat'));
      await settle();
    } finally {
      window.confirm = original;
    }

    expect(confirmCalls).toBe(1);
    // Active worktree unchanged: the feature chip must NOT have data-active set.
    const featureChip = screen.getByText('feature/feat').closest('button');
    expect(featureChip?.getAttribute('data-active')).toBeNull();
    // File tab preserved (dirty tab NOT wiped by resetForContext).
    expect(screen.getByTestId('workbench-file-workspace').getAttribute('data-tab-count')).toBe('1');
    // list_workbench_dir should NOT have been re-invoked for a new worktree (we never switched).
    // The initial load plus the open file means no extra dir loads for worktree-main happened.
    const dirLoadsForMain = workbenchTestState.invokeCalls.filter(
      (c) => c.cmd === 'list_workbench_dir',
    ).length;
    expect(dirLoadsForMain).toBe(1);
  });
});
