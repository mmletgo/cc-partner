// @vitest-environment jsdom
/**
 * Workbench 终端域 characterization 测试。
 *
 * Business Logic: 锁住终端 focus/resize/split/close pane 调用语义、终端层在切到 browser/files 视图时
 * 保持常驻（仅 data-hidden 切换）、以及 session status 事件驱动 UI 更新等当前可观察行为。
 *
 * Code Logic: 通过 fake invoke 记录命令序列（waitForInvoke 等待 effect 链路完成）；通过 emitWorkbenchEvent
 * 触发 terminal-status 事件；通过 data-testid 桩组件 + DOM 断言验证视图切换。
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
  installTauriInternals,
  invokeCallsFor,
  renderWorkbench,
  setInvokeHandler,
  waitFor,
  waitForInvoke,
} from './testing/workbenchTestHarness';

async function settle(): Promise<void> {
  await flushMacrotasks();
}

describe('Workbench terminal domain (characterization)', () => {
  test('terminal focus fires focus_workbench_session once active session is established', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession({ id: 's1', name: 'main shell' });
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
          return { ok: true, sessionId: session.id };
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    await waitForInvoke('focus_workbench_session');

    const focusCalls = invokeCallsFor('focus_workbench_session');
    expect(focusCalls.length).toBeGreaterThan(0);
    expect(focusCalls[0].args.sessionId).toBe(session.id);
  });

  test('split pane right / down / close fire corresponding invoke commands', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession({
      id: 's1',
      name: 'main shell',
      supportsPanes: true,
      paneCount: 2,
    });
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
        case 'split_workbench_pane':
          return { ok: true, sessionId: session.id, direction: call.args.direction };
        case 'close_workbench_pane':
          return { ok: true, sessionId: session.id, closedWindow: false };
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    // 等 activeSession 真正建立，split 按钮才会启用。
    await waitForInvoke('focus_workbench_session');

    // 点击“左右分屏”触发 split_workbench_pane { direction: 'right' }。
    fireEvent.click(screen.getByRole('button', { name: '左右分屏' }));
    await waitFor(() => {
      const calls = invokeCallsFor('split_workbench_pane').filter(
        (c) => c.args.direction === 'right',
      );
      if (calls.length !== 1) throw new Error(`right split calls=${calls.length}`);
    });

    // 点击“上下分屏”触发 split_workbench_pane { direction: 'down' }。
    fireEvent.click(screen.getByRole('button', { name: '上下分屏' }));
    await waitFor(() => {
      const calls = invokeCallsFor('split_workbench_pane').filter(
        (c) => c.args.direction === 'down',
      );
      if (calls.length !== 1) throw new Error(`down split calls=${calls.length}`);
    });

    // 点击“关闭当前 pane”触发 close_workbench_pane。
    fireEvent.click(screen.getByRole('button', { name: '关闭当前 pane' }));
    await waitForInvoke('close_workbench_pane');
    expect(invokeCallsFor('close_workbench_pane').length).toBe(1);
  });

  test('terminal layer stays mounted (only data-hidden toggles) when switching to browser view', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession({ id: 's1', name: 'main shell' });
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
    await settle();

    const terminalLayer = document.querySelector('[class*="terminalLayer"]');
    expect(terminalLayer).toBeTruthy();
    // 初始 workspaceView='terminal'，data-hidden 不存在。
    expect(terminalLayer?.getAttribute('data-hidden')).toBeNull();

    // 点击“预览”切到 browser 视图。
    fireEvent.click(screen.getByRole('button', { name: '预览' }));
    await waitFor(() => {
      const layer = document.querySelector('[class*="terminalLayer"]');
      if (layer?.getAttribute('data-hidden') !== 'true') {
        throw new Error('terminal layer not hidden');
      }
    });

    // terminalLayer 仍存在（常驻语义），仅 data-hidden 切换；browser workspace 挂载。
    const terminalLayerAfter = document.querySelector('[class*="terminalLayer"]');
    expect(terminalLayerAfter).toBeTruthy();
    expect(terminalLayerAfter?.getAttribute('data-hidden')).toBe('true');
    expect(screen.getByTestId('workbench-browser-workspace')).toBeTruthy();
  });

  test('terminal-status event updates session status in inspector card', async () => {
    installTauriInternals();
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession({ id: 's1', name: 'main shell', status: 'running' });
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
    await settle();

    // 触发 terminal-status 事件把 s1 标记为 exited。
    emitWorkbenchEvent('workbench:terminal-status', {
      sessionId: 's1',
      status: 'exited',
      exitCode: 0,
      ts: Date.now(),
    });
    await waitFor(() => {
      const statusCard = document.querySelector('[class*="statusCard"]');
      if (!statusCard?.textContent?.includes('已退出')) {
        throw new Error('status not updated to exited');
      }
    });

    const statusCard = document.querySelector('[class*="statusCard"]');
    expect(statusCard?.textContent).toContain('已退出');
  });
});
