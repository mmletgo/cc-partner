// @vitest-environment jsdom
/**
 * Workbench 浮层域 characterization 测试。
 *
 * Business Logic: 锁住 Prompt 优化浮层的打开/关闭与远端离线禁用、Session 搜索（⌘K 风格）打开/关闭与
 * resume→刷新 sessions / focus 的生命周期，这些是后续 controller 抽取必须保持的可观察行为。
 *
 * Code Logic: 通过工具栏按钮触发浮层开关；通过桩 WorkbenchSessionSearch 的 resume/close 按钮驱动
 * onResumed/onClose；断言 invoke 调用与浮层 DOM 存在性。
 */
import { describe, expect, test } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';

import {
  buildDependencyContextValue,
  buildLocalProject,
  buildProjectsContextValue,
  buildRemoteProject,
  buildSession,
  buildWorktree,
  flushMacrotasks,
  invokeCallsFor,
  renderWorkbench,
  setInvokeHandler,
  waitFor,
  waitForInvoke,
} from './testing/workbenchTestHarness';

const REMOTE_OFFLINE_ERROR = '远端设备不在线';

async function settle(): Promise<void> {
  await flushMacrotasks();
}

describe('Workbench overlay domain (characterization)', () => {
  test('prompt optimizer panel opens and closes via toolbar toggle', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession();
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [worktree];
        case 'list_workbench_sessions':
          return [session];
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
    await waitForInvoke('focus_workbench_session');

    // 点击“Prompt 优化”按钮打开浮层。
    fireEvent.click(screen.getByRole('button', { name: 'Prompt 优化' }));
    await waitFor(() => {
      if (!screen.queryByPlaceholderText(/粘贴要优化的需求/)) {
        throw new Error('prompt panel textarea not rendered');
      }
    });

    // 再次点击关闭：textarea 消失。
    fireEvent.click(screen.getByRole('button', { name: 'Prompt 优化' }));
    await waitFor(() => {
      if (screen.queryByPlaceholderText(/粘贴要优化的需求/)) {
        throw new Error('prompt panel should close');
      }
    });
  });

  test('prompt optimizer toggle is disabled when remote project offline', async () => {
    const remote = buildRemoteProject();
    const worktree = buildWorktree({ id: 'wt-r', projectId: remote.id });
    const session = buildSession({ id: 's-r', projectId: remote.id, worktreeId: 'wt-r' });

    // 第一轮所有读命令离线，让 remoteWriteDisabled=true 稳定。
    let offline = true;
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [remote];
        case 'list_workbench_worktrees':
          return offline ? Promise.reject(new Error(REMOTE_OFFLINE_ERROR)) : [worktree];
        case 'list_workbench_sessions':
          return offline ? Promise.reject(new Error(REMOTE_OFFLINE_ERROR)) : [session];
        case 'list_workbench_git_commits':
          return offline ? Promise.reject(new Error(REMOTE_OFFLINE_ERROR)) : [];
        case 'list_workbench_dir':
          return offline ? Promise.reject(new Error(REMOTE_OFFLINE_ERROR)) : [];
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [remote], activeProjectId: remote.id }),
      buildDependencyContextValue(),
    );
    await settle();

    // 远端离线时 Prompt 优化按钮 disabled（无 activeSession 时也 disabled，离线下两者叠加）。
    const button = screen.getByRole('button', { name: 'Prompt 优化' });
    expect(button.hasAttribute('disabled')).toBe(true);
  });

  test('session search opens, resume triggers sessions reload and focuses new session', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession();
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [worktree];
        case 'list_workbench_sessions':
          return [session];
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [];
        case 'focus_workbench_session':
          return { ok: true, sessionId: call.args.sessionId };
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    await waitForInvoke('focus_workbench_session');

    // 点击“搜索 session”打开浮层。
    fireEvent.click(screen.getByRole('button', { name: '搜索 session' }));
    const searchPanel = await waitFor(() => {
      const node = screen.getByTestId('workbench-session-search');
      if (node.getAttribute('data-open') !== 'true') {
        throw new Error('session search not open');
      }
      return node;
    });
    expect(searchPanel.getAttribute('data-project-id')).toBe(project.id);

    // 点击桩的 resume 按钮触发 onResumed('resumed-session')。
    const listCountBefore = invokeCallsFor('list_workbench_sessions').length;
    fireEvent.click(screen.getByTestId('workbench-session-search-resume'));
    await waitFor(() => {
      if (invokeCallsFor('list_workbench_sessions').length <= listCountBefore) {
        throw new Error('sessions not reloaded after resume');
      }
    });

    // onResumed 会触发 loadSessions + focusSession('resumed-session')。
    expect(invokeCallsFor('focus_workbench_session').some((c) => c.args.sessionId === 'resumed-session')).toBe(true);

    // resume 后浮层应关闭。
    await waitFor(() => {
      const node = screen.getByTestId('workbench-session-search');
      if (node.getAttribute('data-open') === 'true') {
        throw new Error('session search should close after resume');
      }
    });
  });

  test('session search close button calls onClose', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession();
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [worktree];
        case 'list_workbench_sessions':
          return [session];
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
    await waitForInvoke('focus_workbench_session');

    fireEvent.click(screen.getByRole('button', { name: '搜索 session' }));
    await waitFor(() => {
      const node = screen.getByTestId('workbench-session-search');
      if (node.getAttribute('data-open') !== 'true') throw new Error('not open');
    });

    fireEvent.click(screen.getByTestId('workbench-session-search-close'));
    await waitFor(() => {
      const node = screen.getByTestId('workbench-session-search');
      if (node.getAttribute('data-open') === 'true') throw new Error('should be closed');
    });
  });
});
